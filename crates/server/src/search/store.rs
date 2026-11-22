//! SQLite tables and statements for the search index.
//!
//! One vector per `(user_id, source, event_id)` under one model
//! fingerprint. `search_sources` holds, per user and source, the fingerprint
//! and dimension the stored vectors were made with and the indexing cursor
//! (the highest event id already looked at). A different fingerprint
//! discards that source's vectors and restarts its cursor, so vectors from
//! two models or two dimensions are never compared.
//!
//! Every read of event text joins the membership table in the same
//! statement, so a person who is no longer a member gets nothing back.

use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};

use super::Source;

/// Kinds that are indexed: people's messages and the agent's final replies.
pub const INDEXED_KINDS: &str = "('message', 'agent')";

pub fn init(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS search_sources (
             user_id     TEXT NOT NULL,
             source      TEXT NOT NULL,
             fingerprint TEXT NOT NULL,
             dims        INTEGER,
             cursor      INTEGER NOT NULL DEFAULT 0,
             PRIMARY KEY (user_id, source)
         );
         CREATE TABLE IF NOT EXISTS search_vectors (
             user_id     TEXT NOT NULL,
             source      TEXT NOT NULL,
             event_id    INTEGER NOT NULL,
             fingerprint TEXT NOT NULL,
             room_id     TEXT,
             text_hash   BLOB NOT NULL,
             vector      BLOB NOT NULL,
             PRIMARY KEY (user_id, source, event_id)
         );",
    )
}

pub fn text_hash(text: &str) -> Vec<u8> {
    Sha256::digest(text.as_bytes()).to_vec()
}

/// The indexing state of one source for one user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceState {
    pub dims: Option<usize>,
    pub cursor: i64,
}

/// The state of `source` under `fingerprint`. A missing row is created; a
/// row with another fingerprint is reset, and that source's vectors are
/// deleted, in one transaction.
pub fn ensure_source(
    conn: &mut Connection,
    user: &str,
    source: &Source,
    fingerprint: &str,
) -> rusqlite::Result<SourceState> {
    let key = source.key();
    let tx = conn.transaction()?;
    let row: Option<(String, Option<i64>, i64)> = tx
        .query_row(
            "SELECT fingerprint, dims, cursor FROM search_sources WHERE user_id = ?1 AND source = ?2",
            params![user, key],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let state = match row {
        Some((fp, dims, cursor)) if fp == fingerprint => SourceState {
            dims: dims.map(|d| d as usize),
            cursor,
        },
        stale => {
            if stale.is_some() {
                tracing::info!(source = %key, "search model changed; discarding the source's vectors");
            }
            tx.execute(
                "DELETE FROM search_vectors WHERE user_id = ?1 AND source = ?2",
                params![user, key],
            )?;
            tx.execute(
                "INSERT INTO search_sources (user_id, source, fingerprint, dims, cursor)
                 VALUES (?1, ?2, ?3, NULL, 0)
                 ON CONFLICT (user_id, source) DO UPDATE
                 SET fingerprint = excluded.fingerprint, dims = NULL, cursor = 0",
                params![user, key, fingerprint],
            )?;
            SourceState {
                dims: None,
                cursor: 0,
            }
        }
    };
    tx.commit()?;
    Ok(state)
}

/// Is `user` allowed to read `source` right now?
pub fn can_read(conn: &Connection, user: &str, source: &Source) -> rusqlite::Result<bool> {
    match source {
        Source::Chat => Ok(true),
        Source::Space(sid) => conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM space_members WHERE space_id = ?1 AND user_id = ?2)",
            params![sid, user],
            |r| r.get(0),
        ),
    }
}

/// One event read for indexing or for a result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventRow {
    pub id: i64,
    pub room_id: Option<String>,
    pub actor: String,
    pub text: String,
}

/// The next `limit` indexable-kind events of `source` after `cursor`,
/// oldest first; empty when `user` may not read it.
pub fn events_after(
    conn: &Connection,
    user: &str,
    source: &Source,
    cursor: i64,
    limit: usize,
) -> rusqlite::Result<Vec<EventRow>> {
    let limit = limit as i64;
    match source {
        Source::Chat => {
            let mut stmt = conn.prepare_cached(&format!(
                "SELECT id, actor, text FROM personal_events
                 WHERE user_id = ?1 AND id > ?2 AND kind IN {INDEXED_KINDS}
                 ORDER BY id ASC LIMIT ?3"
            ))?;
            let rows = stmt.query_map(params![user, cursor, limit], |r| {
                Ok(EventRow {
                    id: r.get(0)?,
                    room_id: None,
                    actor: r.get(1)?,
                    text: r.get(2)?,
                })
            })?;
            rows.collect()
        }
        Source::Space(sid) => {
            let mut stmt = conn.prepare_cached(&format!(
                "SELECT e.id, e.room_id, e.actor, e.text FROM space_events e
                 WHERE e.space_id = ?1 AND e.id > ?2 AND e.kind IN {INDEXED_KINDS}
                   AND EXISTS (SELECT 1 FROM space_members m
                               WHERE m.space_id = ?1 AND m.user_id = ?3)
                 ORDER BY e.id ASC LIMIT ?4"
            ))?;
            let rows = stmt.query_map(params![sid, cursor, user, limit], |r| {
                Ok(EventRow {
                    id: r.get(0)?,
                    room_id: Some(r.get(1)?),
                    actor: r.get(2)?,
                    text: r.get(3)?,
                })
            })?;
            rows.collect()
        }
    }
}

