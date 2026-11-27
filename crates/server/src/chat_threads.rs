//! Threads of a personal Chat.
//!
//! A **Chat thread** is a separate conversation inside one person's own
//! Chat: a new thread with a title, or a reply thread anchored to one
//! `message` or `agent` event of the main Chat (at most one per event).
//! Threads stay one level deep. Each one is its own conversation for the
//! Chat agent, with its own history, summaries and turns; its events are
//! `personal_events` rows with the thread's `thread_id` (`NULL` is the main
//! Chat).
//!
//! Only the Chat's owner sees or manages its threads; there is no admin
//! bypass, and an unknown thread is 404.
//!
//! - `GET /v1/chat/threads`, `POST /v1/chat/threads {title?}`
//! - `POST /v1/chat/events/{eid}/thread`: the reply thread of a main-Chat
//!   message or reply (200 when it exists, 201 when created).
//! - `PATCH /v1/chat/threads/{tid} {title}`: rename.
//! - `DELETE /v1/chat/threads/{tid}`: permanent. One transaction removes the
//!   thread and everything in it: events, reactions, attachment links,
//!   search vectors, summaries and pending decisions, so nothing of it
//!   reaches a model again. Its running turns are stopped.

use std::collections::HashMap;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::AppState;
use crate::api::{ApiError, ApiResult, JsonBody, db, empty_object};
use crate::auth::Human;
use crate::db::{Db, Event, Replies, kind};

/// Most threads one person's Chat may have.
pub const MAX_THREADS: usize = 500;
/// Longest title, in characters.
pub const MAX_TITLE: usize = 80;
/// The title of a new thread when none is given.
pub const DEFAULT_TITLE: &str = "New thread";

/// The kinds a reply thread may be anchored to.
const ANCHORABLE: [&str; 2] = [kind::MESSAGE, kind::AGENT];

pub fn init(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS chat_threads (
             id              TEXT PRIMARY KEY NOT NULL,
             user_id         TEXT NOT NULL,
             title           TEXT NOT NULL,
             anchor_event_id INTEGER,
             created_at      INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS chat_threads_user ON chat_threads (user_id, created_at);
         CREATE UNIQUE INDEX IF NOT EXISTS chat_threads_anchor
             ON chat_threads (user_id, anchor_event_id) WHERE anchor_event_id IS NOT NULL;",
    )?;
    // Additive: existing Chat events stay in the main Chat (`NULL`).
    let has: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM pragma_table_info('personal_events') WHERE name = 'thread_id')",
        [],
        |r| r.get(0),
    )?;
    if !has {
        conn.execute_batch("ALTER TABLE personal_events ADD COLUMN thread_id TEXT")?;
    }
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS personal_events_thread
             ON personal_events (user_id, thread_id, id) WHERE thread_id IS NOT NULL",
    )
}

/// A Chat thread as its owner sees it.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ChatThread {
    pub id: String,
    pub title: String,
    /// The main-Chat event a reply thread answers; absent for new threads.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor_event_id: Option<i64>,
    #[serde(serialize_with = "time")]
    pub created_at: Option<i64>,
    /// The latest message or reply in it, if any.
    #[serde(serialize_with = "time")]
    pub last_at: Option<i64>,
    /// `message` and `agent` events in it.
    pub count: u64,
}

fn time<S: serde::Serializer>(ms: &Option<i64>, s: S) -> Result<S::Ok, S::Error> {
    match ms.and_then(crate::db::rfc3339_utc) {
        Some(t) => s.serialize_str(&t),
        None => s.serialize_none(),
    }
}

const COLUMNS: &str = "t.id, t.title, t.anchor_event_id, t.created_at,
    (SELECT MAX(e.created_at) FROM personal_events e
     WHERE e.user_id = t.user_id AND e.thread_id = t.id AND e.kind IN ('message', 'agent')),
    (SELECT COUNT(*) FROM personal_events e
     WHERE e.user_id = t.user_id AND e.thread_id = t.id AND e.kind IN ('message', 'agent'))";

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ChatThread> {
    Ok(ChatThread {
        id: r.get(0)?,
        title: r.get(1)?,
        anchor_event_id: r.get(2)?,
        created_at: crate::db::event_time(r.get(3)?),
        last_at: r.get::<_, Option<i64>>(4)?.and_then(crate::db::event_time),
        count: r.get::<_, i64>(5)?.max(0) as u64,
    })
}

