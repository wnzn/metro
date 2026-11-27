//! Reply threads and replies.
//!
//! A **reply thread** is a child room of a top-level room, anchored to one
//! `message`, `agent` or `thread` event in it, with at most one thread per
//! event. Its internal name is `reply-<event id>` (names of that shape are
//! taken for every other room); clients show it as "Thread". Reply threads
//! inherit the Space's members and runner like every thread, are never
//! nested, and count only toward their own per-Space `max_reply_threads`,
//! not toward `max_rooms` or `max_threads_per_room`.
//!
//! A message in a thread may reply to an event of the same thread or, in a
//! reply thread, to its anchor (`reply_to`, an id only). Messages in
//! top-level rooms can't reply. A reply to an agent's message addresses
//! that agent explicitly ([`reply_agent`]), unless the sender opts out.

use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::db::{Db, Event, ROOM_COLUMNS, Replies, Room, kind, row_room};
use crate::space_agents::SpaceAgent;

const NAME_PREFIX: &str = "reply-";

/// The kinds a reply thread may be anchored to, and a reply may answer.
const REPLYABLE: [&str; 3] = [kind::MESSAGE, kind::AGENT, kind::THREAD];

const EVENT_COLUMNS: &str =
    "id, kind, actor, actor_id, text, mentions, audience, activity, created_at";

/// Whether `name` has the shape of a reply thread's internal name.
pub fn is_reply_thread_name(name: &str) -> bool {
    name.get(..NAME_PREFIX.len())
        .is_some_and(|p| p.eq_ignore_ascii_case(NAME_PREFIX))
        && name.len() > NAME_PREFIX.len()
        && name[NAME_PREFIX.len()..]
            .bytes()
            .all(|b| b.is_ascii_digit())
}

fn thread_name(event_id: i64) -> String {
    format!("{NAME_PREFIX}{event_id}")
}

/// Why [`get_or_create`] made no thread.
#[derive(Debug, PartialEq, Eq)]
pub enum ThreadError {
    /// The sender isn't a member now, or the room or event doesn't exist
    /// there.
    NotFound,
    /// The room is itself a thread: threads stay one level deep.
    Nested,
    /// The event is not a message, an agent reply or a thread event.
    Kind,
    /// The Space has `max_reply_threads` reply threads.
    TooMany,
    /// An older room already has the thread's name.
    NameTaken,
}

/// The reply thread of event `event_id` in top-level room `room_id`,
/// created if needed (`true`), with every check in one transaction.
pub fn get_or_create(
    db: &Db,
    space_id: &str,
    room_id: &str,
    event_id: i64,
    user_id: &str,
    max_reply_threads: usize,
) -> anyhow::Result<Result<(Room, bool), ThreadError>> {
    let mut conn = db.lock();
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if !Db::room_member_in(&tx, space_id, room_id, user_id)? {
        return Ok(Err(ThreadError::NotFound));
    }
    let Some(room) = Db::room_in(&tx, space_id, room_id)? else {
        return Ok(Err(ThreadError::NotFound));
    };
    if room.parent_room_id.is_some() {
        return Ok(Err(ThreadError::Nested));
    }
    let Some(anchor) = event_in(&tx, space_id, room_id, event_id)? else {
        return Ok(Err(ThreadError::NotFound));
    };
    // A deleted anchor keeps its existing thread but never gets a new one.
    let deleted = anchor.kind == kind::DELETED;
    if !deleted && !REPLYABLE.contains(&anchor.kind.as_str()) {
        return Ok(Err(ThreadError::Kind));
    }
    if let Some(existing) = tx
        .query_row(
            &format!(
                "SELECT {ROOM_COLUMNS} FROM space_rooms
                 WHERE space_id = ?1 AND parent_room_id = ?2 AND anchor_event_id = ?3"
            ),
            params![space_id, room_id, event_id],
            row_room,
        )
        .optional()?
    {
        return Ok(Ok((existing, false)));
    }
    if deleted {
        return Ok(Err(ThreadError::Kind));
    }
    let count: i64 = tx.query_row(
        "SELECT COUNT(*) FROM space_rooms WHERE space_id = ?1 AND anchor_event_id IS NOT NULL",
        [space_id],
        |r| r.get(0),
    )?;
    if usize::try_from(count).unwrap_or(usize::MAX) >= max_reply_threads {
        return Ok(Err(ThreadError::TooMany));
    }
    let thread = Room {
        id: format!("r-{}", crate::db::short_id()),
        name: thread_name(event_id),
        parent_room_id: Some(room.id),
        anchor_event_id: Some(event_id),
    };
    match tx.execute(
        "INSERT INTO space_rooms (space_id, id, name, parent_room_id, anchor_event_id, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            space_id,
            thread.id,
            thread.name,
            thread.parent_room_id,
            event_id,
            crate::db::now_ms()
        ],
    ) {
        Ok(_) => {}
        Err(e) if crate::db::is_unique_violation(&e) => return Ok(Err(ThreadError::NameTaken)),
        Err(e) => return Err(e.into()),
    }
    tx.commit()?;
    Ok(Ok((thread, true)))
}

