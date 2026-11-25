//! Model connections: which model a person's agent runs, System One
//! decisions and embeddings use.
//!
//! Each user may save connections to model providers. Every connection has
//! one fixed **capability** from its adapter (`chat`, `system_one` or
//! `embeddings`; rows from before capabilities are `chat`) and is never used
//! for another. Each capability has at most one default per person. Every
//! agent run a person starts (personal Chat, a Space room, and the thread
//! runs that run delegates) uses that person's default *chat* connection,
//! never anyone else's unless they explicitly chose a connection shared with
//! them ([`sharing`]). A user with no default chat connection falls back to
//! the operator's `METRO_MODEL` environment model, as before. A chosen
//! connection that cannot be used fails the run with a clear error; it never
//! silently falls back to the environment model or to another connection.
//!
//! There are two kinds of connection:
//!
//! - **Server** connections are Rig adapters from the compiled registry
//!   (OpenAI, Anthropic Messages, Gemini and the registered
//!   OpenAI-compatible gateways) or the custom OpenAI-compatible adapter.
//!   The server calls the provider. An API key is write-only through the
//!   API, sealed with the operator's master key ([`secret`]) and opened only
//!   to build the model for a run or a model listing. Base URLs are either
//!   the dialect's own HTTPS endpoint or an exact entry of
//!   `METRO_PROVIDER_ALLOWED_BASE_URLS`, and requests follow no redirects.
//! - **Device** connections (`device-*`) run in the owner's own
//!   `metro-connector`; the server stores no vendor credential for them and
//!   relays requests through [`crate::device_relay`].
//!
//! Typed calls ([`decide`], [`embed`], [`media_embed`]) take only the
//! payload: the model, endpoint and key always come from the saved
//! connection.
//!
//! **Media embeddings.** Only the EmbeddingGemma 2 adapters
//! (`embeddinggemma-2` on an operator-approved endpoint,
//! `device-embeddinggemma-2` on the owner's device) accept documents and
//! media ([`media_embed`]); their catalog entries say `media_inputs: true`.
//! They also embed text for message search with the usual [`embed`]
//! (`POST {base}/embeddings`). A text-only embeddings connection never
//! stands in for media: it answers [`UseError::MediaUnsupported`].

pub mod api;
mod budget;
pub mod catalog;
pub mod guard;
mod listed;
pub mod models;
pub mod secret;
pub mod sharing;
pub mod store;
pub(crate) mod turn;
pub mod typed;

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, bail};
use axum::Router;
use axum::routing::{get, post};
use rig_core::DynModel;
use rig_core::catalog::{Catalog, ModelFacts};
use rig_core::http_client::DynHttpClient;
use rig_core::operation::Completion;
use rig_core::providers::openai::OpenAIConfig;
use rig_core::providers::registry::ProviderConfig;
use tokio::sync::Semaphore;

use crate::AppState;
use crate::db::Db;
use crate::device_relay::Caller;
use crate::device_relay::DeviceError;
use catalog::{Adapter, Entry};
use guard::{Grant, GuardedHttp};
use secret::{MasterKey, OpenError};
use sharing::{Access, Authorized};
use store::Row;

pub use catalog::{Capability, Execution};
pub use store::Visibility;

pub const ALLOWED_BASE_URLS_ENV: &str = "METRO_PROVIDER_ALLOWED_BASE_URLS";
/// Most connections one user may save.
pub const MAX_CONNECTIONS_PER_USER: usize = 20;
/// Most model ids returned by one listing.
pub const MAX_LISTED_MODELS: usize = 1_000;
/// Longest model id, in characters.
pub const MAX_MODEL_CHARS: usize = 256;
/// Longest connection name, in characters.
pub const MAX_NAME_CHARS: usize = 64;
/// Wall clock for one upstream model listing, every page included.
const LIST_TIMEOUT: Duration = Duration::from_secs(20);
/// Model listings in flight at once, across the server.
const MAX_LISTINGS: usize = 4;
/// Longest operator-approved base URL.
const MAX_BASE_URL_CHARS: usize = 2_048;
/// Typed calls (System One, embeddings) in flight at once, across the server.
pub const MAX_TYPED_CALLS: usize = 16;
/// Requests other people may have in flight at once on one shared
/// connection (the owner's own use is not counted).
pub const MAX_BORROWED_IN_FLIGHT: usize = 4;
/// Media embeddings calls in flight at once, across the server (each may
/// carry an 8 MiB asset); they also count against [`MAX_TYPED_CALLS`].
pub const MAX_MEDIA_CALLS: usize = 4;
/// Of those, what one person (stable user id) may have in flight, whatever
/// connections they use, and what one connection may serve at once,
/// whoever uses it: nobody takes every slot from everyone else.
pub const MAX_MEDIA_CALLS_PER_CALLER: usize = 2;
pub const MAX_MEDIA_CALLS_PER_CONNECTION: usize = 3;

/// Create the connection table. Called from `Db::init`.
pub fn init(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    store::init(conn).context("creating provider_connections")
}

/// `GET /v1/provider-catalog`, `GET|POST /v1/providers`,
/// `PATCH|DELETE /v1/providers/{id}`, `POST /v1/providers/{id}/models`
/// (also `.../model-list/refresh`: list and merge into the connection's
/// models), `GET|POST|PATCH|DELETE /v1/providers/{id}/model-list`,
/// `GET|PUT /v1/providers/{id}/sharing` and `GET /v1/models/available`.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/provider-catalog", get(api::catalog))
        .route("/v1/providers", get(api::list).post(api::create))
        .route(
            "/v1/providers/{id}",
            axum::routing::patch(api::update).delete(api::remove),
        )
        .route("/v1/providers/{id}/models", post(api::models))
        .route(
            "/v1/providers/{id}/model-list",
            get(models::get_list)
                .post(models::add)
                .patch(models::patch_one)
                .delete(models::remove_one),
        )
        .route("/v1/providers/{id}/model-list/refresh", post(api::models))
        .route(
            "/v1/providers/{id}/sharing",
            get(sharing::get_sharing).put(sharing::put_sharing),
        )
        .route("/v1/models/available", get(sharing::available))
}

/// Server state for provider connections.
#[derive(Clone)]
pub struct ProviderService {
    inner: Arc<Inner>,
}

