//! Decisions made after a message is stored.
//!
//! With `"defer_decisions": true`, `POST /v1/spaces/{sid}/rooms/{rid}/messages`
//! stores and returns the message at once (`"decisions_pending": true`) when
//! it would otherwise wait on System One: the sender's opt-in
//! classification, the classifier's pick for the generic `@agent`, or
//! use-case routing. A personal Chat message likewise returns before its
//! turn is routed and started.
//!
//! **Durable.** Such a message gets a `pending_decisions` row in the same
//! transaction that stores it. The decision itself is never stored on the
//! message, so it can be made again: at startup, [`resume`] makes every
//! decision still pending, in message order. The row goes in the same
//! transaction that registers the run it starts (a Space run's acceptance,
//! or a Chat turn's `started` marker), which also checks that the message
//! still exists, so a run starts at most once and never for a deleted
//! message. A decision that starts nothing removes its row when it ends,
//! and deleting the message removes it too.
//!
//! **Bounded.** At most [`AT_ONCE`] decisions run at once on the server,
//! and [`PER_USER`] per person; more wait their turn, none is dropped. A
//! person's Chat turns start in the order their messages were sent.
//!
//! **Sender-only notes.** `POST /v1/spaces/{sid}/rooms/{rid}/messages/{eid}/decisions`
//! (body `{}`) waits for a Space decision and returns the sender-only notes
//! as POST metadata, shaped like the message POST's response. Only the
//! authenticated sender of that message in that room gets them, only while
//! still a member and while the message is still in the transcript, and
//! only once. Notes are kept in memory only, for [`KEEP`] after the
//! decision finished: a restart, a second call, a session of someone else,
//! a lost membership or a decision whose task went away gets 404 (the
//! decision itself still runs). Nothing here is ever an event.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use rusqlite::{Connection, params};
use serde::Serialize;
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore, watch};

use crate::AppState;
use crate::api::{ApiError, ApiResult, db, empty_object, member_of, room_of};
use crate::auth::Human;
use crate::db::Event;
use crate::messages::{Finished, SenderNote};
use crate::model_routing::AddressingNote;

/// How long a finished outcome waits for its sender.
const KEEP: Duration = Duration::from_secs(10 * 60);
/// An unfinished outcome older than this is given up on.
const STALE: Duration = Duration::from_secs(30 * 60);
/// Outcomes held at once; the oldest go first.
const MAX_PENDING: usize = 4096;
/// How long one call waits before answering "still pending".
const WAIT: Duration = Duration::from_secs(90);
/// Deferred decisions running at once on this server.
pub const AT_ONCE: usize = 8;
/// Deferred decisions running at once for one person.
pub const PER_USER: usize = 2;

// ---- Durable pending decisions ----

/// Which event sequence a pending decision's message is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Space,
    Personal,
}

impl Scope {
    fn as_str(self) -> &'static str {
        match self {
            Scope::Space => "space",
            Scope::Personal => "personal",
        }
    }
}

pub fn init(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS pending_decisions (
             scope      TEXT NOT NULL CHECK (scope IN ('space', 'personal')),
             event_id   INTEGER NOT NULL,
             user_id    TEXT NOT NULL,
             space_id   TEXT,
             room_id    TEXT,
             payload    TEXT NOT NULL,
             created_at INTEGER NOT NULL,
             PRIMARY KEY (scope, event_id)
         );",
    )
}

/// A decision to make for a message about to be stored.
#[derive(Clone, Debug)]
pub struct NewPending {
    pub scope: Scope,
    pub user_id: String,
    /// The Space and room of a Space message.
    pub place: Option<(String, String)>,
    /// What the decision needs beyond the stored message: see
    /// [`crate::messages`] and [`crate::api`].
    pub payload: serde_json::Value,
}

/// Write `p` for event `event_id`, inside the transaction that stores it.
pub fn insert_in(conn: &Connection, p: &NewPending, event_id: i64) -> rusqlite::Result<()> {
    let (space, room) = match &p.place {
        Some((s, r)) => (Some(s.as_str()), Some(r.as_str())),
        None => (None, None),
    };
    conn.execute(
        "INSERT INTO pending_decisions (scope, event_id, user_id, space_id, room_id, payload, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            p.scope.as_str(),
            event_id,
            p.user_id,
            space,
            room,
            p.payload.to_string(),
            crate::db::now_ms()
        ],
    )?;
    Ok(())
}

