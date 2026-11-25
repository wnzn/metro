//! Durable Spaces, memberships, rooms and room events in SQLite.
//!
//! One connection behind a mutex, used from `spawn_blocking`. Event ids come
//! from one `AUTOINCREMENT` sequence, so within a room they are strictly
//! increasing in commit order and work as a polling cursor.
//!
//! The single-room build kept unscoped `rooms` and `events` tables. Their
//! history belongs to nobody in particular, so it is never shown in a Space:
//! a database holding such history is refused at startup (see
//! [`LegacyHistory`]), and the new schema uses different table names so the
//! old tables can never be read by accident.

use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::Context;
use rusqlite::{Connection, ErrorCode, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};

#[path = "reactions.rs"]
pub mod reactions;

pub const LOBBY_ID: &str = "lobby";
pub const LOBBY_NAME: &str = "lobby";
/// 3 added registered runners (`runners`, `space_runners`); a version 2
/// database is upgraded in place, additively.
const SCHEMA_VERSION: i64 = 3;

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Owner,
    Member,
}

impl Role {
    fn as_str(self) -> &'static str {
        match self {
            Role::Owner => "owner",
            Role::Member => "member",
        }
    }
    fn parse(s: &str) -> rusqlite::Result<Self> {
        match s {
            "owner" => Ok(Role::Owner),
            "member" => Ok(Role::Member),
            other => Err(rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                format!("unknown role {other}").into(),
            )),
        }
    }
}

/// A Space as one user sees it.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct SpaceView {
    pub id: String,
    pub name: String,
    /// The owner's short description; empty when none was set.
    pub description: String,
    pub role: Role,
    pub runner_paired: bool,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Member {
    pub user_id: String,
    pub role: Role,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Room {
    pub id: String,
    pub name: String,
    /// The parent room of a thread; absent for top-level rooms.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_room_id: Option<String>,
    /// A reply thread's anchor: the event of the parent room it replies to
    /// (see [`crate::replies`]). Absent for rooms and agent-made threads.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor_event_id: Option<i64>,
}

impl Room {
    /// A reply thread under one message, not a room or an agent's thread.
    pub fn is_reply_thread(&self) -> bool {
        self.anchor_event_id.is_some()
    }
}

/// Per-message metadata, stored in the event's JSON metadata column beside
/// spans (never copied text) and returned flattened on the event. Only
/// `message` events carry it; it is never model history.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageMeta {
    /// The event this message replies to: in the same thread, or the
    /// thread's anchor (see [`crate::replies`]). An id only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<i64>,
    /// Sent on the author's behalf by something other than their own
    /// composer: `"metro"` for their personal Chat's meta-agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<Via>,
    /// A copy of another message, posted by the forwarder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forwarded: Option<Forwarded>,
}

impl MessageMeta {
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }

    /// Each key read on its own: an unreadable key is dropped, never guessed,
    /// and never hides the others.
    fn from_object(m: &serde_json::Map<String, serde_json::Value>) -> Self {
        fn key<T: serde::de::DeserializeOwned>(
            m: &serde_json::Map<String, serde_json::Value>,
            k: &str,
        ) -> Option<T> {
            m.get(k).and_then(|v| T::deserialize(v).ok())
        }
        Self {
            reply_to: key(m, "reply_to"),
            via: key(m, "via"),
            forwarded: key(m, "forwarded"),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Via {
    Metro,
}

/// Where a forwarded message came from, as much as its new readers may know.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Forwarded {
    /// The source author's display name when it was forwarded.
    pub author_name: String,
    pub author_kind: ForwardedAuthor,
    /// The source's server time, RFC 3339 UTC, or `null` when unknown.
    pub sent_at: Option<String>,
    /// Only when the source is in the same Space as the copy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub same_space: Option<SameSpace>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ForwardedAuthor {
    Person,
    Agent,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SameSpace {
    pub room_id: String,
    pub event_id: i64,
}

/// The reply thread of an anchor event, as hydrated on room event pages.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Replies {
    pub room_id: String,
    /// `message` and `agent` events in the thread.
    pub count: u64,
    #[serde(serialize_with = "serialize_event_time")]
    pub last_at: Option<i64>,
    /// Up to three people who wrote in it, most recent first.
    pub participant_ids: Vec<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Event {
    pub id: i64,
    pub kind: String,
    /// Display name, as it was when the event was written.
    pub actor: String,
    /// The stable user id of a person's event; `None` for server actors.
    pub actor_id: Option<String>,
    pub text: String,
    /// Who a message was addressed to, as resolved when it was written:
    /// `[{"kind":"agent"}]`, `[{"kind":"user","id":"bob"}]`, ... Empty for
    /// everything but messages.
    pub mentions: serde_json::Value,
    /// Display ranges indexing `mentions`. Empty is authoritative; absent
    /// leaves the spelling of mentions in older events unspecified.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spans: Option<Vec<crate::mentions::MentionSpan>>,
    /// `room`, `agent`, `users` or `mixed`; `room` for non-messages.
    pub audience: String,
    /// The agent turn or Demo run that wrote this event, when the server
    /// knows it. Absent on people's messages, reactions and older history.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activity: Option<Activity>,
    /// When the server stored this event, in epoch milliseconds (the
    /// `created_at` column). `None` when the stored value is not a usable
    /// time (see [`event_time`]); never filled in with "now". On the wire it
    /// is an RFC 3339 UTC string with millisecond precision and a `Z`
    /// suffix, or `null`. Only the server sets it; ordering stays by `id`.
    #[serde(serialize_with = "serialize_event_time")]
    pub created_at: Option<i64>,
    /// Library files attached to a person's message, as the reader may see
    /// them now (see [`crate::attachments`]). Absent when there are none,
    /// and never filled in for the agent's history.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<crate::attachments::Attachment>,
    /// `reply_to`, `via` and `forwarded` of a message, each absent when unset.
    #[serde(flatten)]
    pub meta: MessageMeta,
    /// On an anchor event in a room page: its reply thread.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replies: Option<Replies>,
}

/// Earliest stored `created_at` taken as a real time: 2000-01-01T00:00:00Z
/// in milliseconds. Metro has always written epoch milliseconds; anything
/// below this (0, negative, or a seconds value from a foreign import) is not
/// a time Metro recorded, so it is reported as unknown rather than guessed.
const EARLIEST_EVENT_MS: i64 = 946_684_800_000;

/// The stored `created_at` milliseconds as a known time, or `None`.
pub fn event_time(ms: i64) -> Option<i64> {
    (ms >= EARLIEST_EVENT_MS && chrono::DateTime::from_timestamp_millis(ms).is_some()).then_some(ms)
}

/// `ms` (epoch milliseconds) as RFC 3339 UTC, e.g. `2026-11-22T03:04:05.123Z`.
pub fn rfc3339_utc(ms: i64) -> Option<String> {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
}

fn serialize_event_time<S: serde::Serializer>(ms: &Option<i64>, s: S) -> Result<S::Ok, S::Error> {
    match ms.and_then(rfc3339_utc) {
        Some(t) => s.serialize_str(&t),
        None => s.serialize_none(),
    }
}

/// One agent turn or Demo run, as tagged on the events it produced.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Activity {
    /// The Space run id (`run_…`), or `chat_…` / `demo_…`; stable for the turn.
    pub id: String,
    /// Who is working, for display: [`Activity::AGENT_NAME`], a thread's
    /// name, or `Demo`. Never the person who started it.
    pub name: String,
    /// [`activity::AGENT`] or [`activity::DEMO`].
    pub source: String,
    /// Only on `activity` lifecycle events: one of the [`activity`] phases.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    /// Only on an agent turn's `started` event: which model answered and
    /// why ([`crate::model_routing`]). No prompts, no credentials.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<crate::model_routing::RouteNote>,
    /// The Space agent working this turn ([`crate::space_agents`]), on
    /// every event of the turn. Absent in personal Chat, Demo and older
    /// history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentBadge>,
    /// Only on a Space turn's `started` event: how its agent was chosen,
    /// e.g. `Picked by classifier: Coder` or `Addressed: @coder`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pick: Option<String>,
    /// On a `page_draft` event only: public card metadata, never the draft body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_draft: Option<crate::pages::store::DraftCard>,
    /// Only on a web tool's `tool_call` / `tool_result` / `error` events:
    /// which tool, the provider that answered and every attempt
    /// ([`crate::web_tools`]). Never a query result or page text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<crate::web_tools::ToolNote>,
}

/// A Space agent as shown on its events: enough to draw its name and
/// avatar (`GET /v1/spaces/{sid}/agents/{id}/avatar` when
/// `avatar_version` is set, else its initial on `color`).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentBadge {
    pub id: String,
    pub handle: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_version: Option<String>,
    pub color: String,
}

impl Activity {
    /// Display name of the built-in agent.
    pub const AGENT_NAME: &'static str = "Metro";
    pub const DEMO_NAME: &'static str = "Demo";

    pub fn agent(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            source: activity::AGENT.to_owned(),
            phase: None,
            route: None,
            agent: None,
            pick: None,
            page_draft: None,
            tool: None,
        }
    }

    /// This activity worked by the Space agent `agent`.
    pub fn with_agent(mut self, agent: AgentBadge) -> Self {
        self.agent = Some(agent);
        self
    }

    /// A personal Chat turn: there is no registered run, so it gets its own id.
    pub fn chat() -> Self {
        Self::agent(
            format!("chat_{}", uuid::Uuid::new_v4().simple()),
            Self::AGENT_NAME,
        )
    }

    pub fn demo() -> Self {
        Self {
            id: format!("demo_{}", uuid::Uuid::new_v4().simple()),
            name: Self::DEMO_NAME.to_owned(),
            source: activity::DEMO.to_owned(),
            phase: None,
            route: None,
            agent: None,
            pick: None,
            page_draft: None,
            tool: None,
        }
    }

    /// This activity on a lifecycle event of `phase`. The route is not
    /// carried over: it belongs on the `started` event only.
    pub fn at(&self, phase: &str) -> Self {
        Self {
            phase: Some(phase.to_owned()),
            route: None,
            pick: None,
            tool: None,
            ..self.clone()
        }
    }

    /// This activity with a web tool's note.
    pub fn with_tool(&self, tool: crate::web_tools::ToolNote) -> Self {
        Self {
            phase: None,
            route: None,
            pick: None,
            tool: Some(tool),
            ..self.clone()
        }
    }

    /// This activity with the route its turn took.
    pub fn with_route(mut self, route: crate::model_routing::RouteNote) -> Self {
        self.route = Some(route);
        self
    }
}

