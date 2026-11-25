//! Each connection's own list of models.
//!
//! A connection keeps the models its owner may pick on it
//! (`provider_connection_models`): every model its provider listed when the
//! owner fetched the list (enabled by default; the owner opts out), and any
//! model id the owner added by hand. `provider_connections.model` stays the
//! connection's default model and is always one of its enabled models.
//!
//! - A refresh adds newly listed models (enabled) and marks models that are
//!   no longer listed unavailable; it never deletes the owner's entries or
//!   names.
//! - Display names: the owner's own name, else the provider's catalog name,
//!   else the id.
//! - Self-hosted OpenAI-compatible endpoints (server-stored or on the
//!   owner's device) carry per-model thinking options: a bounded list of
//!   effort values and the wire they are sent with. Other providers use
//!   Metro's documented lists and reject a thinking setting.
//!
//! Only the owner manages the list. A share grants use of the enabled
//! models, never the list's management.

use axum::Json;
use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::json;

use super::catalog::{self, Adapter, Capability, Entry};
use super::store::{self, Row};
use crate::AppState;
use crate::api::ApiError;
use crate::auth::Human;

type ApiResult<T> = Result<T, ApiError>;

/// Most models one connection keeps.
pub const MAX_MODELS_PER_CONNECTION: usize = super::MAX_LISTED_MODELS;
/// Longest display name, in characters.
pub const MAX_DISPLAY_NAME_CHARS: usize = 128;
/// Most effort values one model offers.
pub const MAX_EFFORTS: usize = 8;
/// Longest effort value.
pub const MAX_EFFORT_CHARS: usize = 32;

/// How a selected effort is sent to a self-hosted OpenAI-compatible model.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum EffortWire {
    /// Nothing is sent (the model takes no effort setting).
    #[default]
    None,
    /// Chat Completions' top-level `"reasoning_effort": EFFORT`.
    ReasoningEffort,
    /// `"reasoning": {"effort": EFFORT}`.
    Reasoning,
}

impl EffortWire {
    /// The request fields for `effort`, or `None` when nothing is sent.
    pub fn params(self, effort: &str) -> Option<serde_json::Value> {
        match self {
            EffortWire::None => None,
            EffortWire::ReasoningEffort => Some(json!({ "reasoning_effort": effort })),
            EffortWire::Reasoning => Some(json!({ "reasoning": { "effort": effort } })),
        }
    }
}

/// A self-hosted model's thinking options.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Thinking {
    /// The effort values a turn may pick, in the order shown.
    #[serde(default)]
    pub efforts: Vec<String>,
    #[serde(default)]
    pub wire: EffortWire,
}

/// Validate thinking options; `None` for "no thinking options".
pub fn check_thinking(t: Thinking) -> Result<Option<Thinking>, String> {
    if t.efforts.len() > MAX_EFFORTS {
        return Err(format!("thinking.efforts has at most {MAX_EFFORTS} values"));
    }
    let mut seen: Vec<String> = Vec::with_capacity(t.efforts.len());
    for e in &t.efforts {
        let ok = !e.is_empty()
            && e.len() <= MAX_EFFORT_CHARS
            && e.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-'));
        if !ok {
            return Err(format!(
                "thinking.efforts values must be 1..={MAX_EFFORT_CHARS} characters of a-z, 0-9, \
                 '_' or '-'"
            ));
        }
        if seen.contains(e) {
            return Err("thinking.efforts has a duplicate value".into());
        }
        seen.push(e.clone());
    }
    match (t.efforts.is_empty(), t.wire) {
        (true, EffortWire::None) => Ok(None),
        (true, _) => Err("thinking.wire needs at least one value in thinking.efforts".into()),
        (false, EffortWire::None) => Err(
            "thinking.efforts needs a thinking.wire (\"reasoning_effort\" or \"reasoning\")".into(),
        ),
        (false, _) => Ok(Some(Thinking {
            efforts: seen,
            wire: t.wire,
        })),
    }
}

/// Whether connections of `entry` take per-model thinking options: the
/// self-hosted OpenAI-compatible adapters (custom, on the device, and the
/// registered OpenAI-format gateways other than OpenAI itself).
pub fn custom_thinking(entry: &Entry) -> bool {
    entry.capability == Capability::Chat
        && match entry.adapter {
            Adapter::Custom => true,
            Adapter::Device => entry.id == catalog::DEVICE_OPENAI_COMPATIBLE,
            Adapter::Registered(_) => entry.format == "openai" && entry.id != "openai",
            Adapter::SystemOne | Adapter::Embeddings => false,
        }
}

