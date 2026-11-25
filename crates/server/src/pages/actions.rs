//! A page's buttons that do work on the server, run **as the person who
//! clicks them**:
//!
//! - `runAgent` posts the button's literal prompt as the clicker's own
//!   message to the button's named agent in its room (no classification;
//!   the normal addressing rules for a named agent), and the answering
//!   model follows the clicker's own order: their model for that agent in
//!   that Space, their Space default, then their routing or default chat
//!   connection. Nothing falls back. On a personal page with
//!   `room="chat"` the personal agent answers in the page's own
//!   conversation, which never appears in the person's Chat.
//! - `runSchedule` runs one of the clicker's own scheduled tasks now
//!   ([`crate::schedules::run_now_for`]); nobody else's.
//!
//! **Confirmation.** When the page version's last author isn't the
//! clicker, the first click on that version answers 428 with what would
//! run; the client shows it and repeats the request with `confirm: true`,
//! which is recorded per person, page and version. A new version asks
//! again. Navigation and refresh buttons run in the app only.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use super::spec::{PageSpec, action};
use super::store::{self, PageRow, VersionRow};
use crate::AppState;
use crate::api::{ApiError, ApiResult, db};
use crate::auth::Human;
use crate::db::{Activity, Addressed, Author, actor, kind};
use crate::model_routing::{self as routing, AddressedBy};

fn reply(status: StatusCode, body: serde_json::Value) -> Response {
    (status, Json(body)).into_response()
}

/// Run `block_key` of `page`'s head version for `who`.
pub async fn run(
    state: &AppState,
    who: &Human,
    page: &PageRow,
    head: &VersionRow,
    spec: &PageSpec,
    block_key: &str,
    confirm: bool,
) -> ApiResult<Response> {
    let Some(block) = spec.block(block_key) else {
        return Err(ApiError::not_found("no such button on this page"));
    };
    let Some(a) = block.action.clone() else {
        return Err(ApiError::not_found("no such button on this page"));
    };
    if a.kind != action::RUN_AGENT && a.kind != action::RUN_SCHEDULE {
        return Err(ApiError::bad_request(
            "this button opens a room or page or refreshes the page; the app does that itself",
        ));
    }
    // Checks before the confirmation, so the sheet never offers something
    // that can't run.
    let target_space = match (&page.space_id, &a.target.space_id) {
        (Some(own), None) if !a.target.chat => Some(own.clone()),
        (None, other) => other.clone(),
        // A Space page never acts outside its Space.
        _ => {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "this button can't run",
            ));
        }
    };
    if a.kind == action::RUN_AGENT
        && let Some(sid) = &target_space
    {
        crate::api::member_of(state, sid, who).await?;
    }
    let schedule = if a.kind == action::RUN_SCHEDULE {
        let (uid, id) = (who.id.clone(), a.schedule_id.clone().unwrap_or_default());
        let task = db(state, move |db| {
            Ok(crate::schedules::list_tasks(db, &uid)?
                .into_iter()
                .find(|t| t.id == id))
        })
        .await?;
        let Some(task) = task else {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "only the schedule's owner can run it",
            ));
        };
        if let Some(sid) = &page.space_id
            && task.target.as_ref().is_none_or(|(s, _)| s != sid)
        {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "this schedule doesn't run in this page's Space",
            ));
        }
        Some(task)
    } else {
        None
    };

    if head.author_id != who.id {
        let (uid, pid, v) = (who.id.clone(), page.id.clone(), head.version);
        let done = db(state, move |db| store::confirmed(db, &uid, &pid, v)).await?;
        if !done {
            if !confirm {
                let what = match &schedule {
                    Some(task) => json!({
                        "kind": action::RUN_SCHEDULE,
                        "schedule_id": task.id,
                        "schedule": task.name,
                        "prompt": task.prompt,
                    }),
                    None => json!({
                        "kind": action::RUN_AGENT,
                        "prompt": a.prompt,
                        "agent": a.target.agent,
                        "agent_id": a.target.agent_id,
                        "room": a.target.room,
                        "room_id": a.target.room_id,
                        "space_id": target_space,
                        "chat": a.target.chat,
                    }),
                };
                return Ok(reply(
                    StatusCode::PRECONDITION_REQUIRED,
                    json!({
                        "error": "confirmation required: this page version was last edited by \
                                  someone else; this runs as you, with your model",
                        "confirmation": {
                            "page_id": page.id,
                            "version": head.version,
                            "block_key": block_key,
                            "edited_by": head.author_id,
                            "edited_at": crate::db::rfc3339_utc(head.created_at),
                            "runs_as": who.id,
                            "action": what,
                        }
                    }),
                ));
            }
            let (uid, pid, v) = (who.id.clone(), page.id.clone(), head.version);
            db(state, move |db| store::confirm(db, &uid, &pid, v)).await?;
        }
    }

    if let Some(task) = schedule {
        let run = crate::schedules::run_now_for(state, &who.id, &task.id).await?;
        return Ok(reply(
            StatusCode::OK,
            json!({"kind": action::RUN_SCHEDULE, "run": run}),
        ));
    }
    let prompt = a.prompt.clone().unwrap_or_default();
    match target_space {
        Some(sid) => run_in_room(state, who, page, head, block_key, &sid, &a, &prompt).await,
        None => run_in_page_chat(state, who, page, head, block_key, &prompt).await,
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_in_room(
    state: &AppState,
    who: &Human,
    page: &PageRow,
    head: &VersionRow,
    block_key: &str,
    sid: &str,
    a: &super::spec::Action,
    prompt: &str,
) -> ApiResult<Response> {
    let room_id = a.target.room_id.clone().unwrap_or_default();
    let agent_id = a.target.agent_id.clone().unwrap_or_default();
    let (s, r, ag) = (sid.to_owned(), room_id.clone(), agent_id.clone());
    let (room, agent) = db(state, move |db| {
        Ok((db.get_room(&s, &r)?, db.space_agent(&s, &ag)?))
    })
    .await?;
    let room = room.ok_or_else(|| ApiError::not_found("no such room"))?;
    let agent = agent.ok_or_else(|| ApiError::not_found("no such agent"))?;
    let addressed = Addressed {
        mentions: json!([{"kind": "agent", "agent_id": agent.id}]),
        audience: "agent".to_owned(),
    };
    let author = who.author();
    let (s, r, au, text) = (
        sid.to_owned(),
        room.id.clone(),
        author.clone(),
        prompt.to_owned(),
    );
    // Membership and the room are rechecked in the write transaction.
    let event = db(state, move |db| {
        db.append_event_as_member(&s, &r, kind::MESSAGE, &au, &text, &addressed)
    })
    .await?
    .ok_or_else(|| ApiError::not_found("no such room"))?;
    let settings = routing::preferences(state, &who.id)
        .await
        .map_err(ApiError::internal)?;
    let prefs = crate::space_prefs::load(state, sid, &who.id)
        .await
        .map_err(ApiError::internal)?;
    let route = crate::space_prefs::route_for(
        state,
        &who.id,
        sid,
        &prefs,
        &settings,
        &agent.id,
        prompt,
        AddressedBy::Explicit,
    )
    .await;
    let pick = format!(
        "Addressed: @{} (from the page \"{}\")",
        agent.handle, page.title
    );
    match crate::agent::start_room_run(
        state,
        sid,
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
            let (pid, v, k, uid, s, r, eid, rid) = (
                page.id.clone(),
                head.version,
                block_key.to_owned(),
                who.id.clone(),
                sid.to_owned(),
                room.id.clone(),
                event.id,
                info.id.clone(),
            );
            db(state, move |db| {
                store::record_run(db, &pid, v, &k, &uid, Some((&s, &r)), eid, &rid)
            })
            .await?;
            Ok(reply(
                StatusCode::ACCEPTED,
                json!({"kind": action::RUN_AGENT, "started": true, "run_id": info.id,
                       "event_id": event.id, "space_id": sid, "room_id": room.id}),
            ))
        }
        Err(reason) => {
            state
                .append(
                    sid,
                    &room.id,
                    kind::ERROR,
                    &Author::server(actor::SYSTEM),
                    &reason,
                )
                .await;
            Ok(reply(
                StatusCode::ACCEPTED,
                json!({"kind": action::RUN_AGENT, "started": false, "error": reason,
                       "event_id": event.id, "space_id": sid, "room_id": room.id}),
            ))
        }
    }
}

