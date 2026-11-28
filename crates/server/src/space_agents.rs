//! Agents of a Space: several named agents per Space, each with a handle
//! people can mention (`@coder`), a one-line description, owner-written
//! instructions, an avatar and an allowlist of the tools its runs may get.
//!
//! - Every Space has at least one agent and exactly one **default** agent.
//!   The generic `@agent` and Ask agent without an agent id mean the
//!   default agent (or the one the sender's opt-in classifier picks; see
//!   [`crate::space_prefs`]). A Space created before agents existed gets
//!   one default agent named [`Activity::AGENT_NAME`] with the handle
//!   `agent` and the original default tools, so its behaviour is unchanged.
//! - Members read every field, instructions included. Only the owner
//!   creates, edits, deletes and sets avatars. Outsiders and unknown ids
//!   get 404, members 403.
//! - The last agent can't be deleted; deleting the default promotes the
//!   oldest remaining agent.
//! - Instructions are appended after Metro's built-in preamble; they can't
//!   replace it ([`crate::agent`]).
//! - Avatars follow the personal avatar rules ([`crate::profiles`]): PNG or
//!   JPEG upload, decoded under limits and re-encoded as a clean PNG, read
//!   only with a member's bearer token, never cached. Without an image the
//!   client shows `initial` on `color`.

use axum::Json;
use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::json;

use crate::AppState;
use crate::api::{ApiError, ApiResult, JsonBody, db, member_of, owner_of};
use crate::auth::Human;
use crate::db::{Activity, now_ms, short_id};
use crate::tools::RunnerTool;

/// Most agents one Space may have.
pub const MAX_AGENTS: usize = 16;
/// Longest handle (`[a-z0-9_-]`).
pub const MAX_HANDLE: usize = 32;
/// Longest agent name, in characters.
pub const MAX_NAME: usize = 64;
/// Longest description (one line), in characters.
pub const MAX_DESCRIPTION: usize = 160;
/// Longest instructions, in characters.
pub const MAX_INSTRUCTIONS: usize = 4_000;
/// The handle of the agent every existing Space gets, and the reserved
/// text handle of the generic `@agent`; no other agent may take it.
pub const DEFAULT_HANDLE: &str = "agent";
/// The thread tools (`thread_create`, `thread_send`, ...), as one entry of
/// the allowlist.
pub const THREADS_TOOL: &str = "threads";
/// The colours an agent without an image may be shown on.
pub const COLORS: [&str; 8] = [
    "#4f46e5", "#0891b2", "#059669", "#ca8a04", "#ea580c", "#dc2626", "#db2777", "#7c3aed",
];
const NO_AGENT: &str = "no such agent";

/// Every entry an agent's tool allowlist may hold. The web tools run on the
/// computer of whoever asks (`crate::web_tools`).
pub fn all_tools() -> Vec<String> {
    RunnerTool::ALL
        .iter()
        .map(|t| t.runner_name().to_owned())
        .chain([THREADS_TOOL.to_owned()])
        .chain(crate::pages::agent_tools::NAMES.map(str::to_owned))
        .chain([
            crate::web_tools::WEB_SEARCH.to_owned(),
            crate::web_tools::WEB_FETCH.to_owned(),
        ])
        .collect()
}

/// Pages and web tools are opt-in, even for the Space's built-in agent.
pub(crate) fn default_tools() -> Vec<String> {
    RunnerTool::DEFAULT
        .iter()
        .map(|t| t.runner_name().to_owned())
        .chain([THREADS_TOOL.to_owned()])
        .collect()
}

/// One agent of a Space, as members see it.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct SpaceAgent {
    pub id: String,
    pub space_id: String,
    pub name: String,
    pub handle: String,
    pub description: String,
    pub instructions: String,
    /// Allowed tools: entries of [`all_tools`]. Empty: no optional tools.
    pub tools: Vec<String>,
    pub is_default: bool,
    /// Changes on every avatar upload; `None` without an image.
    pub avatar_version: Option<String>,
    /// Shown behind `initial` when there is no image.
    pub color: String,
    pub initial: String,
    #[serde(serialize_with = "rfc3339")]
    pub created_at: i64,
    #[serde(serialize_with = "rfc3339")]
    pub updated_at: i64,
}

fn rfc3339<S: serde::Serializer>(ms: &i64, s: S) -> Result<S::Ok, S::Error> {
    match crate::db::rfc3339_utc(*ms) {
        Some(t) => s.serialize_str(&t),
        None => s.serialize_none(),
    }
}