/// The server-written `thread` event a parent run posts in its room when it
/// creates thread `id` named `name`.
pub fn thread_started_text(name: &str, id: &str) -> String {
    format!("Started thread \"{name}\" ({id}).")
}

/// Activity sources and lifecycle phases.
pub mod activity {
    pub const AGENT: &str = "agent";
    pub const DEMO: &str = "demo";
    pub const STARTED: &str = "started";
    pub const COMPLETED: &str = "completed";
    pub const FAILED: &str = "failed";
    pub const CANCELLED: &str = "cancelled";
    /// Written at startup for a turn that was still active when the server stopped.
    pub const INTERRUPTED: &str = "interrupted";
}

/// Recipients of a new event.
#[derive(Clone, Debug)]
pub struct Addressed {
    /// A JSON array of mentions.
    pub mentions: serde_json::Value,
    pub audience: String,
    pub spans: Option<Vec<crate::mentions::MentionSpan>>,
}

impl Addressed {
    /// Not addressed to anyone in particular.
    pub fn room() -> Self {
        Self {
            mentions: serde_json::Value::Array(Vec::new()),
            audience: "room".to_owned(),
            spans: None,
        }
    }
}

/// Event kinds the server writes.
pub mod kind {
    /// A person's message.
    pub const MESSAGE: &str = "message";
    /// The agent's final reply.
    pub const AGENT: &str = "agent";
    /// The agent (or Demo) asked the runner to run a tool.
    pub const TOOL_CALL: &str = "tool_call";
    /// The runner's answer to a tool call.
    pub const TOOL_RESULT: &str = "tool_result";
    /// A Space agent left a page draft for the person who started the run.
    pub const PAGE_DRAFT: &str = "page_draft";
    /// A person started Demo.
    pub const DEMO: &str = "demo";
    /// A failure or notice from the server.
    pub const ERROR: &str = "error";
    /// A thread's result or status, mirrored into its parent room.
    pub const THREAD: &str = "thread";
    /// An agent run changed state without a reply (for example, it was cancelled).
    pub const STATUS: &str = "status";
    /// An agent turn or Demo run started or ended; `activity.phase` says
    /// which, and the text is empty. Never a message, never model history.
    pub const ACTIVITY: &str = "activity";
    /// The summary of the model's reasoning in one model turn of an agent
    /// run, as the provider returned it (bounded). Tagged with the run's
    /// activity; never a message, never model history, never indexed.
    pub const REASONING_SUMMARY: &str = "reasoning_summary";
}

/// Actors the server writes for non-human events.
pub mod actor {
    pub const AGENT: &str = "agent";
    pub const RUNNER: &str = "runner";
    pub const SYSTEM: &str = "system";
}

/// Who wrote an event.
#[derive(Clone, Debug)]
pub struct Author {
    pub id: Option<String>,
    pub name: String,
}

impl Author {
    pub fn server(name: &str) -> Self {
        Self {
            id: None,
            name: name.to_owned(),
        }
    }
}

#[derive(Debug)]
pub enum LimitError {
    Duplicate,
    /// A thread cannot have threads of its own.
    Nested,
    TooMany,
    Other(anyhow::Error),
}

impl From<rusqlite::Error> for LimitError {
    fn from(e: rusqlite::Error) -> Self {
        LimitError::Other(e.into())
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum AddMember {
    Added,
    Already(Role),
}

#[derive(Debug, PartialEq, Eq)]
pub enum RemoveMember {
    Removed,
    NotMember,
    IsOwner,
}

/// The single-room build's history is in this database.
#[derive(Debug)]
pub struct LegacyHistory {
    pub rooms: i64,
    pub events: i64,
}

impl std::fmt::Display for LegacyHistory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "this database holds history from the single-room build ({} rooms, {} events) that \
             belongs to no Space. Metro will not assign it to anyone. Stop the server, keep this \
             file as your backup, and start with METRO_DB pointing at a new, empty file. \
             See docs/spaces.md#upgrading-from-the-single-room-build.",
            self.rooms, self.events
        )
    }
}

impl std::error::Error for LegacyHistory {}

#[derive(Clone)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
}

