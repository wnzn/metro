//! HTTP handlers for personal model connections.
//!
//! Every connection belongs to the person who created it, and only they can
//! see, change, share, list models with or delete it: another user's
//! connection id answers 404, exactly like an unknown one, even when it is
//! shared with the caller (shared models are listed by
//! `GET /v1/models/available`, without keys or endpoints). API keys are
//! accepted and never returned (`has_key` says whether one is saved).

use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use rig_core::error::ProviderError as RigProviderError;
use rig_core::model::ModelList;
use rig_core::providers::registry::ProviderConfig;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::json;

use super::catalog::{self, Adapter, Capability, Entry, Execution};
use super::secret::MAX_API_KEY_BYTES;
use super::store::{self, Row};
use super::{
    LIST_TIMEOUT, MAX_CONNECTIONS_PER_USER, MAX_LISTED_MODELS, MAX_MODEL_CHARS, MAX_NAME_CHARS,
    UseError, normalize_base_url,
};
use crate::AppState;
use crate::api::ApiError;
use crate::auth::Human;
use crate::device_relay::DeviceError;

type ApiResult<T> = Result<T, ApiError>;

const NO_CONNECTION: &str = "no such model connection";

fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, message)
}
fn not_found() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, NO_CONNECTION)
}
fn conflict(message: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::CONFLICT, message)
}

/// A connection as its owner sees it.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct ConnectionView {
    pub id: String,
    pub name: String,
    pub provider_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Empty until a model is chosen.
    pub model: String,
    pub is_default: bool,
    pub has_key: bool,
    pub execution: Execution,
    /// Fixed by the provider: what this connection is used for.
    pub capability: Capability,
    /// Whether it embeds documents and media (EmbeddingGemma 2 adapters).
    pub media_inputs: bool,
}

impl From<&Row> for ConnectionView {
    fn from(row: &Row) -> Self {
        Self {
            id: row.id.clone(),
            name: row.name.clone(),
            provider_id: row.provider_id.clone(),
            base_url: row.base_url.clone(),
            model: row.model.clone(),
            is_default: row.is_default,
            has_key: row.sealed_key.is_some(),
            execution: catalog::find(&row.provider_id).map_or(Execution::Server, |e| e.execution),
            capability: row.capability,
            media_inputs: catalog::find(&row.provider_id).is_some_and(|e| e.media_inputs),
        }
    }
}