impl SpaceAgent {
    pub fn allows(&self, tool: &str) -> bool {
        self.tools.iter().any(|t| t == tool)
    }

    /// Who a run is, as carried on its events.
    pub fn badge(&self) -> crate::db::AgentBadge {
        crate::db::AgentBadge {
            id: self.id.clone(),
            handle: self.handle.clone(),
            name: self.name.clone(),
            avatar_version: self.avatar_version.clone(),
            color: self.color.clone(),
        }
    }
}

fn initial(name: &str) -> String {
    name.chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_else(|| "?".to_owned())
}

// ---- Schema ----

/// Create the table and give every Space without an agent its default one.
/// Idempotent; called from `Db::init` after `spaces` exists.
pub fn init(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS space_agents (
             id             TEXT PRIMARY KEY NOT NULL,
             space_id       TEXT NOT NULL REFERENCES spaces (id),
             name           TEXT NOT NULL,
             handle         TEXT NOT NULL,
             description    TEXT NOT NULL,
             instructions   TEXT NOT NULL DEFAULT '',
             tools          TEXT NOT NULL DEFAULT '[]',
             is_default     INTEGER NOT NULL DEFAULT 0 CHECK (is_default IN (0, 1)),
             color          TEXT NOT NULL,
             avatar_png     BLOB,
             avatar_version TEXT,
             created_at     INTEGER NOT NULL,
             updated_at     INTEGER NOT NULL,
             UNIQUE (space_id, handle)
         );
         CREATE INDEX IF NOT EXISTS space_agents_space ON space_agents (space_id, created_at);
         CREATE UNIQUE INDEX IF NOT EXISTS space_agents_one_default
             ON space_agents (space_id) WHERE is_default = 1;",
    )?;
    let missing: Vec<String> = {
        let mut stmt = conn.prepare(
            "SELECT id FROM spaces s
             WHERE NOT EXISTS (SELECT 1 FROM space_agents a WHERE a.space_id = s.id)",
        )?;
        stmt.query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    for sid in missing {
        insert_default(conn, &sid, "The Space's built-in agent.", "")?;
    }
    Ok(())
}

/// Give agents with the full 0.6.2 runner tool set the two new defaults.
/// Keep the original order and leave narrower allowlists as saved.
pub(crate) fn migrate_runner_tools(conn: &Connection) -> anyhow::Result<()> {
    const OLD_DEFAULTS: [&str; 5] = [
        "workspace.list",
        "workspace.read",
        "workspace.check",
        "workspace.mcp_list",
        "workspace.mcp",
    ];
    let tx = conn.unchecked_transaction()?;
    if tx.execute(
        "INSERT INTO metro_meta (key, value) VALUES ('agent_runner_tools_v064', '1')
         ON CONFLICT (key) DO NOTHING",
        [],
    )? == 1
    {
        let agents: Vec<(String, String)> = {
            let mut stmt = tx.prepare("SELECT id, tools FROM space_agents")?;
            stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?
        };
        let mut update = tx.prepare("UPDATE space_agents SET tools = ?1 WHERE id = ?2")?;
        for (id, raw) in agents {
            // Like normal reads, never grant tools from an invalid allowlist.
            let Ok(mut tools) = serde_json::from_str::<Vec<String>>(&raw) else {
                continue;
            };
            if !OLD_DEFAULTS
                .iter()
                .all(|tool| tools.iter().any(|t| t == tool))
            {
                continue;
            }
            let before = tools.len();
            for tool in [RunnerTool::Write, RunnerTool::Shell].map(RunnerTool::runner_name) {
                if !tools.iter().any(|t| t == tool) {
                    tools.push(tool.to_owned());
                }
            }
            if tools.len() != before {
                update.execute(params![serde_json::to_string(&tools)?, id])?;
            }
        }
    }
    tx.commit()?;
    Ok(())
}

/// The default agent of a new Space, in its creating transaction.
pub fn seed_default(tx: &Transaction<'_>, space_id: &str) -> rusqlite::Result<()> {
    insert_default(
        tx,
        space_id,
        "Helps everyone in this Space with questions and tasks.",
        "You are this Space's general helper. Use the Space context to understand who is here and what the Space \
         is for. Prefer answers the whole room can use. If another agent in the roster is clearly better suited, \
         say which one (by @handle) instead of guessing. Ask one short question when a request is ambiguous.",
    )
}