struct Inner {
    master: Option<MasterKey>,
    allowed: Vec<String>,
    http: DynHttpClient,
    listings: Semaphore,
    typed_calls: Arc<Semaphore>,
    media_calls: Arc<Semaphore>,
    /// Media calls in flight by caller and by connection.
    media_in_flight: Arc<Mutex<MediaInFlight>>,
    /// The in-flight cap of each shared connection, by connection id.
    borrowed: Mutex<HashMap<String, Arc<Semaphore>>>,
}

impl ProviderService {
    /// Read `METRO_PROVIDER_MASTER_KEY` / `METRO_PROVIDER_KEY_FILE` and
    /// `METRO_PROVIDER_ALLOWED_BASE_URLS`. Neither is required; a malformed
    /// value is an error.
    /// `_db` is accepted for the state constructor's convenience; connections
    /// are read through `AppState::db`.
    pub fn from_env(db_path: &Path, _db: Db) -> anyhow::Result<Self> {
        let master = MasterKey::from_env(db_path)?;
        let allowed = parse_allowed_base_urls(
            std::env::var(ALLOWED_BASE_URLS_ENV)
                .ok()
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty()),
        )?;
        if master.is_none() {
            tracing::info!(
                "no provider master key is configured: personal connections that need an API key \
                 cannot be saved or used"
            );
        }
        Self::new(master, allowed)
    }

    /// A service with an explicit master key and allowlist (already
    /// normalized with [`normalize_base_url`]), as tests and embedders build it.
    pub fn new(master: Option<MasterKey>, allowed: Vec<String>) -> anyhow::Result<Self> {
        Ok(Self {
            inner: Arc::new(Inner {
                master,
                allowed,
                http: no_redirect_client()?,
                listings: Semaphore::new(MAX_LISTINGS),
                typed_calls: Arc::new(Semaphore::new(MAX_TYPED_CALLS)),
                media_calls: Arc::new(Semaphore::new(MAX_MEDIA_CALLS)),
                media_in_flight: Arc::new(Mutex::new(MediaInFlight::default())),
                borrowed: Mutex::new(HashMap::new()),
            }),
        })
    }

    pub fn has_master_key(&self) -> bool {
        self.inner.master.is_some()
    }

    /// The operator-approved base URLs, normalized.
    pub fn allowed_base_urls(&self) -> &[String] {
        &self.inner.allowed
    }

    pub(crate) fn master(&self) -> Option<&MasterKey> {
        self.inner.master.as_ref()
    }

    pub(crate) fn http(&self) -> DynHttpClient {
        self.inner.http.clone()
    }

    /// The in-flight cap other people share on connection `id`.
    pub(crate) fn borrow_slots(&self, id: &str) -> Arc<Semaphore> {
        let mut map = self
            .inner
            .borrowed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        map.entry(id.to_owned())
            .or_insert_with(|| Arc::new(Semaphore::new(MAX_BORROWED_IN_FLIGHT)))
            .clone()
    }

    /// The endpoint and opened key of a typed server connection.
    pub(crate) fn typed_endpoint(
        &self,
        entry: &Entry,
        row: &Row,
    ) -> Result<(String, Option<String>), UseError> {
        if !matches!(entry.adapter, Adapter::SystemOne | Adapter::Embeddings) {
            return Err(UseError::Unsupported);
        }
        let key = self.open_key(row)?;
        if entry.requires_key && key.is_none() {
            return Err(UseError::KeyMissing);
        }
        Ok((
            self.effective_base_url(entry, row.base_url.as_deref())?,
            key,
        ))
    }

    /// The API key of `row`, opened. `Ok(None)` for a connection without one.
    pub(crate) fn open_key(&self, row: &Row) -> Result<Option<String>, UseError> {
        let Some(sealed) = &row.sealed_key else {
            return Ok(None);
        };
        let Some(master) = self.master() else {
            return Err(UseError::NoMasterKey);
        };
        match master.open(&row.user_id, &row.id, sealed) {
            Ok(key) => Ok(Some(key)),
            Err(OpenError::Malformed | OpenError::Rejected) => Err(UseError::KeyUnreadable),
        }
    }

    /// The Rig provider configuration of a server connection, with its key.
    pub(crate) fn server_config(
        &self,
        entry: &Entry,
        row: &Row,
    ) -> Result<ProviderConfig, UseError> {
        let key = self.open_key(row)?;
        if entry.requires_key && key.is_none() {
            return Err(UseError::KeyMissing);
        }
        let base_url = self.effective_base_url(entry, row.base_url.as_deref())?;
        let key = key.unwrap_or_default();
        let config = match entry.adapter {
            Adapter::Registered(id) => id.config(key).ok_or(UseError::Unsupported)?,
            Adapter::Custom => {
                ProviderConfig::OpenAi(OpenAIConfig::with_key(&catalog::CUSTOM_DIALECT, key))
            }
            Adapter::Device | Adapter::SystemOne | Adapter::Embeddings => {
                return Err(UseError::Unsupported);
            }
        };
        Ok(config.with_base_url(base_url))
    }

    /// Where a server connection sends requests: its approved override, or
    /// the dialect's own HTTPS endpoint. Rechecked on every use, so an
    /// endpoint the operator removed from the allowlist stops working.
    pub(crate) fn effective_base_url(
        &self,
        entry: &Entry,
        base_url: Option<&str>,
    ) -> Result<String, UseError> {
        match base_url {
            Some(url) if self.inner.allowed.iter().any(|a| a == url) => Ok(url.to_owned()),
            Some(_) => Err(UseError::EndpointNotApproved),
            None => entry
                .default_base_url
                .clone()
                .ok_or(UseError::EndpointNotApproved),
        }
    }
}

#[derive(Default)]
struct MediaInFlight {
    by_caller: HashMap<String, usize>,
    by_connection: HashMap<String, usize>,
}

/// A media call's place in the per-caller and per-connection caps, given
/// back when dropped (answered, failed or cancelled).
struct MediaSlot {
    table: Arc<Mutex<MediaInFlight>>,
    caller: String,
    connection: String,
    _global: tokio::sync::OwnedSemaphorePermit,
}

