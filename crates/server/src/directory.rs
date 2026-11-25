//! Server-wide cards for people who have authenticated at least once.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{Query, State, rejection::QueryRejection};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router, routing::get};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::api::{self, ApiError, ApiResult};
use crate::auth::Human;

#[derive(Debug, Serialize)]
pub(crate) struct Card {
    pub id: String,
    pub display_name: String,
    pub avatar_version: Option<String>,
}

impl Card {
    pub(crate) fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            display_name: row.get(1)?,
            avatar_version: row.get(2)?,
        })
    }
}

pub(crate) fn listed(conn: &Connection, id: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM profiles WHERE user_id = ?1)",
        [id],
        |row| row.get(0),
    )
}

const BURST: Duration = Duration::from_secs(60);
const TOKEN: Duration = Duration::from_secs(2);

#[derive(Clone, Default)]
pub(crate) struct RateLimit(Arc<Mutex<Buckets>>);

#[derive(Default)]
struct Buckets {
    users: HashMap<String, (Instant, Duration)>,
    last_sweep: Option<Instant>,
}

impl Buckets {
    fn take(&mut self, id: &str, now: Instant) -> bool {
        if self.last_sweep.is_none_or(|t| now - t >= BURST) {
            self.users.retain(|_, (t, _)| now - *t < BURST);
            self.last_sweep = Some(now);
        }
        let (last, credit) = self.users.entry(id.to_owned()).or_insert((now, BURST));
        *credit = (*credit + (now - *last)).min(BURST);
        *last = now;
        if *credit < TOKEN {
            return false;
        }
        *credit -= TOKEN;
        true
    }
}

#[derive(Deserialize)]
struct Search {
    q: Option<String>,
    cursor: Option<String>,
    limit: Option<usize>,
}

#[derive(Serialize, Deserialize)]
struct Cursor(String, String);

impl Cursor {
    fn decode(encoded: &str) -> ApiResult<Self> {
        // Bound the decode before allocation; names/ids are each <= 256 bytes.
        (encoded.len() <= 2048)
            .then(|| URL_SAFE_NO_PAD.decode(encoded).ok())
            .flatten()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .ok_or_else(|| ApiError::bad_request("invalid directory cursor"))
    }

    fn encode(card: &Card) -> anyhow::Result<String> {
        Ok(URL_SAFE_NO_PAD.encode(serde_json::to_vec(&(&card.display_name, &card.id))?))
    }
}

#[derive(Serialize)]
struct Page {
    users: Vec<Card>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_cursor: Option<String>,
}

fn page(
    conn: &Connection,
    reader: &str,
    q: &str,
    cursor: Option<Cursor>,
    limit: usize,
) -> anyhow::Result<Page> {
    let like = crate::quick_search::like_pattern(q);
    let prefix = &like[1..];
    let word = format!("% {prefix}");
    let (name, id) = cursor
        .as_ref()
        .map(|c| (Some(c.0.as_str()), Some(c.1.as_str())))
        .unwrap_or_default();
    let mut stmt = conn.prepare_cached(
        "SELECT id, display_name, avatar_version FROM directory_cards
         WHERE id <> ?1
           AND (?2 = '' OR id = ?2 OR display_name LIKE ?3 ESCAPE '\\'
                                   OR display_name LIKE ?4 ESCAPE '\\')
           AND (?5 IS NULL OR (display_name COLLATE NOCASE, id) > (?5, ?6))
         ORDER BY display_name COLLATE NOCASE, id LIMIT ?7",
    )?;
    let mut users = stmt
        .query_map(
            params![reader, q, prefix, word, name, id, limit as i64 + 1],
            Card::from_row,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let next_cursor = if users.len() > limit {
        users.pop();
        users.last().map(Cursor::encode).transpose()?
    } else {
        None
    };
    Ok(Page { users, next_cursor })
}

async fn list(
    State(state): State<AppState>,
    who: Human,
    query: Result<Query<Search>, QueryRejection>,
) -> ApiResult<Response> {
    let allowed = state
        .directory_limit
        .0
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .take(&who.id, Instant::now());
    if !allowed {
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "too many directory requests; try again shortly",
        ));
    }
    let Query(search) = query?;
    let q = search.q.unwrap_or_default();
    if q.chars().count() > 64 || q.chars().any(char::is_control) {
        return Err(ApiError::bad_request(
            "q must be at most 64 printable characters",
        ));
    }
    let cursor = search
        .cursor
        .filter(|c| !c.is_empty())
        .map(|c| Cursor::decode(&c))
        .transpose()?;
    let limit = search.limit.unwrap_or(20).clamp(1, 25);
    let result = api::db(&state, move |db| {
        page(&db.lock(), &who.id, q.trim(), cursor, limit)
    })
    .await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(result)).into_response())
}

pub(crate) fn router() -> Router<AppState> {
    Router::new().route("/v1/directory", get(list))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_bucket_is_per_user_and_refills() {
        let mut buckets = Buckets::default();
        let now = Instant::now();
        for _ in 0..30 {
            assert!(buckets.take("a", now));
        }
        assert!(!buckets.take("a", now));
        assert!(!buckets.take("a", now + Duration::from_secs(1)));
        assert!(buckets.take("a", now + TOKEN));
        assert!(!buckets.take("a", now + TOKEN));
        assert!(buckets.take("b", now + TOKEN));
        assert!(buckets.take("c", now + BURST + TOKEN));
        assert_eq!(buckets.users.len(), 1);
    }
}