impl Db {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("opening SQLite database {}", path.display()))?;
        Self::init(conn).with_context(|| format!("METRO_DB={}", path.display()))
    }

    pub fn open_in_memory() -> anyhow::Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> anyhow::Result<Self> {
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        // WAL is not available for in-memory databases; ignore its answer.
        let _: String = conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))?;
        conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA synchronous = NORMAL;")?;
        check_legacy(&conn)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS metro_meta (
                 key   TEXT PRIMARY KEY NOT NULL,
                 value TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS spaces (
                 id                TEXT PRIMARY KEY NOT NULL,
                 name              TEXT NOT NULL,
                 owner_id          TEXT NOT NULL,
                 runner_token_hash BLOB UNIQUE,
                 runner_generation INTEGER NOT NULL DEFAULT 0,
                 runner_drop_before INTEGER NOT NULL DEFAULT 0,
                 created_at        INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS spaces_owner ON spaces (owner_id);
             CREATE TABLE IF NOT EXISTS space_members (
                 space_id   TEXT NOT NULL REFERENCES spaces (id),
                 user_id    TEXT NOT NULL,
                 role       TEXT NOT NULL CHECK (role IN ('owner', 'member')),
                 added_at   INTEGER NOT NULL,
                 PRIMARY KEY (space_id, user_id)
             );
             CREATE INDEX IF NOT EXISTS space_members_user ON space_members (user_id);
             CREATE TABLE IF NOT EXISTS space_rooms (
                 space_id   TEXT NOT NULL REFERENCES spaces (id),
                 id         TEXT NOT NULL,
                 name       TEXT NOT NULL,
                 parent_room_id TEXT,
                 created_at INTEGER NOT NULL,
                 PRIMARY KEY (space_id, id),
                 FOREIGN KEY (space_id, parent_room_id) REFERENCES space_rooms (space_id, id)
             );
             CREATE INDEX IF NOT EXISTS space_rooms_parent ON space_rooms (space_id, parent_room_id);
             CREATE UNIQUE INDEX IF NOT EXISTS space_rooms_name
                 ON space_rooms (space_id, name COLLATE NOCASE);
             CREATE TABLE IF NOT EXISTS space_events (
                 id         INTEGER PRIMARY KEY AUTOINCREMENT,
                 space_id   TEXT NOT NULL,
                 room_id    TEXT NOT NULL,
                 kind       TEXT NOT NULL,
                 actor      TEXT NOT NULL,
                 actor_id   TEXT,
                 text       TEXT NOT NULL,
                 mentions   TEXT NOT NULL DEFAULT '[]',
                 audience   TEXT NOT NULL DEFAULT 'room',
                 created_at INTEGER NOT NULL,
                 FOREIGN KEY (space_id, room_id) REFERENCES space_rooms (space_id, id)
             );
             CREATE INDEX IF NOT EXISTS space_events_room ON space_events (space_id, room_id, id);
             CREATE TABLE IF NOT EXISTS personal_events (
                 id         INTEGER PRIMARY KEY AUTOINCREMENT,
                 user_id    TEXT NOT NULL,
                 kind       TEXT NOT NULL,
                 actor      TEXT NOT NULL,
                 actor_id   TEXT,
                 text       TEXT NOT NULL,
                 mentions   TEXT NOT NULL DEFAULT '[]',
                 audience   TEXT NOT NULL DEFAULT 'room',
                 created_at INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS personal_events_user ON personal_events (user_id, id);",
        )?;
        add_activity_columns(&conn)?;
        add_space_description(&conn)?;
        add_room_anchor(&conn)?;
        reactions::init(&conn)?;
        crate::runners::init(&conn)?;
        add_runner_queue_floor(&conn)?;
        crate::admin_users::init(&conn)?;
        crate::admin_settings::init(&conn)?;
        crate::profiles::init(&conn)?;
        crate::dms::init(&conn)?;
        crate::providers::init(&conn)?;
        crate::device_relay::init(&conn)?;
        crate::web_devices::init(&conn)?;
        crate::model_routing::init(&conn)?;
        crate::search::init(&conn)?;
        crate::schedules::init(&conn)?;
        crate::library::init(&conn)?;
        // References library_assets; its links go when a file is deleted.
        crate::attachments::init(&conn)?;
        // References library_assets, so it comes after it.
        crate::library_search::init(&conn)?;
        crate::monitoring::init(&conn)?;
        // After spaces, members and model routing settings exist.
        crate::space_agents::init(&conn)?;
        crate::space_prefs::init(&conn)?;
        // After spaces (Space pages reference them).
        crate::pages::init(&conn)?;
        let version: Option<String> = conn
            .query_row(
                "SELECT value FROM metro_meta WHERE key = 'schema_version'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        match version {
            None => {
                conn.execute(
                    "INSERT INTO metro_meta (key, value) VALUES ('schema_version', ?1)",
                    [SCHEMA_VERSION.to_string()],
                )?;
            }
            Some(v) if v == SCHEMA_VERSION.to_string() => {}
            // The new tables were created above; nothing existing changes.
            Some(v) if v == "2" => {
                conn.execute(
                    "UPDATE metro_meta SET value = ?1 WHERE key = 'schema_version'",
                    [SCHEMA_VERSION.to_string()],
                )?;
                tracing::info!("database schema upgraded from version 2 to {SCHEMA_VERSION}");
            }
            Some(v) => anyhow::bail!(
                "database schema version {v} is not supported by this server (expects {SCHEMA_VERSION})"
            ),
        }
        interrupt_orphaned_activity(&conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// The connection, for crate modules that run their own statements or
    /// transactions. Hold it only inside a [`Db::call`] closure.
    pub(crate) fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Run `f` on the connection off the async executor.
    pub async fn call<T, F>(&self, f: F) -> anyhow::Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&Db) -> anyhow::Result<T> + Send + 'static,
    {
        let db = self.clone();
        tokio::task::spawn_blocking(move || f(&db))
            .await
            .context("database task panicked")?
    }

    /// Bind this database to the identity namespace whose user ids it
    /// stores, or refuse when it already belongs to another one.
    pub fn bind_identity_namespace(&self, namespace: &str) -> anyhow::Result<()> {
        let conn = self.lock();
        let stored: Option<String> = conn
            .query_row(
                "SELECT value FROM metro_meta WHERE key = 'identity_namespace'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        match stored {
            None => {
                conn.execute(
                    "INSERT INTO metro_meta (key, value) VALUES ('identity_namespace', ?1)",
                    [namespace],
                )?;
                Ok(())
            }
            Some(stored) if stored == namespace => Ok(()),
            Some(stored) => anyhow::bail!(
                "this database stores user ids from identity namespace `{stored}`, but the server is \
                 configured for `{namespace}`. Ids from different providers must never be mixed. Point \
                 METRO_DB at a new file, or restore the previous auth configuration. See docs/spaces.md."
            ),
        }
    }

    // ---- Spaces and members ----

    pub fn create_space(
        &self,
        owner_id: &str,
        name: &str,
        max_owned: usize,
    ) -> Result<SpaceView, LimitError> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let owned: i64 = tx.query_row(
            "SELECT COUNT(*) FROM spaces WHERE owner_id = ?1",
            [owner_id],
            |r| r.get(0),
        )?;
        if usize::try_from(owned).unwrap_or(usize::MAX) >= max_owned {
            return Err(LimitError::TooMany);
        }
        let id = format!("s-{}", short_id());
        let now = now_ms();
        tx.execute(
            "INSERT INTO spaces (id, name, owner_id, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![id, name, owner_id, now],
        )?;
        tx.execute(
            "INSERT INTO space_members (space_id, user_id, role, added_at) VALUES (?1, ?2, 'owner', ?3)",
            params![id, owner_id, now],
        )?;
        insert_room(&tx, &id, LOBBY_ID, LOBBY_NAME)?;
        crate::space_agents::seed_default(&tx, &id)?;
        tx.commit()?;
        Ok(SpaceView {
            id,
            name: name.to_owned(),
            description: String::new(),
            role: Role::Owner,
            runner_paired: false,
        })
    }

    /// The Spaces `user_id` belongs to.
    pub fn list_spaces(&self, user_id: &str) -> anyhow::Result<Vec<SpaceView>> {
        let conn = self.lock();
        let mut stmt = conn.prepare_cached(
            "SELECT s.id, s.name, m.role,
                    (s.runner_token_hash IS NOT NULL
                     OR EXISTS (SELECT 1 FROM space_runners a WHERE a.space_id = s.id))
             , s.description
             FROM space_members m JOIN spaces s ON s.id = m.space_id
             WHERE m.user_id = ?1 ORDER BY s.created_at, s.rowid",
        )?;
        let rows = stmt.query_map([user_id], row_space)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// `space_id` as `user_id` sees it, or `None` when they are not a member
    /// (or it does not exist; the two are deliberately indistinguishable).
    pub fn membership(&self, space_id: &str, user_id: &str) -> anyhow::Result<Option<SpaceView>> {
        let conn = self.lock();
        Ok(conn
            .query_row(
                "SELECT s.id, s.name, m.role,
                    (s.runner_token_hash IS NOT NULL
                     OR EXISTS (SELECT 1 FROM space_runners a WHERE a.space_id = s.id))
                 , s.description
                 FROM space_members m JOIN spaces s ON s.id = m.space_id
                 WHERE m.space_id = ?1 AND m.user_id = ?2",
                params![space_id, user_id],
                row_space,
            )
            .optional()?)
    }

    pub fn list_members(&self, space_id: &str) -> anyhow::Result<Vec<Member>> {
        let conn = self.lock();
        let mut stmt = conn.prepare_cached(
            "SELECT user_id, role FROM space_members WHERE space_id = ?1
             ORDER BY role = 'member', added_at, rowid",
        )?;
        let rows = stmt.query_map([space_id], |r| {
            Ok(Member {
                user_id: r.get(0)?,
                role: Role::parse(&r.get::<_, String>(1)?)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn add_member(
        &self,
        space_id: &str,
        user_id: &str,
        max_members: usize,
    ) -> Result<AddMember, LimitError> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let existing: Option<String> = tx
            .query_row(
                "SELECT role FROM space_members WHERE space_id = ?1 AND user_id = ?2",
                params![space_id, user_id],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(role) = existing {
            return Ok(AddMember::Already(Role::parse(&role)?));
        }
        let count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM space_members WHERE space_id = ?1",
            [space_id],
            |r| r.get(0),
        )?;
        if usize::try_from(count).unwrap_or(usize::MAX) >= max_members {
            return Err(LimitError::TooMany);
        }
        tx.execute(
            "INSERT INTO space_members (space_id, user_id, role, added_at) VALUES (?1, ?2, ?3, ?4)",
            params![space_id, user_id, Role::Member.as_str(), now_ms()],
        )?;
        tx.commit()?;
        Ok(AddMember::Added)
    }

    pub fn remove_member(&self, space_id: &str, user_id: &str) -> anyhow::Result<RemoveMember> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let role: Option<String> = tx
            .query_row(
                "SELECT role FROM space_members WHERE space_id = ?1 AND user_id = ?2",
                params![space_id, user_id],
                |r| r.get(0),
            )
            .optional()?;
        let result = match role.as_deref().map(Role::parse).transpose()? {
            None => RemoveMember::NotMember,
            Some(Role::Owner) => RemoveMember::IsOwner,
            Some(Role::Member) => {
                tx.execute(
                    "DELETE FROM space_members WHERE space_id = ?1 AND user_id = ?2",
                    params![space_id, user_id],
                )?;
                // Their own library search index of this Space goes in the
                // same transaction; the Space's files and other members'
                // index rows stay.
                crate::library_search::hooks::member_removed(&tx, space_id, user_id)?;
                // Their per-Space preferences go with their membership.
                crate::space_prefs::member_removed(&tx, space_id, user_id)?;
                // Their page preferences, confirmations and drafts there too.
                crate::pages::hooks::member_removed(&tx, space_id, user_id)?;
                RemoveMember::Removed
            }
        };
        tx.commit()?;
        Ok(result)
    }

    // ---- Runner credentials ----

    /// Store the hash of a new dedicated runner token for `space_id`,
    /// replacing any previous one, and return the new runner generation.
    /// `None` when a registered runner is attached: a dedicated token can
    /// never replace or rotate it.
    pub fn set_runner_token(&self, space_id: &str, hash: &[u8]) -> anyhow::Result<Option<u64>> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let attached: bool = tx.query_row(
            "SELECT EXISTS (SELECT 1 FROM space_runners WHERE space_id = ?1)",
            [space_id],
            |r| r.get(0),
        )?;
        if attached {
            return Ok(None);
        }
        let generation: i64 = tx.query_row(
            "UPDATE spaces SET runner_token_hash = ?2,
                 runner_drop_before = CASE WHEN runner_token_hash IS NULL THEN runner_generation + 1 ELSE runner_drop_before END,
                 runner_generation = runner_generation + 1
             WHERE id = ?1 RETURNING runner_generation",
            params![space_id, hash],
            |r| r.get(0),
        )?;
        tx.commit()?;
        Ok(Some(generation as u64))
    }

    /// Remove `space_id`'s dedicated runner token and return the new
    /// generation. `None` when a registered runner is attached instead
    /// (detach it with [`Db::detach_runner`]).
    pub fn clear_runner_token(&self, space_id: &str) -> anyhow::Result<Option<u64>> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let attached: bool = tx.query_row(
            "SELECT EXISTS (SELECT 1 FROM space_runners WHERE space_id = ?1)",
            [space_id],
            |r| r.get(0),
        )?;
        if attached {
            return Ok(None);
        }
        let generation: i64 = tx.query_row(
            "UPDATE spaces SET runner_token_hash = NULL, runner_drop_before = runner_generation + 1,
                 runner_generation = runner_generation + 1
             WHERE id = ?1 RETURNING runner_generation",
            [space_id],
            |r| r.get(0),
        )?;
        tx.commit()?;
        Ok(Some(generation as u64))
    }

    /// The Space and runner generation a dedicated runner token hash belongs to.
    pub fn runner_by_token_hash(&self, hash: &[u8]) -> anyhow::Result<Option<(String, u64)>> {
        let conn = self.lock();
        Ok(conn
            .query_row(
                "SELECT id, runner_generation FROM spaces WHERE runner_token_hash = ?1",
                [hash],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64)),
            )
            .optional()?)
    }

    /// Whether `space_id` has a runner: a dedicated token or an attached
    /// registered runner.
    pub fn runner_paired(&self, space_id: &str) -> anyhow::Result<bool> {
        let conn = self.lock();
        Ok(conn
            .query_row(
                "SELECT runner_token_hash IS NOT NULL
                        OR EXISTS (SELECT 1 FROM space_runners WHERE space_id = spaces.id)
                 FROM spaces WHERE id = ?1",
                [space_id],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(false))
    }

    /// `space_id`'s current runner generation. It changes whenever who may
    /// serve the Space changes: dedicated token issued, rotated or revoked;
    /// registered runner attached, detached, reassigned, rotated or deleted.
    pub fn space_runner_generation(&self, space_id: &str) -> anyhow::Result<Option<u64>> {
        Ok(read_runner_queue_version(&self.lock(), space_id)?.map(|v| v.generation))
    }

    /// Bind a new job to the current committed assignment and insert it
    /// before another connection can change that assignment.
    pub(crate) fn with_runner_queue_version<T>(
        &self,
        space_id: &str,
        action: impl FnOnce(crate::jobs::RunnerVersion) -> T,
    ) -> anyhow::Result<Option<T>> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        Ok(read_runner_queue_version(&tx, space_id)?.map(action))
    }

    #[cfg(test)]
    pub(crate) fn runner_queue_version(
        &self,
        space_id: &str,
    ) -> anyhow::Result<Option<crate::jobs::RunnerVersion>> {
        Ok(read_runner_queue_version(&self.lock(), space_id)?)
    }

    /// Revalidate an authenticated runner's exact generations just before
    /// accessing in-memory jobs. The transaction keeps assignment
    /// writes out until this synchronous queue action is finished.
    pub(crate) fn with_runner_queue_access<T>(
        &self,
        authenticated: &[(String, u64)],
        action: impl FnOnce(&[(String, crate::jobs::RunnerVersion)]) -> T,
    ) -> anyhow::Result<T> {
        let mut conn = self.lock();
        if authenticated.is_empty() {
            return Ok(action(&[]));
        }
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let mut verified = Vec::with_capacity(authenticated.len());
        for (sid, generation) in authenticated {
            if let Some(version) = read_runner_queue_version(&tx, sid)?
                && version.generation == *generation
            {
                verified.push((sid.clone(), version));
            }
        }
        Ok(action(&verified))
    }

    // ---- Rooms and events ----

    pub fn list_rooms(&self, space_id: &str) -> anyhow::Result<Vec<Room>> {
        let conn = self.lock();
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT {ROOM_COLUMNS} FROM space_rooms WHERE space_id = ?1 ORDER BY created_at, rowid"
        ))?;
        let rows = stmt.query_map([space_id], row_room)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn get_room(&self, space_id: &str, room_id: &str) -> anyhow::Result<Option<Room>> {
        Ok(Self::room_in(&self.lock(), space_id, room_id)?)
    }

    pub(crate) fn room_in(
        conn: &Connection,
        space_id: &str,
        room_id: &str,
    ) -> rusqlite::Result<Option<Room>> {
        conn.query_row(
            &format!("SELECT {ROOM_COLUMNS} FROM space_rooms WHERE space_id = ?1 AND id = ?2"),
            params![space_id, room_id],
            row_room,
        )
        .optional()
    }

    /// Create a top-level room, or with `parent` a thread under that
    /// top-level room. `max_rooms` bounds the Space, `max_children` the
    /// parent; reply threads count toward neither (see [`crate::replies`]).
    /// Names shaped like a reply thread's are taken.
    pub fn create_room(
        &self,
        space_id: &str,
        name: &str,
        parent: Option<&str>,
        max_rooms: usize,
        max_children: usize,
    ) -> Result<Room, LimitError> {
        if crate::replies::is_reply_thread_name(name) {
            return Err(LimitError::Duplicate);
        }
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM space_rooms WHERE space_id = ?1 AND anchor_event_id IS NULL",
            [space_id],
            |r| r.get(0),
        )?;
        if usize::try_from(count).unwrap_or(usize::MAX) >= max_rooms {
            return Err(LimitError::TooMany);
        }
        if let Some(parent) = parent {
            let grandparent: Option<Option<String>> = tx
                .query_row(
                    "SELECT parent_room_id FROM space_rooms WHERE space_id = ?1 AND id = ?2",
                    params![space_id, parent],
                    |r| r.get(0),
                )
                .optional()?;
            match grandparent {
                None => {
                    return Err(LimitError::Other(anyhow::anyhow!(
                        "parent room does not exist"
                    )));
                }
                Some(Some(_)) => return Err(LimitError::Nested),
                Some(None) => {}
            }
            let children: i64 = tx.query_row(
                "SELECT COUNT(*) FROM space_rooms
                 WHERE space_id = ?1 AND parent_room_id = ?2 AND anchor_event_id IS NULL",
                params![space_id, parent],
                |r| r.get(0),
            )?;
            if usize::try_from(children).unwrap_or(usize::MAX) >= max_children {
                return Err(LimitError::TooMany);
            }
        }
        let id = format!("r-{}", short_id());
        match tx.execute(
            "INSERT INTO space_rooms (space_id, id, name, parent_room_id, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![space_id, id, name, parent, now_ms()],
        ) {
            Ok(_) => {}
            Err(e) if is_unique_violation(&e) => return Err(LimitError::Duplicate),
            Err(e) => return Err(e.into()),
        }
        tx.commit()?;
        Ok(Room {
            id,
            name: name.to_owned(),
            parent_room_id: parent.map(str::to_owned),
            anchor_event_id: None,
        })
    }

    /// The parent-room events from before `thread` (a child room named
    /// `name`) was created: those before the server-written `Started
    /// thread` marker the parent run left there, newest `limit`. Empty when
    /// there is no such marker, so nothing later (or forged) counts.
    pub fn events_before_thread(
        &self,
        space_id: &str,
        parent_room_id: &str,
        thread_id: &str,
        name: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<Event>> {
        let marker: Option<i64> = {
            let conn = self.lock();
            conn.query_row(
                "SELECT id FROM space_events
                 WHERE space_id = ?1 AND room_id = ?2 AND kind = 'thread' AND actor_id IS NULL
                   AND text = ?3
                 ORDER BY id LIMIT 1",
                params![
                    space_id,
                    parent_room_id,
                    thread_started_text(name, thread_id)
                ],
                |r| r.get(0),
            )
            .optional()?
        };
        match marker {
            Some(id) => self.events_before(space_id, parent_room_id, id, limit),
            None => Ok(Vec::new()),
        }
    }

    /// The most recent event of `kinds` in a room, if any.
    pub fn last_event_of(
        &self,
        space_id: &str,
        room_id: &str,
        kinds: &[&str],
    ) -> anyhow::Result<Option<Event>> {
        let conn = self.lock();
        let mut stmt = conn.prepare_cached(
            "SELECT id, kind, actor, actor_id, text, mentions, audience, activity, created_at FROM space_events
             WHERE space_id = ?1 AND room_id = ?2 AND kind IN (SELECT value FROM json_each(?3))
             ORDER BY id DESC LIMIT 1",
        )?;
        let kinds = serde_json::to_string(kinds)?;
        Ok(stmt
            .query_row(params![space_id, room_id, kinds], row_event)
            .optional()?)
    }

    pub fn append_event(
        &self,
        space_id: &str,
        room_id: &str,
        kind: &str,
        author: &Author,
        text: &str,
        addressed: &Addressed,
    ) -> anyhow::Result<Event> {
        self.append_event_with(space_id, room_id, kind, author, text, addressed, None)
    }

    /// [`Db::append_event`] tagged with the agent turn or Demo run that wrote it.
    #[allow(clippy::too_many_arguments)]
    pub fn append_event_with(
        &self,
        space_id: &str,
        room_id: &str,
        kind: &str,
        author: &Author,
        text: &str,
        addressed: &Addressed,
        activity: Option<&Activity>,
    ) -> anyhow::Result<Event> {
        let conn = self.lock();
        Self::append_event_in(
            &conn, space_id, room_id, kind, author, text, addressed, activity,
        )
    }

    /// Append an event inside the caller's transaction.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn append_event_in(
        conn: &Connection,
        space_id: &str,
        room_id: &str,
        kind: &str,
        author: &Author,
        text: &str,
        addressed: &Addressed,
        activity: Option<&Activity>,
    ) -> anyhow::Result<Event> {
        Self::insert_event_in(
            conn,
            space_id,
            room_id,
            kind,
            author,
            text,
            addressed,
            activity,
            &MessageMeta::default(),
        )
    }

    /// Append a person's `message` with its [`MessageMeta`] inside the
    /// caller's transaction. The caller checks membership, the room and the
    /// metadata's references (see [`crate::messages`]).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn append_message_in(
        conn: &Connection,
        space_id: &str,
        room_id: &str,
        author: &Author,
        text: &str,
        addressed: &Addressed,
        meta: &MessageMeta,
    ) -> anyhow::Result<Event> {
        Self::insert_event_in(
            conn,
            space_id,
            room_id,
            kind::MESSAGE,
            author,
            text,
            addressed,
            None,
            meta,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_event_in(
        conn: &Connection,
        space_id: &str,
        room_id: &str,
        kind: &str,
        author: &Author,
        text: &str,
        addressed: &Addressed,
        activity: Option<&Activity>,
        meta: &MessageMeta,
    ) -> anyhow::Result<Event> {
        let at = now_ms();
        conn.execute(
            "INSERT INTO space_events (space_id, room_id, kind, actor, actor_id, text, mentions, audience, created_at, activity)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                space_id,
                room_id,
                kind,
                author.name,
                author.id,
                text,
                addressed.mentions.to_string(),
                addressed.audience,
                at,
                event_metadata_json(activity, addressed.spans.as_deref(), meta)?,
            ],
        )?;
        let mut event = new_event(
            conn.last_insert_rowid(),
            kind,
            author,
            text,
            addressed,
            activity,
            at,
        );
        event.meta = meta.clone();
        Ok(event)
    }

    pub(crate) fn room_member_in(
        conn: &Connection,
        space_id: &str,
        room_id: &str,
        user_id: &str,
    ) -> rusqlite::Result<bool> {
        conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM space_members WHERE space_id = ?1 AND user_id = ?2)
                AND EXISTS (SELECT 1 FROM space_rooms WHERE space_id = ?1 AND id = ?3)",
            params![space_id, user_id, room_id],
            |r| r.get(0),
        )
    }

    pub(crate) fn space_agent_exists_in(
        conn: &Connection,
        space_id: &str,
        agent_id: &str,
    ) -> rusqlite::Result<bool> {
        conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM space_agents WHERE space_id = ?1 AND id = ?2)",
            params![space_id, agent_id],
            |r| r.get(0),
        )
    }

    /// Recheck the concrete recipients in the transaction that writes a room message.
    pub(crate) fn recipients_current_in(
        conn: &Connection,
        space_id: &str,
        recipients: &[crate::mentions::Mention],
    ) -> rusqlite::Result<bool> {
        use crate::mentions::Mention;
        for m in recipients {
            let exists: bool = match m {
                Mention::Agent => continue,
                Mention::AgentId { agent_id } => Self::space_agent_exists_in(conn, space_id, agent_id)?,
                Mention::User { id } => conn.query_row(
                    "SELECT EXISTS (SELECT 1 FROM space_members WHERE space_id = ?1 AND user_id = ?2)",
                    params![space_id, id],
                    |r| r.get(0),
                )?,
            };
            if !exists {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Append a message written by `author` (a person) to `room_id` of
    /// `space_id`, in one transaction with the check that they are a
    /// member of the Space now and the room exists. `None`, with nothing
    /// written, when either check fails.
    pub fn append_event_as_member(
        &self,
        space_id: &str,
        room_id: &str,
        kind: &str,
        author: &Author,
        text: &str,
        addressed: &Addressed,
    ) -> anyhow::Result<Option<Event>> {
        let Some(user_id) = author.id.as_deref() else {
            return Ok(None);
        };
        let conn = self.lock();
        let tx = conn.unchecked_transaction()?;
        if !Self::room_member_in(&tx, space_id, room_id, user_id)? {
            return Ok(None);
        }
        let event =
            Self::append_event_in(&tx, space_id, room_id, kind, author, text, addressed, None)?;
        tx.commit()?;
        Ok(Some(event))
    }

    /// Events of a room with an id above `after`, oldest first.
    pub fn events_after(
        &self,
        space_id: &str,
        room_id: &str,
        after: i64,
        limit: usize,
    ) -> anyhow::Result<Vec<Event>> {
        let conn = self.lock();
        let mut stmt = conn.prepare_cached(
            "SELECT id, kind, actor, actor_id, text, mentions, audience, activity, created_at FROM space_events
             WHERE space_id = ?1 AND room_id = ?2 AND id > ?3 ORDER BY id ASC LIMIT ?4",
        )?;
        let rows = stmt.query_map(params![space_id, room_id, after, limit as i64], row_event)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// The last `limit` events of a room with an id below `before`, oldest first.
    pub fn events_before(
        &self,
        space_id: &str,
        room_id: &str,
        before: i64,
        limit: usize,
    ) -> anyhow::Result<Vec<Event>> {
        let conn = self.lock();
        let mut stmt = conn.prepare_cached(
            "SELECT id, kind, actor, actor_id, text, mentions, audience, activity, created_at FROM space_events
             WHERE space_id = ?1 AND room_id = ?2 AND id < ?3
               AND kind NOT IN ('activity', 'reasoning_summary', 'page_draft')
             ORDER BY id DESC LIMIT ?4",
        )?;
        let rows = stmt.query_map(params![space_id, room_id, before, limit as i64], row_event)?;
        let mut events: Vec<Event> = rows.collect::<Result<_, _>>()?;
        events.reverse();
        Ok(events)
    }
}

impl Db {
    pub fn append_personal(
        &self,
        user_id: &str,
        kind: &str,
        author: &Author,
        text: &str,
        addressed: &Addressed,
    ) -> anyhow::Result<Event> {
        self.append_personal_with(user_id, kind, author, text, addressed, None)
    }

    /// [`Db::append_personal`] tagged with the agent turn that wrote it.
    pub fn append_personal_with(
        &self,
        user_id: &str,
        kind: &str,
        author: &Author,
        text: &str,
        addressed: &Addressed,
        activity: Option<&Activity>,
    ) -> anyhow::Result<Event> {
        let conn = self.lock();
        let at = now_ms();
        conn.execute(
            "INSERT INTO personal_events (user_id, kind, actor, actor_id, text, mentions, audience, created_at, activity)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                user_id,
                kind,
                author.name,
                author.id,
                text,
                addressed.mentions.to_string(),
                addressed.audience,
                at,
                activity_json(activity)?,
            ],
        )?;
        Ok(new_event(
            conn.last_insert_rowid(),
            kind,
            author,
            text,
            addressed,
            activity,
            at,
        ))
    }

    pub fn personal_after(
        &self,
        user_id: &str,
        after: i64,
        limit: usize,
    ) -> anyhow::Result<Vec<Event>> {
        let conn = self.lock();
        let mut stmt = conn.prepare_cached(
            "SELECT id, kind, actor, actor_id, text, mentions, audience, activity, created_at FROM personal_events
             WHERE user_id = ?1 AND id > ?2 ORDER BY id ASC LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![user_id, after, limit as i64], row_event)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn personal_before(
        &self,
        user_id: &str,
        before: i64,
        limit: usize,
    ) -> anyhow::Result<Vec<Event>> {
        let conn = self.lock();
        let mut stmt = conn.prepare_cached(
            "SELECT id, kind, actor, actor_id, text, mentions, audience, activity, created_at FROM personal_events
             WHERE user_id = ?1 AND id < ?2 AND kind NOT IN ('activity', 'reasoning_summary')
             ORDER BY id DESC LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![user_id, before, limit as i64], row_event)?;
        let mut events: Vec<Event> = rows.collect::<Result<_, _>>()?;
        events.reverse();
        Ok(events)
    }
}

/// Refuse a database that still holds the single-room build's history. Its
/// empty tables (the seeded lobby and nothing else) are left untouched.
fn check_legacy(conn: &Connection) -> anyhow::Result<()> {
    let has = |table: &str| -> rusqlite::Result<bool> {
        conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
            [table],
            |r| r.get(0),
        )
    };
    let rooms = if has("rooms")? {
        conn.query_row("SELECT COUNT(*) FROM rooms WHERE id <> 'lobby'", [], |r| {
            r.get(0)
        })?
    } else {
        0
    };
    let events = if has("events")? {
        conn.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))?
    } else {
        0
    };
    if rooms > 0 || events > 0 {
        return Err(LegacyHistory { rooms, events }.into());
    }
    Ok(())
}