fn insert_default(
    conn: &Connection,
    space_id: &str,
    description: &str,
    instructions: &str,
) -> rusqlite::Result<()> {
    let now = now_ms();
    conn.execute(
        "INSERT INTO space_agents (id, space_id, name, handle, description, instructions, tools,
                                   is_default, color, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1, ?8, ?9, ?9)",
        params![
            new_id(),
            space_id,
            Activity::AGENT_NAME,
            DEFAULT_HANDLE,
            description,
            instructions,
            serde_json::to_string(&default_tools()).unwrap_or_else(|_| "[]".to_owned()),
            COLORS[0],
            now,
        ],
    )?;
    Ok(())
}

fn new_id() -> String {
    format!("ag-{}", short_id())
}

pub fn valid_agent_id(id: &str) -> bool {
    id.len() == 19
        && id.starts_with("ag-")
        && id[3..]
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

const COLUMNS: &str = "id, space_id, name, handle, description, instructions, tools, is_default,
                       avatar_version, color, created_at, updated_at";

fn row_agent(r: &rusqlite::Row<'_>) -> rusqlite::Result<SpaceAgent> {
    let tools: String = r.get(6)?;
    let name: String = r.get(2)?;
    Ok(SpaceAgent {
        id: r.get(0)?,
        space_id: r.get(1)?,
        initial: initial(&name),
        name,
        handle: r.get(3)?,
        description: r.get(4)?,
        instructions: r.get(5)?,
        // Unreadable lists grant nothing.
        tools: serde_json::from_str(&tools).unwrap_or_default(),
        is_default: r.get(7)?,
        avatar_version: r.get(8)?,
        color: r.get(9)?,
        created_at: r.get(10)?,
        updated_at: r.get(11)?,
    })
}

/// The agents of `space_id`, the default first, then oldest first.
pub fn list_in(conn: &Connection, space_id: &str) -> rusqlite::Result<Vec<SpaceAgent>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {COLUMNS} FROM space_agents WHERE space_id = ?1
         ORDER BY is_default DESC, created_at, rowid"
    ))?;
    stmt.query_map([space_id], row_agent)?.collect()
}

pub fn get_in(conn: &Connection, space_id: &str, id: &str) -> rusqlite::Result<Option<SpaceAgent>> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM space_agents WHERE space_id = ?1 AND id = ?2"),
        params![space_id, id],
        row_agent,
    )
    .optional()
}

impl crate::db::Db {
    pub fn space_agents(&self, space_id: &str) -> anyhow::Result<Vec<SpaceAgent>> {
        Ok(list_in(&self.lock(), space_id)?)
    }

    pub fn space_agent(&self, space_id: &str, id: &str) -> anyhow::Result<Option<SpaceAgent>> {
        Ok(get_in(&self.lock(), space_id, id)?)
    }

    pub fn default_space_agent(&self, space_id: &str) -> anyhow::Result<Option<SpaceAgent>> {
        Ok(list_in(&self.lock(), space_id)?
            .into_iter()
            .find(|a| a.is_default))
    }
}

// ---- Validation ----

fn check_agent_name(name: &str) -> ApiResult<String> {
    let name = name.trim();
    if name.is_empty()
        || name.chars().count() > MAX_NAME
        || name
            .chars()
            .any(|c| c.is_control() || crate::admin_users::deceptive(c))
    {
        return Err(ApiError::bad_request(format!(
            "name must be 1..={MAX_NAME} printable characters"
        )));
    }
    Ok(name.to_owned())
}

fn check_handle(handle: &str) -> ApiResult<String> {
    if handle.is_empty()
        || handle.len() > MAX_HANDLE
        || !handle
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
    {
        return Err(ApiError::bad_request(format!(
            "handle must be 1..={MAX_HANDLE} of a-z, 0-9, _ and -"
        )));
    }
    Ok(handle.to_owned())
}

fn check_description(text: &str) -> ApiResult<String> {
    let text = text.trim();
    if text.is_empty()
        || text.chars().count() > MAX_DESCRIPTION
        || text
            .chars()
            .any(|c| c.is_control() || crate::admin_users::deceptive(c))
    {
        return Err(ApiError::bad_request(format!(
            "description must be one line of 1..={MAX_DESCRIPTION} printable characters"
        )));
    }
    Ok(text.to_owned())
}

