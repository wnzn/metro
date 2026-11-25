//! Who may use a model connection, and the HTTP API for sharing one.
//!
//! A connection is private to its owner until the owner shares it. A share
//! grants *use* and never credentials: nobody but the owner ever sees the
//! connection's key, base URL, sign-in or device token, and only the owner
//! can change or revoke the share (organization admins included: they get
//! no say over another person's connection).
//!
//! - `spaces`: usable inside each selected Space, while both the caller and
//!   the owner are members of it now. Leaving or being removed from the
//!   Space ends it at once.
//! - `organization`: usable by every signed-in user of this server, in any
//!   scope, personal Chat included.
//!
//! A borrowed connection runs on the owner's endpoint, key or device, so the
//! owner can see those requests and may pay for them; `/v1/models/available`
//! says so for every borrowed entry. The one exception is an account-bound
//! subscription sign-in (`device-openai-oauth`, Sign in with ChatGPT): it is
//! never run for anyone else. Sharing it publishes a bring-your-own
//! **preset** that holds no credential (the model name only). A person who
//! picks it runs that model on their *own* Sign in with ChatGPT connection,
//! or gets a clear error before any call. Metro never falls back to their
//! API-billed OpenAI key on its own (they can pick that connection
//! explicitly), and never uses the owner's account. Every request of a
//! preset run rechecks the preset's share, so revoking it stops the next
//! round. Accounts are never shared.
//!
//! Every use rechecks the grant: a revoked share, a removed member or a
//! deleted connection stops new inference, and an id the caller may not use
//! answers exactly like an unknown one.

use std::collections::BTreeSet;

use axum::Json;
use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::UseError;
use super::catalog::{self, Capability, Execution};
use super::store::{self, Row, Share, Visibility};
use crate::AppState;
use crate::api::ApiError;
use crate::auth::Human;

type ApiResult<T> = Result<T, ApiError>;

/// Most Spaces one connection can be shared with.
pub const MAX_SHARE_SPACES: usize = 32;
/// Longest Space id considered.
const MAX_SPACE_ID: usize = 128;

/// The only provider a subscription preset runs on for the person who picks
/// it: their own Sign in with ChatGPT. Never an API-billed key picked on
/// their behalf (they can choose such a connection explicitly instead), and
/// never the owner's account.
const PRESET_TARGET: &str = catalog::DEVICE_OPENAI_OAUTH;

/// How the caller would use a connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Access {
    /// The caller's own connection.
    Own,
    /// Someone else's, run on the owner's connection (endpoint, key or
    /// device) under their share.
    Borrowed(Visibility),
    /// Someone else's subscription preset, run on the caller's own
    /// connection `own_connection`.
    Preset {
        visibility: Visibility,
        own_connection: String,
    },
}

/// What a use resolves to: `row` is the connection that runs (for a preset,
/// the caller's own, with the preset's model) and `shared` the connection
/// the caller chose.
#[derive(Clone, Debug)]
pub struct Authorized {
    pub row: Row,
    pub access: Access,
    pub chosen: String,
    pub chosen_owner: String,
}

impl Authorized {
    pub fn borrowed(&self) -> bool {
        matches!(self.access, Access::Borrowed(_))
    }
}

/// Whether `caller` may use `row` in `space` now. Sync, on a locked
/// connection. `None` for no access (reported as not found).
pub fn visible(
    conn: &Connection,
    caller: &str,
    space: Option<&str>,
    row: &Row,
) -> rusqlite::Result<Option<Visibility>> {
    if row.user_id == caller {
        return Ok(Some(store::share_of(conn, &row.id)?.visibility));
    }
    if row.model.is_empty() {
        return Ok(None);
    }
    let share = store::share_of(conn, &row.id)?;
    Ok(match share.visibility {
        Visibility::Personal => None,
        Visibility::Organization => Some(Visibility::Organization),
        Visibility::Spaces => match space {
            Some(sid)
                if share.space_ids.iter().any(|s| s == sid)
                    && store::is_member(conn, sid, caller)?
                    && store::is_member(conn, sid, &row.user_id)? =>
            {
                Some(Visibility::Spaces)
            }
            _ => None,
        },
    })
}

