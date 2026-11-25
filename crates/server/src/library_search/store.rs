//! SQLite tables and statements for library search.
//!
//! - `library_search_settings`: each person's media embedding connection
//!   (`embedding_id`), separate from the message search `embedding_id`.
//! - `library_search_assets`: one row per person and file they indexed,
//!   with the source (`personal` or `space:<id>`), the model fingerprint,
//!   the SHA-256 of the bytes that were embedded, and the vector dimension.
//! - `library_search_segments`: that file's segment vectors (page, time
//!   range, text range or whole image), with the same fingerprint.
//!
//! Every index is per searcher: user A's rows are only ever read with
//! `user_id = A`, and Space rows are read only while A is a member of that
//! Space (the same statement joins `space_members`). A person's rows for a
//! Space they left are deleted on their next request. Rows reference
//! `library_assets(id) ON DELETE CASCADE`, so deleting a file (or its
//! Space) deletes everyone's index of it in the same statement.
//!
//! Races. Every row carries the connection it was built with and a
//! `write_seq` from `library_search_clock`, bumped by each store. A request
//! takes a [`snapshot`] of the clock before it learns anything about the
//! model, and its purges only touch rows with `write_seq <= snapshot`. So a
//! slow request holding an old fingerprint can't delete rows a newer
//! request already wrote with the fresh model. [`store_asset`] revalidates
//! everything that could have changed while the model ran, in one
//! immediate transaction: the current setting, the file's existence, hash,
//! Space and readability, and the connection grant.

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use super::GrantCheck;
use super::protocol::{Kind, Locator, Segment};
use crate::search::vector;

/// Index tables from before `write_seq` existed hold only derived data;
/// they are dropped and rebuilt (people reindex).
fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    for table in ["library_search_segments", "library_search_assets"] {
        let mut stmt = conn.prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))?;
        let cols: Vec<String> = stmt
            .query_map([], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        if !cols.is_empty() && !cols.iter().any(|c| c == "write_seq") {
            conn.execute_batch(&format!("DROP TABLE {table}"))?;
        }
    }
    Ok(())
}

pub fn init(conn: &Connection) -> rusqlite::Result<()> {
    migrate(conn)?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS library_search_settings (
             user_id      TEXT PRIMARY KEY NOT NULL,
             embedding_id TEXT,
             updated_at   INTEGER NOT NULL,
             selection_rev INTEGER NOT NULL DEFAULT 0
         );
         CREATE TABLE IF NOT EXISTS library_search_clock (
             id    INTEGER PRIMARY KEY CHECK (id = 1),
             value INTEGER NOT NULL
         );
         INSERT OR IGNORE INTO library_search_clock (id, value) VALUES (1, 0);
         CREATE TABLE IF NOT EXISTS library_search_assets (
             user_id       TEXT NOT NULL,
             asset_id      TEXT NOT NULL REFERENCES library_assets (id) ON DELETE CASCADE,
             source        TEXT NOT NULL,
             space_id      TEXT,
             connection_id TEXT NOT NULL,
             kind          TEXT NOT NULL,
             fingerprint   TEXT NOT NULL,
             content_hash  TEXT NOT NULL,
             model         TEXT NOT NULL,
             dims          INTEGER NOT NULL,
             segments      INTEGER NOT NULL,
             partial       INTEGER NOT NULL,
             write_seq     INTEGER NOT NULL,
             indexed_at    INTEGER NOT NULL,
             PRIMARY KEY (user_id, asset_id)
         );
         CREATE INDEX IF NOT EXISTS library_search_assets_by_asset
             ON library_search_assets (asset_id);
         CREATE INDEX IF NOT EXISTS library_search_assets_by_connection
             ON library_search_assets (connection_id);
         CREATE TABLE IF NOT EXISTS library_search_segments (
             user_id       TEXT NOT NULL,
             asset_id      TEXT NOT NULL REFERENCES library_assets (id) ON DELETE CASCADE,
             seq           INTEGER NOT NULL,
             source        TEXT NOT NULL,
             space_id      TEXT,
             connection_id TEXT NOT NULL,
             kind          TEXT NOT NULL,
             fingerprint   TEXT NOT NULL,
             content_hash  TEXT NOT NULL,
             write_seq     INTEGER NOT NULL,
             locator       TEXT NOT NULL,
             excerpt       TEXT,
             vector        BLOB NOT NULL,
             PRIMARY KEY (user_id, asset_id, seq)
         );
         CREATE INDEX IF NOT EXISTS library_search_segments_by_source
             ON library_search_segments (user_id, source, fingerprint);
         CREATE INDEX IF NOT EXISTS library_search_segments_by_asset
             ON library_search_segments (asset_id);
         CREATE INDEX IF NOT EXISTS library_search_segments_by_connection
             ON library_search_segments (connection_id);",
    )?;
    let has_revision: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM pragma_table_info('library_search_settings')
                         WHERE name = 'selection_rev')",
        [],
        |r| r.get(0),
    )?;
    if !has_revision {
        conn.execute_batch(
            "ALTER TABLE library_search_settings ADD COLUMN selection_rev INTEGER NOT NULL DEFAULT 0;",
        )?;
    }
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS library_search_settings_by_embedding
         ON library_search_settings (embedding_id);",
    )
}