fn insert_room(tx: &Transaction<'_>, space_id: &str, id: &str, name: &str) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO space_rooms (space_id, id, name, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![space_id, id, name, now_ms()],
    )?;
    Ok(())
}

/// The columns [`row_room`] reads, in order.
pub(crate) const ROOM_COLUMNS: &str = "id, name, parent_room_id, anchor_event_id";

pub(crate) fn row_room(r: &rusqlite::Row<'_>) -> rusqlite::Result<Room> {
    Ok(Room {
        id: r.get(0)?,
        name: r.get(1)?,
        parent_room_id: r.get(2)?,
        anchor_event_id: r.get(3)?,
    })
}

fn row_space(r: &rusqlite::Row<'_>) -> rusqlite::Result<SpaceView> {
    Ok(SpaceView {
        id: r.get(0)?,
        name: r.get(1)?,
        description: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
        role: Role::parse(&r.get::<_, String>(2)?)?,
        runner_paired: r.get(3)?,
    })
}

/// An event row of `SELECT id, kind, actor, actor_id, text, mentions,
/// audience, activity, created_at`.
pub(crate) fn row_event(r: &rusqlite::Row<'_>) -> rusqlite::Result<Event> {
    let mentions: String = r.get(5)?;
    let metadata: Option<String> = r.get(7)?;
    let kind: String = r.get(1)?;
    let (spans, meta) = match metadata.as_deref() {
        Some(raw) if kind == kind::MESSAGE => {
            match serde_json::from_str::<serde_json::Value>(raw) {
                Ok(serde_json::Value::Object(mut m)) => (
                    // Unknown/malformed spans must not turn into legacy text inference.
                    m.remove("spans")
                        .map(|v| serde_json::from_value(v).unwrap_or_default()),
                    MessageMeta::from_object(&m),
                ),
                _ => (Some(Vec::new()), MessageMeta::default()),
            }
        }
        _ => (None, MessageMeta::default()),
    };
    Ok(Event {
        id: r.get(0)?,
        kind,
        actor: r.get(2)?,
        actor_id: r.get(3)?,
        text: r.get(4)?,
        mentions: serde_json::from_str(&mentions)
            .unwrap_or_else(|_| serde_json::Value::Array(Vec::new())),
        spans,
        audience: r.get(6)?,
        // Unreadable metadata is dropped, never guessed.
        activity: metadata.and_then(|a| serde_json::from_str(&a).ok()),
        created_at: r.get::<_, Option<i64>>(8)?.and_then(event_time),
        attachments: Vec::new(),
        meta,
        replies: None,
    })
}