impl ProviderService {
    /// A media call slot for `caller` on `connection`, or `Busy` when the
    /// server, that person or that connection is at its cap.
    fn media_slot(&self, caller: &str, connection: &str) -> Result<MediaSlot, ProviderError> {
        let global = self
            .inner
            .media_calls
            .clone()
            .try_acquire_owned()
            .map_err(|_| ProviderError::Busy)?;
        let mut t = self
            .inner
            .media_in_flight
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let by_caller = t.by_caller.get(caller).copied().unwrap_or(0);
        let by_connection = t.by_connection.get(connection).copied().unwrap_or(0);
        if by_caller >= MAX_MEDIA_CALLS_PER_CALLER
            || by_connection >= MAX_MEDIA_CALLS_PER_CONNECTION
        {
            return Err(ProviderError::Busy);
        }
        *t.by_caller.entry(caller.to_owned()).or_default() += 1;
        *t.by_connection.entry(connection.to_owned()).or_default() += 1;
        Ok(MediaSlot {
            table: self.inner.media_in_flight.clone(),
            caller: caller.to_owned(),
            connection: connection.to_owned(),
            _global: global,
        })
    }
}

impl Drop for MediaSlot {
    fn drop(&mut self) {
        let mut guard = self
            .table
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let t = &mut *guard;
        for (map, key) in [
            (&mut t.by_caller, &self.caller),
            (&mut t.by_connection, &self.connection),
        ] {
            if let Some(n) = map.get_mut(key) {
                *n -= 1;
                if *n == 0 {
                    map.remove(key);
                }
            }
        }
    }
}

/// A requests client that follows no redirect, so no credential is ever
/// forwarded to another URL; a 3xx answer is an error.
fn no_redirect_client() -> anyhow::Result<DynHttpClient> {
    let client = rig_reqwest::reqwest::Client::builder()
        .redirect(rig_reqwest::reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .build()
        .context("building the provider HTTP client")?;
    Ok(DynHttpClient::new(rig_reqwest::ReqwestClient::from(client)))
}

/// Parse `METRO_PROVIDER_ALLOWED_BASE_URLS`: a JSON array of absolute
/// `http`/`https` URLs without credentials, query or fragment. Each is kept
/// as written minus trailing slashes, and matched exactly.
pub fn parse_allowed_base_urls(raw: Option<&str>) -> anyhow::Result<Vec<String>> {
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    let urls: Vec<String> = serde_json::from_str(raw).with_context(|| {
        format!("{ALLOWED_BASE_URLS_ENV} must be a JSON array of URLs, such as [\"http://127.0.0.1:11434/v1\"]")
    })?;
    let mut out: Vec<String> = Vec::with_capacity(urls.len());
    for url in urls {
        let url = normalize_base_url(&url)
            .with_context(|| format!("{ALLOWED_BASE_URLS_ENV} has an invalid entry"))?;
        if !out.contains(&url) {
            out.push(url);
        }
    }
    Ok(out)
}

/// A base URL in the form it is stored and compared: trimmed, without
/// trailing slashes, and checked to be a plain absolute HTTP(S) URL.
pub fn normalize_base_url(url: &str) -> anyhow::Result<String> {
    let url = url.trim().trim_end_matches('/');
    if url.is_empty() || url.chars().count() > MAX_BASE_URL_CHARS {
        bail!("a base URL must be 1..={MAX_BASE_URL_CHARS} characters");
    }
    let parsed = rig_reqwest::reqwest::Url::parse(url).context("not an absolute URL")?;
    if !matches!(parsed.scheme(), "http" | "https") {
        bail!("a base URL must use http or https");
    }
    if parsed.host_str().is_none_or(str::is_empty) {
        bail!("a base URL must name a host");
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        bail!("a base URL must not carry credentials");
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        bail!("a base URL must not have a query or fragment");
    }
    Ok(url.to_owned())
}

/// Why a connection could not be used. Messages are for the connection's
/// owner and carry no key material.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UseError {
    NotFound,
    NoModel,
    NoMasterKey,
    KeyUnreadable,
    KeyMissing,
    EndpointNotApproved,
    Unsupported,
    DeviceOffline,
    TurnModelNotAllowed,
    TurnEffortNotAllowed,
    TurnChanged,
    /// The connection serves another capability.
    WrongCapability {
        expected: Capability,
        actual: Capability,
    },
    /// A shared subscription preset for this model needs the caller's own
    /// ChatGPT sign-in or OpenAI connection, and they have none.
    PresetNeedsOwnConnection(String),
    /// The share this use relied on was revoked, the caller or owner left
    /// the Space, or the connection was deleted.
    Revoked,
    /// The shared connection is at its in-flight cap.
    Busy,
    /// A documents-and-media call on a connection whose adapter embeds text
    /// only.
    MediaUnsupported,
    Internal,
}

impl std::fmt::Display for UseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UseError::WrongCapability { expected, actual } => {
                return write!(
                    f,
                    "that model connection is a {} connection, not a {} one",
                    actual.as_str(),
                    expected.as_str()
                );
            }
            UseError::PresetNeedsOwnConnection(model) => {
                return write!(
                    f,
                    "this shared preset runs {model} on your own Sign in with ChatGPT \
                     connection, and you have none. Metro never uses someone else's ChatGPT \
                     subscription, and never switches to an API-billed key on its own. Add \
                     Sign in with ChatGPT in Settings and pick the preset again, or pick your \
                     own OpenAI connection explicitly to use your API key"
                );
            }
            _ => {}
        }
        f.write_str(match self {
            UseError::NotFound => "your default model connection no longer exists",
            UseError::NoModel => "your model connection has no model selected",
            UseError::NoMasterKey => {
                "your model connection has a saved API key, but this server has no provider \
                 master key configured, so it cannot be used; ask the operator to set \
                 METRO_PROVIDER_MASTER_KEY or METRO_PROVIDER_KEY_FILE"
            }
            UseError::KeyUnreadable => {
                "the saved API key of your model connection cannot be decrypted with this \
                 server's master key; enter the key again"
            }
            UseError::KeyMissing => "your model connection needs an API key; add one in Settings",
            UseError::EndpointNotApproved => {
                "your model connection's endpoint is not on this server's approved list"
            }
            UseError::Unsupported => {
                "your model connection's provider is not supported by this server"
            }
            UseError::DeviceOffline => "the device for this model connection is offline",
            UseError::TurnModelNotAllowed => "model_id is not selectable on that connection",
            UseError::TurnEffortNotAllowed => {
                "reasoning_effort is not supported by that model on that connection"
            }
            UseError::TurnChanged => "that model connection changed; choose the model again",
            UseError::Revoked => {
                "access to that shared model connection has ended: its owner stopped sharing \
                 it, someone left the Space, or it was deleted"
            }
            UseError::Busy => "that shared model connection is busy; try again shortly",
            UseError::MediaUnsupported => {
                "that embeddings connection embeds text only and cannot index documents or \
                 media; choose an EmbeddingGemma 2 connection (embeddinggemma-2 or \
                 device-embeddinggemma-2)"
            }
            UseError::Internal => "your model connection could not be read",
            UseError::WrongCapability { .. } | UseError::PresetNeedsOwnConnection(_) => "",
        })
    }
}

