//! SQLite storage for library assets. Every read joins the caller's current
//! Space membership in the same statement; every write checks it inside the
//! same `IMMEDIATE` transaction that changes rows.

use std::collections::HashSet;

use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};

use super::validate::AssetKind;
use super::{Asset, LibraryLimits, Page, Scope};
use crate::db::Db;

pub fn init(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS library_assets (
             seq         INTEGER PRIMARY KEY AUTOINCREMENT,
             id          TEXT NOT NULL UNIQUE,
             uploader_id TEXT NOT NULL,
             space_id    TEXT REFERENCES spaces (id) ON DELETE CASCADE,
             name        TEXT NOT NULL,
             kind        TEXT NOT NULL,
             mime_type   TEXT NOT NULL,
             size_bytes  INTEGER NOT NULL CHECK (size_bytes > 0),
             sha256      TEXT NOT NULL,
             created_at  INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS library_assets_personal
             ON library_assets (uploader_id, seq) WHERE space_id IS NULL;
         CREATE INDEX IF NOT EXISTS library_assets_space ON library_assets (space_id, seq);
         CREATE INDEX IF NOT EXISTS library_assets_uploader ON library_assets (uploader_id);
         CREATE TABLE IF NOT EXISTS library_blobs (
             asset_id TEXT PRIMARY KEY NOT NULL
                      REFERENCES library_assets (id) ON DELETE CASCADE,
             bytes    BLOB NOT NULL
         );",
    )
}

/// Columns of `a` (library_assets) read by [`row_asset`].
const COLUMNS: &str = "a.seq, a.id, a.uploader_id, a.space_id, a.name, a.kind, a.mime_type,
                       a.size_bytes, a.sha256, a.created_at";

/// `a` is readable by `?1`: their own personal asset, or an asset of a
/// Space they are a member of now.
const VISIBLE: &str = "((a.space_id IS NULL AND a.uploader_id = ?1)
     OR (a.space_id IS NOT NULL AND EXISTS (
         SELECT 1 FROM space_members m WHERE m.space_id = a.space_id AND m.user_id = ?1)))";

fn row_asset(r: &Row<'_>) -> rusqlite::Result<Asset> {
    let kind: String = r.get(5)?;
    let kind = AssetKind::parse(&kind).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            5,
            rusqlite::types::Type::Text,
            format!("unknown asset kind {kind:?}").into(),
        )
    })?;
    Ok(Asset {
        seq: r.get(0)?,
        id: r.get(1)?,
        uploader_id: r.get(2)?,
        space_id: r.get(3)?,
        name: r.get(4)?,
        kind,
        mime_type: r.get(6)?,
        size_bytes: r.get::<_, i64>(7)?.max(0) as u64,
        sha256: r.get(8)?,
        created_at: r.get(9)?,
    })
}

pub fn visible(db: &Db, user: &str, id: &str) -> anyhow::Result<Option<Asset>> {
    let conn = db.lock();
    let sql = format!("SELECT {COLUMNS} FROM library_assets a WHERE a.id = ?2 AND {VISIBLE}");
    Ok(conn
        .prepare_cached(&sql)?
        .query_row(params![user, id], row_asset)
        .optional()?)
}

pub fn bytes_for(db: &Db, user: &str, id: &str) -> anyhow::Result<Option<(Asset, Vec<u8>)>> {
    let conn = db.lock();
    let sql = format!(
        "SELECT {COLUMNS}, b.bytes FROM library_assets a
         JOIN library_blobs b ON b.asset_id = a.id
         WHERE a.id = ?2 AND {VISIBLE}"
    );
    Ok(conn
        .prepare_cached(&sql)?
        .query_row(params![user, id], |r| Ok((row_asset(r)?, r.get(10)?)))
        .optional()?)
}

/// Bytes `[start, start + len)` of an asset `user` may read now, with its
/// metadata; the slice is cut by SQLite so the rest is never copied.
pub fn range_for(
    db: &Db,
    user: &str,
    id: &str,
    start: u64,
    len: u64,
) -> anyhow::Result<Option<(Asset, Vec<u8>)>> {
    let conn = db.lock();
    let sql = format!(
        "SELECT {COLUMNS}, substr(b.bytes, ?3, ?4) FROM library_assets a
         JOIN library_blobs b ON b.asset_id = a.id
         WHERE a.id = ?2 AND {VISIBLE}"
    );
    Ok(conn
        .prepare_cached(&sql)?
        .query_row(params![user, id, start as i64 + 1, len as i64], |r| {
            Ok((row_asset(r)?, r.get(10)?))
        })
        .optional()?)
}