/// `user`'s threads, most recently active first.
pub fn list(conn: &Connection, user: &str) -> rusqlite::Result<Vec<ChatThread>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {COLUMNS} FROM chat_threads t WHERE t.user_id = ?1"
    ))?;
    let mut out: Vec<ChatThread> = stmt.query_map([user], row)?.collect::<Result<_, _>>()?;
    out.sort_by(|a, b| {
        b.last_at
            .or(b.created_at)
            .cmp(&a.last_at.or(a.created_at))
            .then_with(|| b.id.cmp(&a.id))
    });
    Ok(out)
}

/// Thread `id` of `user`, or `None` (unknown and someone else's alike).
pub fn get_in(conn: &Connection, user: &str, id: &str) -> rusqlite::Result<Option<ChatThread>> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM chat_threads t WHERE t.user_id = ?1 AND t.id = ?2"),
        params![user, id],
        row,
    )
    .optional()
}

pub fn exists_in(conn: &Connection, user: &str, id: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM chat_threads WHERE user_id = ?1 AND id = ?2)",
        params![user, id],
        |r| r.get(0),
    )
}

/// The thread of `user`'s Chat event `eid`: `Some(None)` for the main Chat,
/// `None` when there is no such event.
pub fn thread_of_in(
    conn: &Connection,
    user: &str,
    eid: i64,
) -> rusqlite::Result<Option<Option<String>>> {
    conn.query_row(
        "SELECT thread_id FROM personal_events WHERE id = ?1 AND user_id = ?2",
        params![eid, user],
        |r| r.get(0),
    )
    .optional()
}

/// The anchor of `user`'s thread `id`: a main-Chat message or reply, or
/// `None` for a new thread and once the anchor was deleted.
pub fn anchor_in(conn: &Connection, user: &str, id: &str) -> rusqlite::Result<Option<Event>> {
    conn.query_row(
        "SELECT e.id, e.kind, e.actor, e.actor_id, e.text, e.mentions, e.audience, e.activity, e.created_at
         FROM chat_threads t JOIN personal_events e
             ON e.id = t.anchor_event_id AND e.user_id = t.user_id AND e.thread_id IS NULL
         WHERE t.user_id = ?1 AND t.id = ?2 AND e.kind IN ('message', 'agent')",
        params![user, id],
        crate::db::row_event,
    )
    .optional()
}

/// A title trimmed and checked, or `None` when it isn't usable.
pub fn check_title(title: &str) -> Option<String> {
    let title = title.trim();
    (!title.is_empty()
        && title.chars().count() <= MAX_TITLE
        && !title.chars().any(char::is_control))
    .then(|| title.to_owned())
}

/// A reply thread's title: the start of its anchor's text, on one line.
fn anchor_title(text: &str) -> String {
    let line: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut title: String = line.chars().take(MAX_TITLE).collect();
    if line.chars().count() > MAX_TITLE {
        title = title.chars().take(MAX_TITLE - 1).collect::<String>() + "…";
    }
    if title.is_empty() {
        "Thread".to_owned()
    } else {
        title
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ThreadError {
    /// No such event (or thread) in this Chat.
    NotFound,
    /// The event is itself in a thread: threads stay one level deep.
    Nested,
    /// Not a message or a reply.
    Kind,
    /// The Chat has [`MAX_THREADS`] threads.
    TooMany,
}

fn count_in(conn: &Connection, user: &str) -> rusqlite::Result<usize> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM chat_threads WHERE user_id = ?1",
        [user],
        |r| r.get(0),
    )?;
    Ok(usize::try_from(n).unwrap_or(usize::MAX))
}

fn insert_in(
    conn: &Connection,
    user: &str,
    title: &str,
    anchor: Option<i64>,
) -> rusqlite::Result<String> {
    let id = format!("ct-{}", crate::db::short_id());
    conn.execute(
        "INSERT INTO chat_threads (id, user_id, title, anchor_event_id, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![id, user, title, anchor, crate::db::now_ms()],
    )?;
    Ok(id)
}

/// A new thread of `user`'s Chat.
pub fn create(db: &Db, user: &str, title: &str) -> anyhow::Result<Result<ChatThread, ThreadError>> {
    let mut conn = db.lock();
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if count_in(&tx, user)? >= MAX_THREADS {
        return Ok(Err(ThreadError::TooMany));
    }
    let id = insert_in(&tx, user, title, None)?;
    let thread = get_in(&tx, user, &id)?.ok_or_else(|| anyhow::anyhow!("thread vanished"))?;
    tx.commit()?;
    Ok(Ok(thread))
}