fn check_instructions(text: &str) -> ApiResult<String> {
    let text = text.replace("\r\n", "\n");
    let text = text.trim();
    if text.chars().count() > MAX_INSTRUCTIONS
        || text
            .chars()
            .any(|c| (c.is_control() && c != '\n' && c != '\t') || crate::admin_users::deceptive(c))
    {
        return Err(ApiError::bad_request(format!(
            "instructions must be at most {MAX_INSTRUCTIONS} characters"
        )));
    }
    Ok(text.to_owned())
}

fn check_tools(tools: &[String]) -> ApiResult<Vec<String>> {
    let all = all_tools();
    let mut out: Vec<String> = Vec::new();
    for t in tools {
        if !all.contains(t) {
            return Err(ApiError::bad_request(format!(
                "tools may only name {}",
                all.join(", ")
            )));
        }
        if !out.contains(t) {
            out.push(t.clone());
        }
    }
    // Stored in catalogue order.
    Ok(all.into_iter().filter(|t| out.contains(t)).collect())
}

fn check_color(color: &str) -> ApiResult<String> {
    if COLORS.contains(&color) {
        Ok(color.to_owned())
    } else {
        Err(ApiError::bad_request(format!(
            "color must be one of {}",
            COLORS.join(", ")
        )))
    }
}

// ---- Writes ----

pub enum Write {
    Done(Box<SpaceAgent>),
    NotFound,
    HandleTaken,
    TooMany,
    LastAgent,
}

/// What a create or PATCH sets; `None` keeps the current value.
#[derive(Default)]
struct Fields {
    name: Option<String>,
    handle: Option<String>,
    description: Option<String>,
    instructions: Option<String>,
    tools: Option<Vec<String>>,
    color: Option<String>,
    make_default: bool,
}

/// Whether `handle` would be ambiguous in `space_id`: another agent has
/// it, or a current member's id is the same text handle.
fn handle_taken(
    conn: &Connection,
    space_id: &str,
    handle: &str,
    except: Option<&str>,
) -> rusqlite::Result<bool> {
    let agent: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM space_agents WHERE space_id = ?1 AND handle = ?2
                        AND id IS NOT ?3)",
        params![space_id, handle, except],
        |r| r.get(0),
    )?;
    let member: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM space_members WHERE space_id = ?1
                        AND lower(user_id) = ?2)",
        params![space_id, handle],
        |r| r.get(0),
    )?;
    Ok(agent || member)
}

fn create(conn: &mut Connection, space_id: &str, f: Fields) -> anyhow::Result<Write> {
    let tx = conn.transaction()?;
    let count: i64 = tx.query_row(
        "SELECT COUNT(*) FROM space_agents WHERE space_id = ?1",
        [space_id],
        |r| r.get(0),
    )?;
    if usize::try_from(count).unwrap_or(usize::MAX) >= MAX_AGENTS {
        return Ok(Write::TooMany);
    }
    let handle = f.handle.unwrap_or_default();
    if handle_taken(&tx, space_id, &handle, None)? {
        return Ok(Write::HandleTaken);
    }
    let id = new_id();
    if f.make_default {
        tx.execute(
            "UPDATE space_agents SET is_default = 0 WHERE space_id = ?1",
            [space_id],
        )?;
    }
    let now = now_ms();
    tx.execute(
        "INSERT INTO space_agents (id, space_id, name, handle, description, instructions, tools,
                                   is_default, color, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)",
        params![
            id,
            space_id,
            f.name.unwrap_or_default(),
            handle,
            f.description.unwrap_or_default(),
            f.instructions.unwrap_or_default(),
            serde_json::to_string(&f.tools.unwrap_or_default())?,
            f.make_default,
            f.color
                .unwrap_or_else(|| COLORS[(count as usize) % COLORS.len()].to_owned()),
            now,
        ],
    )?;
    let agent = get_in(&tx, space_id, &id)?.expect("just inserted");
    tx.commit()?;
    Ok(Write::Done(Box::new(agent)))
}

