//! Page storage: pages, their append-only versions, drafts, per-person
//! preferences and action confirmations, the origins of runs started from
//! pages, and the page-only conversation of personal-page Chat runs.
//!
//! All functions are synchronous; call them inside [`crate::db::Db::call`].
//! Access is decided by the caller ([`super::access`]); these functions
//! only store and read.

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use super::access::{Rights, member_role, rights_in};
use super::spec::{Action, PageSpec};
use crate::admin_settings;
use crate::db::{Activity, Author, Db, Event, Room, event_time, now_ms};

/// How long a soft-deleted page can be restored.
pub const DELETED_RETENTION_MS: i64 = 30 * 24 * 60 * 60 * 1000;
/// How long an unapplied draft is kept.
pub const DRAFT_RETENTION_MS: i64 = 7 * 24 * 60 * 60 * 1000;
/// Most drafts one person keeps at once (the oldest go first).
pub const MAX_DRAFTS_PER_PERSON: usize = 20;
/// Drafts one person may have generating at once.
pub const MAX_GENERATING_PER_PERSON: i64 = 2;
/// Page-only Chat events kept per page and person.
pub const MAX_PAGE_EVENTS: usize = 500;

/// The page tables.
const TABLES: &str = "CREATE TABLE IF NOT EXISTS pages (
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
                        CHECK (kind IN ('create', 'prompt', 'manual', 'restore', 'template', 'rename')),
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
             status       TEXT NOT NULL CHECK (status IN ('generating', 'ready', 'failed', 'discarded')),
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
             error      TEXT,
             created_at INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS page_action_authors (
             page_id     TEXT NOT NULL REFERENCES pages (id) ON DELETE CASCADE,
             version     INTEGER NOT NULL,
             user_id     TEXT NOT NULL,
             action_hash TEXT NOT NULL,
             PRIMARY KEY (page_id, version, user_id, action_hash)
         );
         CREATE INDEX IF NOT EXISTS page_action_authors_user
             ON page_action_authors (page_id, user_id, action_hash);
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
         CREATE INDEX IF NOT EXISTS page_events_page ON page_events (page_id, user_id, id);";

pub fn init(conn: &Connection) -> anyhow::Result<()> {
    use anyhow::Context;
    conn.execute_batch(TABLES).context("creating page tables")?;
    upgrade(conn).context("upgrading page tables")?;
    // A rebuilt table lost its indexes: create anything missing again.
    conn.execute_batch(TABLES)
        .context("recreating page indexes")?;
    recover_drafts(conn).context("recovering page drafts")?;
    recover_runs(conn).context("recovering page runs")?;
    Ok(())
}

/// Bring tables created by earlier builds of this lane up to the current
/// shape, idempotently: missing columns are added, and tables whose CHECK
/// lacks a newer value are rebuilt with their rows kept.
fn upgrade(conn: &Connection) -> rusqlite::Result<()> {
    let has_column = |table: &str, column: &str| -> rusqlite::Result<bool> {
        conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM pragma_table_info(?1) WHERE name = ?2)",
            params![table, column],
            |r| r.get(0),
        )
    };
    if !has_column("page_runs", "error")? {
        conn.execute_batch("ALTER TABLE page_runs ADD COLUMN error TEXT")?;
    }
    let ddl = |table: &str| -> rusqlite::Result<String> {
        conn.query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |r| r.get(0),
        )
    };
    // (table, a value its CHECK must allow, the columns to copy).
    let rebuilds = [
        (
            "page_versions",
            "'rename'",
            "page_id, version, author_id, kind, prompt, model_ref, restored_from, source, spec, \
             created_at",
        ),
        (
            "page_drafts",
            "'discarded'",
            "id, page_id, scope, space_id, author_id, base_version, block_key, prompt, status, \
             source, spec, diagnostics, error, model_ref, attempts, created_at, expires_at",
        ),
    ];
    for (table, needed, columns) in rebuilds {
        let current = ddl(table)?;
        if current.contains(needed) {
            continue;
        }
        let fresh = current_table_ddl(table);
        conn.execute_batch(&format!(
            "SAVEPOINT upgrade_{table};
             ALTER TABLE {table} RENAME TO {table}_old;
             {fresh};
             INSERT INTO {table} ({columns}) SELECT {columns} FROM {table}_old;
             DROP TABLE {table}_old;
             RELEASE upgrade_{table};"
        ))?;
    }
    Ok(())
}

/// The current CREATE TABLE statement of `table`, from [`init`].
fn current_table_ddl(table: &str) -> String {
    let start = TABLES
        .find(&format!("CREATE TABLE IF NOT EXISTS {table} ("))
        .expect("a page table");
    let end = start + TABLES[start..].find(");").expect("its end") + 1;
    TABLES[start..end].replace("IF NOT EXISTS ", "")
}

/// Why a page run never got going: the server stopped first.
pub const RUN_INTERRUPTED: &str = "Interrupted by a server restart before the run started.";

/// At startup, no page run is starting: an origin that never got its run
/// id, or a page-only run that never recorded a start, is marked
/// interrupted (never replayed), and a page-only turn left `started` gets
/// an `interrupted` marker, like room and Chat turns.
pub fn recover_runs(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE page_runs SET error = ?1 WHERE error IS NULL AND (run_id = ''
           OR (space_id IS NULL AND NOT EXISTS (SELECT 1 FROM page_events e
               WHERE e.page_id = page_runs.page_id AND e.user_id = page_runs.user_id
                 AND e.kind = 'activity'
                 AND json_extract(e.activity, '$.id') = page_runs.run_id)))",
        [RUN_INTERRUPTED],
    )?;
    let mut stmt = conn.prepare(
        "SELECT e.page_id, e.user_id, e.activity, e.actor, e.actor_id FROM page_events e
         WHERE e.kind = 'activity' AND json_extract(e.activity, '$.phase') = 'started'
           AND NOT EXISTS (SELECT 1 FROM page_events l
               WHERE l.kind = 'activity' AND l.id > e.id
                 AND l.page_id = e.page_id AND l.user_id = e.user_id
                 AND json_extract(l.activity, '$.id') = json_extract(e.activity, '$.id'))",
    )?;
    let orphans: Vec<(String, String, String, String, Option<String>)> = stmt
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?
        .collect::<Result<_, _>>()?;
    for (page, user, activity, actor, actor_id) in orphans {
        let Some(json) = serde_json::from_str::<Activity>(&activity)
            .ok()
            .and_then(|a| serde_json::to_string(&a.at(crate::db::activity::INTERRUPTED)).ok())
        else {
            continue;
        };
        conn.execute(
            "INSERT INTO page_events (page_id, user_id, kind, actor, actor_id, text, activity, created_at)
             VALUES (?1, ?2, 'activity', ?3, ?4, '', ?5, ?6)",
            params![page, user, actor, actor_id, json, now_ms()],
        )?;
    }
    Ok(())
}

/// What the restart message says on a draft whose generation was cut off.
pub const INTERRUPTED: &str = "Interrupted by a server restart; try again.";