fn activity_json(activity: Option<&Activity>) -> anyhow::Result<Option<String>> {
    Ok(activity.map(serde_json::to_string).transpose()?)
}

/// Space event metadata shares the existing JSON column. Keep the old
/// activity keys and the mentions array unchanged, including for older
/// readers. A span-only object has no activity id, so it is not a run event.
/// Message metadata ([`MessageMeta`]) adds its own keys beside them.
pub(crate) fn event_metadata_json(
    activity: Option<&Activity>,
    spans: Option<&[crate::mentions::MentionSpan]>,
    meta: &MessageMeta,
) -> anyhow::Result<Option<String>> {
    if spans.is_none() && meta.is_empty() {
        return activity_json(activity);
    }
    #[derive(Serialize)]
    struct Metadata<'a> {
        #[serde(flatten)]
        activity: Option<&'a Activity>,
        #[serde(skip_serializing_if = "Option::is_none")]
        spans: Option<&'a [crate::mentions::MentionSpan]>,
        #[serde(flatten)]
        meta: &'a MessageMeta,
    }
    Ok(Some(serde_json::to_string(&Metadata {
        activity,
        spans,
        meta,
    })?))
}

/// Additive: older databases gain a `description` column on `spaces`;
/// existing Spaces get an empty description.
fn add_space_description(conn: &Connection) -> rusqlite::Result<()> {
    let has: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM pragma_table_info('spaces') WHERE name = 'description')",
        [],
        |r| r.get(0),
    )?;
    if !has {
        conn.execute_batch("ALTER TABLE spaces ADD COLUMN description TEXT NOT NULL DEFAULT ''")?;
    }
    Ok(())
}