/// [`insert_in`] on its own, for an event stored already (tools and tests).
pub fn insert(db: &crate::db::Db, p: &NewPending, event_id: i64) -> anyhow::Result<()> {
    Ok(insert_in(&db.lock(), p, event_id)?)
}

/// How many decisions are pending.
pub fn count(db: &crate::db::Db) -> anyhow::Result<i64> {
    Ok(db
        .lock()
        .query_row("SELECT COUNT(*) FROM pending_decisions", [], |r| r.get(0))?)
}

/// Drop the pending decision of event `event_id`, if any, inside the
/// caller's transaction.
pub fn forget_in(conn: &Connection, scope: Scope, event_id: i64) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM pending_decisions WHERE scope = ?1 AND event_id = ?2",
        params![scope.as_str(), event_id],
    )?;
    Ok(())
}

/// [`forget_in`] on its own; failures are logged.
pub async fn forget(state: &AppState, scope: Scope, event_id: i64) {
    if let Err(e) = state
        .db
        .call(move |db| Ok(forget_in(&db.lock(), scope, event_id)?))
        .await
    {
        tracing::error!(error = %format!("{e:#}"), "clearing a pending decision");
    }
}

/// One pending decision as stored.
#[derive(Clone, Debug)]
pub struct Stored {
    pub scope: Scope,
    pub event_id: i64,
    pub user_id: String,
    pub space_id: Option<String>,
    pub room_id: Option<String>,
    pub payload: serde_json::Value,
}

fn pending_rows(conn: &Connection) -> rusqlite::Result<Vec<Stored>> {
    let mut stmt = conn.prepare(
        "SELECT scope, event_id, user_id, space_id, room_id, payload FROM pending_decisions
         ORDER BY created_at, event_id",
    )?;
    let rows = stmt.query_map([], |r| {
        let scope: String = r.get(0)?;
        let payload: String = r.get(5)?;
        Ok(Stored {
            scope: if scope == "space" {
                Scope::Space
            } else {
                Scope::Personal
            },
            event_id: r.get(1)?,
            user_id: r.get(2)?,
            space_id: r.get(3)?,
            room_id: r.get(4)?,
            payload: serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null),
        })
    })?;
    rows.collect()
}

/// Make every decision left pending by an earlier run of the server, in
/// the order their messages were stored. Call once at startup.
pub async fn resume(state: AppState) -> anyhow::Result<usize> {
    let rows = state.db.call(|db| Ok(pending_rows(&db.lock())?)).await?;
    let n = rows.len();
    if n > 0 {
        tracing::info!(
            pending = n,
            "resuming System One decisions pending at shutdown"
        );
    }
    for row in rows {
        let state = state.clone();
        match row.scope {
            Scope::Space => {
                tokio::spawn(async move { crate::messages::resume_space(state, row).await });
            }
            // Taken in order now, so the Chat's turns keep their send order.
            Scope::Personal => {
                let ticket = state.decisions.chat_ticket(&row.user_id);
                tokio::spawn(async move { crate::api::resume_chat_turn(state, row, ticket).await });
            }
        }
    }
    Ok(n)
}

// ---- Concurrency and order ----

/// Both permits a deferred decision holds while it calls System One.
pub struct Slot {
    _all: OwnedSemaphorePermit,
    _mine: OwnedSemaphorePermit,
}

struct Line {
    next: u64,
    /// Every ticket below this is done.
    serving: u64,
    /// Tickets done out of order, above `serving`.
    done: std::collections::BTreeSet<u64>,
    notify: Arc<Notify>,
}

/// A place in a person's line of Chat turns, taken when the message is
/// stored. Dropping it (or the [`Turn`] it becomes) lets the next one go.
pub struct Ticket {
    lines: Arc<Mutex<HashMap<String, Line>>>,
    user: String,
    number: u64,
    served: bool,
}

/// The person's Chat turn whose turn it is.
pub struct Turn(#[allow(dead_code)] Ticket);

impl Ticket {
    /// Wait until every earlier ticket of this person is done.
    pub async fn wait(self) -> Turn {
        loop {
            let notify = {
                let lines = self.lines.lock().unwrap_or_else(|e| e.into_inner());
                match lines.get(&self.user) {
                    Some(line) if line.serving < self.number => line.notify.clone(),
                    _ => break,
                }
            };
            let notified = notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let lines = self.lines.lock().unwrap_or_else(|e| e.into_inner());
                if lines
                    .get(&self.user)
                    .is_none_or(|line| line.serving >= self.number)
                {
                    break;
                }
            }
            notified.await;
        }
        Turn(self)
    }
}

