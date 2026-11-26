//! Decisions made after a message is stored.
//!
//! With `"defer_decisions": true`, `POST /v1/spaces/{sid}/rooms/{rid}/messages`
//! stores and returns the message at once (`"decisions_pending": true`) when
//! it would otherwise wait on System One: the sender's opt-in
//! classification, the classifier's pick for the generic `@agent`, or
//! use-case routing. A personal Chat message likewise returns before its
//! turn is routed and started.
//!
//! **Durable, and claimed.** Such a message gets a `pending_decisions`
//! row in the same transaction that stores it, claimed by this process
//! ([`Pending::epoch`]). The decision is never stored on the message, so it
//! can be made again. Once made, one transaction replaces the row with one
//! `pending_targets` row per agent run it starts, plus the room errors it
//! posts. Each run's row is claimed (deleted) in the transaction that
//! registers the run and writes its `started` marker, which also checks
//! that the message still exists: a run starts at most once, never for a
//! deleted message, and a crash after that leaves a `started` marker that
//! the next startup closes as `interrupted`, visibly. A Chat turn's row is
//! settled with its `started` marker the same way.
//!
//! **Recovery.** At startup, before the listener accepts anything,
//! [`resume`] claims every row another process left (in one transaction:
//! `UPDATE ... RETURNING`) and enqueues it, in message order; a Chat turn
//! gets its place in its sender's line then. Only a row's claimant ever
//! dispatches it, so nothing runs twice. Work is reloaded from the live
//! message after every wait and before every System One call; a message
//! deleted meanwhile goes no further and reaches no model.
//!
//! **Stop.** A Chat message's POST returns its `turn_id`. Stopping it before
//! it starts (`POST /v1/chat/turns/{id}/stop`) is saved in its pending row
//! before the call answers, so neither this process nor the next starts it.
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
use rusqlite::{Connection, OptionalExtension, params};
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
         );
         -- The decision for a Space message was made: one row per agent run
         -- still to start, settled with that run's `started` marker.
         CREATE TABLE IF NOT EXISTS pending_targets (
             event_id     INTEGER NOT NULL,
             agent_id     TEXT NOT NULL,
             position     INTEGER NOT NULL,
             space_id     TEXT NOT NULL,
             room_id      TEXT NOT NULL,
             user_id      TEXT NOT NULL,
             pick         TEXT,
             addressed_by TEXT NOT NULL,
             choice       TEXT,
             claimed_by   TEXT,
             created_at   INTEGER NOT NULL,
             PRIMARY KEY (event_id, agent_id)
         );",
    )?;
    // Additive, for databases from builds before claims and Stop.
    for (column, decl) in [
        ("claimed_by", "TEXT"),
        ("cancelled", "INTEGER NOT NULL DEFAULT 0"),
        ("turn_id", "TEXT"),
    ] {
        let has: bool = conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM pragma_table_info('pending_decisions') WHERE name = ?1)",
            [column],
            |r| r.get(0),
        )?;
        if !has {
            conn.execute_batch(&format!(
                "ALTER TABLE pending_decisions ADD COLUMN {column} {decl}"
            ))?;
        }
    }
    Ok(())
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
    /// The process that makes it ([`Pending::epoch`]): whoever stores it.
    pub claimed_by: String,
    /// A Chat turn's id, known to its sender before it starts (for Stop).
    pub turn_id: Option<String>,
}

/// Write `p` for event `event_id`, inside the transaction that stores it.
pub fn insert_in(conn: &Connection, p: &NewPending, event_id: i64) -> rusqlite::Result<()> {
    let (space, room) = match &p.place {
        Some((s, r)) => (Some(s.as_str()), Some(r.as_str())),
        None => (None, None),
    };
    conn.execute(
        "INSERT INTO pending_decisions
             (scope, event_id, user_id, space_id, room_id, payload, created_at, claimed_by, turn_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            p.scope.as_str(),
            event_id,
            p.user_id,
            space,
            room,
            p.payload.to_string(),
            crate::db::now_ms(),
            p.claimed_by,
            p.turn_id,
        ],
    )?;
    Ok(())
}

/// [`insert_in`] on its own, for an event stored already (tools and tests).
pub fn insert(db: &crate::db::Db, p: &NewPending, event_id: i64) -> anyhow::Result<()> {
    Ok(insert_in(&db.lock(), p, event_id)?)
}

