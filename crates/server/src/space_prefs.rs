//! One person's preferences in one Space (`GET|PUT /v1/spaces/{sid}/prefs`):
//!
//! - `classify_unaddressed` (default off): let the person's own System One
//!   connection decide whether their unaddressed messages in this Space are
//!   for one of its agents, and which. It can be turned on only while they
//!   have a System One connection usable in this Space (otherwise 409).
//!   Personal Chat sends directly, without classification.
//! - `default_model`: the person's default chat model in this Space.
//! - `agent_models`: per agent id, the person's default chat model for that
//!   agent here.
//! - `routing` (default on): whether the person's use-case routing (when
//!   they turned it on in `/v1/model-routing`) applies in this Space.
//!
//! A run's model: an explicit model for the turn, else the person's model
//! for that agent here, else their model for this Space, else their routing
//! or default chat connection. Every selection is one of the person's own
//! usable connections (or one shared with them here), checked when saved
//! and again when used; a selection that can no longer be used fails the
//! turn visibly. Nothing falls back to another person's connection or
//! `METRO_MODEL`.
//!
//! Only the person reads or writes their own preferences; outsiders get
//! 404. Leaving or being removed from the Space deletes them.

use std::collections::BTreeMap;

use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::routing::get;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::api::{ApiError, ApiResult, JsonBody, db, member_of};
use crate::auth::Human;
use crate::model_routing::{AddressedBy, Route, RouteNote, RouteSource, RoutingSettings};
use crate::providers::UseError;

const MIGRATED_KEY: &str = "space_member_prefs_from_infer_addressing";

/// A chat model choice: a connection, and optionally one of its models
/// and a reasoning effort (as for a turn's explicit model).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    pub provider_connection_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpacePrefs {
    #[serde(default)]
    pub classify_unaddressed: bool,
    #[serde(default)]
    pub default_model: Option<Selection>,
    /// Agent id → selection.
    #[serde(default)]
    pub agent_models: BTreeMap<String, Selection>,
    #[serde(default = "yes")]
    pub routing: bool,
}

fn yes() -> bool {
    true
}

impl Default for SpacePrefs {
    fn default() -> Self {
        Self {
            classify_unaddressed: false,
            default_model: None,
            agent_models: BTreeMap::new(),
            routing: true,
        }
    }
}

// ---- Schema ----

/// Create the table; once, give every person who had turned on the old
/// all-rooms addressing opt-in the same opt-in in each Space they are a
/// member of. Called from `Db::init` after routing settings exist.
pub fn init(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS space_member_prefs (
             space_id             TEXT NOT NULL REFERENCES spaces (id),
             user_id              TEXT NOT NULL,
             classify_unaddressed INTEGER NOT NULL DEFAULT 0 CHECK (classify_unaddressed IN (0, 1)),
             default_model        TEXT,
             agent_models         TEXT NOT NULL DEFAULT '{}',
             routing              INTEGER NOT NULL DEFAULT 1 CHECK (routing IN (0, 1)),
             updated_at           INTEGER NOT NULL,
             PRIMARY KEY (space_id, user_id)
         );",
    )?;
    let done: bool = conn
        .query_row(
            "SELECT 1 FROM metro_meta WHERE key = ?1",
            [MIGRATED_KEY],
            |_| Ok(true),
        )
        .optional()?
        .unwrap_or(false);
    if !done {
        let tx = conn.unchecked_transaction()?;
        tx.execute(
            "INSERT OR IGNORE INTO space_member_prefs (space_id, user_id, classify_unaddressed, updated_at)
             SELECT m.space_id, m.user_id, 1, ?1
             FROM space_members m JOIN model_routing_settings r ON r.user_id = m.user_id
             WHERE json_valid(r.settings)
               AND json_extract(r.settings, '$.infer_addressing') = 1
               AND json_extract(r.settings, '$.classifier_id') IS NOT NULL",
            [crate::db::now_ms()],
        )?;
        tx.execute(
            "INSERT INTO metro_meta (key, value) VALUES (?1, '1')",
            [MIGRATED_KEY],
        )?;
        tx.commit()?;
    }
    Ok(())
}