/// The model one run uses: a person's chosen connection.
pub struct SelectedModel {
    pub connection_id: String,
    pub provider_id: String,
    pub model_id: String,
    pub model: DynModel<Completion>,
    /// An output cap the run must set: Anthropic's Messages API requires
    /// `max_tokens`, and Rig knows the limit only for catalogued models.
    pub max_tokens: Option<u64>,
    /// The connection that runs (for a preset, the person's own one, not
    /// the preset) and its `config_rev` (endpoint, credential and model)
    /// when this model was built, when Metro tracks one.
    pub built_from: Option<(String, i64)>,
}

impl std::fmt::Debug for SelectedModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SelectedModel")
            .field("connection_id", &self.connection_id)
            .field("provider_id", &self.provider_id)
            .field("model_id", &self.model_id)
            .finish_non_exhaustive()
    }
}

fn read_error(e: anyhow::Error) -> UseError {
    tracing::error!(error = %format!("{e:#}"), "reading a model connection");
    UseError::Internal
}

/// The model of `user_id`'s default chat connection for a run in `space`
/// (`None`: personal Chat), `Ok(None)` when they have none (the caller may
/// then use the environment model), or why the default cannot be used (the
/// caller must not fall back). In a Space, every request rechecks that they
/// are still a member.
pub async fn default_model(
    state: &AppState,
    user_id: &str,
    space: Option<&str>,
) -> Result<Option<SelectedModel>, UseError> {
    let user = user_id.to_owned();
    let row = state
        .db
        .call(move |db| Ok(store::default_for(&db.lock(), &user, Capability::Chat)?))
        .await
        .map_err(read_error)?;
    match row {
        None => Ok(None),
        Some(row) => {
            let authorized = Authorized {
                chosen: row.id.clone(),
                chosen_owner: row.user_id.clone(),
                row,
                access: Access::Own,
            };
            build_model(state, user_id, space, authorized).map(Some)
        }
    }
}

/// The id of `user_id`'s default connection for `capability`, if any.
pub async fn default_connection(
    state: &AppState,
    user_id: &str,
    capability: Capability,
) -> Result<Option<String>, UseError> {
    let user = user_id.to_owned();
    state
        .db
        .call(move |db| Ok(store::default_for(&db.lock(), &user, capability)?.map(|r| r.id)))
        .await
        .map_err(read_error)
}

/// The model of chat connection `connection_id` for a run `user_id` starts
/// in their personal Chat: their own, or one shared with the organization.
/// Same as [`connection_model_for`] with no Space.
pub async fn connection_model(
    state: &AppState,
    user_id: &str,
    connection_id: &str,
) -> Result<SelectedModel, UseError> {
    connection_model_for(state, user_id, None, connection_id).await
}

/// The model of chat connection `id` for a run `caller` starts in `space`
/// (`None`: their personal Chat). Their own connection; someone else's only
/// while it is shared with them there (it then runs on the owner's
/// connection, rechecking the share before every request); or a
/// subscription preset, which runs on the caller's own connection. Anything
/// else is [`UseError::NotFound`], exactly like an unknown id.
pub async fn connection_model_for(
    state: &AppState,
    caller: &str,
    space: Option<&str>,
    id: &str,
) -> Result<SelectedModel, UseError> {
    let authorized = resolve(state, caller, space, id).await?;
    if authorized.row.capability != Capability::Chat {
        return Err(UseError::WrongCapability {
            expected: Capability::Chat,
            actual: authorized.row.capability,
        });
    }
    build_model(state, caller, space, authorized)
}

/// Re-resolve an explicit per-turn model and effort when the run starts.
pub(crate) async fn turn_model_for(
    state: &AppState,
    caller: &str,
    space: Option<&str>,
    id: &str,
    selection: &turn::TurnSelection,
) -> Result<SelectedModel, UseError> {
    let mut authorized = resolve(state, caller, space, id).await?;
    if let Some(sid) = space {
        let (sid, caller) = (sid.to_owned(), caller.to_owned());
        let member = state
            .db
            .call(move |db| Ok(store::is_member(&db.lock(), &sid, &caller)?))
            .await
            .map_err(read_error)?;
        if !member {
            return Err(UseError::NotFound);
        }
    }
    {
        let (st, sel, a) = (state.clone(), selection.clone(), authorized.clone());
        state
            .db
            .call(move |db| Ok(sel.recheck(&st, &db.lock(), &a)))
            .await
            .map_err(read_error)??;
    }
    authorized.row.model.clone_from(&selection.model_id);
    build_model_for_turn(state, caller, space, authorized, Some(selection))
}

async fn resolve(
    state: &AppState,
    caller: &str,
    space: Option<&str>,
    id: &str,
) -> Result<Authorized, UseError> {
    if !api::valid_connection_id(id) {
        return Err(UseError::NotFound);
    }
    let (c, s, i) = (caller.to_owned(), space.map(str::to_owned), id.to_owned());
    state
        .db
        .call(move |db| Ok(sharing::authorize(&db.lock(), &c, s.as_deref(), &i)))
        .await
        .map_err(read_error)?
}

/// `max_tokens` for an Anthropic Messages model Rig's catalog does not list.
pub const UNCATALOGUED_MAX_TOKENS: u64 = 8_192;