/// Additive: rooms gain the nullable `anchor_event_id` of reply threads,
/// with at most one thread per anchor. Existing rooms stay unanchored.
fn add_room_anchor(conn: &Connection) -> rusqlite::Result<()> {
    let has: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM pragma_table_info('space_rooms') WHERE name = 'anchor_event_id')",
        [],
        |r| r.get(0),
    )?;
    if !has {
        conn.execute_batch("ALTER TABLE space_rooms ADD COLUMN anchor_event_id INTEGER")?;
    }
    conn.execute_batch(
        "CREATE UNIQUE INDEX IF NOT EXISTS space_rooms_anchor
             ON space_rooms (space_id, anchor_event_id) WHERE anchor_event_id IS NOT NULL",
    )
}

fn add_runner_queue_floor(conn: &Connection) -> rusqlite::Result<()> {
    let has = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM pragma_table_info('spaces') WHERE name = 'runner_drop_before')",
        [], |r| r.get::<_, bool>(0),
    )?;
    if !has {
        // Jobs are process-local, so there is no old queue to migrate.
        conn.execute_batch(
            "ALTER TABLE spaces ADD COLUMN runner_drop_before INTEGER NOT NULL DEFAULT 0",
        )?;
    }
    Ok(())
}

fn read_runner_queue_version(
    conn: &Connection,
    sid: &str,
) -> rusqlite::Result<Option<crate::jobs::RunnerVersion>> {
    conn.query_row(
        "SELECT runner_generation, runner_drop_before FROM spaces WHERE id = ?1",
        [sid],
        |r| {
            Ok(crate::jobs::RunnerVersion {
                generation: r.get::<_, i64>(0)? as u64,
                drop_before: r.get::<_, i64>(1)? as u64,
            })
        },
    )
    .optional()
}

/// Additive: older databases gain a nullable `activity` column; their
/// existing events keep `NULL`, so no correlation is made up for them.
fn add_activity_columns(conn: &Connection) -> rusqlite::Result<()> {
    for table in ["space_events", "personal_events"] {
        let has: bool = conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM pragma_table_info(?1) WHERE name = 'activity')",
            [table],
            |r| r.get(0),
        )?;
        if !has {
            conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN activity TEXT"))?;
        }
        conn.execute_batch(&format!(
            "CREATE INDEX IF NOT EXISTS {table}_activity ON {table} (id) WHERE kind = 'activity'"
        ))?;
    }
    Ok(())
}

/// One event table's turn scope, for [`interrupt_orphaned_activity`].
struct ActivityScope {
    table: &'static str,
    /// The columns that, with the turn id, identify one turn's events.
    scope: &'static str,
    /// The `metro_meta` key of this table's reconciled high-water mark.
    mark_key: &'static str,
    insert: &'static str,
}

const ACTIVITY_SCOPES: [ActivityScope; 2] = [
    ActivityScope {
        table: "space_events",
        scope: "space_id, room_id",
        mark_key: "activity_reconciled_through:space_events",
        insert: "INSERT INTO space_events (space_id, room_id, kind, actor, actor_id, text, created_at, activity)
                 VALUES (?1, ?2, 'activity', ?3, ?4, '', ?5, ?6)",
    },
    ActivityScope {
        table: "personal_events",
        scope: "user_id",
        mark_key: "activity_reconciled_through:personal_events",
        insert: "INSERT INTO personal_events (user_id, kind, actor, actor_id, text, created_at, activity)
                 VALUES (?1, 'activity', ?3, ?4, '', ?5, ?6)",
    },
];

/// (scope ids, activity JSON, actor, actor id) of one orphaned start. The
/// activity is `None` when it isn't text (a JSONB blob, never written by this
/// server), so it can't be read back.
type OrphanedStart = ([String; 2], Option<String>, String, Option<String>);

/// The query behind [`orphaned_starts`]: the started event of each turn in
/// `s.table` whose latest lifecycle event is that start, oldest first. With
/// `bounded`, only turns with an `activity` event above `?1` (the
/// high-water mark); without it, every row, whatever its id.
///
/// A turn is the scope columns plus `json_extract(activity, '$.id')`, the
/// same comparison as before this pass was set-based. `activity` that
/// isn't JSON5 text (canonical JSON included) or strictly valid JSONB
/// belongs to no turn (this server never writes it), and so does one
/// without an id; neither ends a turn nor is interrupted, and neither can
/// make startup fail.
fn orphaned_starts_sql(s: &ActivityScope, bounded: bool) -> String {
    let scope_cols = if s.table == "space_events" {
        "e.space_id, e.room_id"
    } else {
        "e.user_id, ''"
    };
    // json_valid flags (SQLite 3.45+; the bundled one is newer): 0x02 JSON5
    // text, 0x08 strictly valid JSONB. Text that fails it would make
    // json_extract raise "malformed JSON"; the strict JSONB check, unlike the
    // superficial one (0x04), also keeps corrupt blobs out. The CASE keeps
    // json_extract from seeing a row that fails it, and only rows that pass
    // reach the GROUP BY and the outer json_extract.
    format!(
        "SELECT {scope_cols}, e.activity, e.actor, e.actor_id
         FROM (SELECT MAX(id) AS last FROM {t}
               WHERE kind = 'activity' {bound}
                 AND CASE WHEN json_valid(activity, 10) THEN json_extract(activity, '$.id') END IS NOT NULL
               GROUP BY {scope}, json_extract(activity, '$.id')) m
         JOIN {t} e ON e.id = m.last
         WHERE json_extract(e.activity, '$.phase') = 'started'
         ORDER BY e.id",
        t = s.table,
        scope = s.scope,
        bound = if bounded { "AND id > ?1" } else { "" },
    )
}

/// The rows of [`orphaned_starts_sql`]: above `after`, or all of them.
fn orphaned_starts(
    conn: &Connection,
    s: &ActivityScope,
    after: Option<i64>,
) -> rusqlite::Result<Vec<OrphanedStart>> {
    let mut stmt = conn.prepare(&orphaned_starts_sql(s, after.is_some()))?;
    let params: Vec<i64> = after.into_iter().collect();
    let rows = stmt.query_map(rusqlite::params_from_iter(params), |r| {
        Ok((
            [r.get::<_, String>(0)?, r.get::<_, String>(1)?],
            match r.get_ref(2)? {
                rusqlite::types::ValueRef::Text(t) => {
                    std::str::from_utf8(t).ok().map(str::to_owned)
                }
                _ => None,
            },
            r.get::<_, String>(3)?,
            r.get::<_, Option<String>>(4)?,
        ))
    })?;
    rows.collect()
}

/// Runs live only in this process, so at startup no turn is active. Every
/// turn whose last lifecycle event is still `started` gets exactly one
/// `interrupted` event in the same scope, by the same actor, so it never
/// looks live forever. One server per database file is assumed (as for the
/// in-memory run registry).
///
/// The pass is set-based (one grouped scan, not a lookup per started
/// event), and bounded by a per-table high-water mark: once a pass commits,
/// every turn whose events all lie at or below the table's AUTOINCREMENT
/// sequence is closed (or its start can't be read back as an [`Activity`],
/// which every later pass would skip again), and no new event can get an id
/// at or below it (the server never inserts an explicit id). So
/// the next start only needs turns with an `activity` event above the mark:
/// for such a turn every one of its events above the mark is scanned, and
/// the newest of them is its newest overall. Mark and sequence are
/// untrusted metadata: a mark that isn't the text of a positive integer,
/// or is above the sequence (it was reset), and a sequence that isn't an
/// integer, all mean a full scan with no lower bound on ids. A mark is
/// written only from an integer sequence; while the sequence isn't one, the
/// mark is removed. Failing to read either is still an error. Everything,
/// the mark included, is written in one transaction, so a crash at any
/// point leaves either nothing or all of it.
///
/// Maintenance that resets a table's event ids or its `sqlite_sequence`
/// row, or restores event rows into it, must delete that table's
/// `activity_reconciled_through:<table>` key in the same transaction.
/// Deleting events (as deleting a Space does) and `VACUUM` keep the
/// sequence, so they need nothing.
///
/// Known limit: a start that can't be read back as an [`Activity`] is
/// skipped (and counted in one warning), and once it is below the mark no
/// later start looks at it again. That only matters if a future build reads
/// activity more permissively; such a build must change the mark keys
/// (`activity_reconciled_through:*`) so its first start scans everything.
fn interrupt_orphaned_activity(conn: &Connection) -> rusqlite::Result<()> {
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
    let now = now_ms();
    let interrupted = |activity: &str| -> Option<String> {
        let a: Activity = serde_json::from_str(activity).ok()?;
        serde_json::to_string(&a.at(activity::INTERRUPTED)).ok()
    };
    let mut unreadable = 0_usize;
    for s in &ACTIVITY_SCOPES {
        let seq = reconciled_sequence(&tx, s)?;
        let mark = reconciled_mark(&tx, s)?;
        // The mark bounds the scan only when both can be trusted.
        let after = match (mark, seq) {
            (Some(m), Some(seq)) if 0 < m && m <= seq => Some(m),
            _ => None,
        };
        let orphans = orphaned_starts(&tx, s, after)?;
        let mut stmt = tx.prepare(s.insert)?;
        for (ids, activity, actor, actor_id) in orphans {
            let Some(json) = activity.as_deref().and_then(interrupted) else {
                unreadable += 1;
                continue;
            };
            stmt.execute(params![ids[0], ids[1], actor, actor_id, now, json])?;
        }
        drop(stmt);
        match reconciled_sequence(&tx, s)? {
            Some(through) if Some(through) == mark => {}
            Some(through) => {
                tx.execute(
                    "INSERT INTO metro_meta (key, value) VALUES (?1, ?2)
                     ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                    params![s.mark_key, through.to_string()],
                )?;
            }
            None => {
                tx.execute("DELETE FROM metro_meta WHERE key = ?1", [s.mark_key])?;
            }
        }
    }
    tx.commit()?;
    if unreadable > 0 {
        tracing::warn!(
            count = unreadable,
            "left turns open at startup whose started event can't be read as activity"
        );
    }
    Ok(())
}