impl Drop for Ticket {
    fn drop(&mut self) {
        if self.served {
            return;
        }
        self.served = true;
        let mut lines = self.lines.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(line) = lines.get_mut(&self.user) {
            // One that ends early (a panic, a drop before its turn) is done,
            // but the line moves only past tickets that all are.
            line.done.insert(self.number);
            while line.done.remove(&line.serving) {
                line.serving += 1;
            }
            line.notify.notify_waiters();
            if line.serving >= line.next {
                lines.remove(&self.user);
            }
        }
    }
}

// ---- Sender-only outcomes ----

/// The sender, Space, room and message an outcome belongs to.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Key {
    user: String,
    space: String,
    room: String,
    event: i64,
}

impl Key {
    pub fn new(user: &str, space: &str, room: &str, event: i64) -> Self {
        Self {
            user: user.to_owned(),
            space: space.to_owned(),
            room: room.to_owned(),
            event,
        }
    }
}

/// What the sender gets back.
#[derive(Clone, Debug)]
pub struct Outcome {
    addressing_note: Option<SenderNote>,
    routing_note: Option<AddressingNote>,
    classified: bool,
}

impl From<Finished> for Outcome {
    fn from(f: Finished) -> Self {
        Self {
            addressing_note: f.addressing_note,
            routing_note: f.routing_note,
            classified: f.classified,
        }
    }
}

struct Entry {
    event: Event,
    done: watch::Receiver<Option<Outcome>>,
    sender: Option<watch::Sender<Option<Outcome>>>,
    at: Instant,
    finished: Option<Instant>,
}

impl Entry {
    /// Kept: unfinished and still being worked on, or finished recently.
    fn live(&self, now: Instant) -> bool {
        match self.finished {
            Some(t) => now.duration_since(t) < KEEP,
            None => self.sender.is_some() && now.duration_since(self.at) < STALE,
        }
    }
}

type Entries = HashMap<Key, Entry>;

/// Deferred outcomes by [`Key`], plus the decision slots and Chat lines.
/// Memory only.
pub struct Pending {
    entries: Arc<Mutex<Entries>>,
    all: Arc<Semaphore>,
    users: Mutex<HashMap<String, Arc<Semaphore>>>,
    lines: Arc<Mutex<HashMap<String, Line>>>,
}

impl Default for Pending {
    fn default() -> Self {
        Self {
            entries: Arc::default(),
            all: Arc::new(Semaphore::new(AT_ONCE)),
            users: Mutex::default(),
            lines: Arc::default(),
        }
    }
}

/// Finishes one registered outcome. Dropped unfinished (its task ended or
/// panicked), the outcome is removed and waiters get nothing.
pub struct Finisher {
    entries: Arc<Mutex<Entries>>,
    key: Key,
    done: bool,
}

impl Finisher {
    pub fn finish(mut self, outcome: Outcome) {
        self.done = true;
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = entries.get_mut(&self.key) {
            entry.finished = Some(Instant::now());
            if let Some(tx) = entry.sender.take() {
                tx.send_replace(Some(outcome));
            }
        }
    }
}

impl Drop for Finisher {
    fn drop(&mut self) {
        if !self.done {
            let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
            // Dropping the sender wakes every waiter with nothing.
            entries.remove(&self.key);
        }
    }
}

fn sweep(entries: &mut Entries) {
    let now = Instant::now();
    entries.retain(|_, e| e.live(now));
}

impl Pending {
    /// Hold the outcome of `event`'s decisions for its sender.
    pub fn register(&self, key: Key, event: Event) -> Finisher {
        let (tx, rx) = watch::channel(None);
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        sweep(&mut entries);
        while entries.len() >= MAX_PENDING {
            let Some(oldest) = entries
                .iter()
                .min_by_key(|(_, e)| e.at)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            entries.remove(&oldest);
        }
        entries.insert(
            key.clone(),
            Entry {
                event,
                done: rx,
                sender: Some(tx),
                at: Instant::now(),
                finished: None,
            },
        );
        Finisher {
            entries: self.entries.clone(),
            key,
            done: false,
        }
    }

