//! Deleting a message.
//!
//! - `DELETE /v1/spaces/{sid}/rooms/{rid}/events/{eid}`: a member deletes
//!   their own message; the Space owner deletes any message or agent reply
//!   of their Space.
//! - `DELETE /v1/chat/events/{eid}`: the owner of a personal Chat deletes
//!   any message or Metro reply in it.
//! - `DELETE /v1/dms/{id}/events/{eid}` ([`crate::dms`]): a participant
//!   deletes their own DM message.
//!
//! There is no admin bypass. A Space, room, Chat event or DM the caller
//! can't read, and an unknown id, is 404; a member who may not delete it
//! gets 403. Only `message` and `agent` events can be deleted (409 for
//! other kinds).
//!
//! One transaction removes the message from the transcript with its
//! reactions (and their `reaction` events), attachment links, everyone's
//! search vectors and any conversation summary that covers it, so no later
//! run sends it to a model. A message that anchors a reply thread (even an
//! empty one) keeps a [`kind::DELETED`] row with no text, recipients or
//! metadata, so the thread keeps its place; otherwise the row is gone. A
//! pending System One decision for it is dropped ([`crate::decisions`]). A [`kind::MESSAGE_DELETED`] event in the
//! same stream tells every reader, like a `reaction` event. Runs already
//! started go on; deleting never starts agent work and never calls a model.

use axum::extract::{Path, State};
use axum::routing::delete;
use axum::{Json, Router};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::AppState;
use crate::api::{ApiError, ApiResult, db};
use crate::auth::Human;
use crate::db::{Addressed, Author, Db, Event, kind, new_event, now_ms};

/// The JSON `text` of a [`kind::MESSAGE_DELETED`] event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeletionPayload {
    pub target_event_id: i64,
    /// The target stays as a [`kind::DELETED`] row (it anchors a thread).
    pub tombstone: bool,
}

/// Why nothing was deleted.
#[derive(Debug, PartialEq, Eq)]
pub enum Refused {
    /// Not readable by the caller, or no such message.
    NotFound,
    /// Readable, but the caller may not delete it.
    Forbidden,
    /// Not a message or agent reply.
    Kind,
}

impl From<Refused> for ApiError {
    fn from(r: Refused) -> Self {
        match r {
            Refused::NotFound => ApiError::not_found("no such message"),
            Refused::Forbidden => ApiError::new(
                axum::http::StatusCode::FORBIDDEN,
                "you can delete only your own messages here",
            ),
            Refused::Kind => ApiError::new(
                axum::http::StatusCode::CONFLICT,
                "only messages and agent replies can be deleted",
            ),
        }
    }
}

fn deletable(kind: &str) -> bool {
    kind == kind::MESSAGE || kind == kind::AGENT
}

fn marker(who: &Human, id: i64, payload: &DeletionPayload, at: i64) -> anyhow::Result<Event> {
    let author = Author {
        id: Some(who.id.clone()),
        name: who.name.clone(),
    };
    Ok(new_event(
        id,
        kind::MESSAGE_DELETED,
        &author,
        &serde_json::to_string(payload)?,
        &Addressed::room(),
        None,
        at,
    ))
}

/// `CASE` guard: `json_extract` fails on text that isn't JSON.
const REACTION_TARGET: &str =
    "CASE WHEN json_valid(text) THEN json_extract(text, '$.target_event_id') END";

/// A summary whose range includes the event bound as parameter `?{n}`.
fn covers(n: usize) -> String {
    format!(
        "CASE WHEN json_valid(activity) THEN json_extract(activity, '$.compaction.from_event_id') END <= ?{n}
         AND CASE WHEN json_valid(activity) THEN json_extract(activity, '$.compaction.through_event_id') END >= ?{n}"
    )
}