/// The write clock now; see the module docs.
pub fn snapshot(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT value FROM library_search_clock WHERE id = 1",
        [],
        |r| r.get(0),
    )
}

/// Delete rows from both tables matching `cond` (on columns both have).
fn delete_both(
    conn: &Connection,
    cond: &str,
    p: &[&dyn rusqlite::ToSql],
) -> rusqlite::Result<usize> {
    conn.execute(
        &format!("DELETE FROM library_search_segments WHERE {cond}"),
        p,
    )?;
    conn.execute(
        &format!("DELETE FROM library_search_assets WHERE {cond}"),
        p,
    )
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ---- Settings ----

pub fn selection(conn: &Connection, user: &str) -> rusqlite::Result<(Option<String>, i64)> {
    Ok(conn
        .query_row(
            "SELECT embedding_id, selection_rev FROM library_search_settings WHERE user_id = ?1",
            [user],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .unwrap_or_default())
}

/// Only selections pointing to the connection whose destination moved.
/// The caller holds the endpoint update and index purge transaction.
pub fn invalidate_destination(
    tx: &rusqlite::Transaction<'_>,
    connection_id: &str,
) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE library_search_settings SET selection_rev = selection_rev + 1
         WHERE embedding_id = ?1",
        [connection_id],
    )?;
    Ok(())
}

#[cfg(test)]
pub fn embedding_id(conn: &Connection, user: &str) -> rusqlite::Result<Option<String>> {
    Ok(selection(conn, user)?.0)
}

#[derive(Debug)]
pub enum SelectionWrite {
    Saved(i64),
    Stale,
    Unusable(super::MediaError),
}

/// Compare (if supplied), save and purge the old index in one write
/// transaction. A stale write leaves both the selection and index intact.
pub fn save_selection(
    conn: &mut Connection,
    user: &str,
    id: Option<&str>,
    expected: Option<Option<&str>>,
    expected_revision: Option<i64>,
    grant: &GrantCheck,
) -> rusqlite::Result<SelectionWrite> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let (before, revision) = selection(&tx, user)?;
    if expected.is_some_and(|expected| expected != before.as_deref())
        || expected_revision.is_some_and(|expected| expected != revision)
    {
        return Ok(SelectionWrite::Stale);
    }
    // Same transaction and personal scope, before any purge or write.
    if let Some(id) = id
        && let Err(error) = grant(&tx, None, id)
    {
        return Ok(SelectionWrite::Unusable(error));
    }
    let changed = before.as_deref() != id;
    if changed {
        tx.execute(
            "DELETE FROM library_search_segments WHERE user_id = ?1",
            [user],
        )?;
        tx.execute(
            "DELETE FROM library_search_assets WHERE user_id = ?1",
            [user],
        )?;
    }
    let revision = tx.query_row(
        "INSERT INTO library_search_settings (user_id, embedding_id, updated_at, selection_rev)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (user_id) DO UPDATE
         SET embedding_id = excluded.embedding_id, updated_at = excluded.updated_at,
             selection_rev = selection_rev + ?4
         RETURNING selection_rev",
        params![user, id, now_ms(), i64::from(changed)],
        |r| r.get(0),
    )?;
    tx.commit()?;
    Ok(SelectionWrite::Saved(revision))
}

