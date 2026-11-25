//! Private, two-person conversations. None of these writes dispatch an agent.

use axum::extract::{Path, Query, State, rejection::QueryRejection};
use axum::http::StatusCode;
use axum::routing::{delete, get, put};
use axum::{Json, Router};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::AppState;
use crate::api::{self, ApiError, ApiResult, JsonBody};
use crate::auth::{Human, valid_id};
use crate::db::reactions::{Reaction, valid_emoji};
use crate::db::{Event, event_time, kind, now_ms, rfc3339_utc, short_id};
use crate::directory::{self, Card};

pub(crate) fn init(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS dms (
             id TEXT PRIMARY KEY NOT NULL,
             user_lo TEXT NOT NULL, user_hi TEXT NOT NULL,
             created_by TEXT NOT NULL, created_at INTEGER NOT NULL,
             UNIQUE (user_lo, user_hi), CHECK (user_lo < user_hi)
         );
         CREATE INDEX IF NOT EXISTS dms_user_hi ON dms (user_hi);
         CREATE TABLE IF NOT EXISTS dm_state (
             dm_id TEXT NOT NULL REFERENCES dms (id) ON DELETE CASCADE,
             user_id TEXT NOT NULL,
             last_read INTEGER NOT NULL DEFAULT 0, hidden_before INTEGER NOT NULL DEFAULT 0,
             PRIMARY KEY (dm_id, user_id)
         );
         CREATE TABLE IF NOT EXISTS dm_events (
             id INTEGER PRIMARY KEY AUTOINCREMENT,
             dm_id TEXT NOT NULL REFERENCES dms (id) ON DELETE CASCADE,
             kind TEXT NOT NULL CHECK (kind = 'message'),
             actor TEXT NOT NULL, actor_id TEXT, text TEXT NOT NULL,
             activity TEXT, created_at INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS dm_events_dm ON dm_events (dm_id, id);
         CREATE INDEX IF NOT EXISTS dm_events_by_actor ON dm_events (dm_id, actor_id, id);
         CREATE TABLE IF NOT EXISTS dm_reactions (
             dm_id TEXT NOT NULL REFERENCES dms (id) ON DELETE CASCADE,
             event_id INTEGER NOT NULL REFERENCES dm_events (id) ON DELETE CASCADE,
             user_id TEXT NOT NULL, emoji TEXT NOT NULL,
             PRIMARY KEY (event_id, user_id, emoji)
         );
         CREATE INDEX IF NOT EXISTS dm_reactions_dm ON dm_reactions (dm_id);",
    )
}

fn ordered<'a>(a: &'a str, b: &'a str) -> [&'a str; 2] {
    if a < b { [a, b] } else { [b, a] }
}

pub(crate) fn are_peers(conn: &Connection, a: &str, b: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM dms WHERE user_lo = ?1 AND user_hi = ?2)",
        ordered(a, b),
        |row| row.get(0),
    )
}

#[derive(Debug)]
pub(crate) enum DmError {
    Request(ApiError),
    Store(anyhow::Error),
}

impl From<ApiError> for DmError {
    fn from(e: ApiError) -> Self {
        Self::Request(e)
    }
}

impl From<rusqlite::Error> for DmError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Store(e.into())
    }
}

impl From<serde_json::Error> for DmError {
    fn from(e: serde_json::Error) -> Self {
        Self::Store(e.into())
    }
}

impl From<DmError> for ApiError {
    fn from(e: DmError) -> Self {
        match e {
            DmError::Request(e) => e,
            DmError::Store(e) => ApiError::internal(e),
        }
    }
}

type DmResult<T> = Result<T, DmError>;

