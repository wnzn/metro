//! A page's buttons that do work on the server, run **as the person who
//! clicks them**:
//!
//! - On a **Space page**, `runAgent` posts the button's literal prompt as
//!   the clicker's own message to the button's named agent in one of the
//!   Space's top-level rooms (never a thread), and the answering model
//!   follows the clicker's own order (their model for that agent in that
//!   Space, their Space default, then their routing or default chat
//!   connection; nothing falls back). `runSchedule` runs one of the
//!   clicker's own scheduled tasks that targets this Space, now.
//! - A **personal page** is strictly page-only: its buttons can only start
//!   the clicker's private personal-agent run whose output appears on the
//!   page. Space rooms and personal Chat are never a personal page's
//!   destination, and personal pages can't run schedules.
//!
//! Every click is checked and claimed in **one transaction**
//! ([`store::claim_action`]): the page is live and visible, the version is
//! still the head (a superseded click is a 409 and writes nothing), the
//! destination is valid now, the clicker vouched for the button (or
//! confirms now, which is recorded per page version; a schedule
//! confirmation is bound to the task revision it showed), and the trigger
//! or schedule occurrence is written. Only then is the run dispatched.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use super::spec::action;
use super::store::{self, ActionError, ActionRequest, Claimed, Denied};
use crate::AppState;
use crate::api::{ApiError, ApiResult, db};
use crate::auth::Human;
use crate::db::{Activity, Author, actor, kind};
use crate::model_routing::{self as routing, AddressedBy};

fn reply(status: StatusCode, body: serde_json::Value) -> Response {
    (status, Json(body)).into_response()
}

pub struct Click {
    pub version: i64,
    pub block_key: String,
    pub confirm: bool,
    pub schedule_revision: Option<i64>,
}

/// Run the button `click.block_key` of page `page_id` for `who`.
///
/// One independent task owns the whole click: the claim, the dispatch and
/// the recording of how it went. The caller only awaits its result, so a
/// client that hangs up at any point (before the claim commits, or after
/// the work was submitted) can't leave a claim without its dispatch or a
/// run without its record.
pub async fn run(
    state: &AppState,
    who: &Human,
    page_id: &str,
    click: Click,
) -> ApiResult<Response> {
    let (st, person, pid) = (state.clone(), who.clone(), page_id.to_owned());
    match tokio::spawn(async move { claim_and_dispatch(&st, &person, &pid, click).await }).await {
        Ok(response) => response,
        Err(e) => Err(ApiError::internal(anyhow::anyhow!(
            "running a page action: {e}"
        ))),
    }
}

async fn claim_and_dispatch(
    state: &AppState,
    who: &Human,
    page_id: &str,
    click: Click,
) -> ApiResult<Response> {
    let turn = Activity::chat();
    let (pid, uid, author, max) = (
        page_id.to_owned(),
        who.id.clone(),
        who.author(),
        state.config.limits.max_message_chars,
    );
    let turn_id = turn.id.clone();
    let claimed = db(state, move |db| {
        let req = ActionRequest {
            page: &pid,
            user: &uid,
            author: &author,
            version: click.version,
            block_key: &click.block_key,
            confirm: click.confirm,
            schedule_revision: click.schedule_revision,
            chat_turn: &turn_id,
            max_prompt_chars: max,
        };
        store::claim_action(db, &req)
    })
    .await?;
    let claimed = match claimed {
        Ok(c) => c,
        Err(e) => return refused(e, max),
    };
    dispatch(state, who, page_id, claimed, turn).await
}

/// Record why a claimed run didn't start.
async fn failed(state: &AppState, page_run: i64, why: &str) {
    let why = why.to_owned();
    if let Err(e) = state
        .db
        .call(move |db| store::set_run(db, page_run, None, Some(&why)))
        .await
    {
        tracing::error!(error = %format!("{e:#}"), "recording a page run failure");
    }
}

const NO_SETTINGS: &str = "Your model settings couldn't be read; the run didn't start.";