#[cfg(test)]
pub fn set_embedding_id(
    conn: &mut Connection,
    user: &str,
    id: Option<&str>,
    expected: Option<Option<&str>>,
) -> rusqlite::Result<SelectionWrite> {
    let grant: GrantCheck = std::sync::Arc::new(|_, _, _| Ok(String::new()));
    save_selection(conn, user, id, expected, None, &grant)
}

// ---- Access ----

/// Is `user` a member of `space` right now?
pub fn is_member(conn: &Connection, space: &str, user: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM space_members WHERE space_id = ?1 AND user_id = ?2)",
        params![space, user],
        |r| r.get(0),
    )
}

/// The condition, on a row aliased `s`, that its owner may still read it.
const READABLE: &str = "(s.space_id IS NULL AND s.source = 'personal'
     OR s.space_id IS NOT NULL AND s.source = 'space:' || s.space_id
        AND EXISTS (SELECT 1 FROM space_members m
                    WHERE m.space_id = s.space_id AND m.user_id = s.user_id))";

/// Delete `user`'s rows for Spaces they are no longer a member of.
pub fn purge_revoked(conn: &mut Connection, user: &str) -> rusqlite::Result<usize> {
    let tx = conn.transaction()?;
    let mut n = 0;
    for table in ["library_search_segments", "library_search_assets"] {
        n += tx.execute(
            &format!("DELETE FROM {table} AS s WHERE s.user_id = ?1 AND NOT {READABLE}"),
            [user],
        )?;
    }
    tx.commit()?;
    Ok(n)
}

/// Delete every person's rows for one file.
pub fn forget_asset(conn: &Connection, asset_id: &str) -> rusqlite::Result<usize> {
    let mut n = conn.execute(
        "DELETE FROM library_search_segments WHERE asset_id = ?1",
        [asset_id],
    )?;
    n += conn.execute(
        "DELETE FROM library_search_assets WHERE asset_id = ?1",
        [asset_id],
    )?;
    Ok(n)
}

/// Delete every person's rows for one Space.
pub fn forget_space(conn: &Connection, space_id: &str) -> rusqlite::Result<usize> {
    let mut n = conn.execute(
        "DELETE FROM library_search_segments WHERE space_id = ?1",
        [space_id],
    )?;
    n += conn.execute(
        "DELETE FROM library_search_assets WHERE space_id = ?1",
        [space_id],
    )?;
    Ok(n)
}

/// Delete one person's rows for one Space (they left or were removed).
pub fn forget_member(conn: &Connection, space_id: &str, user: &str) -> rusqlite::Result<usize> {
    let mut n = conn.execute(
        "DELETE FROM library_search_segments WHERE space_id = ?1 AND user_id = ?2",
        params![space_id, user],
    )?;
    n += conn.execute(
        "DELETE FROM library_search_assets WHERE space_id = ?1 AND user_id = ?2",
        params![space_id, user],
    )?;
    Ok(n)
}

/// Delete `user`'s rows for one file written at or before `snapshot`,
/// only those whose content hash differs from `keep_hash` when one is
/// given (the file changed), all of them otherwise (it is gone).
pub fn forget_stale_asset(
    conn: &Connection,
    user: &str,
    asset_id: &str,
    keep_hash: Option<&str>,
    snapshot: i64,
) -> rusqlite::Result<usize> {
    delete_both(
        conn,
        "user_id = ?1 AND asset_id = ?2 AND write_seq <= ?4
         AND (?3 IS NULL OR content_hash <> ?3)",
        &[&user, &asset_id, &keep_hash, &snapshot],
    )
}