/// Indexable-kind events of `source` after `cursor` that `user` may read.
pub fn count_after(
    conn: &Connection,
    user: &str,
    source: &Source,
    cursor: i64,
) -> rusqlite::Result<u64> {
    let n: i64 = match source {
        Source::Chat => conn.query_row(
            &format!(
                "SELECT COUNT(*) FROM personal_events
                 WHERE user_id = ?1 AND id > ?2 AND kind IN {INDEXED_KINDS}"
            ),
            params![user, cursor],
            |r| r.get(0),
        )?,
        Source::Space(sid) => conn.query_row(
            &format!(
                "SELECT COUNT(*) FROM space_events e
                 WHERE e.space_id = ?1 AND e.id > ?2 AND e.kind IN {INDEXED_KINDS}
                   AND EXISTS (SELECT 1 FROM space_members m
                               WHERE m.space_id = ?1 AND m.user_id = ?3)"
            ),
            params![sid, cursor, user],
            |r| r.get(0),
        )?,
    };
    Ok(n as u64)
}

/// A vector ready to store.
pub struct NewVector {
    pub event_id: i64,
    pub room_id: Option<String>,
    pub text_hash: Vec<u8>,
    pub vector: Vec<u8>,
}

/// Why a batch was not stored.
#[derive(Debug, PartialEq, Eq)]
pub enum StoreOutcome {
    Stored,
    /// The source's fingerprint, dimension or cursor moved while the batch
    /// was being embedded (a concurrent request, or a reconfiguration).
    Raced,
    /// The user lost access while the batch was being embedded.
    Revoked,
}

/// Store a batch embedded from events after `from` and move the cursor to
/// `to`, if the source is still where it was when the batch was read.
#[allow(clippy::too_many_arguments)]
pub fn store_batch(
    conn: &mut Connection,
    user: &str,
    source: &Source,
    fingerprint: &str,
    from: i64,
    to: i64,
    dims: Option<usize>,
    vectors: &[NewVector],
    max_vectors: usize,
) -> rusqlite::Result<(StoreOutcome, usize)> {
    let key = source.key();
    let tx = conn.transaction()?;
    if !can_read(&tx, user, source)? {
        return Ok((StoreOutcome::Revoked, 0));
    }
    let row: Option<(String, Option<i64>, i64)> = tx
        .query_row(
            "SELECT fingerprint, dims, cursor FROM search_sources WHERE user_id = ?1 AND source = ?2",
            params![user, key],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((fp, stored_dims, cursor)) = row else {
        return Ok((StoreOutcome::Raced, 0));
    };
    let stored_dims = stored_dims.map(|d| d as usize);
    let dims_ok = match (stored_dims, dims) {
        (Some(a), Some(b)) => a == b,
        _ => true,
    };
    if fp != fingerprint || cursor != from || !dims_ok {
        return Ok((StoreOutcome::Raced, 0));
    }
    let existing: i64 = tx.query_row(
        "SELECT COUNT(*) FROM search_vectors WHERE user_id = ?1 AND source = ?2",
        params![user, key],
        |r| r.get(0),
    )?;
    let room = max_vectors.saturating_sub(existing as usize);
    let mut stored = 0;
    {
        let mut insert = tx.prepare_cached(
            "INSERT OR REPLACE INTO search_vectors
                 (user_id, source, event_id, fingerprint, room_id, text_hash, vector)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?;
        for v in vectors.iter().take(room) {
            insert.execute(params![
                user,
                key,
                v.event_id,
                fingerprint,
                v.room_id,
                v.text_hash,
                v.vector
            ])?;
            stored += 1;
        }
    }
    // Stop the cursor at the last stored event when the source is full, so
    // the rest is still counted as waiting rather than silently dropped.
    let to = if stored < vectors.len() {
        vectors[stored].event_id - 1
    } else {
        to
    };
    tx.execute(
        "UPDATE search_sources SET cursor = ?3, dims = COALESCE(dims, ?4)
         WHERE user_id = ?1 AND source = ?2",
        params![user, key, to, dims.map(|d| d as i64)],
    )?;
    tx.commit()?;
    Ok((StoreOutcome::Stored, stored))
}

pub fn vector_count(conn: &Connection, user: &str, source: &Source) -> rusqlite::Result<usize> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM search_vectors WHERE user_id = ?1 AND source = ?2",
        params![user, source.key()],
        |r| r.get(0),
    )?;
    Ok(n as usize)
}