/// Where an entry came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// The provider's model list.
    Listed,
    /// Added by the owner (or the connection's model before model lists).
    Manual,
}

/// One model of a connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelRow {
    pub model_id: String,
    /// The owner's own name.
    pub display_name: Option<String>,
    /// The provider's catalog name, from the last listing.
    pub listed_name: Option<String>,
    pub enabled: bool,
    /// `false` once a refresh no longer lists it.
    pub available: bool,
    pub source: Source,
    pub position: i64,
    pub thinking: Option<Thinking>,
}

impl ModelRow {
    /// What pickers show.
    pub fn name(&self) -> &str {
        self.display_name
            .as_deref()
            .or(self.listed_name.as_deref())
            .unwrap_or(&self.model_id)
    }
}

/// Create the table. On first creation, every existing connection's model
/// becomes its one enabled entry.
pub fn init(conn: &Connection) -> rusqlite::Result<()> {
    let existed: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master
                        WHERE type = 'table' AND name = 'provider_connection_models')",
        [],
        |r| r.get(0),
    )?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS provider_connection_models (
             connection_id TEXT NOT NULL
                 REFERENCES provider_connections (id) ON DELETE CASCADE,
             model_id      TEXT NOT NULL CHECK (model_id <> ''),
             display_name  TEXT,
             listed_name   TEXT,
             enabled       INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
             available     INTEGER NOT NULL DEFAULT 1 CHECK (available IN (0, 1)),
             source        TEXT NOT NULL DEFAULT 'manual' CHECK (source IN ('listed', 'manual')),
             position      INTEGER NOT NULL DEFAULT 0,
             thinking      TEXT,
             PRIMARY KEY (connection_id, model_id)
         );",
    )?;
    if !existed {
        conn.execute_batch(
            "INSERT OR IGNORE INTO provider_connection_models
                 (connection_id, model_id, enabled, available, source, position)
             SELECT id, model, 1, 1, 'manual', 0 FROM provider_connections WHERE model <> '';",
        )?;
    }
    Ok(())
}

const COLUMNS: &str =
    "model_id, display_name, listed_name, enabled, available, source, position, thinking";

fn read(r: &rusqlite::Row<'_>) -> rusqlite::Result<ModelRow> {
    let source: String = r.get(5)?;
    let thinking: Option<String> = r.get(7)?;
    Ok(ModelRow {
        model_id: r.get(0)?,
        display_name: r.get(1)?,
        listed_name: r.get(2)?,
        enabled: r.get(3)?,
        available: r.get(4)?,
        source: if source == "listed" {
            Source::Listed
        } else {
            Source::Manual
        },
        position: r.get(6)?,
        // A stored value is always valid; anything else reads as none.
        thinking: thinking.and_then(|t| serde_json::from_str(&t).ok()),
    })
}

/// The models of connection `id`, in order.
pub fn list(conn: &Connection, id: &str) -> rusqlite::Result<Vec<ModelRow>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {COLUMNS} FROM provider_connection_models WHERE connection_id = ?1
         ORDER BY position, model_id"
    ))?;
    stmt.query_map([id], read)?.collect()
}

pub fn get(conn: &Connection, id: &str, model: &str) -> rusqlite::Result<Option<ModelRow>> {
    conn.query_row(
        &format!(
            "SELECT {COLUMNS} FROM provider_connection_models
             WHERE connection_id = ?1 AND model_id = ?2"
        ),
        params![id, model],
        read,
    )
    .optional()
}

/// The enabled models of connection `id`, in order.
pub fn enabled(conn: &Connection, id: &str) -> rusqlite::Result<Vec<ModelRow>> {
    Ok(list(conn, id)?.into_iter().filter(|m| m.enabled).collect())
}

fn count(conn: &Connection, id: &str) -> rusqlite::Result<usize> {
    conn.query_row(
        "SELECT COUNT(*) FROM provider_connection_models WHERE connection_id = ?1",
        [id],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n as usize)
}

fn next_position(conn: &Connection, id: &str) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT COALESCE(MAX(position) + 1, 0) FROM provider_connection_models
         WHERE connection_id = ?1",
        [id],
        |r| r.get(0),
    )
}

fn thinking_json(t: Option<&Thinking>) -> Option<String> {
    t.map(|t| serde_json::to_string(t).expect("thinking serializes"))
}