    fn watch(&self, key: &Key) -> Option<(Event, watch::Receiver<Option<Outcome>>)> {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        sweep(&mut entries);
        entries.get(key).map(|e| (e.event.clone(), e.done.clone()))
    }

    /// The finished outcome, handed out once.
    fn take(&self, key: &Key) -> Option<Outcome> {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        sweep(&mut entries);
        entries.get(key)?.finished?;
        entries.remove(key).and_then(|e| e.done.borrow().clone())
    }

    fn forget(&self, key: &Key) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries.remove(key);
    }

    /// Wait for a decision slot: one of [`AT_ONCE`], and one of `user`'s
    /// [`PER_USER`].
    pub async fn slot(&self, user: &str) -> Slot {
        let mine = {
            let mut users = self.users.lock().unwrap_or_else(|e| e.into_inner());
            // People with no decision running hold no semaphore.
            users.retain(|_, s| Arc::strong_count(s) > 1);
            users
                .entry(user.to_owned())
                .or_insert_with(|| Arc::new(Semaphore::new(PER_USER)))
                .clone()
        };
        let mine = mine.acquire_owned().await.expect("never closed");
        let all = self
            .all
            .clone()
            .acquire_owned()
            .await
            .expect("never closed");
        Slot {
            _all: all,
            _mine: mine,
        }
    }

    /// The next place in `user`'s line of Chat turns.
    pub fn chat_ticket(&self, user: &str) -> Ticket {
        let mut lines = self.lines.lock().unwrap_or_else(|e| e.into_inner());
        let line = lines.entry(user.to_owned()).or_insert_with(|| Line {
            next: 0,
            serving: 0,
            done: Default::default(),
            notify: Arc::new(Notify::new()),
        });
        let number = line.next;
        line.next += 1;
        Ticket {
            lines: self.lines.clone(),
            user: user.to_owned(),
            number,
            served: false,
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.lock().unwrap().len()
    }
}

#[derive(Serialize)]
struct Decided {
    #[serde(flatten)]
    event: Event,
    addressing_note: Option<SenderNote>,
    routing_note: Option<AddressingNote>,
    /// The classifier chose an agent for this unaddressed message.
    classified: bool,
}

fn none_pending() -> ApiError {
    ApiError::not_found("no pending decisions for this message")
}