/// Delete rows built with `connection_id`: everyone's, or everyone's but
/// `except_owner`'s.
pub fn forget_connection(
    conn: &Connection,
    connection_id: &str,
    except_owner: Option<&str>,
) -> rusqlite::Result<usize> {
    delete_both(
        conn,
        "connection_id = ?1 AND (?2 IS NULL OR user_id <> ?2)",
        &[&connection_id, &except_owner],
    )
}

// ---- Indexing ----

pub struct NewAsset<'a> {
    pub selection_revision: Option<i64>,
    pub asset_id: &'a str,
    pub source: &'a str,
    pub space_id: Option<&'a str>,
    pub connection_id: &'a str,
    pub kind: Kind,
    pub fingerprint: &'a str,
    pub content_hash: &'a str,
    pub model: &'a str,
    pub dims: usize,
    pub partial: bool,
    pub segments: &'a [Segment],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreOutcome {
    Stored,
    /// The file is gone or no longer readable by `user`.
    Revoked,
    /// The file's bytes or Space changed.
    Changed,
    /// The request's selection revision no longer matches.
    SelectionChanged,
    /// The setting now names another connection, or the connection's grant
    /// or configuration changed, or it may no longer be used.
    Switched,
    Full,
}

/// Replace `user`'s index of one file, after revalidating, in one
/// immediate transaction, everything that could have changed while the
/// model ran: the setting still names `a.connection_id`; the file still
/// exists with the same hash and Space and `user` may read it (uploader
/// for a personal file, current member for a Space file); `grant` still
/// resolves to `grant_key`; and the index has room. Anything else writes
/// nothing.
pub fn store_asset(
    conn: &mut Connection,
    user: &str,
    a: &NewAsset<'_>,
    grant: &GrantCheck,
    grant_key: &str,
    max_segments: usize,
) -> anyhow::Result<StoreOutcome> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let (selected, revision) = selection(&tx, user)?;
    if a.selection_revision
        .is_some_and(|expected| expected != revision)
    {
        return Ok(StoreOutcome::SelectionChanged);
    }
    if selected.as_deref() != Some(a.connection_id) {
        return Ok(StoreOutcome::Switched);
    }
    let file: Option<(String, Option<String>, String)> = tx
        .query_row(
            "SELECT sha256, space_id, uploader_id FROM library_assets WHERE id = ?1",
            [a.asset_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((sha, space, uploader)) = file else {
        return Ok(StoreOutcome::Revoked);
    };
    let readable = match &space {
        None => uploader == user,
        Some(sid) => is_member(&tx, sid, user)?,
    };
    if !readable {
        return Ok(StoreOutcome::Revoked);
    }
    if sha != a.content_hash || space.as_deref() != a.space_id {
        return Ok(StoreOutcome::Changed);
    }
    match grant(&tx, a.space_id, a.connection_id) {
        Ok(key) if key == grant_key => {}
        Err(e) if e.kind == super::MediaFailure::Internal => {
            return Err(anyhow::anyhow!(e.message));
        }
        _ => return Ok(StoreOutcome::Switched),
    }
    let others: i64 = tx.query_row(
        "SELECT COUNT(*) FROM library_search_segments WHERE user_id = ?1 AND asset_id <> ?2",
        params![user, a.asset_id],
        |r| r.get(0),
    )?;
    if others as usize + a.segments.len() > max_segments {
        return Ok(StoreOutcome::Full);
    }
    let write_seq: i64 = tx.query_row(
        "UPDATE library_search_clock SET value = value + 1 WHERE id = 1 RETURNING value",
        [],
        |r| r.get(0),
    )?;
    tx.execute(
        "DELETE FROM library_search_segments WHERE user_id = ?1 AND asset_id = ?2",
        params![user, a.asset_id],
    )?;
    tx.execute(
        "INSERT INTO library_search_assets
             (user_id, asset_id, source, space_id, connection_id, kind, fingerprint,
              content_hash, model, dims, segments, partial, write_seq, indexed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
         ON CONFLICT (user_id, asset_id) DO UPDATE SET
             source = excluded.source, space_id = excluded.space_id,
             connection_id = excluded.connection_id, kind = excluded.kind,
             fingerprint = excluded.fingerprint, content_hash = excluded.content_hash,
             model = excluded.model, dims = excluded.dims, segments = excluded.segments,
             partial = excluded.partial, write_seq = excluded.write_seq,
             indexed_at = excluded.indexed_at",
        params![
            user,
            a.asset_id,
            a.source,
            a.space_id,
            a.connection_id,
            a.kind.as_str(),
            a.fingerprint,
            a.content_hash,
            a.model,
            a.dims as i64,
            a.segments.len() as i64,
            a.partial,
            write_seq,
            now_ms(),
        ],
    )?;
    {
        let mut stmt = tx.prepare_cached(
            "INSERT INTO library_search_segments
                 (user_id, asset_id, seq, source, space_id, connection_id, kind, fingerprint,
                  content_hash, write_seq, locator, excerpt, vector)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        )?;
        for s in a.segments {
            let locator = serde_json::to_string(&s.locator).expect("locator serializes");
            stmt.execute(params![
                user,
                a.asset_id,
                s.seq as i64,
                a.source,
                a.space_id,
                a.connection_id,
                a.kind.as_str(),
                a.fingerprint,
                a.content_hash,
                write_seq,
                locator,
                s.excerpt,
                vector::encode(&s.vector),
            ])?;
        }
    }
    tx.commit()?;
    Ok(StoreOutcome::Stored)
}

// ---- Searching ----

/// How many files `user` has indexed in `source` (any model), readable now.
pub fn indexed_count(conn: &Connection, user: &str, source: &str) -> rusqlite::Result<usize> {
    let n: i64 = conn.query_row(
        &format!(
            "SELECT COUNT(*) FROM library_search_assets AS s
             WHERE s.user_id = ?1 AND s.source = ?2 AND {READABLE}"
        ),
        params![user, source],
        |r| r.get(0),
    )?;
    Ok(n as usize)
}

/// Delete `user`'s rows in `source`, written at or before `snapshot`, that
/// were built with another connection or fingerprint than
/// `connection_id`/`fingerprint`, or (when `dims` is given) with another
/// vector dimension. Does nothing when the setting no longer names
/// `connection_id`: the request is itself stale. Returns how many files
/// were dropped.
pub fn purge_stale(
    conn: &mut Connection,
    user: &str,
    source: &str,
    (connection_id, expected_revision): (&str, Option<i64>),
    fingerprint: &str,
    dims: Option<usize>,
    snapshot: i64,
) -> rusqlite::Result<usize> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let (id, revision) = selection(&tx, user)?;
    if id.as_deref() != Some(connection_id) || expected_revision.is_some_and(|r| r != revision) {
        return Ok(0);
    }
    let dims = dims.map(|d| d as i64).unwrap_or(-1);
    let stale = "user_id = ?1 AND source = ?2 AND write_seq <= ?6
         AND (connection_id <> ?3 OR fingerprint <> ?4 OR (?5 >= 0 AND asset_id IN
              (SELECT asset_id FROM library_search_assets
               WHERE user_id = ?1 AND source = ?2 AND dims <> ?5)))";
    let p = params![user, source, connection_id, fingerprint, dims, snapshot];
    tx.execute(
        &format!("DELETE FROM library_search_segments WHERE {stale}"),
        p,
    )?;
    let n = tx.execute(
        "DELETE FROM library_search_assets WHERE user_id = ?1 AND source = ?2 AND write_seq <= ?6
         AND (connection_id <> ?3 OR fingerprint <> ?4 OR (?5 >= 0 AND dims <> ?5))",
        p,
    )?;
    tx.commit()?;
    Ok(n)
}

/// The connection is definitively unusable for `source` (deleted, share
/// revoked, no longer a media connection): delete `user`'s rows there
/// written at or before `snapshot`. Does nothing when the setting no longer
/// names `connection_id`.
pub fn purge_source(
    conn: &mut Connection,
    user: &str,
    source: &str,
    (connection_id, expected_revision): (&str, Option<i64>),
    snapshot: i64,
) -> rusqlite::Result<usize> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let (id, revision) = selection(&tx, user)?;
    if id.as_deref() != Some(connection_id) || expected_revision.is_some_and(|r| r != revision) {
        return Ok(0);
    }
    let n = delete_both(
        &tx,
        "user_id = ?1 AND source = ?2 AND write_seq <= ?3",
        &[&user, &source, &snapshot],
    )?;
    tx.commit()?;
    Ok(n)
}