/// Make sure `model` is an enabled entry of connection `id` (added by hand
/// when it is new). Used whenever it becomes the connection's model.
pub fn ensure_enabled(conn: &Connection, id: &str, model: &str) -> rusqlite::Result<()> {
    if model.is_empty() {
        return Ok(());
    }
    let position = next_position(conn, id)?;
    conn.execute(
        "INSERT INTO provider_connection_models
             (connection_id, model_id, enabled, available, source, position)
         VALUES (?1, ?2, 1, 1, 'manual', ?3)
         ON CONFLICT (connection_id, model_id) DO UPDATE SET enabled = 1",
        params![id, model, position],
    )?;
    Ok(())
}

/// Add a model by hand. `false` when it is already there or the list is full.
pub fn add_manual(
    conn: &Connection,
    id: &str,
    model: &str,
    display_name: Option<&str>,
    enabled: bool,
    thinking: Option<&Thinking>,
) -> rusqlite::Result<AddOutcome> {
    if get(conn, id, model)?.is_some() {
        return Ok(AddOutcome::Exists);
    }
    if count(conn, id)? >= MAX_MODELS_PER_CONNECTION {
        return Ok(AddOutcome::Full);
    }
    let position = next_position(conn, id)?;
    conn.execute(
        "INSERT INTO provider_connection_models
             (connection_id, model_id, display_name, enabled, available, source, position,
              thinking)
         VALUES (?1, ?2, ?3, ?4, 1, 'manual', ?5, ?6)",
        params![
            id,
            model,
            display_name,
            enabled,
            position,
            thinking_json(thinking)
        ],
    )?;
    Ok(AddOutcome::Added)
}

#[derive(Debug, PartialEq, Eq)]
pub enum AddOutcome {
    Added,
    Exists,
    Full,
}

/// What a refresh changed.
#[derive(Debug, Default, PartialEq, Eq, Serialize)]
pub struct Synced {
    pub added: usize,
    pub unavailable: usize,
}

/// Merge a listing into connection `id`: new ids are added enabled, listed
/// ones become available with their catalog name, and listed-source entries
/// that are no longer listed become unavailable. Nothing is deleted and the
/// owner's names, switches and thinking options stay.
pub fn sync_listed(
    conn: &Connection,
    id: &str,
    listed: &[(String, Option<String>)],
) -> rusqlite::Result<Synced> {
    let mut out = Synced::default();
    let existing = list(conn, id)?;
    let mut total = existing.len();
    let mut position = next_position(conn, id)?;
    for (model, name) in listed {
        match existing.iter().find(|m| &m.model_id == model) {
            Some(_) => {
                conn.execute(
                    "UPDATE provider_connection_models
                     SET available = 1, listed_name = COALESCE(?3, listed_name)
                     WHERE connection_id = ?1 AND model_id = ?2",
                    params![id, model, name],
                )?;
            }
            None if total < MAX_MODELS_PER_CONNECTION => {
                conn.execute(
                    "INSERT INTO provider_connection_models
                         (connection_id, model_id, listed_name, enabled, available, source,
                          position)
                     VALUES (?1, ?2, ?3, 1, 1, 'listed', ?4)",
                    params![id, model, name, position],
                )?;
                position += 1;
                total += 1;
                out.added += 1;
            }
            None => {}
        }
    }
    for m in &existing {
        if m.source == Source::Listed
            && m.available
            && !listed.iter().any(|(l, _)| l == &m.model_id)
        {
            conn.execute(
                "UPDATE provider_connection_models SET available = 0
                 WHERE connection_id = ?1 AND model_id = ?2",
                params![id, m.model_id],
            )?;
            out.unavailable += 1;
        }
    }
    Ok(out)
}

/// A change to one entry. `None` fields stay.
#[derive(Default)]
pub struct Patch {
    pub display_name: Option<Option<String>>,
    pub enabled: Option<bool>,
    pub thinking: Option<Option<Thinking>>,
    pub position: Option<i64>,
}

/// Apply `patch` to `model` of connection `id`; `false` when there is none.
pub fn patch(conn: &Connection, id: &str, model: &str, p: &Patch) -> rusqlite::Result<bool> {
    let Some(mut m) = get(conn, id, model)? else {
        return Ok(false);
    };
    if let Some(name) = &p.display_name {
        m.display_name.clone_from(name);
    }
    if let Some(flag) = p.enabled {
        m.enabled = flag;
    }
    if let Some(t) = &p.thinking {
        m.thinking.clone_from(t);
    }
    if let Some(pos) = p.position {
        m.position = pos;
    }
    conn.execute(
        "UPDATE provider_connection_models
         SET display_name = ?3, enabled = ?4, thinking = ?5, position = ?6
         WHERE connection_id = ?1 AND model_id = ?2",
        params![
            id,
            model,
            m.display_name,
            m.enabled,
            thinking_json(m.thinking.as_ref()),
            m.position
        ],
    )?;
    Ok(true)
}