/// Resolve connection `id` for `caller` in `space` (`None`: their personal
/// Chat). Errors carry no detail about connections the caller cannot use.
pub fn authorize(
    conn: &Connection,
    caller: &str,
    space: Option<&str>,
    id: &str,
) -> Result<Authorized, UseError> {
    let internal = |e: rusqlite::Error| {
        tracing::error!(error = %e, "reading a model connection grant");
        UseError::Internal
    };
    if !super::api::valid_connection_id(id) {
        return Err(UseError::NotFound);
    }
    let row = store::find(conn, id)
        .map_err(internal)?
        .ok_or(UseError::NotFound)?;
    let Some(visibility) = visible(conn, caller, space, &row).map_err(internal)? else {
        return Err(UseError::NotFound);
    };
    let (chosen, chosen_owner) = (row.id.clone(), row.user_id.clone());
    if row.user_id == caller {
        return Ok(Authorized {
            row,
            access: Access::Own,
            chosen,
            chosen_owner,
        });
    }
    if catalog::is_subscription(&row.provider_id) {
        let own = preset_target(conn, caller, &row.model).map_err(internal)?;
        let Some(mut own) = own else {
            return Err(UseError::PresetNeedsOwnConnection(row.model));
        };
        own.model = row.model.clone();
        let own_connection = own.id.clone();
        return Ok(Authorized {
            row: own,
            access: Access::Preset {
                visibility,
                own_connection,
            },
            chosen,
            chosen_owner,
        });
    }
    Ok(Authorized {
        row,
        access: Access::Borrowed(visibility),
        chosen,
        chosen_owner,
    })
}

/// The caller's own connection a subscription preset for `model` runs on:
/// one of their own `device-openai-oauth` chat connections, one already set
/// to that model first. `None` when they have none: the preset then fails
/// before anything is sent.
pub fn preset_target(
    conn: &Connection,
    caller: &str,
    model: &str,
) -> rusqlite::Result<Option<Row>> {
    Ok(store::list(conn, caller)?
        .into_iter()
        .filter(|r| r.provider_id == PRESET_TARGET && r.capability == Capability::Chat)
        .min_by_key(|r| r.model != model))
}

// ---- HTTP ----

fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, message)
}

fn no_space() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "no such space")
}

fn not_found() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "no such model connection")
}

/// `GET|PUT /v1/providers/{id}/sharing` body and answer.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SharingBody {
    pub visibility: Visibility,
    #[serde(default)]
    pub space_ids: Vec<String>,
}

/// What a share means for the people it reaches, shown with the share.
fn share_view(row: &Row, share: &Share) -> serde_json::Value {
    let preset = catalog::is_subscription(&row.provider_id);
    json!({
        "visibility": share.visibility,
        "space_ids": share.space_ids,
        // `preset`: others get the model name only and run it on their own
        // sign-in or key. `owner_connection`: others run on this connection,
        // which the owner can observe and pays for.
        "mode": if preset { "preset" } else { "owner_connection" },
        "capability": row.capability,
    })
}