/// At startup no generation job is running: drafts still `generating`
/// were cut off by the restart (they fail, so their author sees why), and
/// `discarded` ones were only waiting for their job (they go). Either way
/// their generation slots are free again.
pub fn recover_drafts(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE page_drafts SET status = 'failed', error = ?1 WHERE status = 'generating'",
        [INTERRUPTED],
    )?;
    conn.execute("DELETE FROM page_drafts WHERE status = 'discarded'", [])?;
    Ok(())
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

/// Why a request may not touch a page: 404 (gone, deleted, or not visible
/// to them) or 403 (visible, but not this operation).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Denied {
    Gone,
    Forbidden,
}

/// The right an operation needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Need {
    View,
    Edit,
    Delete,
    Manage,
}

impl Need {
    fn allowed(self, rights: &Rights) -> bool {
        match self {
            Self::View => true,
            Self::Edit => rights.can_edit,
            Self::Delete => rights.can_delete,
            Self::Manage => rights.can_manage,
        }
    }
}

/// The live page `id` and `user`'s rights on it, read on `conn` (inside
/// the caller's transaction), or why not.
pub fn load_in(
    conn: &Connection,
    id: &str,
    user: &str,
    need: Need,
) -> rusqlite::Result<Result<(PageRow, Rights), Denied>> {
    let Some(page) = get_in(conn, id)?.filter(|p| p.deleted_at.is_none()) else {
        return Ok(Err(Denied::Gone));
    };
    let Some(rights) = rights_in(conn, &page, user)? else {
        return Ok(Err(Denied::Gone));
    };
    if !need.allowed(&rights) {
        return Ok(Err(Denied::Forbidden));
    }
    Ok(Ok((page, rights)))
}

/// Live pages of this Space only (metadata, never versions). The caller
/// holds the same lock as its agent grant check.
pub(crate) fn list_space_in(
    conn: &Connection,
    space: &str,
    after: &str,
    limit: usize,
) -> anyhow::Result<Vec<PageRow>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {PAGE_COLUMNS} FROM pages
         WHERE scope = 'space' AND space_id = ?1 AND deleted_at IS NULL AND id > ?2
         ORDER BY id LIMIT ?3",
    ))?;
    let rows = stmt.query_map(params![space, after, limit as i64], row_page)?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Scope is checked before edit rights, so pages outside the Space are
/// indistinguishable from unknown ids even when the caller can edit them.
pub(crate) fn load_space_in(
    conn: &Connection,
    space: &str,
    id: &str,
    user: &str,
    need: Need,
) -> rusqlite::Result<Result<(PageRow, Rights), Denied>> {
    let result = load_in(conn, id, user, Need::View)?;
    Ok(match result {
        Ok((page, rights))
            if page.scope == Scope::Space && page.space_id.as_deref() == Some(space) =>
        {
            if !need.allowed(&rights) {
                Err(Denied::Forbidden)
            } else {
                Ok((page, rights))
            }
        }
        _ => Err(Denied::Gone),
    })
}

#[derive(Debug, PartialEq, Eq)]
pub enum CreateError {
    /// The Space (or person) already has the most pages allowed.
    TooMany(u32),
    /// The author isn't a member of the Space (any more): 404.
    NotAllowed,
}

pub fn new_page_id() -> String {
    format!("pg_{}", uuid::Uuid::new_v4().simple())
}

fn live_count(conn: &Connection, space: Option<&str>, owner: &str) -> rusqlite::Result<i64> {
    match space {
        Some(sid) => conn.query_row(
            "SELECT COUNT(*) FROM pages WHERE space_id = ?1 AND deleted_at IS NULL",
            [sid],
            |r| r.get(0),
        ),
        None => conn.query_row(
            "SELECT COUNT(*) FROM pages WHERE owner_id = ?1 AND deleted_at IS NULL",
            [owner],
            |r| r.get(0),
        ),
    }
}

