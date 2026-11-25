//! "Use my Spaces in Chat": on by default, active once a client showed the
//! notice, per account, with per-Space exclusions.
//!
//! `GET|PUT /v1/chat/spaces-access` with
//! `{"enabled": bool, "notice_shown": bool, "excluded_space_ids": [..]}`.
//! A PUT may send any subset of the fields. Excluded ids must be Spaces the
//! caller belongs to now (404 otherwise, with no hint which one).

use axum::Json;
use axum::extract::State;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::api::{ApiError, ApiResult, JsonBody, db};
use crate::auth::Human;

/// Most Spaces one person can exclude (more than anyone can belong to).
pub const MAX_EXCLUDED: usize = 500;

pub fn init(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS meta_prefs (
             user_id      TEXT PRIMARY KEY NOT NULL,
             spaces       INTEGER NOT NULL DEFAULT 1 CHECK (spaces IN (0, 1)),
             notice_shown INTEGER,
             updated_at   INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS meta_space_exclusions (
             user_id  TEXT NOT NULL,
             space_id TEXT NOT NULL REFERENCES spaces (id),
             PRIMARY KEY (user_id, space_id)
         );
         CREATE INDEX IF NOT EXISTS meta_space_exclusions_space
             ON meta_space_exclusions (space_id);",
    )
}

/// A person's switch and notice state. A missing row means on, notice not
/// yet shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Switch {
    pub enabled: bool,
    pub notice_shown: bool,
}

impl Switch {
    /// Space tools are offered only when both hold.
    pub fn active(self) -> bool {
        self.enabled && self.notice_shown
    }
}

pub fn switch_in(conn: &Connection, user: &str) -> rusqlite::Result<Switch> {
    Ok(conn
        .query_row(
            "SELECT spaces, notice_shown IS NOT NULL FROM meta_prefs WHERE user_id = ?1",
            [user],
            |r| {
                Ok(Switch {
                    enabled: r.get::<_, i64>(0)? == 1,
                    notice_shown: r.get(1)?,
                })
            },
        )
        .optional()?
        .unwrap_or(Switch {
            enabled: true,
            notice_shown: false,
        }))
}

pub fn excluded_in(conn: &Connection, user: &str, space: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM meta_space_exclusions WHERE user_id = ?1 AND space_id = ?2)",
        params![user, space],
        |r| r.get(0),
    )
}

/// The caller's excluded Spaces they still belong to.
fn excluded_ids(conn: &Connection, user: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare_cached(
        "SELECT x.space_id FROM meta_space_exclusions x
         JOIN space_members m ON m.space_id = x.space_id AND m.user_id = x.user_id
         WHERE x.user_id = ?1 ORDER BY x.space_id",
    )?;
    stmt.query_map([user], |r| r.get(0))?.collect()
}

/// Purge hook: removing or leaving a member drops their exclusion of it.
pub fn member_removed(tx: &Transaction<'_>, space: &str, user: &str) -> rusqlite::Result<()> {
    tx.execute(
        "DELETE FROM meta_space_exclusions WHERE space_id = ?1 AND user_id = ?2",
        params![space, user],
    )?;
    Ok(())
}

/// Purge hook: deleting a Space drops every exclusion of it.
pub fn space_deleted(tx: &Transaction<'_>, space: &str) -> rusqlite::Result<()> {
    tx.execute(
        "DELETE FROM meta_space_exclusions WHERE space_id = ?1",
        [space],
    )?;
    Ok(())
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct AccessView {
    pub enabled: bool,
    pub notice_shown: bool,
    pub excluded_space_ids: Vec<String>,
}

fn view_in(conn: &Connection, user: &str) -> rusqlite::Result<AccessView> {
    let s = switch_in(conn, user)?;
    Ok(AccessView {
        enabled: s.enabled,
        notice_shown: s.notice_shown,
        excluded_space_ids: excluded_ids(conn, user)?,
    })
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccessBody {
    pub enabled: Option<bool>,
    /// `true` records that a client showed the notice (kept if already
    /// set); `false` clears it, so the notice shows again.
    pub notice_shown: Option<bool>,
    /// Replaces the whole set.
    pub excluded_space_ids: Option<Vec<String>>,
}

/// Why a PUT was refused.
#[derive(Debug, PartialEq, Eq)]
pub enum PutError {
    /// An excluded id isn't a Space the caller belongs to now.
    NotFound,
    TooMany,
}

/// Apply `body` for `user` in one transaction.
pub fn put(
    conn: &mut Connection,
    user: &str,
    body: &AccessBody,
) -> anyhow::Result<Result<AccessView, PutError>> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    if let Some(ids) = &body.excluded_space_ids {
        if ids.len() > MAX_EXCLUDED {
            return Ok(Err(PutError::TooMany));
        }
        for id in ids {
            let member: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM space_members WHERE space_id = ?1 AND user_id = ?2)",
                params![id, user],
                |r| r.get(0),
            )?;
            if !member {
                return Ok(Err(PutError::NotFound));
            }
        }
        tx.execute(
            "DELETE FROM meta_space_exclusions WHERE user_id = ?1",
            [user],
        )?;
        for id in ids {
            tx.execute(
                "INSERT OR IGNORE INTO meta_space_exclusions (user_id, space_id) VALUES (?1, ?2)",
                params![user, id],
            )?;
        }
    }
    let now = crate::db::now_ms();
    tx.execute(
        "INSERT INTO meta_prefs (user_id, updated_at) VALUES (?1, ?2)
         ON CONFLICT (user_id) DO NOTHING",
        params![user, now],
    )?;
    if let Some(on) = body.enabled {
        tx.execute(
            "UPDATE meta_prefs SET spaces = ?2, updated_at = ?3 WHERE user_id = ?1",
            params![user, on as i64, now],
        )?;
    }
    match body.notice_shown {
        Some(true) => tx.execute(
            "UPDATE meta_prefs SET notice_shown = COALESCE(notice_shown, ?2), updated_at = ?2
             WHERE user_id = ?1",
            params![user, now],
        )?,
        Some(false) => tx.execute(
            "UPDATE meta_prefs SET notice_shown = NULL, updated_at = ?2 WHERE user_id = ?1",
            params![user, now],
        )?,
        None => 0,
    };
    let view = view_in(&tx, user)?;
    tx.commit()?;
    Ok(Ok(view))
}

pub async fn get_access(State(state): State<AppState>, who: Human) -> ApiResult<Json<AccessView>> {
    Ok(Json(
        db(&state, move |db| Ok(view_in(&db.lock(), &who.id)?)).await?,
    ))
}

pub async fn put_access(
    State(state): State<AppState>,
    who: Human,
    body: JsonBody<AccessBody>,
) -> ApiResult<Json<AccessView>> {
    let Json(body) = body?;
    match db(&state, move |db| put(&mut db.lock(), &who.id, &body)).await? {
        Ok(view) => Ok(Json(view)),
        Err(PutError::NotFound) => Err(ApiError::not_found("no such Space")),
        Err(PutError::TooMany) => Err(ApiError::bad_request(format!(
            "at most {MAX_EXCLUDED} excluded Spaces"
        ))),
    }
}
