//! `POST /v1/runs/{id}/retry`: start a failed or cancelled run again, as a
//! new run answering the same message.
//!
//! Only the run's initiator may retry it. The trigger must still exist and
//! be their own `message`, they must still be a member, and the agent must
//! still be in the Space. The new run takes a fresh route (their model
//! choices as they are now, never the old run's pinned connection), with
//! no fallback. Runs live in memory, so after a restart there is nothing to
//! retry. Retrying twice starts two runs.
//!
//! Errors: 404 for an unknown run or a non-member, 403 for anyone but the
//! initiator, 409 for a run that isn't failed or cancelled, whose message
//! or agent is gone, or that couldn't start.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use rusqlite::{OptionalExtension, params};

use crate::AppState;
use crate::agent_dispatch::Target;
use crate::api::{ApiError, ApiResult, empty_object};
use crate::auth::Human;
use crate::db::{Db, kind, row_event};
use crate::model_routing::AddressedBy;
use crate::orchestration::{RunInfo, RunStatus};

#[derive(Debug, PartialEq, Eq)]
pub enum RetryError {
    NotFound,
    Forbidden,
    Conflict(String),
    Internal,
}

impl RetryError {
    pub fn text(&self) -> String {
        match self {
            RetryError::NotFound => "no such run".to_owned(),
            RetryError::Forbidden => "only the person who started a run can retry it".to_owned(),
            RetryError::Conflict(m) => m.clone(),
            RetryError::Internal => "the run couldn't be retried".to_owned(),
        }
    }
}

impl From<RetryError> for ApiError {
    fn from(e: RetryError) -> Self {
        let status = match e {
            RetryError::NotFound => StatusCode::NOT_FOUND,
            RetryError::Forbidden => StatusCode::FORBIDDEN,
            RetryError::Conflict(_) => StatusCode::CONFLICT,
            RetryError::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        };
        ApiError::new(status, e.text())
    }
}

fn internal(e: anyhow::Error) -> RetryError {
    tracing::error!(error = %format!("{e:#}"), "retrying a run");
    RetryError::Internal
}

/// Retry `run_id` as `who`. Returns the new run.
pub async fn retry(state: &AppState, who: &Human, run_id: &str) -> Result<RunInfo, RetryError> {
    let run = state.runs.get(run_id).ok_or(RetryError::NotFound)?;
    let (sid, uid) = (run.space_id.clone(), who.id.clone());
    let member = state
        .db
        .call(move |db| db.membership(&sid, &uid))
        .await
        .map_err(internal)?;
    if member.is_none() {
        return Err(RetryError::NotFound);
    }
    if run.actor != who.id {
        return Err(RetryError::Forbidden);
    }
    if !matches!(run.status, RunStatus::Failed | RunStatus::Cancelled) {
        return Err(RetryError::Conflict(
            "only a failed or cancelled run can be retried".to_owned(),
        ));
    }
    let (Some(agent_id), Some(trigger_id)) = (run.agent_id.clone(), run.trigger_event_id) else {
        return Err(RetryError::Conflict(
            "this run can't be retried; ask again instead".to_owned(),
        ));
    };
    let (sid, rid, uid) = (run.space_id.clone(), run.room_id.clone(), who.id.clone());
    let found = state
        .db
        .call(move |db| {
            let conn = db.lock();
            let trigger = conn
                .query_row(
                    "SELECT id, kind, actor, actor_id, text, mentions, audience, activity, created_at
                     FROM space_events WHERE space_id = ?1 AND room_id = ?2 AND id = ?3",
                    params![sid, rid, trigger_id],
                    row_event,
                )
                .optional()?
                .filter(|e| e.kind == kind::MESSAGE && e.actor_id.as_deref() == Some(uid.as_str()));
            let room = Db::room_in(&conn, &sid, &rid)?;
            let agent = crate::space_agents::get_in(&conn, &sid, &agent_id)?;
            Ok((trigger, room, agent))
        })
        .await
        .map_err(internal)?;
    let (Some(trigger), Some(room)) = (found.0, found.1) else {
        return Err(RetryError::Conflict(
            "the message this run answered no longer exists".to_owned(),
        ));
    };
    let Some(agent) = found.2 else {
        return Err(RetryError::Conflict(
            "the run's agent is no longer in this Space".to_owned(),
        ));
    };
    let settings = crate::model_routing::preferences(state, &who.id)
        .await
        .map_err(internal)?;
    let prefs = crate::space_prefs::load(state, &run.space_id, &who.id)
        .await
        .map_err(internal)?;
    let target = Target {
        agent,
        pick: format!("Retry of {run_id}"),
        addressed_by: AddressedBy::Explicit,
    };
    let started = crate::agent_dispatch::start_runs(
        state,
        &run.space_id,
        &room,
        &who.author(),
        &trigger,
        vec![target],
        None,
        &settings,
        &prefs,
    )
    .await;
    match started.runs.into_iter().next() {
        Some(new) => Ok(new),
        None => Err(RetryError::Conflict(
            started
                .failures
                .into_iter()
                .next()
                .unwrap_or_else(|| "the run couldn't start".to_owned()),
        )),
    }
}

pub async fn retry_run(
    State(state): State<AppState>,
    who: Human,
    Path(id): Path<String>,
    body: axum::body::Bytes,
) -> ApiResult<(StatusCode, Json<RunInfo>)> {
    empty_object(&body)?;
    let run = retry(&state, &who, &id).await?;
    Ok((StatusCode::CREATED, Json(run)))
}