/// The reply thread of main-Chat event `eid`, created if needed (`true`).
pub fn get_or_create(
    db: &Db,
    user: &str,
    eid: i64,
) -> anyhow::Result<Result<(ChatThread, bool), ThreadError>> {
    let mut conn = db.lock();
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let anchor: Option<(String, String, Option<String>)> = tx
        .query_row(
            "SELECT kind, text, thread_id FROM personal_events WHERE id = ?1 AND user_id = ?2",
            params![eid, user],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((anchor_kind, text, thread)) = anchor else {
        return Ok(Err(ThreadError::NotFound));
    };
    if thread.is_some() {
        return Ok(Err(ThreadError::Nested));
    }
    let existing: Option<String> = tx
        .query_row(
            "SELECT id FROM chat_threads WHERE user_id = ?1 AND anchor_event_id = ?2",
            params![user, eid],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(id) = existing {
        let thread = get_in(&tx, user, &id)?.ok_or_else(|| anyhow::anyhow!("thread vanished"))?;
        return Ok(Ok((thread, false)));
    }
    // A deleted anchor keeps its thread but never gets a new one.
    if !ANCHORABLE.contains(&anchor_kind.as_str()) {
        return Ok(Err(ThreadError::Kind));
    }
    if count_in(&tx, user)? >= MAX_THREADS {
        return Ok(Err(ThreadError::TooMany));
    }
    let id = insert_in(&tx, user, &anchor_title(&text), Some(eid))?;
    let thread = get_in(&tx, user, &id)?.ok_or_else(|| anyhow::anyhow!("thread vanished"))?;
    tx.commit()?;
    Ok(Ok((thread, true)))
}

/// Rename thread `id` of `user`; `None` when there is no such thread.
pub fn rename(db: &Db, user: &str, id: &str, title: &str) -> anyhow::Result<Option<ChatThread>> {
    let conn = db.lock();
    let n = conn.execute(
        "UPDATE chat_threads SET title = ?3 WHERE user_id = ?1 AND id = ?2",
        params![user, id, title],
    )?;
    if n == 0 {
        return Ok(None);
    }
    Ok(get_in(&conn, user, id)?)
}

/// What deleting a thread removed: the main-Chat `message_deleted` events
/// written (for a tombstoned anchor that no longer anchors anything).
pub struct Deleted {
    pub events: Vec<Event>,
}

/// Delete thread `id` of `who`'s Chat and everything in it, in one
/// transaction. `None` when there is no such thread.
pub fn delete(db: &Db, who: &Human, id: &str) -> anyhow::Result<Option<Deleted>> {
    let mut conn = db.lock();
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let anchor: Option<Option<i64>> = tx
        .query_row(
            "SELECT anchor_event_id FROM chat_threads WHERE user_id = ?1 AND id = ?2",
            params![who.id, id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(anchor) = anchor else {
        return Ok(None);
    };
    let ids: Vec<i64> = {
        let mut stmt =
            tx.prepare("SELECT id FROM personal_events WHERE user_id = ?1 AND thread_id = ?2")?;
        stmt.query_map(params![who.id, id], |r| r.get(0))?
            .collect::<Result<_, _>>()?
    };
    let json_ids = serde_json::to_string(&ids)?;
    tx.execute(
        "DELETE FROM personal_reactions
         WHERE user_id = ?1 AND target_event_id IN (SELECT value FROM json_each(?2))",
        params![who.id, json_ids],
    )?;
    tx.execute(
        "DELETE FROM message_attachments
         WHERE scope = 'personal' AND event_id IN (SELECT value FROM json_each(?1))",
        [&json_ids],
    )?;
    tx.execute(
        "DELETE FROM search_vectors WHERE user_id = ?1 AND source = 'chat'
           AND event_id IN (SELECT value FROM json_each(?2))",
        params![who.id, json_ids],
    )?;
    tx.execute(
        "DELETE FROM pending_decisions WHERE scope = 'personal'
           AND event_id IN (SELECT value FROM json_each(?1))",
        [&json_ids],
    )?;
    tx.execute(
        "DELETE FROM personal_events WHERE user_id = ?1 AND thread_id = ?2",
        params![who.id, id],
    )?;
    tx.execute(
        "DELETE FROM chat_threads WHERE user_id = ?1 AND id = ?2",
        params![who.id, id],
    )?;
    // A deleted anchor stayed only for its thread: it goes with it.
    let mut events = Vec::new();
    if let Some(anchor) = anchor {
        let gone = tx.execute(
            "DELETE FROM personal_events WHERE id = ?1 AND user_id = ?2 AND kind = ?3",
            params![anchor, who.id, kind::DELETED],
        )?;
        if gone == 1 {
            events.push(crate::deletion::note_chat_in(
                &tx, who, None, anchor, false,
            )?);
        }
    }
    tx.commit()?;
    Ok(Some(Deleted { events }))
}

/// Fill in `replies` on the main-Chat events of `user` that anchor a thread.
pub fn hydrate(db: &Db, user: &str, events: &mut [Event]) -> anyhow::Result<()> {
    if events.is_empty() {
        return Ok(());
    }
    let ids = serde_json::to_string(&events.iter().map(|e| e.id).collect::<Vec<_>>())?;
    let conn = db.lock();
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {COLUMNS} FROM chat_threads t
         WHERE t.user_id = ?1 AND t.anchor_event_id IN (SELECT value FROM json_each(?2))"
    ))?;
    let mut found: HashMap<i64, Replies> = stmt
        .query_map(params![user, ids], row)?
        .filter_map(|t| match t {
            Ok(t) => t.anchor_event_id.map(|a| {
                Ok((
                    a,
                    Replies {
                        room_id: t.id,
                        count: t.count,
                        last_at: t.last_at,
                        // Only the owner writes in their Chat.
                        participant_ids: Vec::new(),
                    },
                ))
            }),
            Err(e) => Some(Err(e)),
        })
        .collect::<Result<_, _>>()?;
    for e in events.iter_mut() {
        e.replies = found.remove(&e.id);
    }
    Ok(())
}

/// The thread named by a request, checked to be the caller's: `None` is
/// the main Chat; an unknown thread is 404.
pub(crate) async fn checked(
    state: &AppState,
    who: &Human,
    thread: Option<String>,
) -> ApiResult<Option<String>> {
    let Some(tid) = thread else {
        return Ok(None);
    };
    let (user, id) = (who.id.clone(), tid.clone());
    if db(state, move |db| Ok(exists_in(&db.lock(), &user, &id)?)).await? {
        Ok(Some(tid))
    } else {
        Err(not_found())
    }
}

fn not_found() -> ApiError {
    ApiError::not_found("no such Chat thread")
}

fn bad_title() -> ApiError {
    ApiError::bad_request(format!(
        "title must be 1..={MAX_TITLE} printable characters"
    ))
}

impl From<ThreadError> for ApiError {
    fn from(e: ThreadError) -> Self {
        match e {
            ThreadError::NotFound => ApiError::not_found("no such message"),
            ThreadError::Nested => {
                ApiError::new(StatusCode::CONFLICT, "threads can't have threads")
            }
            ThreadError::Kind => {
                ApiError::new(StatusCode::CONFLICT, "this event can't have a thread")
            }
            ThreadError::TooMany => ApiError::new(
                StatusCode::CONFLICT,
                format!("a Chat can have at most {MAX_THREADS} threads"),
            ),
        }
    }
}

async fn list_threads(State(state): State<AppState>, who: Human) -> ApiResult<Json<Value>> {
    let threads = db(&state, move |db| Ok(list(&db.lock(), &who.id)?)).await?;
    Ok(Json(json!({ "threads": threads })))
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct CreateBody {
    #[serde(default)]
    title: Option<String>,
}

async fn create_thread(
    State(state): State<AppState>,
    who: Human,
    body: axum::body::Bytes,
) -> ApiResult<(StatusCode, Json<ChatThread>)> {
    let body: CreateBody = if body.iter().all(u8::is_ascii_whitespace) {
        CreateBody::default()
    } else {
        serde_json::from_slice(&body).map_err(|e| ApiError::bad_request(e.to_string()))?
    };
    let title = match body.title {
        Some(t) => check_title(&t).ok_or_else(bad_title)?,
        None => DEFAULT_TITLE.to_owned(),
    };
    let thread = db(&state, move |db| create(db, &who.id, &title)).await??;
    Ok((StatusCode::CREATED, Json(thread)))
}

async fn reply_thread(
    State(state): State<AppState>,
    who: Human,
    Path(eid): Path<i64>,
    body: axum::body::Bytes,
) -> ApiResult<(StatusCode, Json<ChatThread>)> {
    empty_object(&body)?;
    match db(&state, move |db| get_or_create(db, &who.id, eid)).await?? {
        (thread, true) => Ok((StatusCode::CREATED, Json(thread))),
        (thread, false) => Ok((StatusCode::OK, Json(thread))),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PatchBody {
    title: String,
}

async fn patch_thread(
    State(state): State<AppState>,
    who: Human,
    Path(tid): Path<String>,
    body: JsonBody<PatchBody>,
) -> ApiResult<Json<ChatThread>> {
    let Json(body) = body?;
    let title = check_title(&body.title).ok_or_else(bad_title)?;
    db(&state, move |db| rename(db, &who.id, &tid, &title))
        .await?
        .map(Json)
        .ok_or_else(not_found)
}

async fn delete_thread(
    State(state): State<AppState>,
    who: Human,
    Path(tid): Path<String>,
) -> ApiResult<Json<Value>> {
    let (user, id) = (who.clone(), tid.clone());
    let deleted = db(&state, move |db| delete(db, &user, &id))
        .await?
        .ok_or_else(not_found)?;
    // Its turns end now; their later events have nowhere to go.
    state.chat_runs.stop_thread(&who.id, &tid);
    Ok(Json(json!({ "deleted": true, "events": deleted.events })))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/chat/threads", get(list_threads).post(create_thread))
        .route(
            "/v1/chat/threads/{tid}",
            patch(patch_thread).delete(delete_thread),
        )
        .route("/v1/chat/events/{eid}/thread", post(reply_thread))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::db::{Activity, Addressed, Author, activity};

    #[test]
    fn titles_are_checked_and_anchor_titles_fit() {
        assert_eq!(check_title("  Plans  ").as_deref(), Some("Plans"));
        for bad in ["", "   ", "a\tb", &"x".repeat(MAX_TITLE + 1)] {
            assert!(check_title(bad).is_none(), "{bad:?}");
        }
        assert_eq!(anchor_title("  two\nlines "), "two lines");
        assert_eq!(anchor_title(""), "Thread");
        let long = anchor_title(&"y".repeat(200));
        assert_eq!(long.chars().count(), MAX_TITLE);
        assert!(long.ends_with('…'));
    }

    #[test]
    fn a_thread_turn_is_stopped_with_its_thread_only() {
        let turns = Arc::new(crate::meta::ChatTurns::default());
        let (main, _g1) = turns.register("t-main", "alice", None);
        let (side, _g2) = turns.register("t-side", "alice", Some("ct-1"));
        let (bobs, _g3) = turns.register("t-bob", "bob", Some("ct-1"));
        assert!(turns.running_for("alice", Some("ct-1")));
        assert!(turns.running_for("alice", None));
        assert!(!turns.running_for("alice", Some("ct-2")));
        turns.stop_thread("alice", "ct-1");
        assert!(*side.borrow());
        assert!(!*main.borrow() && !*bobs.borrow());
    }

    #[test]
    fn appends_to_a_deleted_thread_write_nothing() {
        let db = Db::open_in_memory().unwrap();
        let t = create(&db, "alice", "T").unwrap().unwrap();
        let author = Author::server("agent");
        let add = |thread: &str| {
            db.append_personal_with(
                "alice",
                Some(thread),
                kind::STATUS,
                &author,
                "x",
                &Addressed::room(),
                None,
            )
        };
        assert!(add(&t.id).is_ok());
        // Not Bob's thread, and not one that exists.
        assert!(
            db.append_personal_with(
                "bob",
                Some(&t.id),
                kind::STATUS,
                &author,
                "x",
                &Addressed::room(),
                None
            )
            .is_err()
        );
        let who = Human {
            id: "alice".into(),
            name: "alice".into(),
        };
        assert!(delete(&db, &who, &t.id).unwrap().is_some());
        assert!(add(&t.id).is_err());
        let left: i64 = db
            .lock()
            .query_row("SELECT COUNT(*) FROM personal_events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(left, 0);
    }

    #[test]
    fn a_turn_left_running_in_a_thread_is_interrupted_there() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metro.sqlite3");
        let tid = {
            let db = Db::open(&path).unwrap();
            let t = create(&db, "alice", "T").unwrap().unwrap();
            let turn = Activity::chat();
            db.append_personal_with(
                "alice",
                Some(&t.id),
                kind::ACTIVITY,
                &Author::server("agent"),
                "",
                &Addressed::room(),
                Some(&turn.at(activity::STARTED)),
            )
            .unwrap();
            t.id
        };
        let db = Db::open(&path).unwrap();
        let ended = |thread: Option<&str>| {
            db.chat_after("alice", thread, 0, 100)
                .unwrap()
                .into_iter()
                .filter(|e| {
                    e.activity.as_ref().and_then(|a| a.phase.as_deref())
                        == Some(activity::INTERRUPTED)
                })
                .count()
        };
        assert_eq!(ended(Some(&tid)), 1);
        assert_eq!(ended(None), 0);
    }
}