/// `s.table`'s AUTOINCREMENT sequence, if it can be trusted: an integer
/// (no row yet is 0). Any other stored value is `None`; a failed read is an
/// error.
fn reconciled_sequence(tx: &Transaction<'_>, s: &ActivityScope) -> rusqlite::Result<Option<i64>> {
    let mut stmt = tx.prepare("SELECT seq FROM sqlite_sequence WHERE name = ?1")?;
    let mut rows = stmt.query([s.table])?;
    let Some(row) = rows.next()? else {
        return Ok(Some(0));
    };
    let seq = match row.get_ref(0)? {
        rusqlite::types::ValueRef::Integer(n) => Some(n),
        _ => None,
    };
    // Two rows for one table is not a sequence SQLite wrote.
    Ok(if rows.next()?.is_some() { None } else { seq })
}

/// `s.table`'s high-water mark, if it can be read: the text of an integer
/// of at least 0 (`0` bounds nothing). Anything else (absent, not text,
/// not UTF-8, not such a number) is `None`; a failed read is an error.
fn reconciled_mark(tx: &Transaction<'_>, s: &ActivityScope) -> rusqlite::Result<Option<i64>> {
    let mut stmt = tx.prepare("SELECT value FROM metro_meta WHERE key = ?1")?;
    let mut rows = stmt.query([s.mark_key])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    Ok(match row.get_ref(0)? {
        rusqlite::types::ValueRef::Text(t) => std::str::from_utf8(t)
            .ok()
            .and_then(|t| t.parse::<i64>().ok())
            .filter(|&m| m >= 0),
        _ => None,
    })
}

pub(crate) fn new_event(
    id: i64,
    kind: &str,
    author: &Author,
    text: &str,
    addressed: &Addressed,
    activity: Option<&Activity>,
    created_at_ms: i64,
) -> Event {
    Event {
        id,
        kind: kind.to_owned(),
        actor: author.name.clone(),
        actor_id: author.id.clone(),
        text: text.to_owned(),
        mentions: addressed.mentions.clone(),
        spans: addressed.spans.clone(),
        audience: addressed.audience.clone(),
        activity: activity.cloned(),
        created_at: event_time(created_at_ms),
        attachments: Vec::new(),
        meta: MessageMeta::default(),
        replies: None,
    }
}

pub(crate) fn is_unique_violation(e: &rusqlite::Error) -> bool {
    matches!(e, rusqlite::Error::SqliteFailure(f, _) if f.code == ErrorCode::ConstraintViolation)
}