/// One stored segment read for scoring.
#[derive(Debug, Clone)]
pub struct SegmentRow {
    pub rowid: i64,
    pub asset_id: String,
    pub seq: i64,
    pub kind: Kind,
    pub vector: Vec<u8>,
}

/// Up to `limit` of `user`'s segments in `source` under `fingerprint`,
/// after `rowid` (ascending), readable now, optionally of one kind.
pub fn segments_page(
    conn: &Connection,
    user: &str,
    source: &str,
    fingerprint: &str,
    kind: Option<Kind>,
    after: i64,
    limit: usize,
) -> rusqlite::Result<Vec<SegmentRow>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT s.rowid, s.asset_id, s.seq, s.kind, s.vector FROM library_search_segments AS s
         WHERE s.user_id = ?1 AND s.source = ?2 AND s.fingerprint = ?3
           AND (?4 IS NULL OR s.kind = ?4) AND s.rowid > ?5 AND {READABLE}
         ORDER BY s.rowid ASC LIMIT ?6"
    ))?;
    let rows = stmt.query_map(
        params![
            user,
            source,
            fingerprint,
            kind.map(Kind::as_str),
            after,
            limit as i64
        ],
        |r| {
            let kind: String = r.get(3)?;
            Ok(SegmentRow {
                rowid: r.get(0)?,
                asset_id: r.get(1)?,
                seq: r.get(2)?,
                kind: Kind::parse(&kind).unwrap_or(Kind::Document),
                vector: r.get(4)?,
            })
        },
    )?;
    rows.collect()
}

