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
use serde::Serialize;

#[path = "reactions.rs"]
pub mod reactions;

pub const LOBBY_ID: &str = "lobby";
pub const LOBBY_NAME: &str = "lobby";
const SCHEMA_VERSION: i64 = 2;

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
    /// `room`, `agent`, `users` or `mixed`; `room` for non-messages.
    pub audience: String,
}

/// Recipients of a new event.
#[derive(Clone, Debug)]
pub struct Addressed {
    /// A JSON array of mentions.
    pub mentions: serde_json::Value,
    pub audience: String,
}

impl Addressed {
    /// Not addressed to anyone in particular.
    pub fn room() -> Self {
        Self {
            mentions: serde_json::Value::Array(Vec::new()),
            audience: "room".to_owned(),
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
    /// A person started Demo.
    pub const DEMO: &str = "demo";
    /// A failure or notice from the server.
    pub const ERROR: &str = "error";
    /// A thread's result or status, mirrored into its parent room.
    pub const THREAD: &str = "thread";
    /// An agent run changed state without a reply (for example, it was cancelled).
    pub const STATUS: &str = "status";
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
        reactions::init(&conn)?;
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
            Some(v) => anyhow::bail!(
                "database schema version {v} is not supported by this server (expects {SCHEMA_VERSION})"
            ),
        }
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
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
        tx.commit()?;
        Ok(SpaceView {
            id,
            name: name.to_owned(),
            role: Role::Owner,
            runner_paired: false,
        })
    }