fn update(conn: &mut Connection, space_id: &str, id: &str, f: Fields) -> anyhow::Result<Write> {
    let tx = conn.transaction()?;
    let Some(current) = get_in(&tx, space_id, id)? else {
        return Ok(Write::NotFound);
    };
    if let Some(handle) = &f.handle
        && *handle != current.handle
        && handle_taken(&tx, space_id, handle, Some(id))?
    {
        return Ok(Write::HandleTaken);
    }
    if f.make_default && !current.is_default {
        tx.execute(
            "UPDATE space_agents SET is_default = 0 WHERE space_id = ?1",
            [space_id],
        )?;
    }
    tx.execute(
        "UPDATE space_agents SET name = ?3, handle = ?4, description = ?5, instructions = ?6,
             tools = ?7, color = ?8, is_default = ?9, updated_at = ?10
         WHERE space_id = ?1 AND id = ?2",
        params![
            space_id,
            id,
            f.name.unwrap_or(current.name),
            f.handle.unwrap_or(current.handle),
            f.description.unwrap_or(current.description),
            f.instructions.unwrap_or(current.instructions),
            serde_json::to_string(&f.tools.unwrap_or(current.tools))?,
            f.color.unwrap_or(current.color),
            f.make_default || current.is_default,
            now_ms(),
        ],
    )?;
    let agent = get_in(&tx, space_id, id)?.expect("still there");
    tx.commit()?;
    Ok(Write::Done(Box::new(agent)))
}

fn remove(conn: &mut Connection, space_id: &str, id: &str) -> anyhow::Result<Write> {
    let tx = conn.transaction()?;
    let agents = list_in(&tx, space_id)?;
    let Some(agent) = agents.iter().find(|a| a.id == id).cloned() else {
        return Ok(Write::NotFound);
    };
    if agents.len() <= 1 {
        return Ok(Write::LastAgent);
    }
    tx.execute(
        "DELETE FROM space_agents WHERE space_id = ?1 AND id = ?2",
        params![space_id, id],
    )?;
    if agent.is_default {
        tx.execute(
            "UPDATE space_agents SET is_default = 1 WHERE id = (
                 SELECT id FROM space_agents WHERE space_id = ?1 ORDER BY created_at, rowid LIMIT 1)",
            [space_id],
        )?;
    }
    // Nobody's per-agent model preference outlives the agent.
    crate::space_prefs::agent_deleted(&tx, space_id, id)?;
    tx.commit()?;
    Ok(Write::Done(Box::new(agent)))
}

fn write_result(w: Write) -> ApiResult<SpaceAgent> {
    match w {
        Write::Done(agent) => Ok(*agent),
        Write::NotFound => Err(ApiError::not_found(NO_AGENT)),
        Write::HandleTaken => Err(ApiError::new(
            StatusCode::CONFLICT,
            "that handle is already used by another agent or a member of this Space",
        )),
        Write::TooMany => Err(ApiError::new(
            StatusCode::CONFLICT,
            format!("a Space can have at most {MAX_AGENTS} agents"),
        )),
        Write::LastAgent => Err(ApiError::new(
            StatusCode::CONFLICT,
            "a Space needs at least one agent; add another before deleting this one",
        )),
    }
}

// ---- HTTP ----

/// `GET|POST /v1/spaces/{sid}/agents`, `GET|PATCH|DELETE
/// /v1/spaces/{sid}/agents/{aid}`, `GET|PUT|DELETE
/// /v1/spaces/{sid}/agents/{aid}/avatar`.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/spaces/{sid}/agents", get(list).post(create_agent))
        .route(
            "/v1/spaces/{sid}/agents/{aid}",
            get(get_agent).patch(patch_agent).delete(delete_agent),
        )
        .route(
            "/v1/spaces/{sid}/agents/{aid}/avatar",
            get(get_avatar)
                .put(put_avatar)
                .delete(delete_avatar)
                .layer(DefaultBodyLimit::max(crate::profiles::MAX_AVATAR_BYTES)),
        )
}

async fn list(
    State(state): State<AppState>,
    who: Human,
    Path(sid): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let space = member_of(&state, &sid, &who).await?;
    let agents = db(&state, move |db| db.space_agents(&space.id)).await?;
    Ok(Json(json!({
        "agents": agents,
        "tools": all_tools(),
        "default_tools": default_tools(),
    })))
}

async fn agent_of(state: &AppState, sid: &str, aid: &str) -> ApiResult<SpaceAgent> {
    if !valid_agent_id(aid) {
        return Err(ApiError::not_found(NO_AGENT));
    }
    let (sid, aid) = (sid.to_owned(), aid.to_owned());
    db(state, move |db| db.space_agent(&sid, &aid))
        .await?
        .ok_or_else(|| ApiError::not_found(NO_AGENT))
}