/// Start the claimed work and record how it went.
async fn dispatch(
    state: &AppState,
    who: &Human,
    page_id: &str,
    claimed: Claimed,
    turn: Activity,
) -> ApiResult<Response> {
    match claimed {
        Claimed::Schedule { claim, task } => {
            let run = crate::schedules::dispatch_claimed_for(state, claim, task).await?;
            Ok(reply(
                StatusCode::OK,
                json!({"kind": action::RUN_SCHEDULE, "run": run}),
            ))
        }
        Claimed::Room {
            event,
            space_id,
            room,
            agent,
            page_run,
            page_title,
        } => {
            let settings = match routing::preferences(state, &who.id).await {
                Ok(s) => s,
                Err(e) => {
                    failed(state, page_run, NO_SETTINGS).await;
                    return Err(ApiError::internal(e));
                }
            };
            let prefs = match crate::space_prefs::load(state, &space_id, &who.id).await {
                Ok(p) => p,
                Err(e) => {
                    failed(state, page_run, NO_SETTINGS).await;
                    return Err(ApiError::internal(e));
                }
            };
            let route = crate::space_prefs::route_for(
                state,
                &who.id,
                &space_id,
                &prefs,
                &settings,
                &agent.id,
                &event.text,
                AddressedBy::Explicit,
            )
            .await;
            // Only Space pages reach here, so the title is the Space's own.
            let pick = format!(
                "Addressed: @{} (from the page \"{page_title}\")",
                agent.handle
            );
            let author = who.author();
            match crate::agent::start_room_run(
                state,
                &space_id,
                &room,
                &author,
                &event,
                route,
                &agent,
                Some(pick),
            )
            .await
            {
                Ok(info) => {
                    let rid = info.id.clone();
                    if let Err(e) = state
                        .db
                        .call(move |db| store::set_run(db, page_run, Some(&rid), None))
                        .await
                    {
                        tracing::error!(error = %format!("{e:#}"), "recording a page run");
                    }
                    Ok(reply(
                        StatusCode::ACCEPTED,
                        json!({"kind": action::RUN_AGENT, "started": true, "run_id": info.id,
                               "event_id": event.id, "space_id": space_id, "room_id": room.id}),
                    ))
                }
                Err(reason) => {
                    state
                        .append(
                            &space_id,
                            &room.id,
                            kind::ERROR,
                            &Author::server(actor::SYSTEM),
                            &reason,
                        )
                        .await;
                    failed(state, page_run, &reason).await;
                    Ok(reply(
                        StatusCode::ACCEPTED,
                        json!({"kind": action::RUN_AGENT, "started": false, "error": reason,
                               "event_id": event.id, "space_id": space_id, "room_id": room.id}),
                    ))
                }
            }
        }
        Claimed::Chat { event, page_run } => {
            let settings = match routing::preferences(state, &who.id).await {
                Ok(s) => s,
                Err(e) => {
                    failed(state, page_run, NO_SETTINGS).await;
                    return Err(ApiError::internal(e));
                }
            };
            let route = routing::route_turn(
                state,
                &who.id,
                None,
                &settings,
                &event.text,
                AddressedBy::Explicit,
                None,
            )
            .await;
            let started = crate::agent::start_page_chat_run(
                state,
                &who.id,
                page_id,
                &event,
                route,
                turn.clone(),
            )
            .await;
            if let Err(e) = &started {
                failed(state, page_run, e).await;
            }
            Ok(reply(
                StatusCode::ACCEPTED,
                match started {
                    Ok(()) => {
                        json!({"kind": action::RUN_AGENT, "started": true, "run_id": turn.id, "chat": true})
                    }
                    Err(e) => {
                        json!({"kind": action::RUN_AGENT, "started": false, "error": e, "chat": true})
                    }
                },
            ))
        }
    }
}

fn refused(e: ActionError, max: usize) -> ApiResult<Response> {
    let forbidden = |m: &str| Err(ApiError::new(StatusCode::FORBIDDEN, m));
    match e {
        ActionError::Denied(Denied::Gone) => Err(ApiError::not_found("no such page")),
        ActionError::Denied(Denied::Forbidden) => forbidden("you can't use this page"),
        ActionError::Conflict { head } => Ok(reply(
            StatusCode::CONFLICT,
            json!({
                "error": "the page changed since this button was shown; reload it and try again",
                "head_version": head,
            }),
        )),
        ActionError::NoButton => Err(ApiError::not_found("no such button on this page")),
        ActionError::Navigation => Err(ApiError::bad_request(
            "this button opens a room or page or refreshes the page; the app does that itself",
        )),
        ActionError::PersonalOnly => forbidden(
            "a personal page's buttons only start private runs shown on the page; Space rooms, \
             personal Chat and schedules aren't a personal page's destination",
        ),
        ActionError::Forbidden(m) => forbidden(m),
        ActionError::Gone(m) => Err(ApiError::not_found(m)),
        ActionError::Thread => Err(ApiError::bad_request(
            "a button can't send work to a thread",
        )),
        ActionError::NeedsConfirmation(c) => Ok(reply(
            StatusCode::PRECONDITION_REQUIRED,
            json!({
                "error": "confirmation required: you haven't written or confirmed this button on \
                          this page version; it runs as you, with your model",
                "confirmation": c,
            }),
        )),
        ActionError::ScheduleChanged { revision } => Ok(reply(
            StatusCode::CONFLICT,
            json!({
                "error": "the scheduled task changed since it was shown; review it and confirm again",
                "schedule_revision": revision,
            }),
        )),
        ActionError::Schedule(e) => Err(e.into()),
        ActionError::TooLong => Err(ApiError::bad_request(format!(
            "this button's prompt is longer than this server's {max}-character message limit"
        ))),
    }
}