/// Delete event `eid` of room `rid` in Space `sid` as `who`; returns the
/// `message_deleted` events written (the target's first, then those of
/// summaries that covered it, which are removed too).
pub fn delete_in_space(
    db: &Db,
    sid: &str,
    rid: &str,
    eid: i64,
    who: &Human,
) -> anyhow::Result<Result<Vec<Event>, Refused>> {
    let mut conn = db.lock();
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let role: Option<String> = tx
        .query_row(
            "SELECT role FROM space_members WHERE space_id = ?1 AND user_id = ?2",
            params![sid, who.id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(role) = role else {
        return Ok(Err(Refused::NotFound));
    };
    if Db::room_in(&tx, sid, rid)?.is_none() {
        return Ok(Err(Refused::NotFound));
    }
    let target: Option<(String, Option<String>)> = tx
        .query_row(
            "SELECT kind, actor_id FROM space_events WHERE id = ?1 AND space_id = ?2 AND room_id = ?3",
            params![eid, sid, rid],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((target_kind, author)) = target else {
        return Ok(Err(Refused::NotFound));
    };
    if target_kind == kind::DELETED {
        return Ok(Err(Refused::NotFound));
    }
    if !deletable(&target_kind) {
        return Ok(Err(Refused::Kind));
    }
    let own = target_kind == kind::MESSAGE && author.as_deref() == Some(who.id.as_str());
    if !own && role != "owner" {
        return Ok(Err(Refused::Forbidden));
    }
    let thread: Option<String> = tx
        .query_row(
            "SELECT id FROM space_rooms WHERE space_id = ?1 AND parent_room_id = ?2 AND anchor_event_id = ?3",
            params![sid, rid, eid],
            |r| r.get(0),
        )
        .optional()?;
    // The thread stays, even empty, under a tombstone of its anchor.
    let tombstone = thread.is_some();
    tx.execute(
        "DELETE FROM space_reactions WHERE space_id = ?1 AND room_id = ?2 AND target_event_id = ?3",
        params![sid, rid, eid],
    )?;
    tx.execute(
        &format!(
            "DELETE FROM space_events WHERE space_id = ?1 AND room_id = ?2 AND kind = 'reaction'
               AND {REACTION_TARGET} = ?3"
        ),
        params![sid, rid, eid],
    )?;
    tx.execute(
        "DELETE FROM message_attachments WHERE scope = 'space' AND event_id = ?1",
        [eid],
    )?;
    tx.execute(
        "DELETE FROM search_vectors WHERE source = ?1 AND event_id = ?2",
        params![format!("space:{sid}"), eid],
    )?;
    if tombstone {
        tx.execute(
            "UPDATE space_events SET kind = ?2, text = '', mentions = '[]', audience = 'room',
                 activity = NULL
             WHERE id = ?1",
            params![eid, kind::DELETED],
        )?;
    } else {
        tx.execute("DELETE FROM space_events WHERE id = ?1", [eid])?;
    }
    crate::decisions::forget_in(&tx, crate::decisions::Scope::Space, eid)?;
    // Summaries that cover it: of its own room, and of the reply thread
    // anchored on it (whose history begins with it).
    let covering: Vec<(i64, String)> = {
        let mut stmt = tx.prepare(&format!(
            "SELECT id, room_id FROM space_events
             WHERE space_id = ?1 AND kind = 'compaction'
               AND (room_id = ?2 OR room_id = ?4)
               AND {}",
            covers(3)
        ))?;
        stmt.query_map(params![sid, rid, eid, thread], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?
        .collect::<Result<_, _>>()?
    };
    let at = now_ms();
    let mut written = Vec::with_capacity(1 + covering.len());
    let mut note = |tx: &Connection, room: &str, target: i64, tombstone: bool| {
        let payload = DeletionPayload {
            target_event_id: target,
            tombstone,
        };
        tx.execute(
            "INSERT INTO space_events
                 (space_id, room_id, kind, actor, actor_id, text, mentions, audience, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, '[]', 'room', ?7)",
            params![
                sid,
                room,
                kind::MESSAGE_DELETED,
                who.name,
                who.id,
                serde_json::to_string(&payload)?,
                at
            ],
        )?;
        written.push(marker(who, tx.last_insert_rowid(), &payload, at)?);
        anyhow::Ok(())
    };
    note(&tx, rid, eid, tombstone)?;
    for (id, room) in covering {
        tx.execute("DELETE FROM space_events WHERE id = ?1", [id])?;
        note(&tx, &room, id, false)?;
    }
    tx.commit()?;
    Ok(Ok(written))
}

/// Delete event `eid` of `who`'s personal Chat; as [`delete_in_space`].
pub fn delete_in_chat(
    db: &Db,
    eid: i64,
    who: &Human,
) -> anyhow::Result<Result<Vec<Event>, Refused>> {
    let mut conn = db.lock();
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let target: Option<String> = tx
        .query_row(
            "SELECT kind FROM personal_events WHERE id = ?1 AND user_id = ?2",
            params![eid, who.id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(target_kind) = target else {
        return Ok(Err(Refused::NotFound));
    };
    if !deletable(&target_kind) {
        return Ok(Err(Refused::Kind));
    }
    tx.execute(
        "DELETE FROM personal_reactions WHERE user_id = ?1 AND target_event_id = ?2",
        params![who.id, eid],
    )?;
    tx.execute(
        &format!(
            "DELETE FROM personal_events WHERE user_id = ?1 AND kind = 'reaction'
               AND {REACTION_TARGET} = ?2"
        ),
        params![who.id, eid],
    )?;
    tx.execute(
        "DELETE FROM message_attachments WHERE scope = 'personal' AND event_id = ?1",
        [eid],
    )?;
    tx.execute(
        "DELETE FROM search_vectors WHERE user_id = ?1 AND source = 'chat' AND event_id = ?2",
        params![who.id, eid],
    )?;
    tx.execute("DELETE FROM personal_events WHERE id = ?1", [eid])?;
    crate::decisions::forget_in(&tx, crate::decisions::Scope::Personal, eid)?;
    let covering: Vec<i64> = {
        let mut stmt = tx.prepare(&format!(
            "SELECT id FROM personal_events WHERE user_id = ?1 AND kind = 'compaction' AND {}",
            covers(2)
        ))?;
        stmt.query_map(params![who.id, eid], |r| r.get(0))?
            .collect::<Result<_, _>>()?
    };
    let at = now_ms();
    let mut written = Vec::with_capacity(1 + covering.len());
    for (target, gone) in
        std::iter::once((eid, false)).chain(covering.into_iter().map(|c| (c, true)))
    {
        if gone {
            tx.execute("DELETE FROM personal_events WHERE id = ?1", [target])?;
        }
        let payload = DeletionPayload {
            target_event_id: target,
            tombstone: false,
        };
        tx.execute(
            "INSERT INTO personal_events
                 (user_id, kind, actor, actor_id, text, mentions, audience, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, '[]', 'room', ?6)",
            params![
                who.id,
                kind::MESSAGE_DELETED,
                who.name,
                who.id,
                serde_json::to_string(&payload)?,
                at
            ],
        )?;
        written.push(marker(who, tx.last_insert_rowid(), &payload, at)?);
    }
    tx.commit()?;
    Ok(Ok(written))
}

fn answer(events: Vec<Event>) -> Json<Value> {
    Json(json!({"deleted": true, "events": events}))
}

async fn delete_space(
    State(state): State<AppState>,
    who: Human,
    Path((sid, rid, eid)): Path<(String, String, i64)>,
) -> ApiResult<Json<Value>> {
    // 404 for a Space or room the caller can't see, before anything else.
    let space = crate::api::member_of(&state, &sid, &who).await?;
    let room = crate::api::room_of(&state, &space, &rid).await?;
    let events = db(&state, move |db| {
        delete_in_space(db, &space.id, &room.id, eid, &who)
    })
    .await??;
    Ok(answer(events))
}

async fn delete_chat(
    State(state): State<AppState>,
    who: Human,
    Path(eid): Path<i64>,
) -> ApiResult<Json<Value>> {
    let events = db(&state, move |db| delete_in_chat(db, eid, &who)).await??;
    Ok(answer(events))
}

pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/spaces/{sid}/rooms/{rid}/events/{eid}",
            delete(delete_space),
        )
        .route("/v1/chat/events/{eid}", delete(delete_chat))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn human(id: &str) -> Human {
        Human {
            id: id.into(),
            name: id.into(),
        }
    }

    fn count(db: &Db, sql: &str, eid: i64) -> i64 {
        db.lock().query_row(sql, [eid], |r| r.get(0)).unwrap()
    }

    #[test]
    fn deleting_removes_links_vectors_and_covering_summaries() {
        let db = Db::open_in_memory().unwrap();
        let space = db.create_space("alice", "S", 10).unwrap();
        let author = Author {
            id: Some("alice".into()),
            name: "alice".into(),
        };
        let keep = db
            .append_event(
                &space.id,
                "lobby",
                kind::MESSAGE,
                &author,
                "keep",
                &Addressed::room(),
            )
            .unwrap();
        let gone = db
            .append_event(
                &space.id,
                "lobby",
                kind::MESSAGE,
                &author,
                "gone",
                &Addressed::room(),
            )
            .unwrap();
        {
            let conn = db.lock();
            conn.execute(
                "INSERT INTO message_attachments (scope, event_id, position, asset_id)
                 VALUES ('space', ?1, 0, NULL)",
                [gone.id],
            )
            .unwrap();
            for (user, event) in [("alice", gone.id), ("bob", gone.id), ("alice", keep.id)] {
                conn.execute(
                    "INSERT INTO search_vectors (user_id, source, event_id, fingerprint, room_id,
                         text_hash, vector)
                     VALUES (?1, ?2, ?3, 'fp', 'lobby', x'00', x'00')",
                    params![user, format!("space:{}", space.id), event],
                )
                .unwrap();
            }
            // Covering in its room, not covering, and covering ids but in
            // another thread of the room: only the first goes.
            conn.execute(
                "INSERT INTO space_rooms (space_id, id, name, parent_room_id, created_at)
                 VALUES (?1, 'other-thread', 'Other', 'lobby', 1)",
                [&space.id],
            )
            .unwrap();
            for (room, from, through) in [
                ("lobby", keep.id, gone.id),
                ("lobby", keep.id, keep.id),
                ("other-thread", keep.id, gone.id),
            ] {
                conn.execute(
                    "INSERT INTO space_events (space_id, room_id, kind, actor, text, created_at, activity)
                     VALUES (?1, ?3, 'compaction', 'Metro', 'summary', 1, ?2)",
                    params![
                        space.id,
                        json!({"compaction": {"from_event_id": from, "through_event_id": through}})
                            .to_string(),
                        room
                    ],
                )
                .unwrap();
            }
        }
        let written = delete_in_space(&db, &space.id, "lobby", gone.id, &human("alice"))
            .unwrap()
            .unwrap();
        // The message, then the one summary that covered it.
        assert_eq!(written.len(), 2);
        assert_eq!(
            count(
                &db,
                "SELECT COUNT(*) FROM message_attachments WHERE event_id = ?1",
                gone.id
            ),
            0
        );
        assert_eq!(
            count(
                &db,
                "SELECT COUNT(*) FROM search_vectors WHERE event_id = ?1",
                gone.id
            ),
            0
        );
        assert_eq!(
            count(
                &db,
                "SELECT COUNT(*) FROM search_vectors WHERE event_id = ?1",
                keep.id
            ),
            1
        );
        assert_eq!(
            count(
                &db,
                "SELECT COUNT(*) FROM space_events WHERE kind = 'compaction' AND ?1 > 0",
                1
            ),
            2
        );
        let left = db.events_after(&space.id, "lobby", 0, 100).unwrap();
        assert!(left.iter().any(|e| e.id == keep.id));
        assert!(!left.iter().any(|e| e.id == gone.id));
        // Bob isn't a member: unknown.
        assert_eq!(
            delete_in_space(&db, &space.id, "lobby", keep.id, &human("bob")).unwrap(),
            Err(Refused::NotFound)
        );
    }
}