/// One page of stored vectors, newest event first, below `before`.
pub struct StoredVector {
    pub event_id: i64,
    pub room_id: Option<String>,
    pub vector: Vec<u8>,
}

pub fn vectors_page(
    conn: &Connection,
    user: &str,
    source: &Source,
    fingerprint: &str,
    before: i64,
    limit: usize,
) -> rusqlite::Result<Vec<StoredVector>> {
    let mut stmt = conn.prepare_cached(
        "SELECT event_id, room_id, vector FROM search_vectors
         WHERE user_id = ?1 AND source = ?2 AND fingerprint = ?3 AND event_id < ?4
         ORDER BY event_id DESC LIMIT ?5",
    )?;
    let rows = stmt.query_map(
        params![user, source.key(), fingerprint, before, limit as i64],
        |r| {
            Ok(StoredVector {
                event_id: r.get(0)?,
                room_id: r.get(1)?,
                vector: r.get(2)?,
            })
        },
    )?;
    rows.collect()
}

/// The current text of indexed events that `user` may still read and whose
/// text still matches what was embedded. Anything deleted, edited, of
/// another kind, or no longer readable is left out.
pub fn current_events(
    conn: &Connection,
    user: &str,
    source: &Source,
    ids: &[i64],
) -> rusqlite::Result<Vec<EventRow>> {
    if !can_read(conn, user, source)? {
        return Ok(Vec::new());
    }
    let key = source.key();
    let mut out = Vec::with_capacity(ids.len());
    let mut hash_stmt = conn.prepare_cached(
        "SELECT text_hash FROM search_vectors WHERE user_id = ?1 AND source = ?2 AND event_id = ?3",
    )?;
    for &id in ids {
        let row = match source {
            Source::Chat => conn
                .prepare_cached(&format!(
                    "SELECT id, actor, text FROM personal_events
                     WHERE user_id = ?1 AND id = ?2 AND kind IN {INDEXED_KINDS}"
                ))?
                .query_row(params![user, id], |r| {
                    Ok(EventRow {
                        id: r.get(0)?,
                        room_id: None,
                        actor: r.get(1)?,
                        text: r.get(2)?,
                    })
                })
                .optional()?,
            Source::Space(sid) => conn
                .prepare_cached(&format!(
                    "SELECT e.id, e.room_id, e.actor, e.text FROM space_events e
                     WHERE e.space_id = ?1 AND e.id = ?2 AND e.kind IN {INDEXED_KINDS}
                       AND EXISTS (SELECT 1 FROM space_members m
                                   WHERE m.space_id = ?1 AND m.user_id = ?3)"
                ))?
                .query_row(params![sid, id, user], |r| {
                    Ok(EventRow {
                        id: r.get(0)?,
                        room_id: Some(r.get(1)?),
                        actor: r.get(2)?,
                        text: r.get(3)?,
                    })
                })
                .optional()?,
        };
        let Some(row) = row else { continue };
        let hash: Option<Vec<u8>> = hash_stmt
            .query_row(params![user, key, id], |r| r.get(0))
            .optional()?;
        if hash.as_deref() == Some(text_hash(&super::embed_text(&row.text)).as_slice()) {
            out.push(row);
        }
    }
    Ok(out)
}

/// Delete `user`'s index of Spaces they are no longer a member of.
pub fn purge_revoked(conn: &mut Connection, user: &str) -> rusqlite::Result<usize> {
    let tx = conn.transaction()?;
    let stale = "SELECT source FROM search_sources s
         WHERE s.user_id = ?1 AND s.source LIKE 'space:%'
           AND NOT EXISTS (SELECT 1 FROM space_members m
                           WHERE m.user_id = ?1 AND 'space:' || m.space_id = s.source)";
    let n = tx.execute(
        &format!("DELETE FROM search_vectors WHERE user_id = ?1 AND source IN ({stale})"),
        params![user],
    )?;
    tx.execute(
        &format!("DELETE FROM search_sources WHERE user_id = ?1 AND source IN ({stale})"),
        params![user],
    )?;
    tx.commit()?;
    Ok(n)
}