pub(crate) fn event_in(
    conn: &Connection,
    space_id: &str,
    room_id: &str,
    event_id: i64,
) -> rusqlite::Result<Option<Event>> {
    conn.query_row(
        &format!(
            "SELECT {EVENT_COLUMNS} FROM space_events WHERE space_id = ?1 AND room_id = ?2 AND id = ?3"
        ),
        params![space_id, room_id, event_id],
        crate::db::row_event,
    )
    .optional()
}

/// Why a `reply_to` is refused.
#[derive(Debug, PartialEq, Eq)]
pub enum ReplyError {
    /// Top-level rooms take no replies: reply in a thread.
    TopLevel,
    /// Not a message, agent reply or thread event of this thread, nor its anchor.
    NotFound,
}

/// The event a message in `room_id` may reply to as `reply_to`: one of the
/// [`REPLYABLE`] kinds in the same thread, or a reply thread's anchor.
pub fn reply_target_in(
    conn: &Connection,
    space_id: &str,
    room_id: &str,
    reply_to: i64,
) -> rusqlite::Result<Result<Event, ReplyError>> {
    let Some(room) = Db::room_in(conn, space_id, room_id)? else {
        return Ok(Err(ReplyError::NotFound));
    };
    let Some(parent) = room.parent_room_id else {
        return Ok(Err(ReplyError::TopLevel));
    };
    let home = if room.anchor_event_id == Some(reply_to) {
        parent
    } else {
        room.id
    };
    Ok(match event_in(conn, space_id, &home, reply_to)? {
        Some(e) if REPLYABLE.contains(&e.kind.as_str()) => Ok(e),
        _ => Err(ReplyError::NotFound),
    })
}

/// The agent a reply to `target` addresses.
#[derive(Debug)]
pub enum ReplyAgent<'a> {
    /// `target` isn't an agent's reply.
    None,
    Agent(&'a SpaceAgent),
    /// The agent that wrote `target` is no longer in the Space.
    Unavailable,
}

/// A reply to an agent's reply addresses that agent: the one named on its
/// activity, or for older replies without one, the Space's default agent.
/// `agents` are the Space's current agents.
pub fn reply_agent<'a>(target: &Event, agents: &'a [SpaceAgent]) -> ReplyAgent<'a> {
    if target.kind != kind::AGENT {
        return ReplyAgent::None;
    }
    let found = match target.activity.as_ref().and_then(|a| a.agent.as_ref()) {
        Some(badge) => agents.iter().find(|a| a.id == badge.id),
        None => agents.iter().find(|a| a.is_default).or(agents.first()),
    };
    found.map_or(ReplyAgent::Unavailable, ReplyAgent::Agent)
}

/// Fill in `replies` on the events of top-level room `room_id` that anchor a
/// reply thread, with one aggregate query for the page.
pub fn hydrate(db: &Db, space_id: &str, room_id: &str, events: &mut [Event]) -> anyhow::Result<()> {
    if events.is_empty() {
        return Ok(());
    }
    let ids = serde_json::to_string(&events.iter().map(|e| e.id).collect::<Vec<_>>())?;
    let conn = db.lock();
    let mut stmt = conn.prepare_cached(
        "SELECT r.anchor_event_id, r.id,
                (SELECT COUNT(*) FROM space_events e
                 WHERE e.space_id = r.space_id AND e.room_id = r.id AND e.kind IN ('message', 'agent')),
                (SELECT MAX(e.created_at) FROM space_events e
                 WHERE e.space_id = r.space_id AND e.room_id = r.id AND e.kind IN ('message', 'agent')),
                (SELECT json_group_array(actor_id) FROM
                    (SELECT e.actor_id, MAX(e.id) AS last FROM space_events e
                     WHERE e.space_id = r.space_id AND e.room_id = r.id AND e.kind = 'message'
                       AND e.actor_id IS NOT NULL
                     GROUP BY e.actor_id ORDER BY last DESC LIMIT 3))
         FROM space_rooms r
         WHERE r.space_id = ?1 AND r.parent_room_id = ?2
           AND r.anchor_event_id IN (SELECT value FROM json_each(?3))",
    )?;
    let mut found: HashMap<i64, Replies> = stmt
        .query_map(params![space_id, room_id, ids], |r| {
            let people: String = r.get(4)?;
            Ok((
                r.get::<_, i64>(0)?,
                Replies {
                    room_id: r.get(1)?,
                    count: r.get::<_, i64>(2)?.max(0) as u64,
                    last_at: r.get::<_, Option<i64>>(3)?.and_then(crate::db::event_time),
                    participant_ids: serde_json::from_str(&people).unwrap_or_default(),
                },
            ))
        })?
        .collect::<Result<_, _>>()?;
    for e in events.iter_mut() {
        e.replies = found.remove(&e.id);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reply_thread_names_are_recognised() {
        for name in ["reply-1", "REPLY-42", "Reply-007"] {
            assert!(is_reply_thread_name(name), "{name}");
        }
        for name in ["reply-", "reply-1a", "replies-1", "reply 1", "reply-١", "r"] {
            assert!(!is_reply_thread_name(name), "{name}");
        }
        assert_eq!(thread_name(12), "reply-12");
    }
}