/// The grant a non-own use rechecks before each request.
fn grant_for(state: &AppState, caller: &str, space: Option<&str>, a: &Authorized) -> Option<Grant> {
    let slots = match a.access {
        Access::Own => return None,
        Access::Borrowed(_) => Some(state.providers.borrow_slots(&a.row.id)),
        Access::Preset { .. } => None,
    };
    Some(Grant {
        db: state.db.clone(),
        caller: caller.to_owned(),
        space: space.map(str::to_owned),
        chosen: a.chosen.clone(),
        target: a.row.id.clone(),
        turn_check: None,
        slots,
        config_rev: None,
    })
}

fn caller_of(caller: &str, space: Option<&str>) -> Caller {
    match space {
        Some(sid) => Caller::in_space(caller, sid),
        None => Caller::personal(caller),
    }
}

/// Build the completion model of an authorized chat connection.
fn build_model(
    state: &AppState,
    caller: &str,
    space: Option<&str>,
    authorized: Authorized,
) -> Result<SelectedModel, UseError> {
    build_model_for_turn(state, caller, space, authorized, None)
}

fn build_model_for_turn(
    state: &AppState,
    caller: &str,
    space: Option<&str>,
    authorized: Authorized,
    selection: Option<&turn::TurnSelection>,
) -> Result<SelectedModel, UseError> {
    let mut grant = grant_for(state, caller, space, &authorized);
    // A use in a Space always carries it, so every request rechecks the
    // caller's membership (own and organization-shared connections too).
    if grant.is_none() && space.is_some() {
        grant = Some(Grant {
            db: state.db.clone(),
            caller: caller.to_owned(),
            space: space.map(str::to_owned),
            chosen: authorized.chosen.clone(),
            target: authorized.row.id.clone(),
            turn_check: None,
            slots: None,
            config_rev: None,
        });
    }
    if let Some(selection) = selection {
        let grant = grant.get_or_insert_with(|| Grant {
            db: state.db.clone(),
            caller: caller.to_owned(),
            space: space.map(str::to_owned),
            chosen: authorized.chosen.clone(),
            target: authorized.row.id.clone(),
            turn_check: None,
            slots: None,
            config_rev: None,
        });
        grant.turn_check = Some(guard::TurnCheck {
            state: state.clone(),
            selection: selection.clone(),
        });
    }
    let Authorized { row, chosen, .. } = authorized;
    let built_from = Some((row.id.clone(), row.config_rev));
    let entry = catalog::find(&row.provider_id).ok_or(UseError::Unsupported)?;
    if row.model.is_empty() {
        return Err(UseError::NoModel);
    }
    let max_tokens = match entry.adapter {
        Adapter::Registered(id)
            if entry.format == "anthropic"
                && Catalog::builtin().facts(id, &row.model).spec().is_none() =>
        {
            Some(UNCATALOGUED_MAX_TOKENS)
        }
        _ => None,
    };
    // Every server request, the owner's own included, is checked against
    // the connection as it was when this model was built.
    let http = || {
        let mut grant = grant.clone().unwrap_or_else(|| Grant {
            db: state.db.clone(),
            caller: caller.to_owned(),
            space: space.map(str::to_owned),
            chosen: chosen.clone(),
            target: row.id.clone(),
            turn_check: None,
            slots: None,
            config_rev: None,
        });
        grant.config_rev = Some(row.config_rev);
        DynHttpClient::new(GuardedHttp {
            inner: state.providers.http(),
            grant,
        })
    };
    let model = match entry.adapter {
        // Recheck an explicit model or preset before each relay request and
        // again before the answer is returned.
        Adapter::Device
            if grant.is_some()
                && (selection.is_some() || row.user_id == caller || space.is_some()) =>
        {
            let mut grant = grant.clone().expect("checked");
            // The authenticated relay already caps borrowed device traffic.
            grant.slots = None;
            let wire = OpenAIConfig::with_key(&guard::DEVICE_WIRE_DIALECT, "metro-device-relay")
                .with_base_url(guard::DEVICE_WIRE_URL)
                .chat(row.model.clone());
            rig_core::Model::new(
                wire,
                guard::GuardedDeviceTransport {
                    relay: state.device.clone(),
                    caller: caller_of(caller, space),
                    connection: row.id.clone(),
                    model: row.model.clone(),
                    grant,
                },
            )
            .into()
        }
        // The caller's own device connection: relayed as its owner.
        Adapter::Device if row.user_id == caller => state
            .device
            .rig_model(state.db.clone(), &row.user_id, &row.id, &row.model)
            .into(),
        // Someone else's device: relayed as the borrower, so the relay
        // checks the share ([`DeviceDirectory`]) at dispatch, before the
        // device claims the job and before the result is returned.
        Adapter::Device => state
            .device
            .rig_model_as(
                state.db.clone(),
                caller_of(caller, space),
                &row.id,
                &row.model,
            )
            .into(),
        Adapter::Registered(id) => {
            let config = state.providers.server_config(entry, &row)?;
            config.completion_model(&row.model, http(), Catalog::builtin().facts(id, &row.model))
        }
        Adapter::Custom => {
            let config = state.providers.server_config(entry, &row)?;
            config.completion_model(&row.model, http(), ModelFacts::default())
        }
        Adapter::SystemOne | Adapter::Embeddings => {
            return Err(UseError::WrongCapability {
                expected: Capability::Chat,
                actual: entry.capability,
            });
        }
    };
    Ok(SelectedModel {
        connection_id: chosen,
        provider_id: row.provider_id,
        model_id: row.model,
        model,
        max_tokens,
        built_from,
    })
}

// ---- Typed calls ----

/// Why a typed call (System One decision, embeddings, fingerprint) failed.
/// Messages carry no key, endpoint or upstream text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderError {
    /// The connection could not be used (not found, not shared, wrong
    /// capability, no key, endpoint not approved, ...).
    Use(UseError),
    /// The payload was refused before anything was sent.
    InvalidInput(String),
    /// The device relay failed.
    Device(DeviceError),
    /// The endpoint failed or answered with something that is not the
    /// documented shape (the shared client's fixed text: no key, URL or
    /// payload).
    Endpoint(String),
    /// Too many typed calls are in flight.
    Busy,
    Internal,
}

impl From<UseError> for ProviderError {
    fn from(e: UseError) -> Self {
        ProviderError::Use(e)
    }
}