/// What a result needs from the index, read again right before it is used.
#[derive(Debug, Clone)]
pub struct Indexed {
    pub source: String,
    pub content_hash: String,
    pub locator: Locator,
    pub excerpt: Option<String>,
}

/// One of `user`'s segments, if it is still stored and readable.
pub fn segment(
    conn: &Connection,
    user: &str,
    asset_id: &str,
    seq: i64,
) -> rusqlite::Result<Option<Indexed>> {
    conn.query_row(
        &format!(
            "SELECT s.source, s.content_hash, s.locator, s.excerpt
             FROM library_search_segments AS s
             JOIN library_search_assets a ON a.user_id = s.user_id AND a.asset_id = s.asset_id
             WHERE s.user_id = ?1 AND s.asset_id = ?2 AND s.seq = ?3
               AND a.fingerprint = s.fingerprint AND a.write_seq = s.write_seq AND {READABLE}"
        ),
        params![user, asset_id, seq],
        |r| {
            let locator: String = r.get(2)?;
            Ok((r.get(0)?, r.get(1)?, locator, r.get(3)?))
        },
    )
    .optional()
    .map(|row| {
        row.and_then(|(source, content_hash, locator, excerpt)| {
            Some(Indexed {
                source,
                content_hash,
                locator: serde_json::from_str(&locator).ok()?,
                excerpt,
            })
        })
    })
}

/// How many segments `user` holds in total.
#[cfg(test)]
pub fn segment_count(conn: &Connection, user: &str) -> rusqlite::Result<usize> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM library_search_segments WHERE user_id = ?1",
        [user],
        |r| r.get(0),
    )?;
    Ok(n as usize)
}