pub fn member_removed(conn: &Connection, space_id: &str, user_id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM space_member_prefs WHERE space_id = ?1 AND user_id = ?2",
        params![space_id, user_id],
    )?;
    Ok(())
}

pub fn agent_deleted(conn: &Connection, space_id: &str, agent_id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE space_member_prefs SET agent_models = json_remove(agent_models, '$.' || ?2)
         WHERE space_id = ?1 AND json_valid(agent_models)",
        params![space_id, format!("\"{agent_id}\"")],
    )?;
    Ok(())
}

pub(crate) fn load_in(
    conn: &Connection,
    space_id: &str,
    user_id: &str,
) -> rusqlite::Result<SpacePrefs> {
    let row: Option<(bool, Option<String>, String, bool)> = conn
        .query_row(
            "SELECT classify_unaddressed, default_model, agent_models, routing
             FROM space_member_prefs WHERE space_id = ?1 AND user_id = ?2",
            params![space_id, user_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    Ok(match row {
        None => SpacePrefs::default(),
        // Unreadable selections are dropped, never guessed.
        Some((classify, default_model, agent_models, routing)) => SpacePrefs {
            classify_unaddressed: classify,
            default_model: default_model.and_then(|m| serde_json::from_str(&m).ok()),
            agent_models: serde_json::from_str(&agent_models).unwrap_or_default(),
            routing,
        },
    })
}

fn store_in(
    conn: &Connection,
    space_id: &str,
    user_id: &str,
    prefs: &SpacePrefs,
) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO space_member_prefs (space_id, user_id, classify_unaddressed, default_model,
                                         agent_models, routing, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT (space_id, user_id) DO UPDATE SET
             classify_unaddressed = excluded.classify_unaddressed,
             default_model = excluded.default_model, agent_models = excluded.agent_models,
             routing = excluded.routing, updated_at = excluded.updated_at",
        params![
            space_id,
            user_id,
            prefs.classify_unaddressed,
            prefs
                .default_model
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?,
            serde_json::to_string(&prefs.agent_models)?,
            prefs.routing,
            crate::db::now_ms(),
        ],
    )?;
    Ok(())
}

/// `user`'s preferences in `space` (defaults when none were saved).
pub async fn load(state: &AppState, space: &str, user: &str) -> anyhow::Result<SpacePrefs> {
    let (space, user) = (space.to_owned(), user.to_owned());
    state
        .db
        .call(move |db| Ok(load_in(&db.lock(), &space, &user)?))
        .await
}

/// Whether `settings`' System One connection is usable by `user` in `space`.
pub async fn classifier_usable(
    state: &AppState,
    user: &str,
    space: &str,
    settings: &RoutingSettings,
) -> anyhow::Result<bool> {
    match settings.classifier_id.as_deref() {
        Some(id) if crate::providers::api::valid_connection_id(id) => {
            state
                .routing
                .backend
                .system_one_usable(state, user, space, id)
                .await
        }
        _ => Ok(false),
    }
}

// ---- Run routes ----

/// The route of a run `user` starts for agent `agent_id` in `space`, with
/// no explicit model for the turn: their model for the agent here, else
/// their model for the Space, else their routing (if on here) or default.
#[allow(clippy::too_many_arguments)]
pub async fn route_for(
    state: &AppState,
    user: &str,
    space: &str,
    prefs: &SpacePrefs,
    settings: &RoutingSettings,
    agent_id: &str,
    text: &str,
    addressed_by: AddressedBy,
) -> Route {
    if let Some(sel) = prefs.agent_models.get(agent_id) {
        return selection_route(
            state,
            user,
            space,
            sel,
            addressed_by,
            RouteSource::AgentDefault,
        )
        .await;
    }
    if let Some(sel) = &prefs.default_model {
        return selection_route(
            state,
            user,
            space,
            sel,
            addressed_by,
            RouteSource::SpaceDefault,
        )
        .await;
    }
    let mut settings = settings.clone();
    if !prefs.routing {
        settings.enabled = false;
    }
    crate::model_routing::route_turn(
        state,
        user,
        Some(space),
        &settings,
        text,
        addressed_by,
        None,
    )
    .await
}

async fn selection_route(
    state: &AppState,
    user: &str,
    space: &str,
    sel: &Selection,
    addressed_by: AddressedBy,
    source: RouteSource,
) -> Route {
    let reason = match source {
        RouteSource::AgentDefault => "your default model for this agent in this Space",
        _ => "your default model in this Space",
    };
    let mut route = Route::chosen(addressed_by, &sel.provider_connection_id);
    route.note = RouteNote {
        source,
        reason: reason.to_owned(),
        ..route.note
    };
    if sel.model_id.is_some() || sel.reasoning_effort.is_some() {
        match crate::providers::turn::check(
            state,
            user,
            Some(space),
            &sel.provider_connection_id,
            sel.model_id.as_deref(),
            sel.reasoning_effort.as_deref(),
        )
        .await
        {
            Ok(selection) => route.turn_selection = Some(selection),
            Err(e) => {
                // A Space route: the room reads a generic failure.
                let refusal = route.refused(Some(space), &e);
                route.blocked = Some(refusal.shown);
                route.sender_note = refusal.note;
            }
        }
    }
    route
}

// ---- HTTP ----

pub fn router() -> Router<AppState> {
    Router::new().route("/v1/spaces/{sid}/prefs", get(get_prefs).put(put_prefs))
}

#[derive(Serialize)]
struct View {
    #[serde(flatten)]
    prefs: SpacePrefs,
    /// Whether `classify_unaddressed` can be turned on now: the person has
    /// a System One connection usable in this Space.
    classifier_available: bool,
}

async fn view(
    state: &AppState,
    who: &Human,
    sid: &str,
    prefs: SpacePrefs,
) -> ApiResult<Json<View>> {
    let settings = crate::model_routing::preferences(state, &who.id)
        .await
        .map_err(ApiError::internal)?;
    let classifier_available = classifier_usable(state, &who.id, sid, &settings)
        .await
        .map_err(ApiError::internal)?;
    Ok(Json(View {
        prefs,
        classifier_available,
    }))
}

async fn get_prefs(
    State(state): State<AppState>,
    who: Human,
    Path(sid): Path<String>,
) -> ApiResult<Json<View>> {
    let space = member_of(&state, &sid, &who).await?;
    let prefs = load(&state, &space.id, &who.id)
        .await
        .map_err(ApiError::internal)?;
    view(&state, &who, &space.id, prefs).await
}

async fn check_selection(
    state: &AppState,
    who: &Human,
    sid: &str,
    field: &str,
    sel: &Selection,
) -> ApiResult<()> {
    let refused = || {
        ApiError::bad_request(format!(
            "{field} must be a chat connection you can use in this Space"
        ))
    };
    if !crate::providers::api::valid_connection_id(&sel.provider_connection_id) {
        return Err(refused());
    }
    if sel.model_id.is_some() || sel.reasoning_effort.is_some() {
        return crate::providers::turn::check(
            state,
            &who.id,
            Some(sid),
            &sel.provider_connection_id,
            sel.model_id.as_deref(),
            sel.reasoning_effort.as_deref(),
        )
        .await
        .map(drop)
        .map_err(|e| match e {
            UseError::NotFound | UseError::WrongCapability { .. } => refused(),
            e => crate::providers::api::use_error(e),
        });
    }
    match state
        .routing
        .backend
        .chat_model(state, &who.id, Some(sid), &sel.provider_connection_id)
        .await
    {
        Err(UseError::NotFound | UseError::WrongCapability { .. }) => Err(refused()),
        Err(UseError::Internal) => Err(ApiError::internal(anyhow::anyhow!(
            "checking a model connection"
        ))),
        // A usable connection with a passing problem (a device offline) is
        // reported by the turn that uses it.
        _ => Ok(()),
    }
}

async fn put_prefs(
    State(state): State<AppState>,
    who: Human,
    Path(sid): Path<String>,
    body: JsonBody<SpacePrefs>,
) -> ApiResult<Json<View>> {
    let space = member_of(&state, &sid, &who).await?;
    let Json(prefs) = body?;
    if prefs.agent_models.len() > crate::space_agents::MAX_AGENTS {
        return Err(ApiError::bad_request("too many agent_models"));
    }
    let space_id = space.id.clone();
    let agents = db(&state, move |db| db.space_agents(&space_id)).await?;
    for agent_id in prefs.agent_models.keys() {
        if !agents.iter().any(|a| a.id == *agent_id) {
            return Err(ApiError::bad_request(
                "agent_models may only name agents of this Space",
            ));
        }
    }
    if let Some(sel) = &prefs.default_model {
        check_selection(&state, &who, &space.id, "default_model", sel).await?;
    }
    for sel in prefs.agent_models.values() {
        check_selection(&state, &who, &space.id, "agent_models", sel).await?;
    }
    if prefs.classify_unaddressed {
        let settings = crate::model_routing::preferences(&state, &who.id)
            .await
            .map_err(ApiError::internal)?;
        if !classifier_usable(&state, &who.id, &space.id, &settings)
            .await
            .map_err(ApiError::internal)?
        {
            return Err(ApiError::new(
                axum::http::StatusCode::CONFLICT,
                "classify_unaddressed needs a System One connection you can use in this Space; \
                 choose one as classifier_id in your model routing settings first",
            ));
        }
    }
    let (sid, uid, saved) = (space.id.clone(), who.id.clone(), prefs.clone());
    // The membership is rechecked in the write.
    let stored = db(&state, move |db| {
        let conn = db.lock();
        let member: bool = conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM space_members WHERE space_id = ?1 AND user_id = ?2)",
            params![sid, uid],
            |r| r.get(0),
        )?;
        if member {
            store_in(&conn, &sid, &uid, &saved)?;
        }
        Ok(member)
    })
    .await?;
    if !stored {
        return Err(ApiError::not_found("no such Space"));
    }
    view(&state, &who, &space.id, prefs).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    #[test]
    fn upgrades_seed_agents_and_carry_the_old_room_opt_in_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metro.sqlite3");
        let (a, b) = {
            let db = Db::open(&path).unwrap();
            let a = db.create_space("alice", "a", 10).unwrap().id;
            let b = db.create_space("bob", "b", 10).unwrap().id;
            db.add_member(&a, "carol", 10).unwrap();
            (a, b)
        };
        // As a database from before agents and per-Space preferences.
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(&format!(
                "DELETE FROM space_agents; DELETE FROM metro_meta WHERE key = '{MIGRATED_KEY}';
                 INSERT INTO model_routing_settings VALUES
                   ('alice', '{{\"infer_addressing\":true,\"classifier_id\":\"pc_x\"}}', 0),
                   ('carol', '{{\"infer_addressing\":false}}', 0);"
            ))
            .unwrap();
        }
        for _ in 0..2 {
            let db = Db::open(&path).unwrap();
            for sid in [&a, &b] {
                let agents = db.space_agents(sid).unwrap();
                assert_eq!(agents.len(), 1);
                assert_eq!(agents[0].handle, "agent");
                assert_eq!(agents[0].name, "Metro");
                assert!(agents[0].is_default);
                assert_eq!(agents[0].tools, crate::space_agents::default_tools());
                assert!(
                    crate::pages::agent_tools::NAMES
                        .iter()
                        .all(|tool| !agents[0].allows(tool))
                );
            }
            let conn = db.lock();
            assert!(load_in(&conn, &a, "alice").unwrap().classify_unaddressed);
            assert!(!load_in(&conn, &a, "carol").unwrap().classify_unaddressed);
            assert!(!load_in(&conn, &b, "alice").unwrap().classify_unaddressed);
        }
        // A later choice is kept: the migration runs once.
        {
            let db = Db::open(&path).unwrap();
            store_in(&db.lock(), &a, "alice", &SpacePrefs::default()).unwrap();
        }
        let db = Db::open(&path).unwrap();
        assert!(
            !load_in(&db.lock(), &a, "alice")
                .unwrap()
                .classify_unaddressed
        );
    }
}