impl From<DeviceError> for ProviderError {
    fn from(e: DeviceError) -> Self {
        ProviderError::Device(e)
    }
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProviderError::Use(e) => e.fmt(f),
            ProviderError::InvalidInput(m) => f.write_str(m),
            ProviderError::Device(e) => e.fmt(f),
            ProviderError::Endpoint(m) => f.write_str(m),
            ProviderError::Busy => {
                f.write_str("too many model calls are running; try again shortly")
            }
            ProviderError::Internal => f.write_str("internal error calling the model provider"),
        }
    }
}

impl std::error::Error for ProviderError {}

impl ProviderError {
    /// A fixed name of the failure, for logs.
    pub fn category(&self) -> &'static str {
        match self {
            ProviderError::Use(_) => "unusable",
            ProviderError::InvalidInput(_) => "invalid_input",
            ProviderError::Device(e) => e.kind(),
            ProviderError::Endpoint(_) => "endpoint",
            ProviderError::Busy => "busy",
            ProviderError::Internal => "internal",
        }
    }

    /// The HTTP status an API handler should answer with.
    pub fn status(&self) -> axum::http::StatusCode {
        use axum::http::StatusCode as S;
        match self {
            ProviderError::Use(UseError::NotFound) => S::NOT_FOUND,
            ProviderError::Use(UseError::Revoked) => S::FORBIDDEN,
            ProviderError::Use(UseError::Busy) | ProviderError::Busy => S::SERVICE_UNAVAILABLE,
            ProviderError::Use(UseError::Internal) | ProviderError::Internal => {
                S::INTERNAL_SERVER_ERROR
            }
            ProviderError::Use(
                UseError::Unsupported
                | UseError::WrongCapability { .. }
                | UseError::MediaUnsupported,
            ) => S::UNPROCESSABLE_ENTITY,
            ProviderError::Use(_) => S::CONFLICT,
            ProviderError::InvalidInput(_) => S::UNPROCESSABLE_ENTITY,
            ProviderError::Device(DeviceError::NotFound) => S::NOT_FOUND,
            ProviderError::Device(DeviceError::TimedOut) => S::GATEWAY_TIMEOUT,
            ProviderError::Endpoint(m) if m.contains("did not answer in time") => {
                S::GATEWAY_TIMEOUT
            }
            ProviderError::Device(
                DeviceError::Offline | DeviceError::QueueFull | DeviceError::NotClaimed,
            ) => S::SERVICE_UNAVAILABLE,
            ProviderError::Device(DeviceError::AccessRevoked) => S::FORBIDDEN,
            ProviderError::Device(DeviceError::InvalidRequest(_)) => S::UNPROCESSABLE_ENTITY,
            ProviderError::Device(DeviceError::TooLarge) => S::PAYLOAD_TOO_LARGE,
            ProviderError::Device(DeviceError::NotPaired) => S::CONFLICT,
            ProviderError::Device(_) | ProviderError::Endpoint(_) => S::BAD_GATEWAY,
        }
    }
}

/// A typed connection, authorized for `caller` in `space`, with the slots
/// it holds while it runs.
struct TypedUse {
    row: Row,
    entry: &'static Entry,
    caller: String,
    space: Option<String>,
    chosen: String,
    access: Access,
    _global: tokio::sync::OwnedSemaphorePermit,
    _borrowed: Option<tokio::sync::OwnedSemaphorePermit>,
}

async fn typed_use(
    state: &AppState,
    caller: &str,
    space: Option<&str>,
    id: &str,
    capability: Capability,
) -> Result<TypedUse, ProviderError> {
    let authorized = resolve(state, caller, space, id).await?;
    if authorized.row.capability != capability {
        return Err(UseError::WrongCapability {
            expected: capability,
            actual: authorized.row.capability,
        }
        .into());
    }
    let Authorized {
        row,
        access,
        chosen,
        ..
    } = authorized;
    let entry = catalog::find(&row.provider_id).ok_or(UseError::Unsupported)?;
    if entry.capability != capability {
        return Err(UseError::Unsupported.into());
    }
    if row.model.is_empty() {
        return Err(UseError::NoModel.into());
    }
    let global = state
        .providers
        .inner
        .typed_calls
        .clone()
        .try_acquire_owned()
        .map_err(|_| ProviderError::Busy)?;
    let borrowed = match access {
        Access::Own => None,
        _ => Some(
            state
                .providers
                .borrow_slots(&row.id)
                .try_acquire_owned()
                .map_err(|_| UseError::Busy)?,
        ),
    };
    Ok(TypedUse {
        row,
        entry,
        caller: caller.to_owned(),
        space: space.map(str::to_owned),
        chosen,
        access,
        _global: global,
        _borrowed: borrowed,
    })
}
/// Before a borrowed typed answer is returned: the grant must still hold,
/// and the connection must still be the one that was called (same row,
/// provider, model, revision and approved endpoint). Otherwise the answer
/// is withheld as [`UseError::Revoked`]. Read in its own short database
/// call after the request finished; nothing is locked while the request
/// runs. The payload already sent cannot be unsent: this only keeps the
/// result from someone who lost access meanwhile. Own connections pass.
async fn recheck_typed(state: &AppState, u: &TypedUse) -> Result<(), ProviderError> {
    if u.access == Access::Own {
        return Ok(());
    }
    let now = match resolve(state, &u.caller, u.space.as_deref(), &u.chosen).await {
        Ok(now) => now,
        Err(UseError::NotFound) => return Err(UseError::Revoked.into()),
        Err(e) => return Err(e.into()),
    };
    let same = now.row.id == u.row.id
        && now.row.provider_id == u.row.provider_id
        && now.row.model == u.row.model
        && now.row.config_rev == u.row.config_rev
        && now.row.capability == u.row.capability
        && now.row.base_url == u.row.base_url
        && match u.entry.execution {
            Execution::Device => true,
            Execution::Server => state
                .providers
                .effective_base_url(u.entry, now.row.base_url.as_deref())
                .is_ok(),
        };
    if same {
        Ok(())
    } else {
        Err(UseError::Revoked.into())
    }
}

/// Where a typed call runs: a server endpoint with its key, or a device.
enum TypedTarget {
    Server { base: String, key: Option<String> },
    Device,
}