async fn run_in_page_chat(
    state: &AppState,
    who: &Human,
    page: &PageRow,
    head: &VersionRow,
    block_key: &str,
    prompt: &str,
) -> ApiResult<Response> {
    // Only the owner ever reaches a personal page; checked again here.
    if page.owner_id.as_deref() != Some(who.id.as_str()) {
        return Err(ApiError::not_found("no such page"));
    }
    let (pid, uid, au, text) = (
        page.id.clone(),
        who.id.clone(),
        who.author(),
        prompt.to_owned(),
    );
    let trigger = db(state, move |db| {
        store::append_page_event(db, &pid, &uid, kind::MESSAGE, &au, &text, None)
    })
    .await?;
    let settings = routing::preferences(state, &who.id)
        .await
        .map_err(ApiError::internal)?;
    let route = routing::route_turn(
        state,
        &who.id,
        None,
        &settings,
        prompt,
        AddressedBy::Explicit,
        None,
    )
    .await;
    let turn = Activity::chat();
    let (pid, v, k, uid, eid, rid) = (
        page.id.clone(),
        head.version,
        block_key.to_owned(),
        who.id.clone(),
        trigger.id,
        turn.id.clone(),
    );
    db(state, move |db| {
        store::record_run(db, &pid, v, &k, &uid, None, eid, &rid)
    })
    .await?;
    let started =
        crate::agent::start_page_chat_run(state, &who.id, &page.id, &trigger, route, turn.clone())
            .await;
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