// These transaction-scoped operations also form the boundary for Forward:
// the caller must do its source and destination checks in the same write.
pub(crate) fn get_or_create(
    tx: &Transaction<'_>,
    user: &str,
    peer: &str,
) -> DmResult<(String, bool)> {
    if user == peer {
        return Err(ApiError::bad_request("you can't message yourself").into());
    }
    if !valid_id(peer) || !directory::listed(tx, peer)? {
        return Err(ApiError::not_found("no such user").into());
    }
    let [lo, hi] = ordered(user, peer);
    let existing = tx
        .query_row(
            "SELECT id FROM dms WHERE user_lo = ?1 AND user_hi = ?2",
            [lo, hi],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(id) = existing {
        return Ok((id, false));
    }
    let id = format!("dm-{}", short_id());
    tx.execute(
        "INSERT INTO dms (id, user_lo, user_hi, created_by, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![id, lo, hi, user, now_ms()],
    )?;
    Ok((id, true))
}

fn no_dm() -> ApiError {
    ApiError::not_found("no such conversation")
}

fn no_message() -> ApiError {
    ApiError::not_found("no such message")
}

fn hidden_before(tx: &Transaction<'_>, id: &str, user: &str) -> DmResult<i64> {
    tx.query_row(
        "SELECT coalesce(s.hidden_before, 0) FROM dms d
         LEFT JOIN dm_state s ON s.dm_id = d.id AND s.user_id = ?2
         WHERE d.id = ?1 AND ?2 IN (d.user_lo, d.user_hi)",
        [id, user],
        |row| row.get(0),
    )
    .optional()?
    .ok_or_else(|| no_dm().into())
}

async fn transact<T: Send + 'static>(
    state: &AppState,
    behavior: TransactionBehavior,
    work: impl FnOnce(&Transaction<'_>) -> DmResult<T> + Send + 'static,
) -> ApiResult<T> {
    api::db(state, move |db| {
        let mut conn = db.lock();
        let tx = conn.transaction_with_behavior(behavior)?;
        let result = work(&tx);
        if result.is_ok() {
            tx.commit()?;
        }
        Ok(result)
    })
    .await?
    .map_err(ApiError::from)
}

#[derive(Serialize)]
struct Last {
    id: i64,
    actor_id: Option<String>,
    preview: String,
    created_at: Option<String>,
}

#[derive(Serialize)]
struct Conversation {
    id: String,
    peer: Card,
    #[serde(skip_serializing_if = "Option::is_none")]
    last: Option<Last>,
    unread: u8,
}

fn conversations(
    conn: &Connection,
    user: &str,
    only_id: Option<&str>,
) -> rusqlite::Result<Vec<Conversation>> {
    let mut stmt = conn.prepare_cached(
        "WITH mine AS (
             SELECT id, user_hi AS peer_id, created_at FROM dms WHERE user_lo = ?1
             UNION ALL
             SELECT id, user_lo AS peer_id, created_at FROM dms WHERE user_hi = ?1
         )
         SELECT p.id, p.display_name, p.avatar_version, d.id,
                e.id, e.actor_id, substr(e.text, 1, 140), e.created_at,
                (SELECT count(*) FROM (
                    SELECT 1 FROM dm_events u WHERE u.dm_id = d.id
                         AND u.id > max(coalesce(s.last_read, 0), coalesce(s.hidden_before, 0))
                         AND u.actor_id = d.peer_id LIMIT 99
                ))
         FROM mine d
         JOIN directory_cards p ON p.id = d.peer_id
         LEFT JOIN dm_state s ON s.dm_id = d.id AND s.user_id = ?1
         LEFT JOIN dm_events e ON e.id = (
             SELECT id FROM dm_events WHERE dm_id = d.id AND id > coalesce(s.hidden_before, 0)
             ORDER BY id DESC LIMIT 1
         )
         WHERE (?2 IS NULL AND (e.id IS NOT NULL OR s.dm_id IS NULL)) OR d.id = ?2
         ORDER BY coalesce(e.created_at, d.created_at) DESC, e.id DESC, d.id
         LIMIT 200",
    )?;
    // An empty new DM has no state row. Hiding writes one even if there are
    // zero events, while marking an empty DM read never creates one.
    stmt.query_map(params![user, only_id], |row| {
        let last = if let Some(id) = row.get(4)? {
            let created_at = event_time(row.get(7)?).and_then(rfc3339_utc);
            Some(Last {
                id,
                actor_id: row.get(5)?,
                preview: row.get(6)?,
                created_at,
            })
        } else {
            None
        };
        Ok(Conversation {
            id: row.get(3)?,
            peer: Card::from_row(row)?,
            last,
            unread: row.get(8)?,
        })
    })?
    .collect()
}

#[derive(Default, Serialize, Deserialize)]
pub(crate) struct MessageMeta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub forwarded: Option<Value>,
}

// Keep the adapter to L1's shared event fields confined to these definitions.
#[derive(Serialize)]
pub(crate) struct MessageEvent {
    #[serde(flatten)]
    pub event: Event,
    #[serde(flatten)]
    metadata: MessageMeta,
}