async fn owned(state: &AppState, who: &Human, id: &str) -> ApiResult<Row> {
    if !super::api::valid_connection_id(id) {
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

pub async fn get_sharing(
    State(state): State<AppState>,
    who: Human,
    Path(id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let row = owned(&state, &who, &id).await?;
    let rid = row.id.clone();
    let share = state
        .db
        .call(move |db| Ok(store::share_of(&db.lock(), &rid)?))
        .await
        .map_err(ApiError::internal)?;
    Ok(Json(share_view(&row, &share)))
}

fn valid_space_id(sid: &str) -> bool {
    !sid.is_empty()
        && sid.len() <= MAX_SPACE_ID
        && sid
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

/// `PUT /v1/providers/{id}/sharing`: replace the share. Owner only.
pub async fn put_sharing(
    State(state): State<AppState>,
    who: Human,
    Path(id): Path<String>,
    body: Result<Json<SharingBody>, JsonRejection>,
) -> ApiResult<Json<serde_json::Value>> {
    let row = owned(&state, &who, &id).await?;
    let Json(body) = body?;
    let space_ids: BTreeSet<String> = body.space_ids.iter().cloned().collect();
    match body.visibility {
        Visibility::Personal | Visibility::Organization if !space_ids.is_empty() => {
            return Err(bad_request(
                "space_ids applies only to visibility \"spaces\"",
            ));
        }
        Visibility::Spaces if space_ids.is_empty() || space_ids.len() > MAX_SHARE_SPACES => {
            return Err(bad_request(format!(
                "visibility \"spaces\" needs 1..={MAX_SHARE_SPACES} space_ids"
            )));
        }
        _ => {}
    }
    if space_ids.iter().any(|s| !valid_space_id(s)) {
        return Err(bad_request("space_ids must be Space ids"));
    }
    if body.visibility != Visibility::Personal && row.model.is_empty() {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "choose a model before sharing this connection",
        ));
    }
    let share = Share {
        visibility: body.visibility,
        space_ids: space_ids.into_iter().collect(),
    };
    let (user, cid, s) = (who.id.clone(), row.id.clone(), share.clone());
    let outcome = state
        .db
        .call(move |db| {
            let mut conn = db.lock();
            for sid in &s.space_ids {
                // A Space the owner isn't in looks like one that doesn't
                // exist: no hint about other people's Spaces.
                if !store::is_member(&conn, sid, &user)? {
                    return Ok(Err(()));
                }
            }
            Ok(Ok(store::set_share(&mut conn, &user, &cid, &s)?))
        })
        .await
        .map_err(ApiError::internal)?;
    match outcome {
        Err(()) => Err(bad_request("space_ids must be Spaces you are a member of")),
        Ok(false) => Err(not_found()),
        Ok(true) => {
            // Pending borrowed device jobs of this connection are rechecked
            // against the new share now, not only at their next step.
            if let Err(e) = state.device.shares_changed(&state.db, Some(&row.id)).await {
                tracing::error!(error = %format!("{e:#}"), connection = %row.id,
                    "rechecking device jobs after a share change");
            }
            tracing::info!(
                connection = %row.id,
                visibility = share.visibility.as_str(),
                spaces = share.space_ids.len(),
                "model connection share replaced"
            );
            Ok(Json(share_view(&row, &share)))
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AvailableQuery {
    #[serde(default)]
    space_id: Option<String>,
    #[serde(default)]
    scope: Option<String>,
}

/// One usable model as a picker shows it. Never a key, base URL or token.
#[derive(Debug, Serialize)]
pub struct AvailableModel {
    pub connection_id: String,
    pub name: String,
    pub model: String,
    pub provider_id: String,
    /// The provider's display name from the catalog.
    pub provider_name: String,
    pub capability: Capability,
    pub visibility: Visibility,
    pub owned: bool,
    /// The person whose connection (or preset) this is.
    pub owner_id: String,
    pub execution: Execution,
    /// `your_connection`, `owner_connection` (the owner's endpoint, key or
    /// device: they can see these requests and may pay for them) or
    /// `your_own_sign_in` (a preset run on your own connection).
    pub runs_on: &'static str,
    /// Who pays the provider: `you` or `owner`.
    pub billing: &'static str,
    /// For a preset: whether you have a connection it can run on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ready: Option<bool>,
    /// Your default for this capability (own connections only).
    pub is_default: bool,
    /// Whether it embeds documents and media (EmbeddingGemma 2 adapters);
    /// text-only embeddings connections are `false` and can't index them.
    pub media_inputs: bool,
    /// What it embeds natively, for media connections.
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    pub modalities: &'static [&'static str],
    /// Selectable chat models in the requested scope. An empty list means
    /// the chat connection is not presently usable for a new selection.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chat_models: Option<Vec<super::turn::ChatModel>>,
}

fn available_entry(
    state: &AppState,
    conn: &Connection,
    caller: &str,
    space: Option<&str>,
    row: &Row,
    visibility: Visibility,
) -> rusqlite::Result<Option<AvailableModel>> {
    let Some(entry) = catalog::find(&row.provider_id) else {
        return Ok(None);
    };
    if row.model.is_empty() && !(row.user_id == caller && row.capability == Capability::Chat) {
        return Ok(None);
    }
    let owned = row.user_id == caller;
    let preset = !owned && catalog::is_subscription(&row.provider_id);
    let (runs_on, billing, mut ready) = if owned {
        ("your_connection", "you", None)
    } else if preset {
        (
            "your_own_sign_in",
            "you",
            Some(preset_target(conn, caller, &row.model)?.is_some()),
        )
    } else {
        ("owner_connection", "owner", None)
    };
    if row.capability == Capability::SystemOne {
        ready = Some(match entry.adapter {
            catalog::Adapter::Device => state.device.status(&row.id).online,
            catalog::Adapter::SystemOne => state.providers.typed_endpoint(entry, row).is_ok(),
            _ => false,
        });
    }
    let chat_models = if row.capability == Capability::Chat {
        let models = match authorize(conn, caller, space, &row.id) {
            Ok(authorized) => super::turn::models(state, conn, &authorized).unwrap_or_default(),
            Err(_) => vec![],
        };
        // Preserve the omission of unconfigured rows unless the owner has
        // actual inventory: a live device report or explicit Settings list.
        if row.model.is_empty() && models.is_empty() {
            return Ok(None);
        }
        Some(models)
    } else {
        None
    };
    Ok(Some(AvailableModel {
        connection_id: row.id.clone(),
        name: row.name.clone(),
        model: row.model.clone(),
        provider_id: row.provider_id.clone(),
        provider_name: entry.name.clone(),
        capability: row.capability,
        visibility,
        owned,
        owner_id: row.user_id.clone(),
        execution: entry.execution,
        runs_on,
        billing,
        ready,
        is_default: owned && row.is_default,
        media_inputs: entry.media_inputs,
        modalities: entry.modalities,
        chat_models,
    }))
}

/// `GET /v1/models/available[?space_id=SID | ?scope=any]`: the models the
/// caller can use there. Without either, their personal Chat: their own and
/// organization shares. `scope=any` is for pickers only (every Space they
/// share with the owner now); each run still checks its own scope.
pub async fn available(
    State(state): State<AppState>,
    who: Human,
    query: Result<Query<AvailableQuery>, QueryRejection>,
) -> ApiResult<Json<serde_json::Value>> {
    let Query(query) = query?;
    let any = match query.scope.as_deref() {
        None => false,
        Some("any") => true,
        Some(_) => return Err(bad_request("scope must be \"any\"")),
    };
    if any && query.space_id.is_some() {
        return Err(bad_request("use either space_id or scope=any"));
    }
    let space = query.space_id;
    if space.as_deref().is_some_and(|s| !valid_space_id(s)) {
        return Err(no_space());
    }
    let caller = who.id.clone();
    let catalog_state = state.clone();
    let models = state
        .db
        .call(move |db| {
            let conn = db.lock();
            if let Some(sid) = &space
                && !store::is_member(&conn, sid, &caller)?
            {
                return Ok(None);
            }
            let mut out = Vec::new();
            for row in store::list(&conn, &caller)? {
                let visibility = store::share_of(&conn, &row.id)?.visibility;
                out.extend(available_entry(
                    &catalog_state,
                    &conn,
                    &caller,
                    space.as_deref(),
                    &row,
                    visibility,
                )?);
            }
            for (row, visibility, granted_space) in
                store::shared_with(&conn, &caller, space.as_deref(), any)?
            {
                out.extend(available_entry(
                    &catalog_state,
                    &conn,
                    &caller,
                    granted_space.as_deref().or(space.as_deref()),
                    &row,
                    visibility,
                )?);
            }
            let has_default = store::default_for(&conn, &caller, Capability::Chat)?.is_some();
            Ok(Some((out, has_default)))
        })
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(no_space)?;
    let (models, has_default) = models;
    // Ask agent runs the operator's model only for someone without a
    // default chat connection; it is never a fallback after a failure.
    let operator = match (&state.config.model, has_default) {
        (Some(config), false) => Some(config.model.clone()),
        _ => None,
    };
    let chat_providers = group_chat(&models, operator.as_deref());
    Ok(Json(json!({
        "models": models,
        "chat_providers": chat_providers,
        "operator_model": operator.map(|m| json!({"model_id": m, "label": OPERATOR_LABEL})),
    })))
}

/// How pickers label the operator's `METRO_MODEL`.
pub const OPERATOR_LABEL: &str = "Server model (operator)";

/// Usable chat connections grouped by provider, in listing order (own
/// first), each with its selectable models; then the operator's model when
/// it is what Ask agent uses for this person.
fn group_chat(models: &[AvailableModel], operator: Option<&str>) -> Vec<serde_json::Value> {
    let mut groups: Vec<(String, String, Vec<serde_json::Value>)> = Vec::new();
    for m in models {
        let Some(chat) = m.chat_models.as_ref().filter(|c| !c.is_empty()) else {
            continue;
        };
        let connection = json!({
            "connection_id": m.connection_id,
            "name": m.name,
            "owned": m.owned,
            "owner_id": m.owner_id,
            "visibility": m.visibility,
            "execution": m.execution,
            "runs_on": m.runs_on,
            "billing": m.billing,
            "ready": m.ready,
            "is_default": m.is_default,
            "default_model": m.model,
            "operator": false,
            "models": chat,
        });
        match groups.iter_mut().find(|(id, ..)| *id == m.provider_id) {
            Some((.., list)) => list.push(connection),
            None => groups.push((
                m.provider_id.clone(),
                m.provider_name.clone(),
                vec![connection],
            )),
        }
    }
    let mut out: Vec<serde_json::Value> = groups
        .into_iter()
        .map(|(id, name, connections)| {
            json!({"provider_id": id, "provider_name": name, "connections": connections})
        })
        .collect();
    if let Some(model) = operator {
        out.push(json!({
            "provider_id": "operator",
            "provider_name": OPERATOR_LABEL,
            "connections": [{
                "connection_id": null,
                "name": OPERATOR_LABEL,
                "owned": false,
                "owner_id": null,
                "visibility": null,
                "execution": "server",
                "runs_on": "server",
                "billing": "operator",
                "ready": true,
                "is_default": true,
                "default_model": model,
                "operator": true,
                "models": [{
                    "id": model,
                    "display_name": model,
                    "reasoning_efforts": [],
                    "is_default": true,
                    "available": true,
                }],
            }],
        }));
    }
    out
}