/// Connection ids are `pc_` and 32 lowercase hex digits.
pub fn valid_connection_id(id: &str) -> bool {
    id.strip_prefix("pc_").is_some_and(|hex| {
        hex.len() == 32
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

fn new_connection_id() -> String {
    format!("pc_{}", uuid::Uuid::new_v4().simple())
}

fn check_name(name: &str) -> ApiResult<String> {
    let name = name.trim();
    if name.is_empty()
        || name.chars().count() > MAX_NAME_CHARS
        || name.chars().any(char::is_control)
    {
        return Err(bad_request(format!(
            "name must be 1..={MAX_NAME_CHARS} printable characters"
        )));
    }
    Ok(name.to_owned())
}

/// A model id as written, or "" for none. Ids such as `qwen3:4b`,
/// `models/gemini-2.5-pro` or `org/model` are all accepted.
pub fn check_model(model: &str) -> ApiResult<String> {
    let model = model.trim();
    if model.chars().count() > MAX_MODEL_CHARS
        || model.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return Err(bad_request(format!(
            "model must be at most {MAX_MODEL_CHARS} characters without spaces"
        )));
    }
    Ok(model.to_owned())
}

fn check_key(key: &str) -> ApiResult<String> {
    let key = key.trim();
    if key.is_empty() || key.len() > MAX_API_KEY_BYTES || key.chars().any(char::is_control) {
        return Err(bad_request(format!(
            "api_key must be 1..={MAX_API_KEY_BYTES} bytes without control characters"
        )));
    }
    Ok(key.to_owned())
}

fn no_master_key() -> ApiError {
    conflict(
        "API keys cannot be saved: this server has no provider master key configured \
         (METRO_PROVIDER_MASTER_KEY or METRO_PROVIDER_KEY_FILE)",
    )
}

fn device_has_no_key() -> ApiError {
    bad_request(
        "device connections keep their credentials on your device; Metro accepts no api_key or \
         base_url for them",
    )
}

/// The provider `id`, if this server offers it.
fn entry_for(state: &AppState, id: &str) -> ApiResult<&'static Entry> {
    catalog::find(id)
        .filter(|e| catalog::usable(e, state.providers.allowed_base_urls()))
        .ok_or_else(|| bad_request("provider_id is not in this server's provider catalog"))
}

async fn load(state: &AppState, who: &Human, id: &str) -> ApiResult<Row> {
    if !valid_connection_id(id) {
        return Err(not_found());
    }
    let (user, id) = (who.id.clone(), id.to_owned());
    state
        .db
        .call(move |db| Ok(store::get(&db.lock(), &user, &id)?))
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(not_found)
}

// ---- Catalog ----

pub async fn catalog(State(state): State<AppState>, _who: Human) -> Json<serde_json::Value> {
    let allowed = state.providers.allowed_base_urls();
    Json(json!({
        "providers": catalog::view(allowed),
        "allowed_base_urls": allowed,
    }))
}

// ---- Connections ----

pub async fn list(State(state): State<AppState>, who: Human) -> ApiResult<Json<serde_json::Value>> {
    let user = who.id.clone();
    let rows = state
        .db
        .call(move |db| Ok(store::list(&db.lock(), &user)?))
        .await
        .map_err(ApiError::internal)?;
    let connections: Vec<ConnectionView> = rows.iter().map(ConnectionView::from).collect();
    Ok(Json(json!({ "connections": connections })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateBody {
    name: String,
    provider_id: String,
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    model: Option<String>,
    /// Explicitly make (or not make) this the default for its capability.
    /// When omitted, a connection created with a model becomes the default
    /// if the user has none of that capability yet.
    #[serde(default)]
    is_default: Option<bool>,
}

pub async fn create(
    State(state): State<AppState>,
    who: Human,
    body: Result<Json<CreateBody>, JsonRejection>,
) -> ApiResult<Response> {
    let Json(body) = body?;
    let name = check_name(&body.name)?;
    let entry = entry_for(&state, body.provider_id.trim())?;
    // An adapter's suggested model only when none is given at all; an
    // explicit "" (choose later) or any other model is kept as written.
    let model = check_model(body.model.as_deref().or(entry.default_model).unwrap_or(""))?;
    if body.is_default == Some(true) && model.is_empty() {
        return Err(conflict(
            "a default connection needs a model; choose one first",
        ));
    }
    let id = new_connection_id();

    let (base_url, sealed_key) = if entry.is_device() {
        if body.base_url.is_some() || body.api_key.is_some() {
            return Err(device_has_no_key());
        }
        (None, None)
    } else {
        let base_url = match body.base_url.as_deref() {
            Some(url) => {
                let url = normalize_base_url(url)
                    .map_err(|e| bad_request(format!("base_url is invalid: {e}")))?;
                if !state.providers.allowed_base_urls().contains(&url) {
                    return Err(bad_request(
                        "base_url must be exactly one of this server's allowed_base_urls",
                    ));
                }
                Some(url)
            }
            None if entry.default_base_url.is_some() => None,
            None => {
                return Err(bad_request(
                    "this provider has no public endpoint; choose a base_url from \
                     allowed_base_urls",
                ));
            }
        };
        let key = body.api_key.as_deref().map(check_key).transpose()?;
        if entry.requires_key && key.is_none() {
            return Err(bad_request("this provider needs an api_key"));
        }
        let sealed = match key {
            None => None,
            Some(key) => {
                let master = state.providers.master().ok_or_else(no_master_key)?;
                Some(
                    master
                        .seal(&who.id, &id, &key)
                        .map_err(ApiError::internal)?,
                )
            }
        };
        (base_url, sealed)
    };

    let mut row = Row {
        id,
        user_id: who.id.clone(),
        name,
        provider_id: entry.id.clone(),
        base_url,
        model,
        is_default: false,
        sealed_key,
        capability: entry.capability,
        config_rev: 0,
    };
    let explicit = body.is_default;
    let saved = state
        .db
        .call(move |db| {
            let mut conn = db.lock();
            row.is_default = match explicit {
                Some(flag) => flag,
                None => {
                    !row.model.is_empty()
                        && store::default_for(&conn, &row.user_id, row.capability)?.is_none()
                }
            };
            let inserted = store::insert(&mut conn, &row, MAX_CONNECTIONS_PER_USER)?;
            Ok(inserted.then_some(row))
        })
        .await
        .map_err(ApiError::internal)?;
    let Some(row) = saved else {
        return Err(conflict(format!(
            "you already have {MAX_CONNECTIONS_PER_USER} model connections"
        )));
    };
    tracing::info!(connection = %row.id, provider = %row.provider_id, "model connection created");
    Ok((StatusCode::CREATED, Json(ConnectionView::from(&row))).into_response())
}

/// `Some(None)` for an explicit `null`, `None` when the field is absent.
fn explicit_null<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateBody {
    #[serde(default)]
    name: Option<String>,
    /// `""` or `null` clears the model.
    #[serde(default, deserialize_with = "explicit_null")]
    model: Option<Option<String>>,
    #[serde(default)]
    is_default: Option<bool>,
    /// Absent: unchanged. A string: replaces the key. `null`: removes it
    /// (only for providers that do not need one).
    #[serde(default, deserialize_with = "explicit_null")]
    api_key: Option<Option<String>>,
}

pub async fn update(
    State(state): State<AppState>,
    who: Human,
    Path(id): Path<String>,
    body: Result<Json<UpdateBody>, JsonRejection>,
) -> ApiResult<Json<ConnectionView>> {
    let mut row = load(&state, &who, &id).await?;
    let Json(body) = body?;
    let entry = catalog::find(&row.provider_id)
        .ok_or_else(|| conflict("this connection's provider is no longer supported"))?;
    if let Some(name) = &body.name {
        row.name = check_name(name)?;
    }
    if let Some(model) = &body.model {
        row.model = check_model(model.as_deref().unwrap_or(""))?;
    }
    match &body.api_key {
        None => {}
        Some(_) if entry.is_device() => return Err(device_has_no_key()),
        Some(None) if entry.requires_key => {
            return Err(bad_request(
                "this provider needs an api_key; it cannot be removed",
            ));
        }
        Some(None) => row.sealed_key = None,
        Some(Some(key)) => {
            let key = check_key(key)?;
            let master = state.providers.master().ok_or_else(no_master_key)?;
            row.sealed_key = Some(
                master
                    .seal(&row.user_id, &row.id, &key)
                    .map_err(ApiError::internal)?,
            );
        }
    }
    if let Some(flag) = body.is_default {
        row.is_default = flag;
    }
    if row.is_default && row.model.is_empty() {
        return Err(conflict(
            "a default connection needs a model; choose one first",
        ));
    }
    let saved = {
        let row = row.clone();
        state
            .db
            .call(move |db| Ok(store::update(&mut db.lock(), &row)?))
            .await
            .map_err(ApiError::internal)?
    };
    let Some(rev) = saved else {
        return Err(not_found());
    };
    row.config_rev = rev;
    Ok(Json(ConnectionView::from(&row)))
}

pub async fn remove(
    State(state): State<AppState>,
    who: Human,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    if !valid_connection_id(&id) {
        return Err(not_found());
    }
    let (user, key) = (who.id.clone(), id.clone());
    let deleted = state
        .db
        .call(move |db| Ok(store::delete(&db.lock(), &user, &key)?))
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(not_found)?;
    if catalog::is_device(&deleted.provider_id) {
        // The relay already refuses the device token of a deleted connection
        // (it rechecks the connection on every request); this drops the
        // token hash and fails its pending jobs.
        if let Err(e) = state
            .device
            .connection_removed(&state.db, &deleted.id)
            .await
        {
            tracing::error!(error = %format!("{e:#}"), connection = %deleted.id, "device cleanup");
        }
    }
    tracing::info!(connection = %deleted.id, "model connection deleted");
    Ok(StatusCode::NO_CONTENT)
}

// ---- Model listing ----

pub async fn models(
    State(state): State<AppState>,
    who: Human,
    Path(id): Path<String>,
    body: Bytes,
) -> ApiResult<Json<serde_json::Value>> {
    let row = load(&state, &who, &id).await?;
    crate::api::empty_object(&body)?;
    let entry = catalog::find(&row.provider_id)
        .ok_or_else(|| conflict("this connection's provider is no longer supported"))?;
    let started_at = std::time::Instant::now();
    let models = match entry.adapter {
        Adapter::Device => state
            .device
            .models(&state.db, &who.id, &row.id)
            .await
            .map_err(device_error)?,
        Adapter::SystemOne | Adapter::Embeddings => list_typed_models(&state, entry, &row).await?,
        Adapter::Registered(_) | Adapter::Custom => list_server_models(&state, entry, &row).await?,
    };
    let models = clean_model_ids(models);
    if super::listed::server_chat(entry, &row)
        && !state
            .providers
            .remember_chat_listing(&state.db, row, started_at, &models)
            .await
            .map_err(ApiError::internal)?
    {
        return Err(conflict(
            "the connection changed while fetching models; fetch them again",
        ));
    }
    Ok(Json(json!({ "models": models })))
}

/// Valid, unique model ids, sorted, at most [`MAX_LISTED_MODELS`].
pub fn clean_model_ids(models: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut ids: Vec<String> = models
        .into_iter()
        .filter_map(|m| check_model(&m).ok().filter(|m| !m.is_empty()))
        .collect();
    ids.sort();
    ids.dedup();
    ids.truncate(MAX_LISTED_MODELS);
    ids
}

pub(crate) fn use_error(e: UseError) -> ApiError {
    let status = match e {
        UseError::NotFound => StatusCode::NOT_FOUND,
        UseError::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        UseError::Unsupported | UseError::WrongCapability { .. } | UseError::MediaUnsupported => {
            StatusCode::UNPROCESSABLE_ENTITY
        }
        UseError::Busy => StatusCode::SERVICE_UNAVAILABLE,
        UseError::Revoked => StatusCode::FORBIDDEN,
        _ => StatusCode::CONFLICT,
    };
    ApiError::new(status, e.to_string())
}

fn device_error(e: DeviceError) -> ApiError {
    let status = match &e {
        // A lost grant looks like a connection the caller can't see.
        DeviceError::NotFound | DeviceError::AccessRevoked => return not_found(),
        DeviceError::NotPaired => StatusCode::CONFLICT,
        DeviceError::InvalidRequest(_) => StatusCode::UNPROCESSABLE_ENTITY,
        DeviceError::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
        DeviceError::Offline | DeviceError::QueueFull | DeviceError::NotClaimed => {
            StatusCode::SERVICE_UNAVAILABLE
        }
        DeviceError::TimedOut => StatusCode::GATEWAY_TIMEOUT,
        DeviceError::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        _ => StatusCode::BAD_GATEWAY,
    };
    ApiError::new(status, e.to_string())
}

async fn list_server_models(state: &AppState, entry: &Entry, row: &Row) -> ApiResult<Vec<String>> {
    if !entry.lists_models {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "this provider offers no model list; enter the model id yourself",
        ));
    }
    let config = state
        .providers
        .server_config(entry, row)
        .map_err(use_error)?;
    let Ok(_permit) = state.providers.inner.listings.try_acquire() else {
        return Err(ApiError::unavailable(
            "too many model listings are running; try again shortly",
        ));
    };
    match tokio::time::timeout(LIST_TIMEOUT, list_upstream(config, state.providers.http())).await {
        Ok(Ok(list)) => Ok(list.data.into_iter().map(|m| m.id).collect()),
        Ok(Err(e)) => {
            // Only the error's kind is logged: upstream text may echo input.
            tracing::warn!(
                connection = %row.id,
                provider = %row.provider_id,
                kind = error_kind(&e),
                "model listing failed"
            );
            Err(ApiError::new(
                StatusCode::BAD_GATEWAY,
                match e {
                    RigProviderError::InvalidAuthentication(_) => {
                        "the provider rejected the API key"
                    }
                    _ => {
                        "the provider did not return a model list; you can still enter the \
                         model id yourself"
                    }
                },
            ))
        }
        Err(_) => Err(ApiError::new(
            StatusCode::GATEWAY_TIMEOUT,
            "the provider took too long to list its models; you can enter the model id yourself",
        )),
    }
}

/// A System One (`{models: [{name}]}`) or embeddings (`{data: [{id}]}`)
/// listing, with the connection's own endpoint and key, no redirects.
async fn list_typed_models(state: &AppState, entry: &Entry, row: &Row) -> ApiResult<Vec<String>> {
    let (base, key) = state
        .providers
        .typed_endpoint(entry, row)
        .map_err(use_error)?;
    let Ok(_permit) = state.providers.inner.listings.try_acquire() else {
        return Err(ApiError::unavailable(
            "too many model listings are running; try again shortly",
        ));
    };
    match super::typed::list_models(&base, key.as_deref(), entry.capability).await {
        // OpenAI lists every model; only its embedding models apply here.
        Ok(models) if entry.id == catalog::OPENAI_EMBEDDINGS => Ok(models
            .into_iter()
            .filter(|m| m.contains("embedding"))
            .collect()),
        Ok(models) => Ok(models),
        Err(e) => {
            tracing::warn!(connection = %row.id, provider = %row.provider_id, error = %e,
                "typed model listing failed");
            Err(ApiError::new(
                e.status(),
                format!("{e}; you can still enter the model name yourself"),
            ))
        }
    }
}

/// One listing with the provider's own wire: OpenAI-family `GET /models`
/// (the dialect's path), Anthropic `GET /v1/models`, Gemini `GET /v1beta/models`.
#[allow(clippy::result_large_err)]
async fn list_upstream(
    config: ProviderConfig,
    http: rig_core::http_client::DynHttpClient,
) -> Result<ModelList, RigProviderError> {
    match config {
        ProviderConfig::OpenAi(c) => c.connect(http).list_models().await,
        ProviderConfig::Anthropic(c) => c.connect(http).list_models().await,
        ProviderConfig::Gemini(c) => c.connect(http).list_models().await,
    }
}

fn error_kind(e: &RigProviderError) -> &'static str {
    match e {
        RigProviderError::Http(_) => "http",
        RigProviderError::Json(_) => "json",
        RigProviderError::Url(_) => "url",
        RigProviderError::Request(_) => "request",
        RigProviderError::Response(_) => "response",
        RigProviderError::Provider(_) => "provider",
        RigProviderError::ProviderResponse(_) => "provider_response",
        RigProviderError::InvalidAuthentication(_) => "invalid_authentication",
        _ => "other",
    }
}