fn event_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MessageEvent> {
    let metadata = row
        .get::<_, Option<String>>(5)?
        .map(|json| serde_json::from_str(&json))
        .transpose()
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(5, rusqlite::types::Type::Text, Box::new(e))
        })?
        .unwrap_or_default();
    Ok(MessageEvent {
        event: Event {
            id: row.get(0)?,
            kind: row.get(1)?,
            actor: row.get(2)?,
            actor_id: row.get(3)?,
            text: row.get(4)?,
            mentions: json!([]),
            spans: Some(Vec::new()),
            audience: "room".into(),
            activity: None,
            created_at: event_time(row.get(6)?),
            attachments: Vec::new(),
        },
        metadata,
    })
}

pub(crate) fn visible_event(
    tx: &Transaction<'_>,
    id: &str,
    user: &str,
    event: i64,
) -> rusqlite::Result<Option<MessageEvent>> {
    tx.query_row(
        "SELECT e.id, e.kind, e.actor, e.actor_id, e.text, e.activity, e.created_at
         FROM dm_events e JOIN dms d ON d.id = e.dm_id
         LEFT JOIN dm_state s ON s.dm_id = d.id AND s.user_id = ?2
         WHERE d.id = ?1 AND ?2 IN (d.user_lo, d.user_hi)
             AND e.id = ?3 AND e.id > coalesce(s.hidden_before, 0)",
        params![id, user, event],
        event_row,
    )
    .optional()
}

pub(crate) fn append_message(
    tx: &Transaction<'_>,
    id: &str,
    who: &Human,
    text: &str,
    meta: &MessageMeta,
) -> DmResult<MessageEvent> {
    hidden_before(tx, id, &who.id)?;
    if let Some(reply) = meta.reply_to
        && visible_event(tx, id, &who.id, reply)?.is_none()
    {
        return Err(
            ApiError::bad_request("reply_to must name a visible message in this DM").into(),
        );
    }
    let activity = (meta.reply_to.is_some() || meta.forwarded.is_some())
        .then(|| serde_json::to_string(meta))
        .transpose()?;
    Ok(tx.query_row(
        "INSERT INTO dm_events (dm_id, kind, actor, actor_id, text, activity, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         RETURNING id, kind, actor, actor_id, text, activity, created_at",
        params![
            id,
            kind::MESSAGE,
            who.name,
            who.id,
            text,
            activity,
            now_ms()
        ],
        event_row,
    )?)
}

fn hide(tx: &Transaction<'_>, id: &str, user: &str) -> rusqlite::Result<()> {
    let last: i64 = tx.query_row(
        "SELECT coalesce(max(id), 0) FROM dm_events WHERE dm_id = ?1",
        [id],
        |row| row.get(0),
    )?;
    tx.execute(
        "INSERT INTO dm_state (dm_id, user_id, hidden_before) VALUES (?1, ?2, ?3)
         ON CONFLICT (dm_id, user_id) DO UPDATE SET hidden_before = excluded.hidden_before",
        params![id, user, last],
    )?;
    // State rows are participant-only. The cascade purges state, transcript
    // and reactions without touching the global event id sequence.
    tx.execute(
        "DELETE FROM dms WHERE id = ?1 AND (
             SELECT count(*) FROM dm_state WHERE dm_id = ?1 AND hidden_before >= ?2
         ) = 2",
        params![id, last],
    )?;
    Ok(())
}