pub fn filter_visible(db: &Db, user: &str, ids: &[String]) -> anyhow::Result<HashSet<String>> {
    let conn = db.lock();
    let sql = format!("SELECT a.id FROM library_assets a WHERE a.id = ?2 AND {VISIBLE}");
    let mut stmt = conn.prepare_cached(&sql)?;
    let mut out = HashSet::new();
    for id in ids {
        if let Some(found) = stmt
            .query_row(params![user, id], |r| r.get::<_, String>(0))
            .optional()?
        {
            out.insert(found);
        }
    }
    Ok(out)
}

fn is_member(conn: &Connection, space: &str, user: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM space_members WHERE space_id = ?1 AND user_id = ?2)",
        params![space, user],
        |r| r.get(0),
    )
}

/// One page in `seq` order (`ascending` for indexers, newest first for
/// people). `before`/`after` bound `seq` exclusively. `None` when a Space
/// scope names a Space `user` is not a member of now.
pub fn page(
    db: &Db,
    user: &str,
    scope: &Scope,
    cursor: Option<i64>,
    ascending: bool,
    limit: usize,
) -> anyhow::Result<Option<Page>> {
    let conn = db.lock();
    let filter = match scope {
        Scope::Personal => "a.space_id IS NULL AND a.uploader_id = ?1",
        Scope::Space(sid) => {
            if !is_member(&conn, sid, user)? {
                return Ok(None);
            }
            // Membership is re-checked in the same statement as the rows.
            "a.space_id = ?2 AND EXISTS (SELECT 1 FROM space_members m
                 WHERE m.space_id = a.space_id AND m.user_id = ?1)"
        }
        Scope::All => VISIBLE,
    };
    let (cmp, order) = if ascending {
        (">", "ASC")
    } else {
        ("<", "DESC")
    };
    let bound = cursor.unwrap_or(if ascending { i64::MIN } else { i64::MAX });
    let sql = format!(
        "SELECT {COLUMNS} FROM library_assets a
         WHERE ({filter}) AND a.seq {cmp} ?3
         ORDER BY a.seq {order} LIMIT ?4"
    );
    let space = match scope {
        Scope::Space(sid) => Some(sid.as_str()),
        _ => None,
    };
    let fetch = limit as i64 + 1;
    let mut stmt = conn.prepare_cached(&sql)?;
    let mut assets = stmt
        .query_map(params![user, space, bound, fetch], row_asset)?
        .collect::<Result<Vec<_>, _>>()?;
    let more = assets.len() > limit;
    assets.truncate(limit);
    let next = if more {
        assets.last().map(|a| a.seq)
    } else {
        None
    };
    Ok(Some(Page {
        assets,
        next_after: next,
    }))
}

/// Newest-first assets `user` may read now whose name contains `pattern`
/// (a `LIKE` pattern with `\` as its escape), of one of `kinds` when given,
/// at most `limit`. Visibility and current membership are in the same
/// statement as the rows, and so is `excluded`: a name containing any of
/// them never matches (it is left out before `LIMIT`, so it never hides
/// another name). Runs on a connection the caller already holds, so it can
/// share one lock with other reads. Never reads blobs.
pub fn find_by_name(
    conn: &Connection,
    user: &str,
    scope: &Scope,
    pattern: &str,
    kinds: Option<&[AssetKind]>,
    excluded: &[&str],
    limit: usize,
) -> anyhow::Result<Vec<Asset>> {
    // Every scope names `:space`: the Space for one Space, NULL otherwise.
    let visible = format!("{} AND :space IS NULL", VISIBLE.replace("?1", ":user"));
    let filter = match scope {
        Scope::Personal => "a.space_id IS NULL AND a.uploader_id = :user AND :space IS NULL",
        Scope::Space(_) => {
            "a.space_id = :space AND EXISTS (SELECT 1 FROM space_members m
                 WHERE m.space_id = a.space_id AND m.user_id = :user)"
        }
        Scope::All => visible.as_str(),
    };
    let sql = format!(
        "SELECT {COLUMNS} FROM library_assets a
         WHERE ({filter}) AND a.name LIKE :pattern ESCAPE '\\'
           AND (:kinds IS NULL OR a.kind IN (SELECT value FROM json_each(:kinds)))
           AND NOT EXISTS (SELECT 1 FROM json_each(:excluded) x WHERE instr(a.name, x.value) > 0)
         ORDER BY a.created_at DESC, a.seq DESC LIMIT :limit"
    );
    let space = match scope {
        Scope::Space(sid) => Some(sid.as_str()),
        _ => None,
    };
    let kinds = kinds
        .map(|ks| serde_json::json!(ks.iter().map(|k| k.as_str()).collect::<Vec<_>>()).to_string());
    let excluded = serde_json::json!(excluded).to_string();
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map(
        rusqlite::named_params! {
            ":user": user,
            ":space": space,
            ":pattern": pattern,
            ":kinds": kinds,
            ":excluded": excluded,
            ":limit": limit as i64,
        },
        row_asset,
    )?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Why an upload could not be stored.