pub fn remove(conn: &Connection, id: &str, model: &str) -> rusqlite::Result<bool> {
    Ok(conn.execute(
        "DELETE FROM provider_connection_models WHERE connection_id = ?1 AND model_id = ?2",
        params![id, model],
    )? > 0)
}

/// After the list changed: a connection without a model takes its first
/// enabled one, and when that is its first model and its owner has no
/// default for its capability, it becomes that default. The stored row
/// after the change.
pub fn settle(conn: &mut Connection, user: &str, id: &str) -> rusqlite::Result<Option<Row>> {
    let Some(mut row) = store::get(conn, user, id)? else {
        return Ok(None);
    };
    if !row.model.is_empty() {
        return Ok(Some(row));
    }
    let Some(first) = enabled(conn, id)?.into_iter().next() else {
        return Ok(Some(row));
    };
    row.model = first.model_id;
    if store::default_for(conn, user, row.capability)?.is_none() {
        row.is_default = true;
    }
    match store::update(conn, &row)? {
        Some(rev) => row.config_rev = rev,
        None => return Ok(None),
    }
    Ok(Some(row))
}

// ---- Effective effort lists ----

// Exact model IDs and supported levels from developers.openai.com/api/docs/models
// (not name prefixes, third-party gateways or arbitrary compatible endpoints).
pub(super) fn openai_efforts(id: &str) -> &'static [&'static str] {
    match id {
        "gpt-6.2-astra" | "gpt-6.2-sol" | "gpt-6.1-sol" => {
            &["low", "medium", "high", "xhigh", "max"]
        }
        "gpt-5.6" | "gpt-5.6-sol" | "gpt-5.6-terra" => {
            &["none", "low", "medium", "high", "xhigh", "max"]
        }
        "gpt-5.5"
        | "gpt-5.5-2026-04-23"
        | "gpt-5.4"
        | "gpt-5.4-2026-03-05"
        | "gpt-5.4-mini"
        | "gpt-5.4-mini-2026-03-17"
        | "gpt-5.4-nano"
        | "gpt-5.4-nano-2026-03-17"
        | "gpt-5.2"
        | "gpt-5.2-2025-12-11" => &["none", "low", "medium", "high", "xhigh"],
        "gpt-5.3-codex" => &["low", "medium", "high", "xhigh"],
        "gpt-5.1" | "gpt-5.1-2025-11-13" => &["none", "low", "medium", "high"],
        "gpt-5.5-pro" => &["medium", "high", "xhigh"],
        _ => &[],
    }
}

// ChatGPT sign-in follows the Codex catalog. Intersect with the levels the
// connector's wire accepts, not the public API-key model's extra levels.
// https://github.com/openai/codex/blob/6c0cee502d215e5df2dd9a303cf2e252229c9e12/codex-rs/models-manager/models.json
pub(super) fn chatgpt_efforts(id: &str) -> &'static [&'static str] {
    match id {
        "gpt-6.2-astra" | "gpt-6.2-sol" | "gpt-6-astra" | "gpt-6.1-sol" | "gpt-6.1-terra"
        | "gpt-6-sol" | "gpt-6-luna" | "gpt-5.6-sol" | "gpt-5.6-terra" | "gpt-5.6-luna" => {
            &["low", "medium", "high", "xhigh", "max"]
        }
        "gpt-5.5" => &["low", "medium", "high", "xhigh"],
        _ => &[],
    }
}

// https://platform.claude.com/docs/en/build-with-claude/effort
pub(super) fn anthropic_efforts(id: &str) -> &'static [&'static str] {
    match id {
        "claude-opus-5-5" | "claude-sonnet-5-5" | "claude-haiku-5-5" => {
            &["low", "medium", "high", "xhigh", "max"]
        }
        _ => &[],
    }
}

/// The documented efforts of `model` on a non-custom connection `row`
/// (empty when Metro documents none). `device_modern`: whether the device
/// advertises its inventory (older connectors take no effort).
pub(super) fn static_efforts(
    row: &Row,
    entry: &Entry,
    model: &str,
    device_modern: bool,
) -> Vec<String> {
    use rig_core::providers::registry::Format;
    let list: &[&str] = match entry.adapter {
        Adapter::Device if device_modern && row.provider_id == catalog::DEVICE_OPENAI_OAUTH => {
            chatgpt_efforts(model)
        }
        Adapter::Registered(id)
            if row.base_url.is_none()
                && entry.id == "openai"
                && id.format() == Some(Format::OpenAi) =>
        {
            openai_efforts(model)
        }
        Adapter::Registered(id)
            if row.base_url.is_none()
                && entry.id == "anthropic"
                && id.format() == Some(Format::Anthropic) =>
        {
            anthropic_efforts(model)
        }
        _ => &[],
    };
    list.iter().map(|s| (*s).to_owned()).collect()
}