async fn get_agent(
    State(state): State<AppState>,
    who: Human,
    Path((sid, aid)): Path<(String, String)>,
) -> ApiResult<Json<SpaceAgent>> {
    let space = member_of(&state, &sid, &who).await?;
    agent_of(&state, &space.id, &aid).await.map(Json)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateBody {
    name: String,
    handle: String,
    description: String,
    #[serde(default)]
    instructions: String,
    /// Default: [`default_tools`].
    #[serde(default)]
    tools: Option<Vec<String>>,
    #[serde(default)]
    color: Option<String>,
    #[serde(default)]
    is_default: bool,
}

async fn create_agent(
    State(state): State<AppState>,
    who: Human,
    Path(sid): Path<String>,
    body: JsonBody<CreateBody>,
) -> ApiResult<(StatusCode, Json<SpaceAgent>)> {
    let space = owner_of(&state, &sid, &who).await?;
    let Json(body) = body?;
    let handle = check_handle(&body.handle)?;
    if handle == DEFAULT_HANDLE {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "@agent is reserved for the Space's default agent; choose another handle",
        ));
    }
    let fields = Fields {
        name: Some(check_agent_name(&body.name)?),
        handle: Some(handle),
        description: Some(check_description(&body.description)?),
        instructions: Some(check_instructions(&body.instructions)?),
        tools: Some(match &body.tools {
            Some(tools) => check_tools(tools)?,
            None => default_tools(),
        }),
        color: body.color.as_deref().map(check_color).transpose()?,
        make_default: body.is_default,
    };
    let w = db(&state, move |db| create(&mut db.lock(), &space.id, fields)).await?;
    write_result(w).map(|a| (StatusCode::CREATED, Json(a)))
}

/// `Some(None)` for an explicit `null`.
fn nullable<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Option<bool>>, D::Error> {
    Option::<bool>::deserialize(d).map(Some)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatchBody {
    name: Option<String>,
    handle: Option<String>,
    description: Option<String>,
    instructions: Option<String>,
    tools: Option<Vec<String>>,
    color: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    is_default: Option<Option<bool>>,
}

async fn patch_agent(
    State(state): State<AppState>,
    who: Human,
    Path((sid, aid)): Path<(String, String)>,
    body: JsonBody<PatchBody>,
) -> ApiResult<Json<SpaceAgent>> {
    let space = owner_of(&state, &sid, &who).await?;
    let current = agent_of(&state, &space.id, &aid).await?;
    let Json(body) = body?;
    let make_default = match body.is_default {
        None | Some(Some(true)) => body.is_default.is_some(),
        _ => {
            return Err(ApiError::bad_request(
                "is_default can only be set to true; make another agent the default instead",
            ));
        }
    };
    let handle = body.handle.as_deref().map(check_handle).transpose()?;
    if let Some(h) = &handle
        && h == DEFAULT_HANDLE
        && current.handle != DEFAULT_HANDLE
    {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "@agent is reserved for the Space's default agent; choose another handle",
        ));
    }
    let fields = Fields {
        name: body.name.as_deref().map(check_agent_name).transpose()?,
        handle,
        description: body
            .description
            .as_deref()
            .map(check_description)
            .transpose()?,
        instructions: body
            .instructions
            .as_deref()
            .map(check_instructions)
            .transpose()?,
        tools: body.tools.as_deref().map(check_tools).transpose()?,
        color: body.color.as_deref().map(check_color).transpose()?,
        make_default,
    };
    let w = db(&state, move |db| {
        update(&mut db.lock(), &space.id, &current.id, fields)
    })
    .await?;
    write_result(w).map(Json)
}

async fn delete_agent(
    State(state): State<AppState>,
    who: Human,
    Path((sid, aid)): Path<(String, String)>,
) -> ApiResult<Json<serde_json::Value>> {
    let space = owner_of(&state, &sid, &who).await?;
    let current = agent_of(&state, &space.id, &aid).await?;
    let sid = space.id.clone();
    let w = db(&state, move |db| remove(&mut db.lock(), &sid, &current.id)).await?;
    let deleted = write_result(w)?;
    let space_id = space.id.clone();
    let default = db(&state, move |db| db.default_space_agent(&space_id)).await?;
    Ok(Json(json!({
        "deleted": deleted.id,
        "default_agent_id": default.map(|a| a.id),
    })))
}