/// How many decisions and planned runs are pending.
pub fn count(db: &crate::db::Db) -> anyhow::Result<i64> {
    Ok(db.lock().query_row(
        "SELECT (SELECT COUNT(*) FROM pending_decisions) + (SELECT COUNT(*) FROM pending_targets)",
        [],
        |r| r.get(0),
    )?)
}

/// Drop everything pending for event `event_id` (its decision, or the runs
/// it planned), inside the caller's transaction.
pub fn forget_in(conn: &Connection, scope: Scope, event_id: i64) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM pending_decisions WHERE scope = ?1 AND event_id = ?2",
        params![scope.as_str(), event_id],
    )?;
    if scope == Scope::Space {
        conn.execute(
            "DELETE FROM pending_targets WHERE event_id = ?1",
            [event_id],
        )?;
    }
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

/// Drop one planned run that won't start (refused, or its agent gone).
pub async fn forget_target(state: &AppState, event_id: i64, agent_id: &str) {
    let agent = agent_id.to_owned();
    if let Err(e) = state
        .db
        .call(move |db| {
            db.lock().execute(
                "DELETE FROM pending_targets WHERE event_id = ?1 AND agent_id = ?2",
                params![event_id, agent],
            )?;
            Ok(())
        })
        .await
    {
        tracing::error!(error = %format!("{e:#}"), "clearing a planned run");
    }
}

/// One run a Space decision planned, as stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedTarget {
    pub agent_id: String,
    pub pick: Option<String>,
    pub addressed_by: crate::model_routing::AddressedBy,
}

/// Turn the claimed decision of Space message `event_id` into its planned
/// runs, with the room errors the decision posts, in one transaction. False
/// (nothing written) when this process no longer holds the claim or the
/// message is gone.
#[allow(clippy::too_many_arguments)]
pub fn plan_in(
    db: &crate::db::Db,
    epoch: &str,
    space: &str,
    room: &str,
    user: &str,
    event_id: i64,
    targets: &[PlannedTarget],
    choice: Option<&serde_json::Value>,
    errors: &[String],
) -> anyhow::Result<bool> {
    let mut conn = db.lock();
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let held = tx.execute(
        "DELETE FROM pending_decisions WHERE scope = 'space' AND event_id = ?1 AND claimed_by = ?2",
        params![event_id, epoch],
    )? == 1;
    let present = crate::replies::event_in(&tx, space, room, event_id)?
        .is_some_and(|e| e.kind == crate::db::kind::MESSAGE);
    if !held || !present {
        return Ok(false);
    }
    let now = crate::db::now_ms();
    for (position, t) in targets.iter().enumerate() {
        tx.execute(
            "INSERT OR IGNORE INTO pending_targets (event_id, agent_id, position, space_id, room_id,
                 user_id, pick, addressed_by, choice, claimed_by, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                event_id,
                t.agent_id,
                position as i64,
                space,
                room,
                user,
                t.pick,
                serde_json::to_value(t.addressed_by)?.as_str(),
                choice.map(|c| c.to_string()),
                epoch,
                now
            ],
        )?;
    }
    for reason in errors {
        crate::db::Db::append_event_in(
            &tx,
            space,
            room,
            crate::db::kind::ERROR,
            &crate::db::Author::server(crate::db::actor::SYSTEM),
            reason,
            &crate::db::Addressed::room(),
            None,
        )?;
    }
    tx.commit()?;
    Ok(true)
}

/// Claim one planned run inside the transaction that registers it and
/// writes its `started` marker: true only for the claimant.
pub fn claim_target_in(
    conn: &Connection,
    epoch: &str,
    event_id: i64,
    agent_id: &str,
) -> rusqlite::Result<bool> {
    Ok(conn.execute(
        "DELETE FROM pending_targets WHERE event_id = ?1 AND agent_id = ?2 AND claimed_by = ?3",
        params![event_id, agent_id, epoch],
    )? == 1)
}

