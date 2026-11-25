//! Editing, deleting and leaving a Space.
//!
//! - `PATCH /v1/spaces/{sid}` (owner): `name`, `description`.
//! - `DELETE /v1/spaces/{sid}` (owner, permanent): the body must repeat the
//!   Space's name as `confirm_name`. Everything of the Space goes in one
//!   transaction: rooms, threads, events, reactions, attachments links,
//!   library files and everyone's index rows of them, search vectors,
//!   schedules, connection share grants to it, per-Space preferences,
//!   agents, pages (with their versions, drafts and preferences),
//!   monitoring entries; its registered runner is detached and its
//!   dedicated runner token revoked (the runner generation is bumped, so
//!   queued and claimed jobs fail). Its runs are stopped.
//! - `POST /v1/spaces/{sid}/leave` (members other than the owner): like
//!   being removed, including the purge of their own library index rows
//!   and preferences for the Space.
//!
//! Outsiders and unknown ids get 404, non-owners 403.

use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{patch, post};
use rusqlite::params;
use serde::Deserialize;
use serde_json::json;

use crate::AppState;
use crate::api::{
    ApiError, ApiResult, JsonBody, check_name, db, empty_object, member_of, owner_of,
};
use crate::auth::Human;
use crate::db::{Db, RemoveMember, Role, SpaceView};

/// Longest Space description, in characters.
pub const MAX_DESCRIPTION: usize = 500;

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/spaces/{sid}",
            patch(patch_space).delete(delete_space).get(get_space),
        )
        .route("/v1/spaces/{sid}/leave", post(leave_space))
}