    /// The Spaces `user_id` belongs to.
    pub fn list_spaces(&self, user_id: &str) -> anyhow::Result<Vec<SpaceView>> {
        let conn = self.lock();
        let mut stmt = conn.prepare_cached(
            "SELECT s.id, s.name, m.role, s.runner_token_hash IS NOT NULL
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
                "SELECT s.id, s.name, m.role, s.runner_token_hash IS NOT NULL
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
                RemoveMember::Removed
            }
        };
        tx.commit()?;
        Ok(result)
    }

    // ---- Runner credentials ----

    /// Store the hash of a new runner token for `space_id`, replacing any
    /// previous one, and return the new credential generation.
    pub fn set_runner_token(&self, space_id: &str, hash: &[u8]) -> anyhow::Result<u64> {
        let conn = self.lock();
        let generation: i64 = conn.query_row(
            "UPDATE spaces SET runner_token_hash = ?2, runner_generation = runner_generation + 1
             WHERE id = ?1 RETURNING runner_generation",
            params![space_id, hash],
            |r| r.get(0),
        )?;
        Ok(generation as u64)
    }

    /// Remove `space_id`'s runner token and return the new generation.
    pub fn clear_runner_token(&self, space_id: &str) -> anyhow::Result<u64> {
        let conn = self.lock();
        let generation: i64 = conn.query_row(
            "UPDATE spaces SET runner_token_hash = NULL, runner_generation = runner_generation + 1
             WHERE id = ?1 RETURNING runner_generation",
            [space_id],
            |r| r.get(0),
        )?;
        Ok(generation as u64)
    }

    /// The Space and credential generation a runner token hash belongs to.
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

    pub fn runner_paired(&self, space_id: &str) -> anyhow::Result<bool> {
        let conn = self.lock();
        Ok(conn
            .query_row(
                "SELECT runner_token_hash IS NOT NULL FROM spaces WHERE id = ?1",
                [space_id],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(false))
    }

    // ---- Rooms and events ----

    pub fn list_rooms(&self, space_id: &str) -> anyhow::Result<Vec<Room>> {
        let conn = self.lock();
        let mut stmt = conn.prepare_cached(
            "SELECT id, name, parent_room_id FROM space_rooms WHERE space_id = ?1 ORDER BY created_at, rowid",
        )?;
        let rows = stmt.query_map([space_id], row_room)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn get_room(&self, space_id: &str, room_id: &str) -> anyhow::Result<Option<Room>> {
        let conn = self.lock();
        Ok(conn
            .query_row(
                "SELECT id, name, parent_room_id FROM space_rooms WHERE space_id = ?1 AND id = ?2",
                params![space_id, room_id],
                row_room,
            )
            .optional()?)
    }

    /// Create a top-level room, or with `parent` a thread under that
    /// top-level room. `max_rooms` bounds the Space, `max_children` the parent.
    pub fn create_room(
        &self,
        space_id: &str,
        name: &str,
        parent: Option<&str>,
        max_rooms: usize,
        max_children: usize,
    ) -> Result<Room, LimitError> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM space_rooms WHERE space_id = ?1",
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
                "SELECT COUNT(*) FROM space_rooms WHERE space_id = ?1 AND parent_room_id = ?2",
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
        })
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
            "SELECT id, kind, actor, actor_id, text, mentions, audience FROM space_events
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
        let conn = self.lock();
        conn.execute(
            "INSERT INTO space_events (space_id, room_id, kind, actor, actor_id, text, mentions, audience, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                space_id,
                room_id,
                kind,
                author.name,
                author.id,
                text,
                addressed.mentions.to_string(),
                addressed.audience,
                now_ms()
            ],
        )?;
        Ok(new_event(
            conn.last_insert_rowid(),
            kind,
            author,
            text,
            addressed,
        ))
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
            "SELECT id, kind, actor, actor_id, text, mentions, audience FROM space_events
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
            "SELECT id, kind, actor, actor_id, text, mentions, audience FROM space_events
             WHERE space_id = ?1 AND room_id = ?2 AND id < ?3 ORDER BY id DESC LIMIT ?4",
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
        let conn = self.lock();
        conn.execute(
            "INSERT INTO personal_events (user_id, kind, actor, actor_id, text, mentions, audience, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                user_id,
                kind,
                author.name,
                author.id,
                text,
                addressed.mentions.to_string(),
                addressed.audience,
                now_ms()
            ],
        )?;
        Ok(new_event(
            conn.last_insert_rowid(),
            kind,
            author,
            text,
            addressed,
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
            "SELECT id, kind, actor, actor_id, text, mentions, audience FROM personal_events
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
            "SELECT id, kind, actor, actor_id, text, mentions, audience FROM personal_events
             WHERE user_id = ?1 AND id < ?2 ORDER BY id DESC LIMIT ?3",
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

fn row_room(r: &rusqlite::Row<'_>) -> rusqlite::Result<Room> {
    Ok(Room {
        id: r.get(0)?,
        name: r.get(1)?,
        parent_room_id: r.get(2)?,
    })
}

fn row_space(r: &rusqlite::Row<'_>) -> rusqlite::Result<SpaceView> {
    Ok(SpaceView {
        id: r.get(0)?,
        name: r.get(1)?,
        role: Role::parse(&r.get::<_, String>(2)?)?,
        runner_paired: r.get(3)?,
    })
}

fn row_event(r: &rusqlite::Row<'_>) -> rusqlite::Result<Event> {
    let mentions: String = r.get(5)?;
    Ok(Event {
        id: r.get(0)?,
        kind: r.get(1)?,
        actor: r.get(2)?,
        actor_id: r.get(3)?,
        text: r.get(4)?,
        mentions: serde_json::from_str(&mentions)
            .unwrap_or_else(|_| serde_json::Value::Array(Vec::new())),
        audience: r.get(6)?,
    })
}

fn new_event(id: i64, kind: &str, author: &Author, text: &str, addressed: &Addressed) -> Event {
    Event {
        id,
        kind: kind.to_owned(),
        actor: author.name.clone(),
        actor_id: author.id.clone(),
        text: text.to_owned(),
        mentions: addressed.mentions.clone(),
        audience: addressed.audience.clone(),
    }
}

fn is_unique_violation(e: &rusqlite::Error) -> bool {
    matches!(e, rusqlite::Error::SqliteFailure(f, _) if f.code == ErrorCode::ConstraintViolation)
}

fn short_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..16].to_owned()
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