/// Inside the transaction that writes a Chat turn's `started` marker:
/// settle its pending decision. False when another process holds it or the
/// sender stopped it (then it's dropped); true when there was none.
pub fn settle_chat_in(conn: &Connection, epoch: &str, event_id: i64) -> rusqlite::Result<bool> {
    let row: Option<(Option<String>, i64)> = conn
        .query_row(
            "SELECT claimed_by, cancelled FROM pending_decisions WHERE scope = 'personal' AND event_id = ?1",
            [event_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(match row {
        None => true,
        // Someone else's to settle.
        Some((claimed, 0)) if claimed.as_deref() != Some(epoch) => false,
        Some((_, cancelled)) => {
            forget_in(conn, Scope::Personal, event_id)?;
            cancelled == 0
        }
    })
}

/// The sender stopped Chat turn `turn_id` before it started: remembered
/// with its pending decision, so a restart doesn't start it either.
pub fn cancel_turn(db: &crate::db::Db, user: &str, turn_id: &str) -> anyhow::Result<bool> {
    Ok(db.lock().execute(
        "UPDATE pending_decisions SET cancelled = 1
         WHERE scope = 'personal' AND user_id = ?1 AND turn_id = ?2",
        params![user, turn_id],
    )? == 1)
}

/// Whether Chat message `event_id`'s turn was stopped before it started.
pub fn cancelled(db: &crate::db::Db, event_id: i64) -> anyhow::Result<bool> {
    Ok(db
        .lock()
        .query_row(
            "SELECT cancelled FROM pending_decisions WHERE scope = 'personal' AND event_id = ?1",
            [event_id],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        == Some(1))
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
    pub turn_id: Option<String>,
}

/// Planned runs of one Space message, as stored.
#[derive(Clone, Debug)]
pub struct StoredTargets {
    pub event_id: i64,
    pub space_id: String,
    pub room_id: String,
    pub user_id: String,
    pub choice: Option<serde_json::Value>,
    pub targets: Vec<PlannedTarget>,
}

/// Everything left pending by another process, claimed for this one
/// (`epoch`) in one transaction. Cancelled Chat turns are dropped.
fn claim_all(
    conn: &mut Connection,
    epoch: &str,
) -> rusqlite::Result<(Vec<Stored>, Vec<StoredTargets>)> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    tx.execute("DELETE FROM pending_decisions WHERE cancelled = 1", [])?;
    let decisions = {
        let mut stmt = tx.prepare(
            "UPDATE pending_decisions SET claimed_by = ?1
             WHERE claimed_by IS NULL OR claimed_by <> ?1
             RETURNING scope, event_id, user_id, space_id, room_id, payload, turn_id, created_at",
        )?;
        let mut rows = stmt
            .query_map([epoch], |r| {
                let scope: String = r.get(0)?;
                let payload: String = r.get(5)?;
                Ok((
                    r.get::<_, i64>(7)?,
                    Stored {
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
                        turn_id: r.get(6)?,
                    },
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.sort_by_key(|(at, s)| (*at, s.event_id));
        rows.into_iter().map(|(_, s)| s).collect::<Vec<_>>()
    };
    let targets = {
        let mut stmt = tx.prepare(
            "UPDATE pending_targets SET claimed_by = ?1
             WHERE claimed_by IS NULL OR claimed_by <> ?1
             RETURNING event_id, space_id, room_id, user_id, choice, agent_id, pick, addressed_by, position",
        )?;
        let mut rows = stmt
            .query_map([epoch], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, Option<String>>(6)?,
                    r.get::<_, String>(7)?,
                    r.get::<_, i64>(8)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.sort_by_key(|r| (r.0, r.8));
        let mut grouped: Vec<StoredTargets> = Vec::new();
        for (event_id, space_id, room_id, user_id, choice, agent_id, pick, by, _) in rows {
            let addressed_by = serde_json::from_value(serde_json::Value::String(by))
                .unwrap_or(crate::model_routing::AddressedBy::Explicit);
            let target = PlannedTarget {
                agent_id,
                pick,
                addressed_by,
            };
            match grouped.last_mut() {
                Some(g) if g.event_id == event_id => g.targets.push(target),
                _ => grouped.push(StoredTargets {
                    event_id,
                    space_id,
                    room_id,
                    user_id,
                    choice: choice.and_then(|c| serde_json::from_str(&c).ok()),
                    targets: vec![target],
                }),
            }
        }
        grouped
    };
    tx.commit()?;
    Ok((decisions, targets))
}

/// Claim and enqueue every decision and planned run left pending by an
/// earlier run of the server, in the order their messages were stored.
/// Call once at startup, before accepting requests: every claim is made
/// (and every Chat turn has its place in line) before any new message can
/// be stored, and only the claimant ever dispatches.
pub async fn resume(state: AppState) -> anyhow::Result<usize> {
    let epoch = state.decisions.epoch().to_owned();
    let (decisions, targets) = state
        .db
        .call(move |db| Ok(claim_all(&mut db.lock(), &epoch)?))
        .await?;
    let n = decisions.len() + targets.len();
    if n > 0 {
        tracing::info!(
            pending = n,
            "resuming System One decisions pending at shutdown"
        );
    }
    for planned in targets {
        let state = state.clone();
        tokio::spawn(async move { crate::messages::resume_targets(state, planned).await });
    }
    for row in decisions {
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
    /// This process's claim on pending decisions.
    epoch: String,
    /// Held while a Chat message is stored and given its place in line, so
    /// line order is storage order.
    chat_insert: Mutex<()>,
    entries: Arc<Mutex<Entries>>,
    all: Arc<Semaphore>,
    users: Mutex<HashMap<String, Arc<Semaphore>>>,
    lines: Arc<Mutex<HashMap<String, Line>>>,
}

impl Default for Pending {
    fn default() -> Self {
        Self {
            epoch: format!("epoch_{}", uuid::Uuid::new_v4().simple()),
            chat_insert: Mutex::default(),
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

    /// This process's claim token.
    pub fn epoch(&self) -> &str {
        &self.epoch
    }

    /// Store a Chat message with `store` and give it the next place in
    /// `user`'s line, both under one lock: line order is storage order.
    pub fn in_chat_order<T>(&self, user: &str, store: impl FnOnce() -> T) -> (T, Ticket) {
        let _held = self.chat_insert.lock().unwrap_or_else(|e| e.into_inner());
        let ticket = self.chat_ticket(user);
        (store(), ticket)
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

    #[test]
    fn only_the_claimant_starts_a_planned_run_and_recovery_claims_once() {
        let db = crate::db::Db::open_in_memory().unwrap();
        let space = db.create_space("alice", "S", 10).unwrap();
        let author = crate::db::Author {
            id: Some("alice".into()),
            name: "alice".into(),
        };
        let msg = db
            .append_event(
                &space.id,
                "lobby",
                kind::MESSAGE,
                &author,
                "hi",
                &Addressed::room(),
            )
            .unwrap();
        let pending = NewPending {
            scope: Scope::Space,
            user_id: "alice".into(),
            place: Some((space.id.clone(), "lobby".into())),
            payload: serde_json::json!({}),
            claimed_by: "old".into(),
            turn_id: None,
        };
        insert(&db, &pending, msg.id).unwrap();
        let targets = [PlannedTarget {
            agent_id: "ag".into(),
            pick: None,
            addressed_by: crate::model_routing::AddressedBy::Explicit,
        }];
        // Another process can't make a decision it doesn't hold.
        assert!(
            !plan_in(
                &db,
                "new",
                &space.id,
                "lobby",
                "alice",
                msg.id,
                &targets,
                None,
                &[]
            )
            .unwrap()
        );
        assert!(
            plan_in(
                &db,
                "old",
                &space.id,
                "lobby",
                "alice",
                msg.id,
                &targets,
                None,
                &[]
            )
            .unwrap()
        );
        // Recovery claims what "old" left, once.
        let (d, t) = claim_all(&mut db.lock(), "new").unwrap();
        assert!(d.is_empty());
        assert_eq!(t.len(), 1);
        assert!(claim_all(&mut db.lock(), "new").unwrap().1.is_empty());
        assert!(
            !claim_target_in(&db.lock(), "old", msg.id, "ag").unwrap(),
            "the old claimant lost it"
        );
        assert!(claim_target_in(&db.lock(), "new", msg.id, "ag").unwrap());
        assert!(
            !claim_target_in(&db.lock(), "new", msg.id, "ag").unwrap(),
            "started once"
        );
        assert_eq!(count(&db).unwrap(), 0);
    }

    #[test]
    fn an_earlier_pending_table_gains_claims_and_stop() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE pending_decisions (
                 scope TEXT NOT NULL, event_id INTEGER NOT NULL, user_id TEXT NOT NULL,
                 space_id TEXT, room_id TEXT, payload TEXT NOT NULL, created_at INTEGER NOT NULL,
                 PRIMARY KEY (scope, event_id));
             INSERT INTO pending_decisions VALUES ('personal', 7, 'alice', NULL, NULL, '{}', 1);",
        )
        .unwrap();
        init(&conn).unwrap();
        init(&conn).unwrap();
        let (claimed, cancelled): (Option<String>, i64) = conn
            .query_row(
                "SELECT claimed_by, cancelled FROM pending_decisions",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            (claimed, cancelled),
            (None, 0),
            "kept, and claimable at startup"
        );
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
