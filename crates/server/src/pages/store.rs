//! Page storage: pages, their append-only versions, drafts, per-person
//! preferences and action confirmations, the origins of runs started from
//! pages, and the page-only conversation of personal-page Chat runs.
//!
//! All functions are synchronous; call them inside [`crate::db::Db::call`].
//! Access is decided by the caller ([`super::access`]); these functions
//! only store and read.

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::Serialize;

use super::limits::PageLimits;
use crate::db::{Activity, Author, Db, Event, event_time, now_ms};

/// How long a soft-deleted page can be restored.
pub const DELETED_RETENTION_MS: i64 = 30 * 24 * 60 * 60 * 1000;
/// How long an unapplied draft is kept.
pub const DRAFT_RETENTION_MS: i64 = 7 * 24 * 60 * 60 * 1000;
/// Most drafts one person keeps at once (the oldest go first).
pub const MAX_DRAFTS_PER_PERSON: usize = 20;
/// Page-only Chat events kept per page and person.
pub const MAX_PAGE_EVENTS: usize = 500;

pub fn init(conn: &Connection) -> anyhow::Result<()> {
    use anyhow::Context;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS pages (
             id           TEXT PRIMARY KEY NOT NULL,
             scope        TEXT NOT NULL CHECK (scope IN ('personal', 'space')),
             owner_id     TEXT,
             space_id     TEXT REFERENCES spaces (id) ON DELETE CASCADE,
             creator_id   TEXT NOT NULL,
             title        TEXT NOT NULL,
             icon         TEXT,
             members_edit INTEGER NOT NULL DEFAULT 0,
             head_version INTEGER NOT NULL,
             deleted_at   INTEGER,
             deleted_by   TEXT,
             created_at   INTEGER NOT NULL,
             updated_at   INTEGER NOT NULL,
             CHECK ((scope = 'personal') = (owner_id IS NOT NULL)),
             CHECK ((scope = 'space') = (space_id IS NOT NULL))
         );
         CREATE INDEX IF NOT EXISTS pages_owner ON pages (owner_id) WHERE owner_id IS NOT NULL;
         CREATE INDEX IF NOT EXISTS pages_space ON pages (space_id) WHERE space_id IS NOT NULL;
         CREATE TABLE IF NOT EXISTS page_versions (
             page_id    TEXT NOT NULL REFERENCES pages (id) ON DELETE CASCADE,
             version    INTEGER NOT NULL,
             author_id  TEXT NOT NULL,
             kind       TEXT NOT NULL
                        CHECK (kind IN ('create', 'prompt', 'manual', 'restore', 'template')),
             prompt     TEXT,
             model_ref  TEXT,
             restored_from INTEGER,
             source     TEXT NOT NULL,
             spec       TEXT NOT NULL,
             created_at INTEGER NOT NULL,
             PRIMARY KEY (page_id, version)
         );
         CREATE TABLE IF NOT EXISTS page_drafts (
             id           TEXT PRIMARY KEY NOT NULL,
             page_id      TEXT REFERENCES pages (id) ON DELETE CASCADE,
             scope        TEXT NOT NULL CHECK (scope IN ('personal', 'space')),
             space_id     TEXT REFERENCES spaces (id) ON DELETE CASCADE,
             author_id    TEXT NOT NULL,
             base_version INTEGER,
             block_key    TEXT,
             prompt       TEXT NOT NULL,
             status       TEXT NOT NULL CHECK (status IN ('generating', 'ready', 'failed')),
             source       TEXT,
             spec         TEXT,
             diagnostics  TEXT,
             error        TEXT,
             model_ref    TEXT,
             attempts     INTEGER NOT NULL DEFAULT 0,
             created_at   INTEGER NOT NULL,
             expires_at   INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS page_drafts_author ON page_drafts (author_id, created_at);
         CREATE TABLE IF NOT EXISTS page_prefs (
             user_id  TEXT NOT NULL,
             page_id  TEXT NOT NULL REFERENCES pages (id) ON DELETE CASCADE,
             pinned   INTEGER NOT NULL DEFAULT 0,
             position INTEGER,
             PRIMARY KEY (user_id, page_id)
         );
         CREATE TABLE IF NOT EXISTS page_confirmations (
             user_id    TEXT NOT NULL,
             page_id    TEXT NOT NULL REFERENCES pages (id) ON DELETE CASCADE,
             version    INTEGER NOT NULL,
             created_at INTEGER NOT NULL,
             PRIMARY KEY (user_id, page_id, version)
         );
         CREATE TABLE IF NOT EXISTS page_runs (
             id         INTEGER PRIMARY KEY AUTOINCREMENT,
             page_id    TEXT NOT NULL REFERENCES pages (id) ON DELETE CASCADE,
             version    INTEGER NOT NULL,
             block_key  TEXT NOT NULL,
             user_id    TEXT NOT NULL,
             space_id   TEXT,
             room_id    TEXT,
             trigger_id INTEGER NOT NULL,
             run_id     TEXT NOT NULL,
             created_at INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS page_runs_block ON page_runs (page_id, block_key, id);
         CREATE TABLE IF NOT EXISTS page_events (
             id         INTEGER PRIMARY KEY AUTOINCREMENT,
             page_id    TEXT NOT NULL REFERENCES pages (id) ON DELETE CASCADE,
             user_id    TEXT NOT NULL,
             kind       TEXT NOT NULL,
             actor      TEXT NOT NULL,
             actor_id   TEXT,
             text       TEXT NOT NULL,
             activity   TEXT,
             created_at INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS page_events_page ON page_events (page_id, user_id, id);",
    )
    .context("creating page tables")
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Personal,
    Space,
}

impl Scope {
    pub fn as_str(&self) -> &'static str {
        match self {
            Scope::Personal => "personal",
            Scope::Space => "space",
        }
    }
    fn parse(s: &str) -> Self {
        if s == "space" {
            Scope::Space
        } else {
            Scope::Personal
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageRow {
    pub id: String,
    pub scope: Scope,
    pub owner_id: Option<String>,
    pub space_id: Option<String>,
    pub creator_id: String,
    pub title: String,
    pub icon: Option<String>,
    pub members_edit: bool,
    pub head_version: i64,
    pub deleted_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

const PAGE_COLUMNS: &str = "id, scope, owner_id, space_id, creator_id, title, icon, members_edit, \
     head_version, deleted_at, created_at, updated_at";

fn row_page(r: &rusqlite::Row<'_>) -> rusqlite::Result<PageRow> {
    Ok(PageRow {
        id: r.get(0)?,
        scope: Scope::parse(&r.get::<_, String>(1)?),
        owner_id: r.get(2)?,
        space_id: r.get(3)?,
        creator_id: r.get(4)?,
        title: r.get(5)?,
        icon: r.get(6)?,
        members_edit: r.get::<_, i64>(7)? != 0,
        head_version: r.get(8)?,
        deleted_at: r.get(9)?,
        created_at: r.get(10)?,
        updated_at: r.get(11)?,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VersionRow {
    pub version: i64,
    pub author_id: String,
    pub kind: String,
    pub prompt: Option<String>,
    pub model_ref: Option<String>,
    pub restored_from: Option<i64>,
    pub source: String,
    pub spec: String,
    pub created_at: i64,
}

const VERSION_COLUMNS: &str =
    "version, author_id, kind, prompt, model_ref, restored_from, source, spec, created_at";

fn row_version(r: &rusqlite::Row<'_>) -> rusqlite::Result<VersionRow> {
    Ok(VersionRow {
        version: r.get(0)?,
        author_id: r.get(1)?,
        kind: r.get(2)?,
        prompt: r.get(3)?,
        model_ref: r.get(4)?,
        restored_from: r.get(5)?,
        source: r.get(6)?,
        spec: r.get(7)?,
        created_at: r.get(8)?,
    })
}

/// A version to store.
#[derive(Clone, Debug)]
pub struct NewVersion {
    pub author_id: String,
    pub kind: &'static str,
    pub prompt: Option<String>,
    pub model_ref: Option<String>,
    pub restored_from: Option<i64>,
    pub source: String,
    pub spec: String,
    pub title: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum CreateError {
    /// The Space (or person) already has the most pages allowed.
    TooMany(usize),
}

pub fn new_page_id() -> String {
    format!("pg_{}", uuid::Uuid::new_v4().simple())
}

/// Create a page with version 1. `space` set: a Space page; otherwise a
/// personal page of `version.author_id`.
pub fn create(
    db: &Db,
    space: Option<&str>,
    version: &NewVersion,
    limits: &PageLimits,
) -> anyhow::Result<Result<PageRow, CreateError>> {
    let mut conn = db.lock();
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let count: i64 = match space {
        Some(sid) => tx.query_row(
            "SELECT COUNT(*) FROM pages WHERE space_id = ?1 AND deleted_at IS NULL",
            [sid],
            |r| r.get(0),
        )?,
        None => tx.query_row(
            "SELECT COUNT(*) FROM pages WHERE owner_id = ?1 AND deleted_at IS NULL",
            [&version.author_id],
            |r| r.get(0),
        )?,
    };
    if usize::try_from(count).unwrap_or(usize::MAX) >= limits.pages_per_space {
        return Ok(Err(CreateError::TooMany(limits.pages_per_space)));
    }
    let id = new_page_id();
    let now = now_ms();
    let (scope, owner) = match space {
        Some(_) => ("space", None),
        None => ("personal", Some(version.author_id.as_str())),
    };
    tx.execute(
        "INSERT INTO pages (id, scope, owner_id, space_id, creator_id, title, head_version, \
         created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7, ?7)",
        params![
            id,
            scope,
            owner,
            space,
            version.author_id,
            version.title,
            now
        ],
    )?;
    insert_version(&tx, &id, 1, version, now)?;
    let page = get_in(&tx, &id)?.ok_or_else(|| anyhow::anyhow!("reading the new page"))?;
    tx.commit()?;
    Ok(Ok(page))
}

fn insert_version(
    conn: &Connection,
    page: &str,
    n: i64,
    v: &NewVersion,
    now: i64,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO page_versions (page_id, version, author_id, kind, prompt, model_ref, \
         restored_from, source, spec, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            page,
            n,
            v.author_id,
            v.kind,
            v.prompt,
            v.model_ref,
            v.restored_from,
            v.source,
            v.spec,
            now
        ],
    )?;
    Ok(())
}

/// Saving a version found a different head than the one it was based on.
#[derive(Debug, PartialEq, Eq)]
pub struct Conflict {
    pub head: i64,
}

/// Append version `base + 1` if the head is still `base` (optimistic
/// concurrency: never a silent overwrite), then prune old versions.
pub fn add_version(
    db: &Db,
    page: &str,
    base: i64,
    version: &NewVersion,
    limits: &PageLimits,
) -> anyhow::Result<Result<i64, Conflict>> {
    let mut conn = db.lock();
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let head: Option<i64> = tx
        .query_row(
            "SELECT head_version FROM pages WHERE id = ?1 AND deleted_at IS NULL",
            [page],
            |r| r.get(0),
        )
        .optional()?;
    let Some(head) = head else {
        return Err(anyhow::anyhow!("the page is gone"));
    };
    if head != base {
        return Ok(Err(Conflict { head }));
    }
    let n = head + 1;
    let now = now_ms();
    insert_version(&tx, page, n, version, now)?;
    tx.execute(
        "UPDATE pages SET head_version = ?2, title = ?3, updated_at = ?4 WHERE id = ?1",
        params![page, n, version.title, now],
    )?;
    prune_versions(&tx, page, n, limits.versions_per_page)?;
    tx.commit()?;
    Ok(Ok(n))
}

/// Keep version 1 and the newest `keep` versions.
fn prune_versions(conn: &Connection, page: &str, head: i64, keep: usize) -> rusqlite::Result<()> {
    let oldest_kept = head - keep as i64 + 1;
    conn.execute(
        "DELETE FROM page_versions WHERE page_id = ?1 AND version <> 1 AND version < ?2",
        params![page, oldest_kept],
    )?;
    conn.execute(
        "DELETE FROM page_confirmations WHERE page_id = ?1 AND version <> 1 AND version < ?2",
        params![page, oldest_kept],
    )?;
    Ok(())
}

fn get_in(conn: &Connection, id: &str) -> rusqlite::Result<Option<PageRow>> {
    conn.query_row(
        &format!("SELECT {PAGE_COLUMNS} FROM pages WHERE id = ?1"),
        [id],
        row_page,
    )
    .optional()
}

/// The page, deleted or not.
pub fn get(db: &Db, id: &str) -> anyhow::Result<Option<PageRow>> {
    Ok(get_in(&db.lock(), id)?)
}

/// Pages `user` can see now: their personal pages and the pages of Spaces
/// they're a member of now. With `deleted`, only the soft-deleted ones
/// they could restore (their own, ones they created, or any in a Space
/// they own) instead.
pub fn list_for(db: &Db, user: &str, deleted: bool) -> anyhow::Result<Vec<PageRow>> {
    let conn = db.lock();
    let sql = if deleted {
        format!(
            "SELECT {PAGE_COLUMNS} FROM pages p WHERE p.deleted_at IS NOT NULL AND (
                 p.owner_id = ?1
                 OR (p.space_id IS NOT NULL AND EXISTS (SELECT 1 FROM space_members m
                     WHERE m.space_id = p.space_id AND m.user_id = ?1
                       AND (m.role = 'owner' OR p.creator_id = ?1))))
             ORDER BY p.deleted_at DESC"
        )
    } else {
        format!(
            "SELECT {PAGE_COLUMNS} FROM pages p WHERE p.deleted_at IS NULL AND (
                 p.owner_id = ?1
                 OR (p.space_id IS NOT NULL AND EXISTS (SELECT 1 FROM space_members m
                     WHERE m.space_id = p.space_id AND m.user_id = ?1)))
             ORDER BY p.scope, p.space_id, p.created_at, p.id"
        )
    };
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([user], row_page)?;
    Ok(rows.collect::<Result<_, _>>()?)
}

pub fn versions(db: &Db, page: &str) -> anyhow::Result<Vec<VersionRow>> {
    let conn = db.lock();
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {VERSION_COLUMNS} FROM page_versions WHERE page_id = ?1 ORDER BY version DESC"
    ))?;
    let rows = stmt.query_map([page], row_version)?;
    Ok(rows.collect::<Result<_, _>>()?)
}

pub fn version(db: &Db, page: &str, n: i64) -> anyhow::Result<Option<VersionRow>> {
    let conn = db.lock();
    Ok(conn
        .query_row(
            &format!(
                "SELECT {VERSION_COLUMNS} FROM page_versions WHERE page_id = ?1 AND version = ?2"
            ),
            params![page, n],
            row_version,
        )
        .optional()?)
}

pub fn set_meta(
    db: &Db,
    page: &str,
    icon: Option<Option<&str>>,
    members_edit: Option<bool>,
) -> anyhow::Result<()> {
    let conn = db.lock();
    if let Some(icon) = icon {
        conn.execute(
            "UPDATE pages SET icon = ?2, updated_at = ?3 WHERE id = ?1",
            params![page, icon, now_ms()],
        )?;
    }
    if let Some(m) = members_edit {
        conn.execute(
            "UPDATE pages SET members_edit = ?2, updated_at = ?3 WHERE id = ?1",
            params![page, m as i64, now_ms()],
        )?;
    }
    Ok(())
}

pub fn soft_delete(db: &Db, page: &str, by: &str) -> anyhow::Result<bool> {
    let conn = db.lock();
    Ok(conn.execute(
        "UPDATE pages SET deleted_at = ?2, deleted_by = ?3 WHERE id = ?1 AND deleted_at IS NULL",
        params![page, now_ms(), by],
    )? == 1)
}

/// Undelete, if the Space (or person) still has room for it.
pub fn undelete(
    db: &Db,
    page: &str,
    limits: &PageLimits,
) -> anyhow::Result<Result<bool, CreateError>> {
    let mut conn = db.lock();
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let Some(row) = get_in(&tx, page)? else {
        return Ok(Ok(false));
    };
    if row.deleted_at.is_none() {
        return Ok(Ok(false));
    }
    let count: i64 = match (&row.space_id, &row.owner_id) {
        (Some(sid), _) => tx.query_row(
            "SELECT COUNT(*) FROM pages WHERE space_id = ?1 AND deleted_at IS NULL",
            [sid],
            |r| r.get(0),
        )?,
        (None, Some(owner)) => tx.query_row(
            "SELECT COUNT(*) FROM pages WHERE owner_id = ?1 AND deleted_at IS NULL",
            [owner],
            |r| r.get(0),
        )?,
        _ => 0,
    };
    if usize::try_from(count).unwrap_or(usize::MAX) >= limits.pages_per_space {
        return Ok(Err(CreateError::TooMany(limits.pages_per_space)));
    }
    tx.execute(
        "UPDATE pages SET deleted_at = NULL, deleted_by = NULL, updated_at = ?2 WHERE id = ?1",
        params![page, now_ms()],
    )?;
    tx.commit()?;
    Ok(Ok(true))
}

/// Purge pages deleted more than [`DELETED_RETENTION_MS`] ago and expired
/// drafts, as of `now`.
pub fn purge_expired(db: &Db, now: i64) -> anyhow::Result<usize> {
    let conn = db.lock();
    let pages = conn.execute(
        "DELETE FROM pages WHERE deleted_at IS NOT NULL AND deleted_at < ?1",
        [now - DELETED_RETENTION_MS],
    )?;
    conn.execute("DELETE FROM page_drafts WHERE expires_at < ?1", [now])?;
    Ok(pages)
}

// ---- Preferences ----

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Prefs {
    pub pinned: bool,
    pub position: Option<i64>,
}

pub fn prefs(db: &Db, user: &str, page: &str) -> anyhow::Result<Prefs> {
    let conn = db.lock();
    Ok(conn
        .query_row(
            "SELECT pinned, position FROM page_prefs WHERE user_id = ?1 AND page_id = ?2",
            params![user, page],
            |r| {
                Ok(Prefs {
                    pinned: r.get::<_, i64>(0)? != 0,
                    position: r.get(1)?,
                })
            },
        )
        .optional()?
        .unwrap_or_default())
}

pub fn all_prefs(db: &Db, user: &str) -> anyhow::Result<std::collections::HashMap<String, Prefs>> {
    let conn = db.lock();
    let mut stmt =
        conn.prepare_cached("SELECT page_id, pinned, position FROM page_prefs WHERE user_id = ?1")?;
    let rows = stmt.query_map([user], |r| {
        Ok((
            r.get::<_, String>(0)?,
            Prefs {
                pinned: r.get::<_, i64>(1)? != 0,
                position: r.get(2)?,
            },
        ))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

pub fn put_prefs(db: &Db, user: &str, page: &str, p: &Prefs) -> anyhow::Result<()> {
    let conn = db.lock();
    conn.execute(
        "INSERT INTO page_prefs (user_id, page_id, pinned, position) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (user_id, page_id) DO UPDATE SET pinned = ?3, position = ?4",
        params![user, page, p.pinned as i64, p.position],
    )?;
    Ok(())
}

// ---- Confirmations ----

pub fn confirmed(db: &Db, user: &str, page: &str, version: i64) -> anyhow::Result<bool> {
    let conn = db.lock();
    Ok(conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM page_confirmations
             WHERE user_id = ?1 AND page_id = ?2 AND version = ?3)",
        params![user, page, version],
        |r| r.get(0),
    )?)
}

pub fn confirm(db: &Db, user: &str, page: &str, version: i64) -> anyhow::Result<()> {
    let conn = db.lock();
    conn.execute(
        "INSERT OR IGNORE INTO page_confirmations (user_id, page_id, version, created_at)
         VALUES (?1, ?2, ?3, ?4)",
        params![user, page, version, now_ms()],
    )?;
    Ok(())
}

// ---- Run origins ----

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageRun {
    pub version: i64,
    pub user_id: String,
    pub space_id: Option<String>,
    pub room_id: Option<String>,
    pub trigger_id: i64,
    pub run_id: String,
    pub created_at: i64,
}

#[allow(clippy::too_many_arguments)]
pub fn record_run(
    db: &Db,
    page: &str,
    version: i64,
    block: &str,
    user: &str,
    target: Option<(&str, &str)>,
    trigger_id: i64,
    run_id: &str,
) -> anyhow::Result<()> {
    let conn = db.lock();
    conn.execute(
        "INSERT INTO page_runs (page_id, version, block_key, user_id, space_id, room_id, \
         trigger_id, run_id, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            page,
            version,
            block,
            user,
            target.map(|t| t.0),
            target.map(|t| t.1),
            trigger_id,
            run_id,
            now_ms()
        ],
    )?;
    // Bounded: the newest 200 per block are enough to find answers.
    conn.execute(
        "DELETE FROM page_runs WHERE page_id = ?1 AND block_key = ?2 AND id NOT IN
             (SELECT id FROM page_runs WHERE page_id = ?1 AND block_key = ?2
              ORDER BY id DESC LIMIT 200)",
        params![page, block],
    )?;
    Ok(())
}

/// The latest run started from `block` of `page`; with `user`, only theirs.
pub fn latest_run(
    db: &Db,
    page: &str,
    block: &str,
    user: Option<&str>,
) -> anyhow::Result<Option<PageRun>> {
    let conn = db.lock();
    Ok(conn
        .query_row(
            "SELECT version, user_id, space_id, room_id, trigger_id, run_id, created_at
             FROM page_runs WHERE page_id = ?1 AND block_key = ?2
               AND (?3 IS NULL OR user_id = ?3)
             ORDER BY id DESC LIMIT 1",
            params![page, block, user],
            |r| {
                Ok(PageRun {
                    version: r.get(0)?,
                    user_id: r.get(1)?,
                    space_id: r.get(2)?,
                    room_id: r.get(3)?,
                    trigger_id: r.get(4)?,
                    run_id: r.get(5)?,
                    created_at: r.get(6)?,
                })
            },
        )
        .optional()?)
}

// ---- Page-only Chat events ----

fn row_page_event(r: &rusqlite::Row<'_>) -> rusqlite::Result<Event> {
    Ok(Event {
        id: r.get(0)?,
        kind: r.get(1)?,
        actor: r.get(2)?,
        actor_id: r.get(3)?,
        text: r.get(4)?,
        mentions: serde_json::Value::Array(Vec::new()),
        audience: "agent".to_owned(),
        activity: r
            .get::<_, Option<String>>(5)?
            .and_then(|a| serde_json::from_str(&a).ok()),
        created_at: r.get::<_, Option<i64>>(6)?.and_then(event_time),
        attachments: Vec::new(),
    })
}

/// Append an event to `user`'s page-only conversation on `page`. These
/// events never appear in their personal Chat, its search or its history.
pub fn append_page_event(
    db: &Db,
    page: &str,
    user: &str,
    kind: &str,
    author: &Author,
    text: &str,
    activity: Option<&Activity>,
) -> anyhow::Result<Event> {
    let conn = db.lock();
    let at = now_ms();
    let activity_json = activity.map(serde_json::to_string).transpose()?;
    conn.execute(
        "INSERT INTO page_events (page_id, user_id, kind, actor, actor_id, text, activity, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![page, user, kind, author.name, author.id, text, activity_json, at],
    )?;
    let id = conn.last_insert_rowid();
    conn.execute(
        "DELETE FROM page_events WHERE page_id = ?1 AND user_id = ?2 AND id NOT IN
             (SELECT id FROM page_events WHERE page_id = ?1 AND user_id = ?2
              ORDER BY id DESC LIMIT ?3)",
        params![page, user, MAX_PAGE_EVENTS as i64],
    )?;
    Ok(crate::db::new_event(
        id,
        kind,
        author,
        text,
        &crate::agent::agent_addressed(),
        activity,
        at,
    ))
}

/// The last `limit` page-only events before `before`, oldest first, without
/// lifecycle markers: the model history of a page-only Chat run.
pub fn page_events_before(
    db: &Db,
    page: &str,
    user: &str,
    before: i64,
    limit: usize,
) -> anyhow::Result<Vec<Event>> {
    let conn = db.lock();
    let mut stmt = conn.prepare_cached(
        "SELECT id, kind, actor, actor_id, text, activity, created_at FROM page_events
         WHERE page_id = ?1 AND user_id = ?2 AND id < ?3
           AND kind NOT IN ('activity', 'reasoning_summary')
         ORDER BY id DESC LIMIT ?4",
    )?;
    let rows = stmt.query_map(params![page, user, before, limit as i64], row_page_event)?;
    let mut events: Vec<Event> = rows.collect::<Result<_, _>>()?;
    events.reverse();
    Ok(events)
}

/// The newest page-only event of `user` on `page` of one of `kinds`,
/// optionally of one activity.
pub fn last_page_event(
    db: &Db,
    page: &str,
    user: &str,
    kinds: &[&str],
    activity_id: Option<&str>,
) -> anyhow::Result<Option<Event>> {
    let conn = db.lock();
    let kinds = serde_json::to_string(kinds)?;
    Ok(conn
        .query_row(
            "SELECT id, kind, actor, actor_id, text, activity, created_at FROM page_events
             WHERE page_id = ?1 AND user_id = ?2 AND kind IN (SELECT value FROM json_each(?3))
               AND (?4 IS NULL OR json_extract(activity, '$.id') = ?4)
             ORDER BY id DESC LIMIT 1",
            params![page, user, kinds, activity_id],
            row_page_event,
        )
        .optional()?)
}

/// Newest room events of `kinds` (optionally of one activity id or one
/// agent id), newest first.
#[allow(clippy::too_many_arguments)]
pub fn room_events(
    db: &Db,
    space: &str,
    room: &str,
    kinds: &[&str],
    activity_id: Option<&str>,
    agent_id: Option<&str>,
    limit: usize,
) -> anyhow::Result<Vec<Event>> {
    let conn = db.lock();
    let kinds = serde_json::to_string(kinds)?;
    let mut stmt = conn.prepare_cached(
        "SELECT id, kind, actor, actor_id, text, activity, created_at FROM space_events
         WHERE space_id = ?1 AND room_id = ?2 AND kind IN (SELECT value FROM json_each(?3))
           AND (?4 IS NULL OR json_extract(activity, '$.id') = ?4)
           AND (?5 IS NULL OR json_extract(activity, '$.agent.id') = ?5)
         ORDER BY id DESC LIMIT ?6",
    )?;
    let rows = stmt.query_map(
        params![space, room, kinds, activity_id, agent_id, limit as i64],
        row_page_event,
    )?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Newest personal Chat events of `kinds`, newest first.
pub fn chat_events(
    db: &Db,
    user: &str,
    kinds: &[&str],
    limit: usize,
) -> anyhow::Result<Vec<Event>> {
    let conn = db.lock();
    let kinds = serde_json::to_string(kinds)?;
    let mut stmt = conn.prepare_cached(
        "SELECT id, kind, actor, actor_id, text, activity, created_at FROM personal_events
         WHERE user_id = ?1 AND kind IN (SELECT value FROM json_each(?2))
         ORDER BY id DESC LIMIT ?3",
    )?;
    let rows = stmt.query_map(params![user, kinds, limit as i64], row_page_event)?;
    Ok(rows.collect::<Result<_, _>>()?)
}

// ---- Drafts ----

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftRow {
    pub id: String,
    pub page_id: Option<String>,
    pub scope: Scope,
    pub space_id: Option<String>,
    pub author_id: String,
    pub base_version: Option<i64>,
    pub block_key: Option<String>,
    pub prompt: String,
    pub status: String,
    pub source: Option<String>,
    pub spec: Option<String>,
    pub diagnostics: Option<String>,
    pub error: Option<String>,
    pub model_ref: Option<String>,
    pub attempts: i64,
    pub created_at: i64,
}

const DRAFT_COLUMNS: &str = "id, page_id, scope, space_id, author_id, base_version, block_key, \
     prompt, status, source, spec, diagnostics, error, model_ref, attempts, created_at";

fn row_draft(r: &rusqlite::Row<'_>) -> rusqlite::Result<DraftRow> {
    Ok(DraftRow {
        id: r.get(0)?,
        page_id: r.get(1)?,
        scope: Scope::parse(&r.get::<_, String>(2)?),
        space_id: r.get(3)?,
        author_id: r.get(4)?,
        base_version: r.get(5)?,
        block_key: r.get(6)?,
        prompt: r.get(7)?,
        status: r.get(8)?,
        source: r.get(9)?,
        spec: r.get(10)?,
        diagnostics: r.get(11)?,
        error: r.get(12)?,
        model_ref: r.get(13)?,
        attempts: r.get(14)?,
        created_at: r.get(15)?,
    })
}

pub struct NewDraft<'a> {
    pub page_id: Option<&'a str>,
    pub space_id: Option<&'a str>,
    pub author_id: &'a str,
    pub base_version: Option<i64>,
    pub block_key: Option<&'a str>,
    pub prompt: &'a str,
}

pub fn create_draft(db: &Db, d: &NewDraft<'_>) -> anyhow::Result<DraftRow> {
    let mut conn = db.lock();
    let tx = conn.transaction()?;
    let id = format!("pgd_{}", uuid::Uuid::new_v4().simple());
    let now = now_ms();
    let scope = if d.space_id.is_some() {
        "space"
    } else {
        "personal"
    };
    tx.execute(
        "INSERT INTO page_drafts (id, page_id, scope, space_id, author_id, base_version, \
         block_key, prompt, status, created_at, expires_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'generating', ?9, ?10)",
        params![
            id,
            d.page_id,
            scope,
            d.space_id,
            d.author_id,
            d.base_version,
            d.block_key,
            d.prompt,
            now,
            now + DRAFT_RETENTION_MS
        ],
    )?;
    tx.execute(
        "DELETE FROM page_drafts WHERE author_id = ?1 AND id NOT IN
             (SELECT id FROM page_drafts WHERE author_id = ?1 ORDER BY created_at DESC, id
              LIMIT ?2)",
        params![d.author_id, MAX_DRAFTS_PER_PERSON as i64],
    )?;
    let row = tx.query_row(
        &format!("SELECT {DRAFT_COLUMNS} FROM page_drafts WHERE id = ?1"),
        [&id],
        row_draft,
    )?;
    tx.commit()?;
    Ok(row)
}

pub fn draft(db: &Db, id: &str) -> anyhow::Result<Option<DraftRow>> {
    let conn = db.lock();
    Ok(conn
        .query_row(
            &format!("SELECT {DRAFT_COLUMNS} FROM page_drafts WHERE id = ?1"),
            [id],
            row_draft,
        )
        .optional()?)
}

/// Record a generation attempt's result (`ready` or `failed`).
#[allow(clippy::too_many_arguments)]
pub fn finish_draft(
    db: &Db,
    id: &str,
    status: &str,
    source: Option<&str>,
    spec: Option<&str>,
    diagnostics: Option<&str>,
    error: Option<&str>,
    model_ref: Option<&str>,
    attempts: i64,
) -> anyhow::Result<()> {
    let conn = db.lock();
    conn.execute(
        "UPDATE page_drafts SET status = ?2, source = ?3, spec = ?4, diagnostics = ?5, error = ?6, \
         model_ref = ?7, attempts = ?8 WHERE id = ?1",
        params![
            id,
            status,
            source,
            spec,
            diagnostics,
            error,
            model_ref,
            attempts
        ],
    )?;
    Ok(())
}

pub fn delete_draft(db: &Db, id: &str) -> anyhow::Result<()> {
    db.lock()
        .execute("DELETE FROM page_drafts WHERE id = ?1", [id])?;
    Ok(())
}

// ---- Purges in the Space transactions ----

/// Hooks run inside the Space delete and member removal transactions.
pub mod hooks {
    use rusqlite::{Transaction, params};

    /// Everything of the Space's pages goes with the Space.
    pub fn space_deleted(tx: &Transaction<'_>, space: &str) -> rusqlite::Result<()> {
        tx.execute("DELETE FROM page_drafts WHERE space_id = ?1", [space])?;
        tx.execute(
            "DELETE FROM page_drafts WHERE page_id IN (SELECT id FROM pages WHERE space_id = ?1)",
            [space],
        )?;
        // Versions, prefs, confirmations, run origins and events cascade.
        tx.execute("DELETE FROM pages WHERE space_id = ?1", [space])?;
        // Personal pages' run origins that pointed into this Space.
        tx.execute("DELETE FROM page_runs WHERE space_id = ?1", [space])?;
        Ok(())
    }

    /// A person leaving or being removed: their preferences, confirmations
    /// and drafts for the Space's pages go in the same transaction. Pages
    /// they created stay, like their messages; their rights end with the
    /// membership.
    pub fn member_removed(tx: &Transaction<'_>, space: &str, user: &str) -> rusqlite::Result<()> {
        tx.execute(
            "DELETE FROM page_prefs WHERE user_id = ?2
                 AND page_id IN (SELECT id FROM pages WHERE space_id = ?1)",
            params![space, user],
        )?;
        tx.execute(
            "DELETE FROM page_confirmations WHERE user_id = ?2
                 AND page_id IN (SELECT id FROM pages WHERE space_id = ?1)",
            params![space, user],
        )?;
        tx.execute(
            "DELETE FROM page_drafts WHERE author_id = ?2 AND (space_id = ?1
                 OR page_id IN (SELECT id FROM pages WHERE space_id = ?1))",
            params![space, user],
        )?;
        Ok(())
    }
}