fn typed_target(state: &AppState, u: &TypedUse) -> Result<TypedTarget, ProviderError> {
    match u.entry.adapter {
        Adapter::SystemOne | Adapter::Embeddings => {
            let (base, key) = state.providers.typed_endpoint(u.entry, &u.row)?;
            Ok(TypedTarget::Server { base, key })
        }
        Adapter::Device => Ok(TypedTarget::Device),
        Adapter::Registered(_) | Adapter::Custom => Err(UseError::Unsupported.into()),
    }
}

/// Fixed fields only: an endpoint's or device's own text never reaches a
/// log.
fn log_typed_failure(u: &TypedUse, e: &ProviderError) {
    tracing::warn!(
        connection = %u.row.id,
        provider = %u.row.provider_id,
        category = e.category(),
        status = e.status().as_u16(),
        "typed model call failed"
    );
}

/// One System One decision on connection `id`, which `caller` may use in
/// `space` (`None`: personal Chat): `state_payload` and `questions` as the
/// System One API takes them (`noul`, `choice` and `score` only), always
/// with the connection's saved model. Returns the validated
/// `{model, answers, usage}` answer: one answer per question, of its type.
pub async fn decide(
    state: &AppState,
    caller: &str,
    space: Option<&str>,
    id: &str,
    state_payload: serde_json::Value,
    questions: serde_json::Value,
) -> Result<serde_json::Value, ProviderError> {
    let u = typed_use(state, caller, space, id, Capability::SystemOne).await?;
    let request = typed::decision_request(&u.row.model, state_payload, questions)?;
    let answer = match typed_target(state, &u)? {
        TypedTarget::Server { base, key } => typed::decide(&base, key.as_deref(), request).await,
        TypedTarget::Device => state
            .device
            .decide(&state.db, &caller_of(caller, space), &u.row.id, request)
            .await
            .map_err(ProviderError::from),
    }
    .inspect_err(|e| log_typed_failure(&u, e))?;
    recheck_typed(state, &u).await?;
    Ok(answer)
}

/// Embeddings of `inputs`, in order, on connection `id`, which `caller` may
/// use in `space`, always with the connection's saved model.
pub async fn embed(
    state: &AppState,
    caller: &str,
    space: Option<&str>,
    id: &str,
    inputs: Vec<String>,
) -> Result<Vec<Vec<f32>>, ProviderError> {
    let u = typed_use(state, caller, space, id, Capability::Embeddings).await?;
    let request = typed::embedding_request(&u.row.model, &inputs)?;
    let vectors = match typed_target(state, &u)? {
        TypedTarget::Server { base, key } => typed::embed(&base, key.as_deref(), request).await,
        TypedTarget::Device => state
            .device
            .embed(&state.db, &caller_of(caller, space), &u.row.id, request)
            .await
            .map_err(ProviderError::from)
            .and_then(|answer| typed::vectors(&answer)),
    }
    .inspect_err(|e| log_typed_failure(&u, e))?;
    if vectors.len() != inputs.len() {
        return Err(ProviderError::Endpoint(
            "the model endpoint returned an invalid embeddings response".into(),
        ));
    }
    recheck_typed(state, &u).await?;
    Ok(vectors)
}

/// Embeddings of one document or one query on media connection `id`,
/// which `caller` may use in `space` (`None`: personal Chat), always with
/// the connection's saved model and endpoint.
///
/// `input` is exactly one of `{"query": TEXT}` or
/// `{"asset": {"name": BASENAME, "mime_type": "type/subtype",
/// "data_base64": BASE64}}`: bytes only (at most 8 MiB decoded), never a URL
/// or path, and no `model` (the connection decides). `mime_type` is one of
/// `metro_model_protocol::MEDIA_MIME_TYPES` (TXT, Markdown, PDF, DOCX, PNG,
/// JPEG, WAV, MP3, MP4, WebM), or a known alias or parameterized form of
/// one, which is sent by its canonical name; anything else (SVG, HTML,
/// GIF, ...) is refused. PDF and DOCX travel as their bytes for the model
/// service to prepare (pages extracted or rendered), never to the model
/// raw. Returns
/// `{model, data: [{index, embedding, locator, excerpt?}], partial}`, sorted
/// by index: one `{kind: "query"}` row for a query, 1..=48 typed rows
/// (`text`, `page`, `image`, `time`) for an asset, `partial: true` when the
/// model service embedded only part of it (see
/// `metro_model_protocol::check_media_response`).
///
/// Only connections whose adapter has `media_inputs` take it; any other
/// embeddings connection answers [`UseError::MediaUnsupported`] before
/// anything is sent. Borrowed use rechecks the grant before dispatch, at the
/// device's claim and before the answer is returned, as every typed call.
pub async fn media_embed(
    state: &AppState,
    caller: &str,
    space: Option<&str>,
    connection_id: &str,
    input: serde_json::Value,
) -> Result<serde_json::Value, ProviderError> {
    let u = typed_use(state, caller, space, connection_id, Capability::Embeddings).await?;
    if !u.entry.media_inputs {
        return Err(UseError::MediaUnsupported.into());
    }
    // The slot is taken before the payload is checked or sent.
    let _media = state.providers.media_slot(caller, &u.row.id)?;
    let request = typed::media_request(&u.row.model, input)?;
    let answer = match typed_target(state, &u)? {
        TypedTarget::Server { base, key } => {
            typed::media_embed(&base, key.as_deref(), request).await
        }
        TypedTarget::Device => state
            .device
            .media_embed_request(&state.db, &caller_of(caller, space), &u.row.id, request)
            .await
            .map_err(ProviderError::from),
    }
    .inspect_err(|e| log_typed_failure(&u, e))?;
    recheck_typed(state, &u).await?;
    Ok(answer)
}