/// The efforts a turn may pick for entry `m` of connection `row`, and the
/// wire of a custom endpoint (`None`: the provider's own wire).
pub(super) fn efforts_of(
    row: &Row,
    entry: &Entry,
    m: &ModelRow,
    device_modern: bool,
) -> (Vec<String>, Option<EffortWire>) {
    if custom_thinking(entry) {
        // A legacy device connector refuses any effort field.
        if entry.is_device() && !device_modern {
            return (Vec::new(), None);
        }
        return match &m.thinking {
            Some(t) => (t.efforts.clone(), Some(t.wire)),
            None => (Vec::new(), Some(EffortWire::None)),
        };
    }
    (static_efforts(row, entry, &m.model_id, device_modern), None)
}

// ---- HTTP ----

/// One model as its connection's owner sees it.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct ModelView {
    pub model_id: String,
    /// What pickers show: `custom_name`, else `listed_name`, else the id.
    pub display_name: String,
    /// The owner's own name.
    pub custom_name: Option<String>,
    /// The provider's catalog name.
    pub listed_name: Option<String>,
    pub enabled: bool,
    pub available: bool,
    pub source: Source,
    pub position: i64,
    /// Whether it is the connection's default model.
    pub is_default: bool,
    /// The owner's thinking options (self-hosted endpoints only).
    pub thinking: Option<Thinking>,
    /// The efforts a turn can pick for it on this connection.
    pub reasoning_efforts: Vec<String>,
}

pub fn views(row: &Row, models: &[ModelRow]) -> Vec<ModelView> {
    let entry = catalog::find(&row.provider_id);
    models
        .iter()
        .map(|m| ModelView {
            model_id: m.model_id.clone(),
            display_name: m.name().to_owned(),
            custom_name: m.display_name.clone(),
            listed_name: m.listed_name.clone(),
            enabled: m.enabled,
            available: m.available,
            source: m.source,
            position: m.position,
            is_default: m.model_id == row.model,
            thinking: m.thinking.clone(),
            reasoning_efforts: entry.map_or_else(Vec::new, |e| efforts_of(row, e, m, true).0),
        })
        .collect()
}

fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, message)
}
fn conflict(message: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::CONFLICT, message)
}
fn no_model() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "that connection has no such model")
}

pub(super) fn check_display_name(name: &str) -> ApiResult<Option<String>> {
    let name = name.trim();
    if name.is_empty() {
        return Ok(None);
    }
    if name.chars().count() > MAX_DISPLAY_NAME_CHARS || name.chars().any(char::is_control) {
        return Err(bad_request(format!(
            "display_name must be at most {MAX_DISPLAY_NAME_CHARS} printable characters"
        )));
    }
    Ok(Some(name.to_owned()))
}

/// A provider catalog name, cleaned the same way; anything unusable is
/// dropped rather than refused.
pub(super) fn clean_listed_name(name: Option<&str>) -> Option<String> {
    name.and_then(|n| check_display_name(n).ok().flatten())
}

fn model_id(raw: &str) -> ApiResult<String> {
    let id = super::api::check_model(raw)?;
    if id.is_empty() {
        return Err(bad_request("model_id is required"));
    }
    Ok(id)
}

pub(super) fn thinking_for(entry: &Entry, raw: Option<Thinking>) -> ApiResult<Option<Thinking>> {
    let Some(raw) = raw else { return Ok(None) };
    let t = check_thinking(raw).map_err(bad_request)?;
    if t.is_some() && !custom_thinking(entry) {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "thinking options can be set only on self-hosted OpenAI-compatible connections; \
             this provider's documented effort levels are used",
        ));
    }
    Ok(t)
}

async fn owned(state: &AppState, who: &Human, id: &str) -> ApiResult<(Row, &'static Entry)> {
    if !super::api::valid_connection_id(id) {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "no such model connection",
        ));
    }
    let (user, id) = (who.id.clone(), id.to_owned());
    let row = state
        .db
        .call(move |db| Ok(store::get(&db.lock(), &user, &id)?))
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "no such model connection"))?;
    let entry = catalog::find(&row.provider_id)
        .ok_or_else(|| conflict("this connection's provider is no longer supported"))?;
    Ok((row, entry))
}