/// Create a page with version 1. `space` set: a Space page; otherwise a
/// personal page of `version.author_id`.
fn create_in(
    tx: &Transaction<'_>,
    space: Option<&str>,
    version: &NewVersion,
) -> anyhow::Result<Result<PageRow, CreateError>> {
    if let Some(sid) = space
        && member_role(tx, sid, &version.author_id)?.is_none()
    {
        return Ok(Err(CreateError::NotAllowed));
    }
    let limit = admin_settings::page_limits(tx)?.pages_per_space;
    if live_count(tx, space, &version.author_id)? >= i64::from(limit) {
        return Ok(Err(CreateError::TooMany(limit)));
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
    insert_version(tx, &id, 1, version, now)?;
    let page = get_in(tx, &id)?.ok_or_else(|| anyhow::anyhow!("reading the new page"))?;
    Ok(Ok(page))
}

/// [`create_in`] in its own transaction.
pub fn create(
    db: &Db,
    space: Option<&str>,
    version: &NewVersion,
) -> anyhow::Result<Result<PageRow, CreateError>> {
    let mut conn = db.lock();
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let out = create_in(&tx, space, version)?;
    if out.is_ok() {
        tx.commit()?;
    }
    Ok(out)
}

fn insert_version(
    conn: &Connection,
    page: &str,
    n: i64,
    v: &NewVersion,
    now: i64,
) -> anyhow::Result<()> {
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
    credit(conn, page, n, v)?;
    Ok(())
}

/// A stable digest of one action, for provenance.
pub fn action_hash(a: &Action) -> String {
    use sha2::Digest;
    let json = serde_json::to_vec(a).unwrap_or_default();
    sha2::Sha256::digest(&json)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn spec_actions(spec: &PageSpec) -> Vec<(Option<String>, Action)> {
    spec.all_blocks()
        .into_iter()
        .filter_map(|b| b.action.clone().map(|a| (b.key.clone(), a)))
        .collect()
}

/// Record which actions version `n` credits to its author, as compact
/// digests read once per request:
/// - `create`, `manual` (a source save) and `template`: every action;
/// - `prompt` (an applied AI draft): only actions it added or changed
///   relative to version `n - 1`, its base (none if that was pruned);
/// - `rename` and `restore`: none.
fn credit(conn: &Connection, page: &str, n: i64, v: &NewVersion) -> anyhow::Result<()> {
    let Ok(spec) = serde_json::from_str::<PageSpec>(&v.spec) else {
        return Ok(());
    };
    let actions: Vec<Action> = match v.kind {
        "create" | "manual" | "template" => {
            spec_actions(&spec).into_iter().map(|(_, a)| a).collect()
        }
        "prompt" => {
            let base: Option<String> = conn
                .query_row(
                    "SELECT spec FROM page_versions WHERE page_id = ?1 AND version = ?2",
                    params![page, n - 1],
                    |r| r.get(0),
                )
                .optional()?;
            let Some(base) = base.and_then(|s| serde_json::from_str::<PageSpec>(&s).ok()) else {
                return Ok(());
            };
            let before = spec_actions(&base);
            spec_actions(&spec)
                .into_iter()
                .filter(|(k, a)| !before.iter().any(|(bk, b)| bk == k && b == a))
                .map(|(_, a)| a)
                .collect()
        }
        _ => Vec::new(),
    };
    for a in actions {
        conn.execute(
            "INSERT OR IGNORE INTO page_action_authors (page_id, version, user_id, action_hash)
             VALUES (?1, ?2, ?3, ?4)",
            params![page, n, v.author_id, action_hash(&a)],
        )?;
    }
    Ok(())
}

/// Why a version wasn't saved.
#[derive(Debug, PartialEq, Eq)]
pub enum SaveError {
    /// The head isn't the version the save was based on.
    Conflict { head: i64 },
    /// Gone (404) or not an editor now (403), checked in the transaction.
    Denied(Denied),
}

/// Append version `base + 1` if `version.author_id` may edit the
/// live page now and its head is still `base` (optimistic concurrency:
/// never a silent overwrite), then prune old versions.
fn add_version_in(
    tx: &Transaction<'_>,
    page: &str,
    base: i64,
    version: &NewVersion,
) -> anyhow::Result<Result<i64, SaveError>> {
    let row = match load_in(tx, page, &version.author_id, Need::Edit)? {
        Ok((row, _)) => row,
        Err(d) => return Ok(Err(SaveError::Denied(d))),
    };
    let head = row.head_version;
    if head != base {
        return Ok(Err(SaveError::Conflict { head }));
    }
    let keep = admin_settings::page_limits(tx)?.versions_per_page;
    let n = head + 1;
    let now = now_ms();
    insert_version(tx, page, n, version, now)?;
    tx.execute(
        "UPDATE pages SET head_version = ?2, title = ?3, updated_at = ?4 WHERE id = ?1",
        params![page, n, version.title, now],
    )?;
    prune_versions(tx, page, n, keep)?;
    Ok(Ok(n))
}

/// [`add_version_in`] in its own transaction.
pub fn add_version(
    db: &Db,
    page: &str,
    base: i64,
    version: &NewVersion,
) -> anyhow::Result<Result<i64, SaveError>> {
    let mut conn = db.lock();
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let out = add_version_in(&tx, page, base, version)?;
    if out.is_ok() {
        tx.commit()?;
    }
    Ok(out)
}

/// Keep version 1 and the newest `keep` versions (and their provenance
/// and confirmations).
fn prune_versions(tx: &Transaction<'_>, page: &str, head: i64, keep: u32) -> rusqlite::Result<()> {
    let oldest_kept = head - i64::from(keep) + 1;
    for table in ["page_versions", "page_confirmations", "page_action_authors"] {
        tx.execute(
            &format!("DELETE FROM {table} WHERE page_id = ?1 AND version <> 1 AND version < ?2"),
            params![page, oldest_kept],
        )?;
    }
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
/// they own) within the restore window instead.
pub fn list_for(db: &Db, user: &str, deleted: bool) -> anyhow::Result<Vec<PageRow>> {
    let conn = db.lock();
    let sql = if deleted {
        format!(
            "SELECT {PAGE_COLUMNS} FROM pages p WHERE p.deleted_at IS NOT NULL AND p.deleted_at >= ?2
               AND (p.owner_id = ?1
                 OR (p.space_id IS NOT NULL AND EXISTS (SELECT 1 FROM space_members m
                     WHERE m.space_id = p.space_id AND m.user_id = ?1
                       AND (m.role = 'owner' OR p.creator_id = ?1))))
             ORDER BY p.deleted_at DESC"
        )
    } else {
        format!(
            "SELECT {PAGE_COLUMNS} FROM pages p WHERE p.deleted_at IS NULL AND ?2 = ?2 AND (
                 p.owner_id = ?1
                 OR (p.space_id IS NOT NULL AND EXISTS (SELECT 1 FROM space_members m
                     WHERE m.space_id = p.space_id AND m.user_id = ?1)))
             ORDER BY p.scope, p.space_id, p.created_at, p.id"
        )
    };
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![user, now_ms() - DELETED_RETENTION_MS], row_page)?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// One version's metadata (no source or spec).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VersionMeta {
    pub version: i64,
    pub author_id: String,
    pub kind: String,
    pub prompt: Option<String>,
    pub model_ref: Option<String>,
    pub restored_from: Option<i64>,
    pub created_at: i64,
}

/// The history list: metadata only, newest first.
pub fn version_list(db: &Db, page: &str) -> anyhow::Result<Vec<VersionMeta>> {
    let conn = db.lock();
    let mut stmt = conn.prepare_cached(
        "SELECT version, author_id, kind, prompt, model_ref, restored_from, created_at
         FROM page_versions WHERE page_id = ?1 ORDER BY version DESC",
    )?;
    let rows = stmt.query_map([page], |r| {
        Ok(VersionMeta {
            version: r.get(0)?,
            author_id: r.get(1)?,
            kind: r.get(2)?,
            prompt: r.get(3)?,
            model_ref: r.get(4)?,
            restored_from: r.get(5)?,
            created_at: r.get(6)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// The history list of `page` for `user`: the grant check and the read
/// under one lock, so a removal can't land between them.
pub fn version_list_for(
    db: &Db,
    page: &str,
    user: &str,
) -> anyhow::Result<Result<(i64, Vec<VersionMeta>), Denied>> {
    let conn = db.lock();
    let head = match load_in(&conn, page, user, Need::View)? {
        Ok((p, _)) => p.head_version,
        Err(d) => return Ok(Err(d)),
    };
    let mut stmt = conn.prepare_cached(
        "SELECT version, author_id, kind, prompt, model_ref, restored_from, created_at
         FROM page_versions WHERE page_id = ?1 ORDER BY version DESC",
    )?;
    let rows = stmt.query_map([page], |r| {
        Ok(VersionMeta {
            version: r.get(0)?,
            author_id: r.get(1)?,
            kind: r.get(2)?,
            prompt: r.get(3)?,
            model_ref: r.get(4)?,
            restored_from: r.get(5)?,
            created_at: r.get(6)?,
        })
    })?;
    Ok(Ok((head, rows.collect::<Result<_, _>>()?)))
}

/// Versions `ns` of `page` for `user` (each `None` if it doesn't exist),
/// with the grant check under the same lock as the reads. `n = 0` means
/// the head.
pub fn versions_for(
    db: &Db,
    page: &str,
    user: &str,
    ns: &[i64],
) -> anyhow::Result<Result<Vec<Option<VersionRow>>, Denied>> {
    let conn = db.lock();
    let head = match load_in(&conn, page, user, Need::View)? {
        Ok((p, _)) => p.head_version,
        Err(d) => return Ok(Err(d)),
    };
    let mut out = Vec::new();
    for n in ns {
        let n = if *n == 0 { head } else { *n };
        out.push(version_in(&conn, page, n)?);
    }
    Ok(Ok(out))
}

pub(crate) fn version_in(
    conn: &Connection,
    page: &str,
    n: i64,
) -> rusqlite::Result<Option<VersionRow>> {
    conn.query_row(
        &format!("SELECT {VERSION_COLUMNS} FROM page_versions WHERE page_id = ?1 AND version = ?2"),
        params![page, n],
        row_version,
    )
    .optional()
}

pub fn version(db: &Db, page: &str, n: i64) -> anyhow::Result<Option<VersionRow>> {
    Ok(version_in(&db.lock(), page, n)?)
}

/// A combined edit of a page's settings, applied all or nothing.
#[derive(Debug, Default)]
pub struct PatchOps {
    /// A rename: the new version and the head it's based on.
    pub rename: Option<(i64, NewVersion)>,
    pub icon: Option<Option<String>>,
    pub members_edit: Option<bool>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PatchError {
    Denied(Denied),
    Conflict {
        head: i64,
    },
    /// `members_edit` on a personal page.
    NotSpacePage,
}

/// Apply `ops` for `user` in one transaction: every right is checked on
/// the live page there, and nothing is written unless everything may be.
pub fn patch(
    db: &Db,
    page: &str,
    user: &str,
    ops: &PatchOps,
) -> anyhow::Result<Result<(), PatchError>> {
    let mut conn = db.lock();
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let (row, rights) = match load_in(&tx, page, user, Need::View)? {
        Ok(v) => v,
        Err(d) => return Ok(Err(PatchError::Denied(d))),
    };
    if ops.members_edit.is_some() {
        if row.space_id.is_none() {
            return Ok(Err(PatchError::NotSpacePage));
        }
        if !rights.can_manage {
            return Ok(Err(PatchError::Denied(Denied::Forbidden)));
        }
    }
    if (ops.icon.is_some() || ops.rename.is_some()) && !rights.can_edit {
        return Ok(Err(PatchError::Denied(Denied::Forbidden)));
    }
    if let Some((base, version)) = &ops.rename {
        match add_version_in(&tx, page, *base, version)? {
            Ok(_) => {}
            Err(SaveError::Conflict { head }) => return Ok(Err(PatchError::Conflict { head })),
            Err(SaveError::Denied(d)) => return Ok(Err(PatchError::Denied(d))),
        }
    }
    let now = now_ms();
    if let Some(icon) = &ops.icon {
        tx.execute(
            "UPDATE pages SET icon = ?2, updated_at = ?3 WHERE id = ?1",
            params![page, icon, now],
        )?;
    }
    if let Some(m) = ops.members_edit {
        tx.execute(
            "UPDATE pages SET members_edit = ?2, updated_at = ?3 WHERE id = ?1",
            params![page, m as i64, now],
        )?;
    }
    tx.commit()?;
    Ok(Ok(()))
}

/// Soft-delete the live page for `user`, if they may delete it now.
pub fn soft_delete(db: &Db, page: &str, user: &str) -> anyhow::Result<Result<(), Denied>> {
    let mut conn = db.lock();
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if let Err(d) = load_in(&tx, page, user, Need::Delete)? {
        return Ok(Err(d));
    }
    tx.execute(
        "UPDATE pages SET deleted_at = ?2, deleted_by = ?3 WHERE id = ?1 AND deleted_at IS NULL",
        params![page, now_ms(), user],
    )?;
    tx.commit()?;
    Ok(Ok(()))
}

#[derive(Debug, PartialEq, Eq)]
pub enum UndeleteError {
    Denied(Denied),
    NotDeleted,
    TooMany(u32),
}

/// Undelete for `user`, if they may restore it now, it's within the restore
/// window, and the Space (or person) still has room for it.
pub fn undelete(db: &Db, page: &str, user: &str) -> anyhow::Result<Result<(), UndeleteError>> {
    let mut conn = db.lock();
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let Some(row) = get_in(&tx, page)? else {
        return Ok(Err(UndeleteError::Denied(Denied::Gone)));
    };
    // Beyond the window, or without the right: as if it didn't exist.
    if rights_in(&tx, &row, user)?.is_none_or(|r| !r.can_delete) {
        return Ok(Err(UndeleteError::Denied(Denied::Gone)));
    }
    if row.deleted_at.is_none() {
        return Ok(Err(UndeleteError::NotDeleted));
    }
    let owner = row.owner_id.as_deref().unwrap_or_default();
    let limit = admin_settings::page_limits(&tx)?.pages_per_space;
    if live_count(&tx, row.space_id.as_deref(), owner)? >= i64::from(limit) {
        return Ok(Err(UndeleteError::TooMany(limit)));
    }
    tx.execute(
        "UPDATE pages SET deleted_at = NULL, deleted_by = NULL, updated_at = ?2 WHERE id = ?1",
        params![page, now_ms()],
    )?;
    tx.commit()?;
    Ok(Ok(()))
}

/// Purge pages deleted more than [`DELETED_RETENTION_MS`] ago and expired
/// drafts that no job is working on, as of `now`.
pub fn purge_expired(db: &Db, now: i64) -> anyhow::Result<usize> {
    let conn = db.lock();
    let pages = conn.execute(
        "DELETE FROM pages WHERE deleted_at IS NOT NULL AND deleted_at < ?1",
        [now - DELETED_RETENTION_MS],
    )?;
    conn.execute(
        "DELETE FROM page_drafts WHERE expires_at < ?1 AND status IN ('ready', 'failed')",
        [now],
    )?;
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

/// Save `user`'s preferences for a page they can see now.
pub fn put_prefs(db: &Db, user: &str, page: &str, p: &Prefs) -> anyhow::Result<Result<(), Denied>> {
    let mut conn = db.lock();
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if let Err(d) = load_in(&tx, page, user, Need::View)? {
        return Ok(Err(d));
    }
    tx.execute(
        "INSERT INTO page_prefs (user_id, page_id, pinned, position) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (user_id, page_id) DO UPDATE SET pinned = ?3, position = ?4",
        params![user, page, p.pinned as i64, p.position],
    )?;
    tx.commit()?;
    Ok(Ok(()))
}

// ---- Confirmations and provenance ----

fn confirmed_in(conn: &Connection, user: &str, page: &str, version: i64) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM page_confirmations
             WHERE user_id = ?1 AND page_id = ?2 AND version = ?3)",
        params![user, page, version],
        |r| r.get(0),
    )
}

pub fn confirmed(db: &Db, user: &str, page: &str, version: i64) -> anyhow::Result<bool> {
    Ok(confirmed_in(&db.lock(), user, page, version)?)
}

/// What `user` already vouched for on one page version: whether they
/// confirmed that version, and the digests of actions credited to them
/// ([`credit`]). Read once per request and asked per button.
#[derive(Debug, Default)]
pub struct Vouched {
    confirmed: bool,
    authored: std::collections::HashSet<String>,
}

impl Vouched {
    /// Whether running `action` needs the confirmation sheet.
    pub fn needs_confirmation(&self, action: &Action) -> bool {
        !self.confirmed && !self.authored.contains(&action_hash(action))
    }
}

fn vouched_in(
    conn: &Connection,
    page: &str,
    version: i64,
    user: &str,
) -> rusqlite::Result<Vouched> {
    if confirmed_in(conn, user, page, version)? {
        return Ok(Vouched {
            confirmed: true,
            authored: Default::default(),
        });
    }
    let mut stmt = conn.prepare_cached(
        "SELECT DISTINCT action_hash FROM page_action_authors WHERE page_id = ?1 AND user_id = ?2",
    )?;
    let authored = stmt
        .query_map(params![page, user], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    Ok(Vouched {
        confirmed: false,
        authored,
    })
}

pub fn vouched(db: &Db, page: &str, version: i64, user: &str) -> anyhow::Result<Vouched> {
    Ok(vouched_in(&db.lock(), page, version, user)?)
}

// ---- Claiming a button ----

/// Where a button sends its work, checked live.
pub enum Destination {
    Room {
        space_id: String,
        room: Room,
        agent: Box<crate::space_agents::SpaceAgent>,
    },
    /// The clicker's page-only conversation.
    Chat,
    Schedule {
        task: Box<crate::schedules::Task>,
        room: Room,
    },
}

impl ActionError {
    /// The reason a disabled button shows.
    pub fn reason(&self) -> &'static str {
        match self {
            ActionError::PersonalOnly => {
                "A personal page's buttons only start private runs shown on the page."
            }
            ActionError::Forbidden(m) | ActionError::Gone(m) => m,
            ActionError::Thread => "A button can't send work to a thread.",
            _ => "This button can't run.",
        }
    }
}

/// The one destination policy, shared by the click claim and `/data`:
/// - a personal page's only destination is the clicker's page-only run;
/// - a Space page's `runAgent` goes to one of its top-level rooms (never a
///   thread) and an agent of the Space that still exists;
/// - a Space page's `runSchedule` runs one of the clicker's own tasks that
///   targets this Space and one of its top-level rooms.
pub fn destination_in(
    conn: &Connection,
    page_space: Option<&str>,
    a: &Action,
    user: &str,
) -> rusqlite::Result<Result<Destination, ActionError>> {
    use super::spec::action as kinds;
    let room_in = |sid: &str, rid: &str| -> rusqlite::Result<Option<Room>> {
        conn.query_row(
            "SELECT id, name, parent_room_id FROM space_rooms WHERE space_id = ?1 AND id = ?2",
            params![sid, rid],
            |r| {
                Ok(Room {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    parent_room_id: r.get(2)?,
                })
            },
        )
        .optional()
    };
    let Some(sid) = page_space else {
        let page_only = a.kind == kinds::RUN_AGENT && a.target.chat && a.target.space_id.is_none();
        return Ok(if page_only {
            Ok(Destination::Chat)
        } else {
            Err(ActionError::PersonalOnly)
        });
    };
    if a.target.chat || a.target.space_id.is_some() {
        return Ok(Err(ActionError::Forbidden("this button can't run")));
    }
    if a.kind == kinds::RUN_AGENT {
        let rid = a.target.room_id.clone().unwrap_or_default();
        let Some(room) = room_in(sid, &rid)? else {
            return Ok(Err(ActionError::Gone(
                "this button's room no longer exists",
            )));
        };
        if room.parent_room_id.is_some() {
            return Ok(Err(ActionError::Thread));
        }
        let agent_id = a.target.agent_id.clone().unwrap_or_default();
        let Some(agent) = crate::space_agents::get_in(conn, sid, &agent_id)? else {
            return Ok(Err(ActionError::Gone(
                "this button's agent no longer exists",
            )));
        };
        return Ok(Ok(Destination::Room {
            space_id: sid.to_owned(),
            room,
            agent: Box::new(agent),
        }));
    }
    let id = a.schedule_id.clone().unwrap_or_default();
    let found = crate::schedules::get_task_in(conn, &id)?.filter(|t| t.owner_id == user);
    let Some(task) = found else {
        return Ok(Err(ActionError::Forbidden(
            "only the schedule's owner can run it",
        )));
    };
    let Some((tsid, trid)) = task.target.clone().filter(|(s, _)| s == sid) else {
        return Ok(Err(ActionError::Forbidden(
            "this schedule doesn't run in this page's Space",
        )));
    };
    match room_in(&tsid, &trid)? {
        None => Ok(Err(ActionError::Gone(
            "this schedule's room no longer exists",
        ))),
        Some(r) if r.parent_room_id.is_some() => Ok(Err(ActionError::Forbidden(
            "this schedule runs in a thread; a page can only run schedules of top-level rooms",
        ))),
        Some(room) => Ok(Ok(Destination::Schedule {
            task: Box::new(task),
            room,
        })),
    }
}

/// A click on a page button that runs something.
pub struct ActionRequest<'a> {
    pub page: &'a str,
    pub user: &'a str,
    pub author: &'a Author,
    pub version: i64,
    pub block_key: &'a str,
    pub confirm: bool,
    /// The schedule revision the confirmation sheet showed.
    pub schedule_revision: Option<i64>,
    /// The id the page-only Chat turn will run as.
    pub chat_turn: &'a str,
    /// The server's message limit: a longer prompt is refused.
    pub max_prompt_chars: usize,
}

/// A claimed click: its trigger (if any) is already stored, as is its run
/// origin and the person's consent. Dispatch exactly this. (One short-lived
/// value per click, so the variants' size difference doesn't matter.)
#[allow(clippy::large_enum_variant)]
pub enum Claimed {
    Room {
        event: Event,
        space_id: String,
        room: Room,
        agent: crate::space_agents::SpaceAgent,
        page_run: i64,
        page_title: String,
    },
    Chat {
        event: Event,
        page_run: i64,
    },
    Schedule {
        claim: crate::schedules::Claim,
        task: crate::schedules::Task,
    },
}

#[derive(Debug)]
pub enum ActionError {
    Denied(Denied),
    Conflict {
        head: i64,
    },
    NoButton,
    /// Opening rooms or pages and refreshing happen in the app.
    Navigation,
    /// A personal page's button that isn't a page-only run.
    PersonalOnly,
    Forbidden(&'static str),
    /// The destination room or agent is gone.
    Gone(&'static str),
    Thread,
    /// Confirm first; the payload describes exactly what would run.
    NeedsConfirmation(serde_json::Value),
    /// The schedule changed since the revision that was confirmed.
    ScheduleChanged {
        revision: i64,
    },
    Schedule(crate::schedules::ScheduleError),
    /// The button's prompt is longer than a message may be.
    TooLong,
}

/// Check and claim one click in a single transaction: the page is live and
/// visible to the clicker, the version is still the head, the button exists
/// and may run from this page, its destination is valid now, the clicker
/// vouched for it (or confirms now, which is recorded), and then its
/// trigger and run origin (or the schedule occurrence) are written. A
/// click on a superseded version writes nothing.
pub fn claim_action(
    db: &Db,
    req: &ActionRequest<'_>,
) -> anyhow::Result<Result<Claimed, ActionError>> {
    use super::spec::action as kinds;
    use crate::db::kind;
    let mut conn = db.lock();
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let (page, _) = match load_in(&tx, req.page, req.user, Need::View)? {
        Ok(v) => v,
        Err(d) => return Ok(Err(ActionError::Denied(d))),
    };
    if page.head_version != req.version {
        return Ok(Err(ActionError::Conflict {
            head: page.head_version,
        }));
    }
    let head = version_in(&tx, &page.id, req.version)?
        .ok_or_else(|| anyhow::anyhow!("a page without its head version"))?;
    let spec: PageSpec = serde_json::from_str(&head.spec)?;
    let Some(a) = spec.block(req.block_key).and_then(|b| b.action.clone()) else {
        return Ok(Err(ActionError::NoButton));
    };
    if a.kind != kinds::RUN_AGENT && a.kind != kinds::RUN_SCHEDULE {
        return Ok(Err(ActionError::Navigation));
    }
    if a.prompt
        .as_ref()
        .is_some_and(|p| p.chars().count() > req.max_prompt_chars)
    {
        return Ok(Err(ActionError::TooLong));
    }
    // The destination, live, by the same policy `/data` shows.
    let page_space = page.space_id.clone();
    let (room_agent, task) = match destination_in(&tx, page_space.as_deref(), &a, req.user)? {
        Ok(Destination::Room {
            space_id,
            room,
            agent,
        }) => (Some((space_id, room, *agent)), None),
        Ok(Destination::Chat) => (None, None),
        Ok(Destination::Schedule { task, room }) => (None, Some((*task, room))),
        Err(e) => return Ok(Err(e)),
    };
    // Consent.
    let vouched = vouched_in(&tx, &page.id, req.version, req.user)?;
    if vouched.needs_confirmation(&a) {
        if !req.confirm {
            let what = match (&task, &room_agent) {
                (Some((t, room)), _) => serde_json::json!({
                    "kind": kinds::RUN_SCHEDULE,
                    "schedule_id": t.id,
                    "schedule": t.name,
                    "schedule_revision": t.revision,
                    "prompt": t.prompt,
                    "space_id": page_space,
                    "room_id": room.id,
                    "room": room.name,
                }),
                (None, Some((sid, room, agent))) => serde_json::json!({
                    "kind": kinds::RUN_AGENT,
                    "prompt": a.prompt,
                    "agent": agent.handle,
                    "agent_id": agent.id,
                    "agent_name": agent.name,
                    "room": room.name,
                    "room_id": room.id,
                    "space_id": sid,
                    "chat": false,
                }),
                (None, None) => serde_json::json!({
                    "kind": kinds::RUN_AGENT,
                    "prompt": a.prompt,
                    "agent": null,
                    "agent_id": null,
                    "room": null,
                    "room_id": null,
                    "space_id": null,
                    "chat": true,
                }),
            };
            return Ok(Err(ActionError::NeedsConfirmation(serde_json::json!({
                "page_id": page.id,
                "version": head.version,
                "block_key": req.block_key,
                "edited_by": head.author_id,
                "edited_at": crate::db::rfc3339_utc(head.created_at),
                "runs_as": req.user,
                "action": what,
            }))));
        }
        // A schedule confirmation is bound to the revision it showed.
        if let Some((t, _)) = &task
            && req.schedule_revision != Some(t.revision)
        {
            return Ok(Err(ActionError::ScheduleChanged {
                revision: t.revision,
            }));
        }
        tx.execute(
            "INSERT OR IGNORE INTO page_confirmations (user_id, page_id, version, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![req.user, page.id, req.version, now_ms()],
        )?;
    }
    if let (Some((t, _)), Some(r)) = (&task, req.schedule_revision)
        && t.revision != r
    {
        return Ok(Err(ActionError::ScheduleChanged {
            revision: t.revision,
        }));
    }
    let now = now_ms();
    let claimed = if let Some((t, _)) = task {
        match crate::schedules::claim_now_in(
            &tx,
            req.user,
            &t.id,
            Some(t.revision),
            page_space.as_deref(),
            chrono::Utc::now(),
        ) {
            Ok((claim, task)) => Claimed::Schedule { claim, task },
            Err(e) => return Ok(Err(ActionError::Schedule(e))),
        }
    } else {
        let text = a.prompt.clone().unwrap_or_default();
        match room_agent {
            Some((sid, room, agent)) => {
                let addressed = crate::db::Addressed {
                    mentions: serde_json::json!([{"kind": "agent", "agent_id": agent.id}]),
                    audience: "agent".to_owned(),
                };
                tx.execute(
                    "INSERT INTO space_events (space_id, room_id, kind, actor, actor_id, text, \
                     mentions, audience, created_at, activity) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL)",
                    params![
                        sid,
                        room.id,
                        kind::MESSAGE,
                        req.author.name,
                        req.author.id,
                        text,
                        addressed.mentions.to_string(),
                        addressed.audience,
                        now
                    ],
                )?;
                let eid = tx.last_insert_rowid();
                let event = crate::db::new_event(
                    eid,
                    kind::MESSAGE,
                    req.author,
                    &text,
                    &addressed,
                    None,
                    now,
                );
                let page_run = insert_run(
                    &tx,
                    &page.id,
                    req.version,
                    req.block_key,
                    req.user,
                    Some((&sid, &room.id)),
                    eid,
                    "",
                )?;
                Claimed::Room {
                    event,
                    space_id: sid,
                    room,
                    agent,
                    page_run,
                    page_title: page.title.clone(),
                }
            }
            None => {
                tx.execute(
                    "INSERT INTO page_events (page_id, user_id, kind, actor, actor_id, text, \
                     activity, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, ?7)",
                    params![
                        page.id,
                        req.user,
                        kind::MESSAGE,
                        req.author.name,
                        req.author.id,
                        text,
                        now
                    ],
                )?;
                let eid = tx.last_insert_rowid();
                let event = crate::db::new_event(
                    eid,
                    kind::MESSAGE,
                    req.author,
                    &text,
                    &crate::agent::agent_addressed(),
                    None,
                    now,
                );
                let page_run = insert_run(
                    &tx,
                    &page.id,
                    req.version,
                    req.block_key,
                    req.user,
                    None,
                    eid,
                    req.chat_turn,
                )?;
                Claimed::Chat { event, page_run }
            }
        }
    };
    tx.commit()?;
    Ok(Ok(claimed))
}

// ---- Run origins ----

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageRun {
    pub version: i64,
    pub user_id: String,
    pub space_id: Option<String>,
    pub room_id: Option<String>,
    pub trigger_id: i64,
    /// Empty until the run started; then its activity id.
    pub run_id: String,
    /// Why the run couldn't start.
    pub error: Option<String>,
    pub created_at: i64,
}

#[allow(clippy::too_many_arguments)]
fn insert_run(
    conn: &Connection,
    page: &str,
    version: i64,
    block: &str,
    user: &str,
    target: Option<(&str, &str)>,
    trigger_id: i64,
    run_id: &str,
) -> rusqlite::Result<i64> {
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
    let id = conn.last_insert_rowid();
    // Bounded: the newest 200 per block are enough to find answers.
    conn.execute(
        "DELETE FROM page_runs WHERE page_id = ?1 AND block_key = ?2 AND id NOT IN
             (SELECT id FROM page_runs WHERE page_id = ?1 AND block_key = ?2
              ORDER BY id DESC LIMIT 200)",
        params![page, block],
    )?;
    Ok(id)
}

/// Record how a claimed run went: its run id, or why it couldn't start.
pub fn set_run(
    db: &Db,
    page_run: i64,
    run_id: Option<&str>,
    error: Option<&str>,
) -> anyhow::Result<()> {
    db.lock().execute(
        "UPDATE page_runs SET run_id = COALESCE(?2, run_id), error = ?3 WHERE id = ?1",
        params![page_run, run_id, error],
    )?;
    Ok(())
}

/// The latest run started from `block` of `page`; with `user`, only theirs.
pub fn latest_run(
    db: &Db,
    viewer: &str,
    page: &str,
    block: &str,
    user: Option<&str>,
) -> anyhow::Result<Option<PageRun>> {
    let conn = db.lock();
    // The viewer's grant on the page is in the same statement as the row:
    // its owner, or a member of its Space now.
    Ok(conn
        .query_row(
            "SELECT r.version, r.user_id, r.space_id, r.room_id, r.trigger_id, r.run_id, r.error,
                    r.created_at
             FROM page_runs r JOIN pages p ON p.id = r.page_id
             WHERE r.page_id = ?1 AND r.block_key = ?2
               AND (?3 IS NULL OR r.user_id = ?3)
               AND p.deleted_at IS NULL
               AND (p.owner_id = ?4 OR EXISTS (SELECT 1 FROM space_members m
                    WHERE m.space_id = p.space_id AND m.user_id = ?4))
             ORDER BY r.id DESC LIMIT 1",
            params![page, block, user, viewer],
            |r| {
                Ok(PageRun {
                    version: r.get(0)?,
                    user_id: r.get(1)?,
                    space_id: r.get(2)?,
                    room_id: r.get(3)?,
                    trigger_id: r.get(4)?,
                    run_id: r.get(5)?,
                    error: r.get(6)?,
                    created_at: r.get(7)?,
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
/// optionally of one activity. Tool errors are not answers.
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
               AND (kind <> 'error' OR
                    (actor = 'agent' AND actor_id IS NULL AND json_extract(activity, '$.tool') IS NULL))
               AND (?4 IS NULL OR json_extract(activity, '$.id') = ?4)
             ORDER BY id DESC LIMIT 1",
            params![page, user, kinds, activity_id],
            row_page_event,
        )
        .optional()?)
}

/// Newest room events of `kinds` (optionally of one activity id or one
/// agent id), newest first, read only while `viewer` is a member of the
/// Space: the membership check is in the same statement as the rows.
/// Page sources exclude scoped tool errors before applying the limit:
/// only the worker's own unscoped error ends an agent run.
#[allow(clippy::too_many_arguments)]
pub fn room_events(
    db: &Db,
    viewer: &str,
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
           AND (kind <> 'error' OR
                (actor = 'agent' AND actor_id IS NULL AND json_extract(activity, '$.tool') IS NULL))
           AND (?4 IS NULL OR json_extract(activity, '$.id') = ?4)
           AND (?5 IS NULL OR json_extract(activity, '$.agent.id') = ?5)
           AND EXISTS (SELECT 1 FROM space_members m
                       WHERE m.space_id = ?1 AND m.user_id = ?7)
         ORDER BY id DESC LIMIT ?6",
    )?;
    let rows = stmt.query_map(
        params![
            space,
            room,
            kinds,
            activity_id,
            agent_id,
            limit as i64,
            viewer
        ],
        row_page_event,
    )?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Room `room` of `space` if `viewer` is a member of the Space now: the
/// membership check is in the same statement as the row.
pub fn room_for(db: &Db, viewer: &str, space: &str, room: &str) -> anyhow::Result<Option<Room>> {
    let conn = db.lock();
    Ok(conn
        .query_row(
            "SELECT r.id, r.name, r.parent_room_id FROM space_rooms r
             WHERE r.space_id = ?1 AND r.id = ?2
               AND EXISTS (SELECT 1 FROM space_members m
                           WHERE m.space_id = r.space_id AND m.user_id = ?3)",
            params![space, room, viewer],
            |r| {
                Ok(Room {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    parent_room_id: r.get(2)?,
                })
            },
        )
        .optional()?)
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

/// A room event's public draft card. The draft endpoint still returns
/// 404 to anyone but its author, even if they know the id from this card.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftCard {
    pub draft_id: String,
    pub title: String,
    pub summary: String,
    pub kind: DraftKind,
    pub page_id: Option<String>,
    pub author_id: String,
    pub expires_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DraftKind {
    New,
    Edit,
}

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
    pub block_key: Option<&'a str>,
    pub prompt: &'a str,
    /// A draft older than this no longer holds a generation slot even if
    /// it still says it's generating: no job runs that long (defence in
    /// depth on top of the startup recovery and the job's drop guard).
    pub busy_window_ms: i64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum DraftError {
    Denied(Denied),
    /// The author already has [`MAX_GENERATING_PER_PERSON`] generating.
    Busy,
    /// `block_key` isn't a block of the page's head.
    NoBlock,
    /// A model supplied a replacement for a specific version, now stale.
    Stale,
}

/// Create a draft in `generating`, reserving one of its author's
/// generation slots. In the same transaction: an edit draft needs the live
/// page and edit rights now (its base is the head now); a new Space page's
/// draft needs membership now.
pub fn create_draft(db: &Db, d: &NewDraft<'_>) -> anyhow::Result<Result<DraftRow, DraftError>> {
    Ok(create_draft_with_base(db, d)?.map(|(row, _)| row))
}

/// [`create_draft`], also returning the base version (for an edit), read
/// in the same transaction as the reservation.
pub(crate) type Reservation = (DraftRow, Option<VersionRow>);

pub fn create_draft_with_base(
    db: &Db,
    d: &NewDraft<'_>,
) -> anyhow::Result<Result<Reservation, DraftError>> {
    let mut conn = db.lock();
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let result = reserve_draft_in(&tx, d, None)?;
    if result.is_ok() {
        tx.commit()?;
    }
    Ok(result)
}

/// Shared reservation for both HTTP and agent drafts. The latter also
/// check the agent grant and insert the room card in this transaction.
pub(crate) fn reserve_draft_in(
    tx: &rusqlite::Transaction<'_>,
    d: &NewDraft<'_>,
    expected_head: Option<i64>,
) -> anyhow::Result<Result<Reservation, DraftError>> {
    let base = match d.page_id {
        Some(pid) => {
            let loaded = match d.space_id {
                Some(space) => load_space_in(tx, space, pid, d.author_id, Need::Edit)?,
                None => match load_in(tx, pid, d.author_id, Need::View)? {
                    Ok((page, rights)) if page.scope == Scope::Personal && rights.can_edit => {
                        Ok((page, rights))
                    }
                    _ => Err(Denied::Gone),
                },
            };
            let (page, _) = match loaded {
                Ok(v) => v,
                Err(e) => return Ok(Err(DraftError::Denied(e))),
            };
            if expected_head.is_some_and(|head| head != page.head_version) {
                return Ok(Err(DraftError::Stale));
            }
            if let Some(k) = d.block_key {
                let head = version_in(tx, pid, page.head_version)?
                    .and_then(|v| serde_json::from_str::<PageSpec>(&v.spec).ok());
                if head.is_none_or(|s| s.block(k).is_none()) {
                    return Ok(Err(DraftError::NoBlock));
                }
            }
            Some(page.head_version)
        }
        None => {
            if let Some(sid) = d.space_id
                && member_role(tx, sid, d.author_id)?.is_none()
            {
                return Ok(Err(DraftError::Denied(Denied::Gone)));
            }
            None
        }
    };
    let busy: i64 = tx.query_row(
        "SELECT COUNT(*) FROM page_drafts WHERE author_id = ?1
             AND status IN ('generating', 'discarded') AND created_at > ?2",
        params![d.author_id, now_ms() - d.busy_window_ms],
        |r| r.get(0),
    )?;
    if busy >= MAX_GENERATING_PER_PERSON {
        return Ok(Err(DraftError::Busy));
    }
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
            base,
            d.block_key,
            d.prompt,
            now,
            now + DRAFT_RETENTION_MS
        ],
    )?;
    tx.execute(
        "DELETE FROM page_drafts WHERE author_id = ?1
             AND status NOT IN ('generating', 'discarded') AND id NOT IN
             (SELECT id FROM page_drafts WHERE author_id = ?1 ORDER BY created_at DESC, rowid DESC
              LIMIT ?2)",
        params![d.author_id, MAX_DRAFTS_PER_PERSON as i64],
    )?;
    let row = tx.query_row(
        &format!("SELECT {DRAFT_COLUMNS} FROM page_drafts WHERE id = ?1"),
        [&id],
        row_draft,
    )?;
    let base_row = match base {
        Some(n) => version_in(tx, d.page_id.unwrap_or_default(), n)?,
        None => None,
    };
    Ok(Ok((row, base_row)))
}

/// A draft, unless it was discarded or has expired.
pub fn draft(db: &Db, id: &str) -> anyhow::Result<Option<DraftRow>> {
    let conn = db.lock();
    Ok(conn
        .query_row(
            &format!(
                "SELECT {DRAFT_COLUMNS} FROM page_drafts
                 WHERE id = ?1 AND status <> 'discarded' AND expires_at > ?2"
            ),
            params![id, now_ms()],
            row_draft,
        )
        .optional()?)
}

/// Whether draft `id`'s job may still call a model for `user`, decided in
/// one read: the draft is still generating (not discarded, failed or
/// expired) and theirs, they're still a member of its Space, and for an
/// edit the page is live and they may still edit it, and they're still a
/// member of every Space in `spaces` (those the request names).
pub fn draft_live(db: &Db, id: &str, user: &str, spaces: &[String]) -> anyhow::Result<bool> {
    draft_live_in(&db.lock(), id, user, spaces)
}

pub(crate) fn draft_live_in(
    conn: &Connection,
    id: &str,
    user: &str,
    spaces: &[String],
) -> anyhow::Result<bool> {
    let row: Option<(Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT page_id, space_id FROM page_drafts
             WHERE id = ?1 AND author_id = ?2 AND status = 'generating' AND expires_at > ?3",
            params![id, user, now_ms()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((page, space)) = row else {
        return Ok(false);
    };
    if let Some(sid) = &space
        && member_role(conn, sid, user)?.is_none()
    {
        return Ok(false);
    }
    if let Some(pid) = &page
        && load_in(conn, pid, user, Need::Edit)?.is_err()
    {
        return Ok(false);
    }
    // Every Space the request names, still theirs.
    for sid in spaces {
        if member_role(conn, sid, user)?.is_none() {
            return Ok(false);
        }
    }
    Ok(true)
}

#[derive(Debug, PartialEq, Eq)]
pub enum ApplyError {
    /// Gone, someone else's, discarded or expired: 404.
    Gone,
    NotReady,
    Denied(Denied),
    Conflict {
        head: i64,
    },
    TooMany(u32),
}

/// Apply draft `id` for its author `user` in one transaction: the draft is
/// still ready, then the page is created or the version saved (with every
/// check those do), then the draft is consumed. On any refusal nothing
/// changes and the draft stays; a concurrent apply or discard can't both
/// succeed. Returns the page id.
pub fn apply_draft(
    db: &Db,
    id: &str,
    user: &str,
    version: &NewVersion,
) -> anyhow::Result<Result<String, ApplyError>> {
    let mut conn = db.lock();
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let d = tx
        .query_row(
            &format!(
                "SELECT {DRAFT_COLUMNS} FROM page_drafts
                 WHERE id = ?1 AND author_id = ?2 AND status <> 'discarded' AND expires_at > ?3"
            ),
            params![id, user, now_ms()],
            row_draft,
        )
        .optional()?;
    let Some(d) = d else {
        return Ok(Err(ApplyError::Gone));
    };
    if d.status != "ready" {
        return Ok(Err(ApplyError::NotReady));
    }
    let page_id = match &d.page_id {
        Some(pid) => {
            let base = d.base_version.unwrap_or(-1);
            match add_version_in(&tx, pid, base, version)? {
                Ok(_) => pid.clone(),
                Err(SaveError::Conflict { head }) => return Ok(Err(ApplyError::Conflict { head })),
                Err(SaveError::Denied(e)) => return Ok(Err(ApplyError::Denied(e))),
            }
        }
        None => match create_in(&tx, d.space_id.as_deref(), version)? {
            Ok(page) => page.id,
            Err(CreateError::TooMany(n)) => return Ok(Err(ApplyError::TooMany(n))),
            Err(CreateError::NotAllowed) => return Ok(Err(ApplyError::Denied(Denied::Gone))),
        },
    };
    tx.execute("DELETE FROM page_drafts WHERE id = ?1", [id])?;
    tx.commit()?;
    Ok(Ok(page_id))
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
    finish_draft_in(
        &db.lock(),
        id,
        status,
        source,
        spec,
        diagnostics,
        error,
        model_ref,
        attempts,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn finish_draft_in(
    conn: &Connection,
    id: &str,
    status: &str,
    source: Option<&str>,
    spec: Option<&str>,
    diagnostics: Option<&str>,
    error: Option<&str>,
    model_ref: Option<&str>,
    attempts: i64,
) -> anyhow::Result<()> {
    conn.execute(
        "UPDATE page_drafts SET status = ?2, source = ?3, spec = ?4, diagnostics = ?5, error = ?6, \
         model_ref = ?7, attempts = ?8 WHERE id = ?1 AND status = 'generating'",
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
    // A draft discarded while it was generating goes once its job is done;
    // until then it kept its generation slot.
    conn.execute(
        "DELETE FROM page_drafts WHERE id = ?1 AND status = 'discarded'",
        [id],
    )?;
    Ok(())
}

/// Discard a draft for its author: a finished one goes now; one still
/// generating is hidden and keeps counting against the author's
/// generations until its job finishes.
pub fn discard_draft(db: &Db, id: &str, user: &str) -> anyhow::Result<bool> {
    let mut conn = db.lock();
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let now = now_ms();
    // Author, state and expiry are checked by the mutations themselves;
    // nothing claimed means nothing to discard (404).
    let marked = tx.execute(
        "UPDATE page_drafts SET status = 'discarded'
         WHERE id = ?1 AND author_id = ?2 AND status = 'generating' AND expires_at > ?3",
        params![id, user, now],
    )?;
    let deleted = tx.execute(
        "DELETE FROM page_drafts
         WHERE id = ?1 AND author_id = ?2 AND status IN ('ready', 'failed') AND expires_at > ?3",
        params![id, user, now],
    )?;
    tx.commit()?;
    Ok(marked + deleted > 0)
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
        // Drafts still generating are discarded, not deleted: they keep
        // their generation slot until their job finishes.
        let theirs = "author_id = ?2 AND (space_id = ?1
                 OR page_id IN (SELECT id FROM pages WHERE space_id = ?1))";
        tx.execute(
            &format!(
                "UPDATE page_drafts SET status = 'discarded' WHERE {theirs} AND status = 'generating'"
            ),
            params![space, user],
        )?;
        tx.execute(
            &format!("DELETE FROM page_drafts WHERE {theirs} AND status IN ('ready', 'failed')"),
            params![space, user],
        )?;
        Ok(())
    }
}