/// `POST .../messages/{eid}/decisions`: see the module docs. 200 with the
/// outcome; 202 `{"decisions_pending": true}` while still deciding.
pub async fn post_decisions(
    State(state): State<AppState>,
    who: Human,
    Path((sid, rid, eid)): Path<(String, String, i64)>,
    body: axum::body::Bytes,
) -> ApiResult<Response> {
    empty_object(&body)?;
    // First check: an outsider learns nothing, not even whether it waits.
    let space = member_of(&state, &sid, &who).await?;
    let room = room_of(&state, &space, &rid).await?;
    let key = Key::new(&who.id, &space.id, &room.id, eid);
    let Some((event, mut done)) = state.decisions.watch(&key) else {
        return Err(none_pending());
    };
    match tokio::time::timeout(WAIT, done.wait_for(Option::is_some)).await {
        Err(_) => {
            return Ok((
                StatusCode::ACCEPTED,
                Json(serde_json::json!({"decisions_pending": true})),
            )
                .into_response());
        }
        // Its task went away without an outcome.
        Ok(Err(_)) => return Err(none_pending()),
        Ok(Ok(_)) => {}
    }
    // Second check, after the wait (which may have been long): a note is
    // never handed out after its sender left or its message was deleted.
    let still = async {
        member_of(&state, &sid, &who).await?;
        let (space_id, room_id, user) = (space.id.clone(), room.id.clone(), who.id.clone());
        db(&state, move |db| {
            Ok(
                crate::replies::event_in(&db.lock(), &space_id, &room_id, eid)?.is_some_and(|e| {
                    e.kind == crate::db::kind::MESSAGE
                        && e.actor_id.as_deref() == Some(user.as_str())
                }),
            )
        })
        .await
    }
    .await;
    match still {
        Ok(true) => {}
        Ok(false) => {
            state.decisions.forget(&key);
            return Err(none_pending());
        }
        Err(e) => {
            state.decisions.forget(&key);
            return Err(e);
        }
    }
    let Some(outcome) = state.decisions.take(&key) else {
        return Err(none_pending());
    };
    Ok((
        StatusCode::OK,
        Json(Decided {
            event,
            addressing_note: outcome.addressing_note,
            routing_note: outcome.routing_note,
            classified: outcome.classified,
        }),
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Addressed, Author, kind, new_event};

    fn event(id: i64) -> Event {
        new_event(
            id,
            kind::MESSAGE,
            &Author {
                id: Some("alice".into()),
                name: "Alice".into(),
            },
            "hi",
            &Addressed::room(),
            None,
            1_800_000_000_000,
        )
    }

    fn outcome() -> Outcome {
        Outcome {
            addressing_note: None,
            routing_note: None,
            classified: true,
        }
    }

    #[test]
    fn an_outcome_is_handed_out_once_and_only_to_its_key() {
        let pending = Pending::default();
        let key = Key::new("alice", "s", "lobby", 7);
        let finisher = pending.register(key.clone(), event(7));
        assert!(pending.take(&key).is_none(), "nothing before it is decided");
        finisher.finish(outcome());
        for other in [
            Key::new("bob", "s", "lobby", 7),
            Key::new("alice", "t", "lobby", 7),
            Key::new("alice", "s", "other", 7),
            Key::new("alice", "s", "lobby", 8),
        ] {
            assert!(pending.take(&other).is_none());
        }
        assert!(pending.take(&key).unwrap().classified);
        assert!(pending.take(&key).is_none(), "never replayed");
        assert_eq!(pending.len(), 0);
    }

    #[test]
    fn an_outcome_whose_task_went_away_is_removed() {
        let pending = Pending::default();
        let key = Key::new("alice", "s", "lobby", 7);
        let finisher = pending.register(key.clone(), event(7));
        let (_, rx) = pending.watch(&key).unwrap();
        drop(finisher);
        assert!(pending.watch(&key).is_none());
        assert!(rx.has_changed().is_err(), "waiters see the sender gone");
        assert_eq!(pending.len(), 0);
    }

    #[test]
    fn finished_outcomes_expire_and_the_registry_is_bounded() {
        let pending = Pending::default();
        let key = Key::new("alice", "s", "lobby", 1);
        pending.register(key.clone(), event(1)).finish(outcome());
        pending
            .entries
            .lock()
            .unwrap()
            .get_mut(&key)
            .unwrap()
            .finished = Some(Instant::now() - KEEP - Duration::from_secs(1));
        assert!(pending.take(&key).is_none(), "swept on take");
        let mut held = Vec::new();
        for i in 0..(MAX_PENDING as i64 + 10) {
            held.push(pending.register(Key::new("alice", "s", "lobby", i), event(i)));
        }
        assert_eq!(pending.len(), MAX_PENDING);
        // Finishers of evicted entries are harmless.
        drop(held);
        assert_eq!(pending.len(), 0);
    }

    #[tokio::test]
    async fn slots_are_bounded_and_waiters_get_one_later() {
        let pending = Arc::new(Pending::default());
        let a = pending.slot("alice").await;
        let _b = pending.slot("alice").await;
        let third = {
            let p = pending.clone();
            tokio::spawn(async move { p.slot("alice").await })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!third.is_finished(), "a third decision of one person waits");
        let _other = pending.slot("bob").await; // other people aren't held up
        drop(a);
        tokio::time::timeout(Duration::from_secs(2), third)
            .await
            .expect("the waiter gets the freed slot")
            .unwrap();
    }

    #[tokio::test]
    async fn chat_tickets_serve_in_order_even_when_one_is_dropped() {
        let pending = Arc::new(Pending::default());
        let order = Arc::new(Mutex::new(Vec::new()));
        let tickets: Vec<_> = (0..4).map(|_| pending.chat_ticket("alice")).collect();
        let mut tasks = Vec::new();
        for (i, t) in tickets.into_iter().enumerate().rev() {
            let order = order.clone();
            tasks.push(tokio::spawn(async move {
                if i == 1 {
                    return; // dropped before its turn
                }
                let turn = t.wait().await;
                order.lock().unwrap().push(i);
                tokio::time::sleep(Duration::from_millis(10)).await;
                drop(turn);
            }));
        }
        for t in tasks {
            t.await.unwrap();
        }
        assert_eq!(*order.lock().unwrap(), vec![0, 2, 3]);
        assert!(pending.lines.lock().unwrap().is_empty());
    }
}