/// The owner's view of connection `row`'s models, read again.
pub(super) fn list_view(conn: &Connection, row: &Row) -> rusqlite::Result<serde_json::Value> {
    let models = list(conn, &row.id)?;
    Ok(json!({
        "connection_id": row.id,
        "default_model": row.model,
        "custom_thinking": catalog::find(&row.provider_id).is_some_and(custom_thinking),
        "models": views(row, &models),
    }))
}

/// `GET /v1/providers/{id}/model-list`
pub async fn get_list(
    State(state): State<AppState>,
    who: Human,
    Path(id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let (row, _) = owned(&state, &who, &id).await?;
    let body = state
        .db
        .call(move |db| Ok(list_view(&db.lock(), &row)?))
        .await
        .map_err(ApiError::internal)?;
    Ok(Json(body))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AddBody {
    model_id: String,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    thinking: Option<Thinking>,
    #[serde(default)]
    enabled: Option<bool>,
    /// Also make it the connection's default model.
    #[serde(default)]
    default: bool,
}

/// `POST /v1/providers/{id}/model-list`: add a model id by hand.
pub async fn add(
    State(state): State<AppState>,
    who: Human,
    Path(id): Path<String>,
    body: Result<Json<AddBody>, JsonRejection>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let (row, entry) = owned(&state, &who, &id).await?;
    let Json(body) = body?;
    let model = model_id(&body.model_id)?;
    let name = body
        .display_name
        .as_deref()
        .map(check_display_name)
        .transpose()?
        .flatten();
    let thinking = thinking_for(entry, body.thinking)?;
    let enabled = body.enabled.unwrap_or(true);
    if body.default && !enabled {
        return Err(conflict("the default model must be enabled"));
    }
    let (user, cid, make_default) = (who.id.clone(), row.id.clone(), body.default);
    let outcome = state
        .db
        .call(move |db| {
            let mut conn = db.lock();
            let tx = conn.transaction()?;
            let outcome = add_manual(
                &tx,
                &cid,
                &model,
                name.as_deref(),
                enabled,
                thinking.as_ref(),
            )?;
            tx.commit()?;
            if outcome != AddOutcome::Added {
                return Ok(Err(outcome));
            }
            if make_default {
                set_default_model(&mut conn, &user, &cid, &model)?;
            }
            let Some(row) = settle(&mut conn, &user, &cid)? else {
                return Ok(Ok(None));
            };
            Ok(Ok(Some(list_view(&conn, &row)?)))
        })
        .await
        .map_err(ApiError::internal)?;
    match outcome {
        Ok(Some(view)) => Ok((StatusCode::CREATED, Json(view))),
        Ok(None) => Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "no such model connection",
        )),
        Err(AddOutcome::Exists) => Err(conflict("that connection already has this model")),
        Err(_) => Err(conflict(format!(
            "a connection keeps at most {MAX_MODELS_PER_CONNECTION} models"
        ))),
    }
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
pub struct PatchBody {
    model_id: String,
    /// A string renames it; `null` or `""` goes back to the catalog name.
    #[serde(default, deserialize_with = "explicit_null")]
    display_name: Option<Option<String>>,
    #[serde(default)]
    enabled: Option<bool>,
    /// An object replaces the thinking options; `null` clears them.
    #[serde(default, deserialize_with = "explicit_null")]
    thinking: Option<Option<Thinking>>,
    #[serde(default)]
    position: Option<i64>,
    /// `true` makes it the connection's default model.
    #[serde(default)]
    default: Option<bool>,
}

fn is_default_model_error() -> ApiError {
    conflict("this is the connection's default model; choose another default model first")
}

/// `PATCH /v1/providers/{id}/model-list`: rename, enable or disable, set
/// thinking options, reorder, or make one model the connection's default.
pub async fn patch_one(
    State(state): State<AppState>,
    who: Human,
    Path(id): Path<String>,
    body: Result<Json<PatchBody>, JsonRejection>,
) -> ApiResult<Json<serde_json::Value>> {
    let (row, entry) = owned(&state, &who, &id).await?;
    let Json(body) = body?;
    let model = model_id(&body.model_id)?;
    if body.default == Some(false) {
        return Err(bad_request(
            "default can only be true; make another model the default instead",
        ));
    }
    let display_name = match body.display_name {
        None => None,
        Some(None) => Some(None),
        Some(Some(n)) => Some(check_display_name(&n)?),
    };
    let thinking = match body.thinking {
        None => None,
        Some(t) => Some(thinking_for(entry, t)?),
    };
    if body
        .position
        .is_some_and(|p| !(0..=(MAX_MODELS_PER_CONNECTION as i64) * 4).contains(&p))
    {
        return Err(bad_request("position is out of range"));
    }
    let p = Patch {
        display_name,
        enabled: body.enabled,
        thinking,
        position: body.position,
    };
    let (user, cid, make_default) = (who.id.clone(), row.id.clone(), body.default == Some(true));
    let outcome = state
        .db
        .call(move |db| {
            let mut conn = db.lock();
            let Some(current) = store::get(&conn, &user, &cid)? else {
                return Ok(Err(ApiError::new(
                    StatusCode::NOT_FOUND,
                    "no such model connection",
                )));
            };
            if p.enabled == Some(false) && current.model == model {
                return Ok(Err(is_default_model_error()));
            }
            if make_default && p.enabled == Some(false) {
                return Ok(Err(conflict("the default model must be enabled")));
            }
            let tx = conn.transaction()?;
            if !patch(&tx, &cid, &model, &p)? {
                return Ok(Err(no_model()));
            }
            tx.commit()?;
            if make_default {
                set_default_model(&mut conn, &user, &cid, &model)?;
            }
            let Some(row) = settle(&mut conn, &user, &cid)? else {
                return Ok(Err(ApiError::new(
                    StatusCode::NOT_FOUND,
                    "no such model connection",
                )));
            };
            Ok(Ok(list_view(&conn, &row)?))
        })
        .await
        .map_err(ApiError::internal)?;
    outcome.map(Json)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoveQuery {
    model_id: String,
}

/// `DELETE /v1/providers/{id}/model-list?model_id=ID`
pub async fn remove_one(
    State(state): State<AppState>,
    who: Human,
    Path(id): Path<String>,
    query: Result<Query<RemoveQuery>, QueryRejection>,
) -> ApiResult<Json<serde_json::Value>> {
    let (row, _) = owned(&state, &who, &id).await?;
    let Query(q) = query?;
    let model = model_id(&q.model_id)?;
    let (user, cid) = (who.id.clone(), row.id.clone());
    let outcome = state
        .db
        .call(move |db| {
            let conn = db.lock();
            let Some(current) = store::get(&conn, &user, &cid)? else {
                return Ok(Err(ApiError::new(
                    StatusCode::NOT_FOUND,
                    "no such model connection",
                )));
            };
            if current.model == model {
                return Ok(Err(is_default_model_error()));
            }
            if !remove(&conn, &cid, &model)? {
                return Ok(Err(no_model()));
            }
            Ok(Ok(list_view(&conn, &current)?))
        })
        .await
        .map_err(ApiError::internal)?;
    outcome.map(Json)
}

/// Make enabled `model` connection `id`'s default model.
fn set_default_model(
    conn: &mut Connection,
    user: &str,
    id: &str,
    model: &str,
) -> rusqlite::Result<bool> {
    let Some(mut row) = store::get(conn, user, id)? else {
        return Ok(false);
    };
    if row.model == model {
        return Ok(true);
    }
    row.model = model.to_owned();
    Ok(store::update(conn, &row)?.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        store::init(&conn).unwrap();
        conn
    }

    fn row(id: &str, model: &str) -> Row {
        Row {
            id: id.into(),
            user_id: "alice".into(),
            name: "c".into(),
            provider_id: catalog::CUSTOM.into(),
            base_url: Some("http://127.0.0.1:1/v1".into()),
            model: model.into(),
            is_default: false,
            sealed_key: None,
            capability: Capability::Chat,
            config_rev: 0,
            edit_rev: 0,
        }
    }

    #[test]
    fn thinking_is_validated_and_bounded() {
        let t = |efforts: &[&str], wire| Thinking {
            efforts: efforts.iter().map(|s| (*s).to_owned()).collect(),
            wire,
        };
        assert_eq!(check_thinking(t(&[], EffortWire::None)), Ok(None));
        assert!(check_thinking(t(&[], EffortWire::Reasoning)).is_err());
        assert!(check_thinking(t(&["low"], EffortWire::None)).is_err());
        assert!(check_thinking(t(&["low", "low"], EffortWire::Reasoning)).is_err());
        assert!(check_thinking(t(&["Low"], EffortWire::Reasoning)).is_err());
        assert!(check_thinking(t(&["a b"], EffortWire::Reasoning)).is_err());
        assert!(check_thinking(t(&[&"x".repeat(33)], EffortWire::Reasoning)).is_err());
        let many: Vec<String> = (0..9).map(|i| format!("e{i}")).collect();
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();
        assert!(check_thinking(t(&refs, EffortWire::Reasoning)).is_err());
        assert_eq!(
            check_thinking(t(&["low", "high"], EffortWire::ReasoningEffort))
                .unwrap()
                .unwrap()
                .efforts,
            ["low", "high"]
        );
        assert_eq!(
            EffortWire::ReasoningEffort.params("low"),
            Some(json!({"reasoning_effort": "low"}))
        );
        assert_eq!(
            EffortWire::Reasoning.params("low"),
            Some(json!({"reasoning": {"effort": "low"}}))
        );
        assert_eq!(EffortWire::None.params("low"), None);
        let bad: Result<Thinking, _> =
            serde_json::from_value(json!({"efforts": ["low"], "wire": "header"}));
        assert!(bad.is_err());
    }

    #[test]
    fn migration_backfills_existing_models_once() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        store::init(&conn).unwrap();
        // Simulate a database from before model lists.
        conn.execute_batch("DROP TABLE provider_connection_models;")
            .unwrap();
        for (id, model) in [("pc_a", "m1"), ("pc_b", "")] {
            conn.execute(
                "INSERT INTO provider_connections
                     (id, user_id, name, provider_id, model, created_at, updated_at)
                 VALUES (?1, 'alice', 'n', 'custom', ?2, 0, 0)",
                params![id, model],
            )
            .unwrap();
        }
        init(&conn).unwrap();
        let a = list(&conn, "pc_a").unwrap();
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].model_id, "m1");
        assert!(a[0].enabled && a[0].available);
        assert!(list(&conn, "pc_b").unwrap().is_empty());
        // Later starts do not re-add a removed entry.
        conn.execute("DELETE FROM provider_connection_models", [])
            .unwrap();
        init(&conn).unwrap();
        assert!(list(&conn, "pc_a").unwrap().is_empty());
    }

    #[test]
    fn sync_adds_enabled_and_marks_vanished_unavailable_keeping_names() {
        let mut conn = db();
        let r = row("pc_x", "pinned");
        assert!(store::insert(&mut conn, &r, 20).unwrap());
        let listed = |ids: &[(&str, Option<&str>)]| -> Vec<(String, Option<String>)> {
            ids.iter()
                .map(|(i, n)| ((*i).to_owned(), n.map(str::to_owned)))
                .collect()
        };
        let s = sync_listed(&conn, "pc_x", &listed(&[("a", Some("A")), ("b", None)])).unwrap();
        assert_eq!(
            s,
            Synced {
                added: 2,
                unavailable: 0
            }
        );
        patch(
            &conn,
            "pc_x",
            "a",
            &Patch {
                display_name: Some(Some("Mine".into())),
                enabled: Some(false),
                ..Patch::default()
            },
        )
        .unwrap();
        let s = sync_listed(&conn, "pc_x", &listed(&[("b", Some("B")), ("c", None)])).unwrap();
        assert_eq!(
            s,
            Synced {
                added: 1,
                unavailable: 1
            }
        );
        let all = list(&conn, "pc_x").unwrap();
        let get = |id: &str| all.iter().find(|m| m.model_id == id).unwrap().clone();
        assert_eq!(get("pinned").source, Source::Manual);
        assert!(get("pinned").available, "manual entries are never marked");
        let a = get("a");
        assert!(!a.available && !a.enabled);
        assert_eq!(a.name(), "Mine");
        assert_eq!(get("b").name(), "B");
        assert!(get("c").enabled);
        // Deleting the connection deletes its list.
        store::delete(&conn, "alice", "pc_x").unwrap();
        assert!(list(&conn, "pc_x").unwrap().is_empty());
    }

    #[test]
    fn settle_fills_a_missing_model_and_first_default() {
        let mut conn = db();
        assert!(store::insert(&mut conn, &row("pc_1", ""), 20).unwrap());
        assert!(store::insert(&mut conn, &row("pc_2", ""), 20).unwrap());
        sync_listed(&conn, "pc_1", &[("m".into(), None)]).unwrap();
        let r = settle(&mut conn, "alice", "pc_1").unwrap().unwrap();
        assert_eq!(r.model, "m");
        assert!(r.is_default);
        sync_listed(&conn, "pc_2", &[("n".into(), None)]).unwrap();
        let r = settle(&mut conn, "alice", "pc_2").unwrap().unwrap();
        assert_eq!(r.model, "n");
        assert!(!r.is_default, "an existing default stays");
    }
}