#[derive(Debug)]
pub enum InsertError {
    /// Not a member of the named Space (or it does not exist).
    NotFound,
    /// A quota would be exceeded; the message is safe to show.
    Quota(String),
    Db(anyhow::Error),
}

impl From<rusqlite::Error> for InsertError {
    fn from(e: rusqlite::Error) -> Self {
        InsertError::Db(e.into())
    }
}

pub struct NewAsset<'a> {
    pub id: &'a str,
    pub uploader: &'a str,
    pub space: Option<&'a str>,
    pub name: &'a str,
    pub kind: AssetKind,
    pub sha256: &'a str,
    pub bytes: &'a [u8],
    pub created_at: i64,
}

pub fn insert(db: &Db, new: &NewAsset<'_>, limits: &LibraryLimits) -> Result<Asset, InsertError> {
    let mut conn = db.lock();
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if let Some(space) = new.space
        && !is_member(&tx, space, new.uploader)?
    {
        return Err(InsertError::NotFound);
    }
    let size = new.bytes.len() as u64;
    let (count, used): (i64, i64) = tx.query_row(
        "SELECT COUNT(*), COALESCE(SUM(size_bytes), 0) FROM library_assets WHERE uploader_id = ?1",
        [new.uploader],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if count as u64 >= limits.max_assets_per_user {
        return Err(InsertError::Quota(format!(
            "you have reached the limit of {} library files",
            limits.max_assets_per_user
        )));
    }
    if used as u64 + size > limits.user_quota_bytes {
        return Err(InsertError::Quota(format!(
            "this upload would exceed your library quota of {} bytes",
            limits.user_quota_bytes
        )));
    }
    if let Some(space) = new.space {
        let used: i64 = tx.query_row(
            "SELECT COALESCE(SUM(size_bytes), 0) FROM library_assets WHERE space_id = ?1",
            [space],
            |r| r.get(0),
        )?;
        if used as u64 + size > limits.space_quota_bytes {
            return Err(InsertError::Quota(format!(
                "this upload would exceed the Space's library quota of {} bytes",
                limits.space_quota_bytes
            )));
        }
    }
    tx.execute(
        "INSERT INTO library_assets
             (id, uploader_id, space_id, name, kind, mime_type, size_bytes, sha256, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            new.id,
            new.uploader,
            new.space,
            new.name,
            new.kind.as_str(),
            new.kind.mime(),
            size as i64,
            new.sha256,
            new.created_at
        ],
    )?;
    let seq = tx.last_insert_rowid();
    tx.execute(
        "INSERT INTO library_blobs (asset_id, bytes) VALUES (?1, ?2)",
        params![new.id, new.bytes],
    )?;
    tx.commit()?;
    Ok(Asset {
        seq,
        id: new.id.to_owned(),
        name: new.name.to_owned(),
        kind: new.kind,
        mime_type: new.kind.mime().to_owned(),
        size_bytes: size,
        space_id: new.space.map(str::to_owned),
        uploader_id: new.uploader.to_owned(),
        sha256: new.sha256.to_owned(),
        created_at: new.created_at,
    })
}

#[derive(Debug, PartialEq, Eq)]
pub enum Delete {
    Deleted,
    /// Readable, but neither its uploader nor the Space owner.
    Forbidden,
    NotFound,
}

/// Delete an asset: a personal one by its uploader; a Space one by its
/// uploader while still a member, or by the Space owner. The blob and any
/// rows that reference the asset go with it.
pub fn delete(db: &Db, user: &str, id: &str) -> anyhow::Result<Delete> {
    let mut conn = db.lock();
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let found: Option<(String, Option<String>, Option<String>)> = tx
        .query_row(
            "SELECT a.uploader_id, a.space_id,
                    (SELECT m.role FROM space_members m
                     WHERE m.space_id = a.space_id AND m.user_id = ?1)
             FROM library_assets a WHERE a.id = ?2",
            params![user, id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let outcome = match found {
        None => Delete::NotFound,
        Some((uploader, None, _)) if uploader != user => Delete::NotFound,
        Some((_, None, _)) => Delete::Deleted,
        Some((_, Some(_), None)) => Delete::NotFound,
        Some((_, Some(_), Some(role))) if role == "owner" => Delete::Deleted,
        Some((uploader, Some(_), Some(_))) if uploader == user => Delete::Deleted,
        Some(_) => Delete::Forbidden,
    };
    if outcome == Delete::Deleted {
        // Drop the search index of this file in the same transaction.
        crate::library_search::hooks::asset_deleted(&tx, id)?;
        tx.execute("DELETE FROM library_blobs WHERE asset_id = ?1", [id])?;
        tx.execute("DELETE FROM library_assets WHERE id = ?1", [id])?;
    }
    tx.commit()?;
    Ok(outcome)
}