/// Whether connection `id` is a media embeddings connection `caller` may
/// use in `space` (`None`: personal Chat and the personal library) right
/// now: `Ok(true)` for a usable EmbeddingGemma 2 connection (own, or shared
/// with them for exactly that scope), `Ok(false)` for a usable connection
/// that embeds text only or serves another capability, and the reason
/// otherwise ([`UseError::NotFound`] for an id they may not use there, like
/// an unknown one; [`UseError::NoModel`], an endpoint no longer approved, an
/// unreadable key, ...). Nothing is sent and no slot is taken; a device
/// connection is not asked whether it is online. Every [`media_embed`]
/// checks all of this again.
pub async fn media_capable(
    state: &AppState,
    caller: &str,
    space: Option<&str>,
    id: &str,
) -> Result<bool, ProviderError> {
    let a = resolve(state, caller, space, id).await?;
    let Some(entry) = catalog::find(&a.row.provider_id) else {
        return Err(UseError::Unsupported.into());
    };
    if a.row.capability != Capability::Embeddings
        || entry.capability != Capability::Embeddings
        || !entry.media_inputs
    {
        return Ok(false);
    }
    if a.row.model.is_empty() {
        return Err(UseError::NoModel.into());
    }
    if entry.execution == Execution::Server {
        state.providers.typed_endpoint(entry, &a.row)?;
    }
    Ok(true)
}

/// [`media_capable`] for a choice saved without a scope (a preference):
/// `Ok(true)` when the connection is usable media somewhere `caller` can
/// use it now: their personal scope (own or organization-shared), or any
/// Space it is shared with in which both they and its owner are members.
/// Only for validating a saved choice. Each use must still call
/// [`media_capable`] / [`media_embed`] with the real destination, which
/// may refuse it (a Space-only share does not cover the personal library or
/// another Space).
pub async fn media_capable_in_any_scope(
    state: &AppState,
    caller: &str,
    id: &str,
) -> Result<bool, ProviderError> {
    let first = media_capable(state, caller, None, id).await;
    if !matches!(first, Err(ProviderError::Use(UseError::NotFound))) {
        return first;
    }
    if !api::valid_connection_id(id) {
        return first;
    }
    let (c, i) = (caller.to_owned(), id.to_owned());
    let spaces: Vec<String> = state
        .db
        .call(move |db| {
            let conn = db.lock();
            let Some(row) = store::find(&conn, &i)? else {
                return Ok(Vec::new());
            };
            let mut out = Vec::new();
            for sid in store::share_of(&conn, &row.id)?.space_ids {
                if sharing::visible(&conn, &c, Some(&sid), &row)?.is_some() {
                    out.push(sid);
                }
            }
            Ok(out)
        })
        .await
        .map_err(read_error)?;
    let mut last = first;
    for sid in spaces {
        match media_capable(state, caller, Some(&sid), id).await {
            Ok(true) => return Ok(true),
            other => last = other,
        }
    }
    last
}

/// A fingerprint of what connection `id` computes for `caller` in `space`:
/// it changes when the model, the endpoint, the adapter or the connection
/// that runs changes, so a search index built with it knows it is stale.
/// Hex SHA-256 over non-secret facts only (no key, no token).
pub async fn model_fingerprint(
    state: &AppState,
    caller: &str,
    space: Option<&str>,
    id: &str,
) -> Result<String, ProviderError> {
    use sha2::{Digest, Sha256};
    let a = resolve(state, caller, space, id).await?;
    let entry = catalog::find(&a.row.provider_id).ok_or(UseError::Unsupported)?;
    if a.row.model.is_empty() {
        return Err(UseError::NoModel.into());
    }
    let endpoint = match entry.execution {
        Execution::Device => "device".to_owned(),
        Execution::Server => state
            .providers
            .effective_base_url(entry, a.row.base_url.as_deref())?,
    };
    let mut h = Sha256::new();
    h.update(b"metro.model-fingerprint.v1");
    for part in [
        a.chosen.as_str(),
        a.row.id.as_str(),
        a.row.provider_id.as_str(),
        a.row.capability.as_str(),
        a.row.model.as_str(),
        endpoint.as_str(),
        &a.row.config_rev.to_string(),
    ] {
        h.update((part.len() as u64).to_be_bytes());
        h.update(part.as_bytes());
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// The lookup the device relay uses to authorize device credentials and
/// dispatch: it reads the provider table on the connection the relay
/// already holds, without locking the database again.
pub struct DeviceDirectory;

impl crate::device_relay::ConnectionDirectory for DeviceDirectory {
    fn device_connection(
        &self,
        conn: &rusqlite::Connection,
        id: &str,
    ) -> anyhow::Result<Option<crate::device_relay::DeviceConnection>> {
        device_connection(conn, id)
    }

    fn shared_use_allowed(
        &self,
        conn: &rusqlite::Connection,
        connection: &crate::device_relay::DeviceConnection,
        user: &str,
        space: Option<&str>,
    ) -> anyhow::Result<bool> {
        device_shared_use_allowed(conn, connection, user, space)
    }

    fn member(&self, conn: &rusqlite::Connection, space: &str, user: &str) -> anyhow::Result<bool> {
        Ok(store::is_member(conn, space, user)?)
    }
}

/// Whether `user` (not the owner) may use device connection `connection`
/// in `space` now under its owner's share. Never for a subscription sign-in:
/// that is shared only as a preset, which runs on the user's own connection.
pub fn device_shared_use_allowed(
    conn: &rusqlite::Connection,
    connection: &crate::device_relay::DeviceConnection,
    user: &str,
    space: Option<&str>,
) -> anyhow::Result<bool> {
    if user == connection.owner_id || catalog::is_subscription(&connection.provider) {
        return Ok(false);
    }
    let Some(row) = store::find(conn, &connection.id)? else {
        return Ok(false);
    };
    if row.user_id != connection.owner_id || !catalog::is_device(&row.provider_id) {
        return Ok(false);
    }
    Ok(sharing::visible(conn, user, space, &row)?.is_some())
}

/// The device connection `id`, or `None` when it does not exist or is not a
/// `device-*` connection. Sync, on a connection the caller holds.
pub fn device_connection(
    conn: &rusqlite::Connection,
    id: &str,
) -> anyhow::Result<Option<crate::device_relay::DeviceConnection>> {
    use rusqlite::OptionalExtension;
    let row: Option<(String, String)> = conn
        .query_row(
            "SELECT user_id, provider_id FROM provider_connections WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(row
        .filter(|(_, provider)| catalog::is_device(provider))
        .map(
            |(owner_id, provider)| crate::device_relay::DeviceConnection {
                id: id.to_owned(),
                owner_id,
                provider,
            },
        ))
}