// ---- Avatars ----

fn png_response(png: Vec<u8>) -> Response {
    let mut response = png.into_response();
    let h = response.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("image/png"));
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; sandbox"),
    );
    h.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("inline; filename=\"avatar.png\""),
    );
    h.insert(
        "cross-origin-resource-policy",
        HeaderValue::from_static("same-origin"),
    );
    response
}

async fn get_avatar(
    State(state): State<AppState>,
    who: Human,
    Path((sid, aid)): Path<(String, String)>,
) -> ApiResult<Response> {
    let space = member_of(&state, &sid, &who).await?;
    let agent = agent_of(&state, &space.id, &aid).await?;
    let png: Option<Vec<u8>> = db(&state, move |db| {
        Ok(db
            .lock()
            .query_row(
                "SELECT avatar_png FROM space_agents
                 WHERE space_id = ?1 AND id = ?2 AND avatar_png IS NOT NULL",
                params![space.id, agent.id],
                |r| r.get(0),
            )
            .optional()?)
    })
    .await?;
    match png {
        Some(png) => Ok(png_response(png)),
        None => Err(ApiError::not_found("no avatar")),
    }
}

async fn put_avatar(
    State(state): State<AppState>,
    who: Human,
    Path((sid, aid)): Path<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Json<SpaceAgent>> {
    let space = owner_of(&state, &sid, &who).await?;
    let agent = agent_of(&state, &space.id, &aid).await?;
    let declared = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| {
            v.split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase()
        });
    let sniffed = if body.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if body.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else {
        None
    };
    match (declared.as_deref(), sniffed) {
        (Some(d @ ("image/png" | "image/jpeg")), Some(s)) if d != s => {
            return Err(ApiError::new(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "the avatar's bytes don't match its Content-Type",
            ));
        }
        (Some("image/png" | "image/jpeg"), _) => {}
        _ => {
            return Err(ApiError::new(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "upload the avatar as image/png or image/jpeg",
            ));
        }
    }
    if body.len() > crate::profiles::MAX_AVATAR_BYTES {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "the avatar is too large",
        ));
    }
    let Ok(permit) = state.avatar_encodes.clone().try_acquire_owned() else {
        return Err(ApiError::unavailable(
            "avatar uploads are busy; try again shortly",
        ));
    };
    let png = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        crate::profiles::reencode_avatar(&body)
    })
    .await
    .map_err(|e| ApiError::internal(e.into()))?
    .map_err(|reason| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, reason))?;
    let version = {
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).map_err(|e| ApiError::internal(anyhow::anyhow!("{e}")))?;
        bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
    };
    let updated = db(&state, move |db| {
        let conn = db.lock();
        conn.execute(
            "UPDATE space_agents SET avatar_png = ?3, avatar_version = ?4, updated_at = ?5
             WHERE space_id = ?1 AND id = ?2",
            params![space.id, agent.id, png, version, now_ms()],
        )?;
        Ok(get_in(&conn, &space.id, &agent.id)?)
    })
    .await?;
    updated
        .map(Json)
        .ok_or_else(|| ApiError::not_found(NO_AGENT))
}

async fn delete_avatar(
    State(state): State<AppState>,
    who: Human,
    Path((sid, aid)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    let space = owner_of(&state, &sid, &who).await?;
    let agent = agent_of(&state, &space.id, &aid).await?;
    db(&state, move |db| {
        db.lock().execute(
            "UPDATE space_agents SET avatar_png = NULL, avatar_version = NULL, updated_at = ?3
             WHERE space_id = ?1 AND id = ?2",
            params![space.id, agent.id, now_ms()],
        )?;
        Ok(())
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shapes() {
        assert!(check_handle("coder_2-x").is_ok());
        for bad in ["", "Coder", "a b", "é", &"a".repeat(33)] {
            assert!(check_handle(bad).is_err(), "{bad}");
        }
        assert!(check_description("Writes code.").is_ok());
        assert!(check_description("two\nlines").is_err());
        assert!(check_description(&"x".repeat(161)).is_err());
        assert!(
            check_tools(&["threads".into(), "workspace.list".into()]).unwrap()[0]
                == "workspace.list"
        );
        assert!(check_tools(&["shell".into()]).is_err());
        assert!(valid_agent_id(&new_id()));
        assert!(!valid_agent_id("ag-xyz"));
        assert_eq!(initial("  coder"), "C");
    }
}