pub(crate) fn short_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..16].to_owned()
}

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
#[path = "db_activity_tests.rs"]
mod activity_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_threads_parent_history_ends_at_its_creation_marker() {
        let db = Db::open_in_memory().unwrap();
        let s = db.create_space("alice", "a", 10).unwrap();
        let alice = Author {
            id: Some("alice".into()),
            name: "Alice".into(),
        };
        let agent = Author::server(actor::AGENT);
        let room = Addressed::room();
        let msg = |author: &Author, k: &str, text: &str| {
            db.append_event(&s.id, LOBBY_ID, k, author, text, &room)
                .unwrap()
        };
        msg(&alice, kind::MESSAGE, "before https://a.example/");
        // A person can't forge the marker.
        msg(
            &alice,
            kind::MESSAGE,
            &thread_started_text("research", "t1"),
        );
        let thread = db
            .create_room(&s.id, "research", Some(LOBBY_ID), 10, 10)
            .unwrap();
        assert!(
            db.events_before_thread(&s.id, LOBBY_ID, &thread.id, "research", 50)
                .unwrap()
                .is_empty(),
            "no server marker yet: nothing counts"
        );
        msg(
            &agent,
            kind::THREAD,
            &thread_started_text("research", &thread.id),
        );
        msg(&alice, kind::MESSAGE, "after https://b.example/");
        let got = db
            .events_before_thread(&s.id, LOBBY_ID, &thread.id, "research", 50)
            .unwrap();
        let texts: Vec<&str> = got.iter().map(|e| e.text.as_str()).collect();
        assert!(texts.contains(&"before https://a.example/"), "{texts:?}");
        assert!(!texts.iter().any(|t| t.contains("b.example")), "{texts:?}");
    }

    #[test]
    fn refuses_populated_legacy_database_and_accepts_empty_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old.sqlite3");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE rooms (id TEXT PRIMARY KEY, name TEXT, created_at INTEGER);
                 CREATE TABLE events (id INTEGER PRIMARY KEY AUTOINCREMENT, room_id TEXT, kind TEXT,
                                      actor TEXT, text TEXT, created_at INTEGER);
                 INSERT INTO rooms VALUES ('lobby', 'lobby', 0);",
            )
            .unwrap();
        }
        // Only the seeded lobby: nothing to lose, nothing exposed.
        Db::open(&path).unwrap();
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute("INSERT INTO events (room_id, kind, actor, text, created_at) VALUES ('lobby','message','alice','old',0)", [])
                .unwrap();
        }
        let err = Db::open(&path).err().unwrap();
        let legacy = err.downcast_ref::<LegacyHistory>().expect("legacy error");
        assert_eq!(legacy.events, 1);
        assert!(format!("{err:#}").contains("new, empty file"));
        // Nothing was changed: the old event is still there.
        let conn = Connection::open(&path).unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn a_restart_interrupts_turns_left_started_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metro.sqlite3");
        let agent = Author::server(actor::AGENT);
        let room = Addressed::room();
        let (sid, live, done, chat) = {
            let db = Db::open(&path).unwrap();
            let s = db.create_space("alice", "a", 10).unwrap();
            let live = Activity::agent("run_live", "research");
            let done = Activity::agent("run_done", Activity::AGENT_NAME);
            let chat = Activity::chat();
            for (a, phases) in [
                (&live, &[activity::STARTED][..]),
                (&done, &[activity::STARTED, activity::COMPLETED][..]),
            ] {
                for p in phases {
                    db.append_event_with(
                        &s.id,
                        LOBBY_ID,
                        kind::ACTIVITY,
                        &agent,
                        "",
                        &room,
                        Some(&a.at(p)),
                    )
                    .unwrap();
                }
            }
            db.append_personal_with(
                "alice",
                kind::ACTIVITY,
                &agent,
                "",
                &room,
                Some(&chat.at(activity::STARTED)),
            )
            .unwrap();
            // Another person's Chat, with a finished turn: untouched.
            let other = Activity::chat();
            for p in [activity::STARTED, activity::FAILED] {
                db.append_personal_with(
                    "bob",
                    kind::ACTIVITY,
                    &agent,
                    "",
                    &room,
                    Some(&other.at(p)),
                )
                .unwrap();
            }
            (s.id, live, done, chat)
        };
        let phases = |events: Vec<Event>| -> Vec<(String, String)> {
            events
                .into_iter()
                .filter_map(|e| e.activity)
                .map(|a| (a.id, a.phase.unwrap()))
                .collect()
        };
        for _ in 0..2 {
            // Reopening twice adds the `interrupted` events only once.
            let db = Db::open(&path).unwrap();
            assert_eq!(
                phases(db.events_after(&sid, LOBBY_ID, 0, 100).unwrap()),
                [
                    (live.id.clone(), "started".into()),
                    (done.id.clone(), "started".into()),
                    (done.id.clone(), "completed".into()),
                    (live.id.clone(), "interrupted".into()),
                ]
            );
            let last = db
                .events_after(&sid, LOBBY_ID, 0, 100)
                .unwrap()
                .pop()
                .unwrap();
            assert_eq!(last.activity.unwrap().name, "research");
            assert_eq!(
                (last.kind.as_str(), last.text.as_str()),
                (kind::ACTIVITY, "")
            );
            assert_eq!(
                phases(db.personal_after("alice", 0, 100).unwrap()),
                [
                    (chat.id.clone(), "started".into()),
                    (chat.id.clone(), "interrupted".into())
                ]
            );
            assert_eq!(db.personal_after("bob", 0, 100).unwrap().len(), 2);
            // Lifecycle events are not model history.
            assert!(
                db.events_before(&sid, LOBBY_ID, i64::MAX, 100)
                    .unwrap()
                    .is_empty()
            );
            assert!(
                db.personal_before("alice", i64::MAX, 100)
                    .unwrap()
                    .is_empty()
            );
        }
    }

    #[test]
    fn older_databases_gain_activity_without_made_up_history() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v3.sqlite3");
        {
            // The event tables as they were before `activity` existed.
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE metro_meta (key TEXT PRIMARY KEY NOT NULL, value TEXT NOT NULL);
                 INSERT INTO metro_meta VALUES ('schema_version', '3');
                 CREATE TABLE space_events (
                     id INTEGER PRIMARY KEY AUTOINCREMENT, space_id TEXT NOT NULL, room_id TEXT NOT NULL,
                     kind TEXT NOT NULL, actor TEXT NOT NULL, actor_id TEXT, text TEXT NOT NULL,
                     mentions TEXT NOT NULL DEFAULT '[]', audience TEXT NOT NULL DEFAULT 'room',
                     created_at INTEGER NOT NULL);
                 CREATE TABLE personal_events (
                     id INTEGER PRIMARY KEY AUTOINCREMENT, user_id TEXT NOT NULL, kind TEXT NOT NULL,
                     actor TEXT NOT NULL, actor_id TEXT, text TEXT NOT NULL,
                     mentions TEXT NOT NULL DEFAULT '[]', audience TEXT NOT NULL DEFAULT 'room',
                     created_at INTEGER NOT NULL);
                 INSERT INTO space_events (space_id, room_id, kind, actor, text, created_at)
                     VALUES ('s-old', 'lobby', 'tool_call', 'agent', 'workspace.list {}', 0);
                 INSERT INTO personal_events (user_id, kind, actor, text, created_at)
                     VALUES ('alice', 'agent', 'agent', 'old reply', 0);",
            )
            .unwrap();
        }
        let db = Db::open(&path).unwrap();
        let old = db.events_after("s-old", "lobby", 0, 10).unwrap();
        assert_eq!(old.len(), 1);
        assert_eq!(old[0].activity, None);
        assert!(
            serde_json::to_value(&old[0])
                .unwrap()
                .get("activity")
                .is_none()
        );
        assert_eq!(db.personal_after("alice", 0, 10).unwrap()[0].activity, None);
    }

    #[test]
    fn older_databases_gain_room_anchors_and_keep_their_rooms() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v050.sqlite3");
        let sid = {
            let db = Db::open(&path).unwrap();
            let s = db.create_space("alice", "a", 10).unwrap();
            db.create_room(&s.id, "t", Some(LOBBY_ID), 10, 10).unwrap();
            // As 0.5 left it: no anchor column, no index.
            db.lock()
                .execute_batch(
                    "DROP INDEX space_rooms_anchor;
                     ALTER TABLE space_rooms DROP COLUMN anchor_event_id;",
                )
                .unwrap();
            s.id
        };
        for _ in 0..2 {
            let db = Db::open(&path).unwrap();
            let rooms = db.list_rooms(&sid).unwrap();
            assert_eq!(rooms.len(), 2);
            assert!(rooms.iter().all(|r| r.anchor_event_id.is_none()));
            let index: bool = db
                .lock()
                .query_row(
                    "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE name = 'space_rooms_anchor')",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert!(index);
        }
    }

    #[test]
    fn message_metadata_round_trips_beside_spans_and_drops_only_bad_keys() {
        let db = Db::open_in_memory().unwrap();
        let s = db.create_space("alice", "a", 10).unwrap();
        let alice = Author {
            id: Some("alice".into()),
            name: "Alice".into(),
        };
        let meta = MessageMeta {
            reply_to: Some(7),
            via: Some(Via::Metro),
            forwarded: Some(Forwarded {
                author_name: "Bob".into(),
                author_kind: ForwardedAuthor::Agent,
                sent_at: rfc3339_utc(1_767_225_600_123),
                same_space: Some(SameSpace {
                    room_id: "lobby".into(),
                    event_id: 3,
                }),
            }),
        };
        let addressed = Addressed {
            spans: Some(Vec::new()),
            ..Addressed::room()
        };
        let written = {
            let conn = db.lock();
            Db::append_message_in(&conn, &s.id, LOBBY_ID, &alice, "hi", &addressed, &meta).unwrap()
        };
        let read = db.events_after(&s.id, LOBBY_ID, 0, 10).unwrap().remove(0);
        assert_eq!(read, written);
        assert_eq!(read.meta, meta);
        assert_eq!(read.spans, Some(Vec::new()));
        assert_eq!(read.activity, None, "metadata is not a run");
        let wire = serde_json::to_value(&read).unwrap();
        assert_eq!(wire["reply_to"], 7);
        assert_eq!(wire["via"], "metro");
        assert_eq!(wire["forwarded"]["author_kind"], "agent");
        assert_eq!(wire["forwarded"]["same_space"]["event_id"], 3);
        // An unreadable key is dropped on its own; spans survive.
        db.lock()
            .execute(
                "UPDATE space_events SET activity = '{\"spans\":[],\"via\":\"elsewhere\",\"reply_to\":7}'",
                [],
            )
            .unwrap();
        let read = db.events_after(&s.id, LOBBY_ID, 0, 10).unwrap().remove(0);
        assert_eq!((read.meta.via, read.meta.reply_to), (None, Some(7)));
        assert_eq!(read.spans, Some(Vec::new()));
        // Only messages carry it.
        let agent = db
            .append_event(
                &s.id,
                LOBBY_ID,
                kind::AGENT,
                &Author::server(actor::AGENT),
                "x",
                &Addressed::room(),
            )
            .unwrap();
        db.lock()
            .execute(
                "UPDATE space_events SET activity = '{\"reply_to\":1}' WHERE id = ?1",
                [agent.id],
            )
            .unwrap();
        let read = db.events_after(&s.id, LOBBY_ID, agent.id - 1, 10).unwrap();
        assert!(read[0].meta.is_empty());
    }

    #[test]
    fn reply_thread_rooms_count_toward_neither_room_limit() {
        let db = Db::open_in_memory().unwrap();
        let s = db.create_space("alice", "a", 10).unwrap();
        let alice = Author {
            id: Some("alice".into()),
            name: "Alice".into(),
        };
        let m = db
            .append_event(
                &s.id,
                LOBBY_ID,
                kind::MESSAGE,
                &alice,
                "m",
                &Addressed::room(),
            )
            .unwrap();
        let (thread, created) =
            crate::replies::get_or_create(&db, &s.id, LOBBY_ID, m.id, "alice", 10)
                .unwrap()
                .unwrap();
        assert!(created && thread.is_reply_thread());
        // Lobby and one thread fill max_rooms = 2 and max_children = 1.
        db.create_room(&s.id, "t", Some(LOBBY_ID), 2, 1).unwrap();
        assert!(matches!(
            db.create_room(&s.id, "u", None, 2, 1),
            Err(LimitError::TooMany)
        ));
        assert!(matches!(
            db.create_room(&s.id, "reply-99", None, 9, 9),
            Err(LimitError::Duplicate)
        ));
        // No agent thread under a reply thread.
        assert!(matches!(
            db.create_room(&s.id, "deeper", Some(&thread.id), 9, 9),
            Err(LimitError::Nested)
        ));
    }

    #[test]
    fn identity_namespace_is_pinned() {
        let db = Db::open_in_memory().unwrap();
        db.bind_identity_namespace("dev").unwrap();
        db.bind_identity_namespace("dev").unwrap();
        assert!(
            db.bind_identity_namespace("oidc:https://idp.example")
                .is_err()
        );
    }

    #[test]
    fn owner_cannot_be_removed() {
        let db = Db::open_in_memory().unwrap();
        let s = db.create_space("alice", "a", 10).unwrap();
        assert_eq!(db.add_member(&s.id, "bob", 10).unwrap(), AddMember::Added);
        assert_eq!(
            db.add_member(&s.id, "bob", 10).unwrap(),
            AddMember::Already(Role::Member)
        );
        assert_eq!(
            db.remove_member(&s.id, "alice").unwrap(),
            RemoveMember::IsOwner
        );
        assert_eq!(
            db.remove_member(&s.id, "bob").unwrap(),
            RemoveMember::Removed
        );
        assert_eq!(
            db.remove_member(&s.id, "bob").unwrap(),
            RemoveMember::NotMember
        );
    }

    #[test]
    fn event_times_are_the_stored_milliseconds_or_honestly_unknown() {
        let db = Db::open_in_memory().unwrap();
        let s = db.create_space("alice", "a", 10).unwrap();
        let alice = Author {
            id: Some("alice".into()),
            name: "Alice".into(),
        };
        let before = now_ms();
        let new = db
            .append_event(
                &s.id,
                LOBBY_ID,
                kind::MESSAGE,
                &alice,
                "now",
                &Addressed::room(),
            )
            .unwrap();
        let mine = db
            .append_personal("alice", kind::MESSAGE, &alice, "mine", &Addressed::room())
            .unwrap();
        let after = now_ms();
        for e in [&new, &mine] {
            let at = e.created_at.unwrap();
            assert!(before <= at && at <= after);
        }
        // Rows as an import or an older tool might have left them: no usable
        // time (0), a seconds value, a known time. Order follows ids.
        {
            let conn = db.lock();
            for (text, at) in [
                ("zero", 0_i64),
                ("seconds", 1_700_000_000),
                ("known", 1_767_225_600_123),
            ] {
                conn.execute(
                    "INSERT INTO space_events (space_id, room_id, kind, actor, actor_id, text, created_at)
                     VALUES (?1, 'lobby', 'message', 'Alice', 'alice', ?2, ?3)",
                    params![s.id, text, at],
                )
                .unwrap();
                conn.execute(
                    "INSERT INTO personal_events (user_id, kind, actor, actor_id, text, created_at)
                     VALUES ('alice', 'message', 'Alice', 'alice', ?1, ?2)",
                    params![text, at],
                )
                .unwrap();
            }
        }
        let room = db.events_after(&s.id, LOBBY_ID, 0, 100).unwrap();
        let history = db.events_before(&s.id, LOBBY_ID, i64::MAX, 100).unwrap();
        let personal = db.personal_after("alice", 0, 100).unwrap();
        assert_eq!(room, history);
        for events in [&room, &personal] {
            let texts: Vec<&str> = events.iter().map(|e| e.text.as_str()).collect();
            assert_eq!(&texts[1..], ["zero", "seconds", "known"]);
            let wire = serde_json::to_value(events).unwrap();
            assert_eq!(
                wire[0]["created_at"],
                serde_json::json!(rfc3339_utc(events[0].created_at.unwrap()))
            );
            assert_eq!(wire[1]["created_at"], serde_json::Value::Null);
            assert_eq!(wire[2]["created_at"], serde_json::Value::Null);
            assert_eq!(wire[3]["created_at"], "2026-01-01T00:00:00.123Z");
        }
        assert_eq!(
            serde_json::to_value(&new).unwrap()["created_at"],
            serde_json::to_value(&room[0]).unwrap()["created_at"]
        );
    }
}