async fn list(State(state): State<AppState>, who: Human) -> ApiResult<Json<Value>> {
    let dms = api::db(&state, move |db| {
        Ok(conversations(&db.lock(), &who.id, None)?)
    })
    .await?;
    Ok(Json(json!({ "dms": dms })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Create {
    user_id: String,
}

async fn create(
    State(state): State<AppState>,
    who: Human,
    body: JsonBody<Create>,
) -> ApiResult<(StatusCode, Json<Conversation>)> {
    let Json(body) = body?;
    transact(&state, TransactionBehavior::Immediate, move |tx| {
        let (id, created) = get_or_create(tx, &who.id, &body.user_id)?;
        let dm = conversations(tx, &who.id, Some(&id))?
            .pop()
            .ok_or_else(no_dm)?;
        let status = if created {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        };
        Ok((status, Json(dm)))
    })
    .await
}

#[derive(Deserialize)]
struct After {
    after: Option<i64>,
}

#[derive(Serialize)]
struct Events {
    events: Vec<MessageEvent>,
    reactions: Vec<Reaction>,
}

async fn events(
    State(state): State<AppState>,
    who: Human,
    Path(id): Path<String>,
    query: Result<Query<After>, QueryRejection>,
) -> ApiResult<Json<Events>> {
    let page = state.config.limits.events_page as i64;
    transact(&state, TransactionBehavior::Deferred, move |tx| {
        let floor = hidden_before(tx, &id, &who.id)?;
        let Query(query) = query.map_err(ApiError::from)?;
        let after = query.after.unwrap_or_default();
        if after < 0 {
            return Err(ApiError::bad_request("after must be a non-negative integer").into());
        }
        let mut stmt = tx.prepare_cached(
            "SELECT id, kind, actor, actor_id, text, activity, created_at FROM dm_events
             WHERE dm_id = ?1 AND id > ?2 ORDER BY id LIMIT ?3",
        )?;
        let events = stmt
            .query_map(params![id, after.max(floor), page], event_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let start = events.first().map(|e| e.event.id);
        let end = events.last().map(|e| e.event.id);
        let mut stmt = tx.prepare_cached(
            "SELECT event_id, emoji, user_id FROM dm_reactions
             WHERE dm_id = ?1 AND event_id BETWEEN ?2 AND ?3
             ORDER BY event_id, emoji, user_id",
        )?;
        let reactions = stmt
            .query_map(params![id, start, end], |row| {
                Ok(Reaction {
                    target_event_id: row.get(0)?,
                    emoji: row.get(1)?,
                    reactor_kind: "user".into(),
                    reactor_id: row.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Json(Events { events, reactions }))
    })
    .await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Post {
    text: String,
    reply_to: Option<i64>,
}

// Matches api::check_text on the pre-L1 branch. Share the validator when
// L1 exposes it; DM posting itself must never use the Space dispatch path.
fn check_text(text: &str, max: usize) -> ApiResult<&str> {
    let text = text.trim();
    if text.is_empty() || text.chars().count() > max {
        return Err(ApiError::bad_request(format!(
            "text must be 1..={max} characters"
        )));
    }
    Ok(text)
}

async fn post(
    State(state): State<AppState>,
    who: Human,
    Path(id): Path<String>,
    body: JsonBody<Post>,
) -> ApiResult<(StatusCode, Json<MessageEvent>)> {
    let max = state.config.limits.max_message_chars;
    transact(&state, TransactionBehavior::Immediate, move |tx| {
        hidden_before(tx, &id, &who.id)?;
        let Json(body) = body.map_err(ApiError::from)?;
        let text = check_text(&body.text, max)?;
        let meta = MessageMeta {
            reply_to: body.reply_to,
            forwarded: None,
        };
        let event = append_message(tx, &id, &who, text, &meta)?;
        Ok((StatusCode::CREATED, Json(event)))
    })
    .await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Read {
    last_read: i64,
}

async fn read(
    State(state): State<AppState>,
    who: Human,
    Path(id): Path<String>,
    body: JsonBody<Read>,
) -> ApiResult<StatusCode> {
    transact(&state, TransactionBehavior::Immediate, move |tx| {
        let floor = hidden_before(tx, &id, &who.id)?;
        let Json(body) = body.map_err(ApiError::from)?;
        let previous = tx
            .query_row(
                "SELECT last_read FROM dm_state WHERE dm_id = ?1 AND user_id = ?2",
                [&id, &who.id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or_default();
        if body.last_read > previous.max(floor) {
            if visible_event(tx, &id, &who.id, body.last_read)?.is_none() {
                return Err(ApiError::bad_request(
                    "last_read must name a visible message in this DM",
                )
                .into());
            }
            tx.execute(
                "INSERT INTO dm_state (dm_id, user_id, last_read) VALUES (?1, ?2, ?3)
                 ON CONFLICT (dm_id, user_id) DO UPDATE SET last_read = excluded.last_read",
                params![id, who.id, body.last_read],
            )?;
        }
        Ok(StatusCode::NO_CONTENT)
    })
    .await
}

async fn hide_for_me(
    State(state): State<AppState>,
    who: Human,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    transact(&state, TransactionBehavior::Immediate, move |tx| {
        hidden_before(tx, &id, &who.id)?;
        hide(tx, &id, &who.id)?;
        Ok(StatusCode::NO_CONTENT)
    })
    .await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Emoji {
    emoji: String,
}

async fn react(
    state: AppState,
    who: Human,
    (id, eid): (String, i64),
    body: JsonBody<Emoji>,
    present: bool,
) -> ApiResult<Json<Value>> {
    transact(&state, TransactionBehavior::Immediate, move |tx| {
        hidden_before(tx, &id, &who.id)?;
        let Json(body) = body.map_err(ApiError::from)?;
        if !valid_emoji(&body.emoji) {
            return Err(ApiError::bad_request("emoji must be one of 👍 👀 ✅ 🙏 ❤️ 🎉").into());
        }
        if visible_event(tx, &id, &who.id, eid)?.is_none() {
            return Err(no_message().into());
        }
        let sql = if present {
            "INSERT INTO dm_reactions (dm_id, event_id, user_id, emoji) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (event_id, user_id, emoji) DO NOTHING"
        } else {
            "DELETE FROM dm_reactions WHERE dm_id = ?1 AND event_id = ?2 AND user_id = ?3 AND emoji = ?4"
        };
        let changed = tx.execute(sql, params![id, eid, who.id, body.emoji])? != 0;
        Ok(Json(json!({ "changed": changed })))
    })
    .await
}

async fn put_reaction(
    State(state): State<AppState>,
    who: Human,
    Path(path): Path<(String, i64)>,
    body: JsonBody<Emoji>,
) -> ApiResult<Json<Value>> {
    react(state, who, path, body, true).await
}

async fn delete_reaction(
    State(state): State<AppState>,
    who: Human,
    Path(path): Path<(String, i64)>,
    body: JsonBody<Emoji>,
) -> ApiResult<Json<Value>> {
    react(state, who, path, body, false).await
}

pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/dms", get(list).post(create))
        .route("/v1/dms/{id}", delete(hide_for_me))
        .route("/v1/dms/{id}/events", get(events))
        .route("/v1/dms/{id}/messages", axum::routing::post(post))
        .route("/v1/dms/{id}/read", put(read))
        .route(
            "/v1/dms/{id}/events/{eid}/reactions",
            put(put_reaction).delete(delete_reaction),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    #[test]
    fn additive_schema_reopens_and_purge_removes_every_dm_row() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("metro.sqlite3");
        let db = Db::open(&file).unwrap();
        for id in ["a", "b"] {
            db.resolve_display_name(id, id).unwrap();
        }
        let (id, event_id) = {
            let mut conn = db.lock();
            let tx = conn.transaction().unwrap();
            let (id, _) = get_or_create(&tx, "a", "b").unwrap();
            let who = Human {
                id: "a".into(),
                name: "A".into(),
            };
            let event = append_message(&tx, &id, &who, "hi", &MessageMeta::default()).unwrap();
            let event_id = event.event.id;
            tx.execute(
                "INSERT INTO dm_reactions (dm_id, event_id, user_id, emoji) VALUES (?1, ?2, 'b', '👍')",
                params![id, event_id],
            ).unwrap();
            hide(&tx, &id, "a").unwrap();
            tx.commit().unwrap();
            (id, event_id)
        };
        drop(db);
        let db = Db::open(&file).unwrap();
        let mut conn = db.lock();
        let tx = conn.transaction().unwrap();
        assert_eq!(hidden_before(&tx, &id, "a").unwrap(), event_id);
        assert!(visible_event(&tx, &id, "b", event_id).unwrap().is_some());
        hide(&tx, &id, "b").unwrap();
        for table in ["dms", "dm_state", "dm_events", "dm_reactions"] {
            let count: u32 = tx
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(count, 0, "{table}");
        }
        let (next, _) = get_or_create(&tx, "a", "b").unwrap();
        let who = Human {
            id: "b".into(),
            name: "B".into(),
        };
        for _ in 0..100 {
            let e = append_message(&tx, &next, &who, "new", &MessageMeta::default()).unwrap();
            assert!(e.event.id > event_id);
        }
        assert_eq!(conversations(&tx, "a", None).unwrap()[0].unread, 99);
        assert_eq!(conversations(&tx, "b", None).unwrap()[0].unread, 0);
        tx.commit().unwrap();
    }
}