async fn get_space(
    State(state): State<AppState>,
    who: Human,
    Path(sid): Path<String>,
) -> ApiResult<Json<SpaceView>> {
    member_of(&state, &sid, &who).await.map(Json)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatchBody {
    name: Option<String>,
    description: Option<String>,
}

fn check_description(text: &str) -> ApiResult<String> {
    let text = text.replace("\r\n", "\n");
    let text = text.trim();
    if text.chars().count() > MAX_DESCRIPTION
        || text
            .chars()
            .any(|c| (c.is_control() && c != '\n') || crate::admin_users::deceptive(c))
    {
        return Err(ApiError::bad_request(format!(
            "description must be at most {MAX_DESCRIPTION} printable characters"
        )));
    }
    Ok(text.to_owned())
}

async fn patch_space(
    State(state): State<AppState>,
    who: Human,
    Path(sid): Path<String>,
    body: JsonBody<PatchBody>,
) -> ApiResult<Json<SpaceView>> {
    let space = owner_of(&state, &sid, &who).await?;
    let Json(body) = body?;
    let name = body
        .name
        .as_deref()
        .map(|n| check_name(&state, n))
        .transpose()?;
    let description = body
        .description
        .as_deref()
        .map(check_description)
        .transpose()?;
    let (sid, uid) = (space.id.clone(), who.id.clone());
    db(&state, move |db| {
        {
            let conn = db.lock();
            conn.execute(
                "UPDATE spaces SET name = COALESCE(?2, name), description = COALESCE(?3, description)
                 WHERE id = ?1",
                params![sid, name, description],
            )?;
        }
        db.membership(&sid, &uid)
    })
    .await?
    .map(Json)
    .ok_or_else(|| ApiError::not_found("no such Space"))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeleteBody {
    confirm_name: String,
}

pub enum Deleted {
    Done { generation: u64 },
    NameMismatch,
    Gone,
}

impl Db {
    /// Delete Space `space_id` and everything in it, if it is still named
    /// `confirm_name` and owned by `owner_id`.
    pub fn delete_space(
        &self,
        space_id: &str,
        owner_id: &str,
        confirm_name: &str,
    ) -> anyhow::Result<Deleted> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let name: Option<String> = rusqlite::OptionalExtension::optional(tx.query_row(
            "SELECT name FROM spaces WHERE id = ?1 AND owner_id = ?2",
            params![space_id, owner_id],
            |r| r.get(0),
        ))?;
        let Some(name) = name else {
            return Ok(Deleted::Gone);
        };
        if name != confirm_name {
            return Ok(Deleted::NameMismatch);
        }
        let generation: i64 = tx.query_row(
            "UPDATE spaces SET runner_token_hash = NULL, runner_generation = runner_generation + 1
             WHERE id = ?1 RETURNING runner_generation",
            [space_id],
            |r| r.get(0),
        )?;
        let sid = space_id;
        tx.execute(
            "DELETE FROM message_attachments WHERE scope = 'space'
             AND event_id IN (SELECT id FROM space_events WHERE space_id = ?1)",
            [sid],
        )?;
        tx.execute("DELETE FROM space_reactions WHERE space_id = ?1", [sid])?;
        let source = format!("space:{sid}");
        tx.execute("DELETE FROM search_vectors WHERE source = ?1", [&source])?;
        tx.execute("DELETE FROM search_sources WHERE source = ?1", [&source])?;
        crate::library_search::hooks::space_deleted(&tx, sid)?;
        tx.execute("DELETE FROM library_assets WHERE space_id = ?1", [sid])?;
        tx.execute("DELETE FROM schedules WHERE space_id = ?1", [sid])?;
        tx.execute(
            "DELETE FROM provider_share_spaces WHERE space_id = ?1",
            [sid],
        )?;
        tx.execute("DELETE FROM space_member_prefs WHERE space_id = ?1", [sid])?;
        crate::pages::hooks::space_deleted(&tx, sid)?;
        tx.execute("DELETE FROM space_agents WHERE space_id = ?1", [sid])?;
        tx.execute("DELETE FROM space_runners WHERE space_id = ?1", [sid])?;
        tx.execute(
            "DELETE FROM monitoring_log WHERE scope_kind = 'space' AND scope_id = ?1",
            [sid],
        )?;
        tx.execute("DELETE FROM space_events WHERE space_id = ?1", [sid])?;
        tx.execute(
            "DELETE FROM space_rooms WHERE space_id = ?1 AND parent_room_id IS NOT NULL",
            [sid],
        )?;
        tx.execute("DELETE FROM space_rooms WHERE space_id = ?1", [sid])?;
        tx.execute("DELETE FROM space_members WHERE space_id = ?1", [sid])?;
        tx.execute("DELETE FROM spaces WHERE id = ?1", [sid])?;
        tx.commit()?;
        Ok(Deleted::Done {
            generation: generation as u64,
        })
    }
}

async fn delete_space(
    State(state): State<AppState>,
    who: Human,
    Path(sid): Path<String>,
    body: JsonBody<DeleteBody>,
) -> ApiResult<StatusCode> {
    let space = owner_of(&state, &sid, &who).await?;
    let Json(body) = body?;
    if body.confirm_name != space.name {
        return Err(ApiError::bad_request(
            "confirm_name must be exactly the Space's name",
        ));
    }
    let (s, o, c) = (space.id.clone(), who.id.clone(), body.confirm_name);
    match db(&state, move |db| db.delete_space(&s, &o, &c)).await? {
        Deleted::Done { generation } => {
            state.jobs.revoke(&space.id, generation);
            let stopped = state
                .runs
                .list()
                .iter()
                .filter(|r| r.space_id == space.id)
                .filter(|r| state.runs.stop(&r.id).is_some())
                .count();
            if let Err(e) = state.device.shares_changed(&state.db, None).await {
                tracing::warn!(error = %format!("{e:#}"), "rechecking borrowed device jobs");
            }
            tracing::info!(space = %space.id, stopped, "Space deleted");
            Ok(StatusCode::NO_CONTENT)
        }
        Deleted::NameMismatch => Err(ApiError::bad_request(
            "confirm_name must be exactly the Space's name",
        )),
        Deleted::Gone => Err(ApiError::not_found("no such Space")),
    }
}

async fn leave_space(
    State(state): State<AppState>,
    who: Human,
    Path(sid): Path<String>,
    body: axum::body::Bytes,
) -> ApiResult<Json<serde_json::Value>> {
    empty_object(&body)?;
    let space = member_of(&state, &sid, &who).await?;
    if space.role == Role::Owner {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "the owner can't leave their Space; delete it instead",
        ));
    }
    let (s, u) = (space.id.clone(), who.id.clone());
    match db(&state, move |db| db.remove_member(&s, &u)).await? {
        RemoveMember::Removed => {
            let stopped = crate::agent::stop_runs_of(&state, &space.id, &who.id);
            if let Err(e) = state.device.shares_changed(&state.db, None).await {
                tracing::warn!(error = %format!("{e:#}"), "rechecking borrowed device jobs");
            }
            Ok(Json(json!({"left": true, "stopped_runs": stopped})))
        }
        RemoveMember::NotMember => Err(ApiError::not_found("no such Space")),
        RemoveMember::IsOwner => Err(ApiError::new(
            StatusCode::CONFLICT,
            "the owner can't leave their Space; delete it instead",
        )),
    }
}
