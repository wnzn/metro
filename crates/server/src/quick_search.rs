//! Quick search: a literal, model-free lookup for typeahead.
//!
//! `POST /v1/search/quick` matches the query as plain text (case-insensitive
//! for ASCII; `%`, `_` and `\` are literal) against:
//!
//! - messages (`message` and `agent` events only, never tool, activity,
//!   status or other events) in the caller's own personal Chat and in the
//!   rooms and threads of Spaces they are a member of now;
//! - file names (and so extensions) of library uploads they may read now:
//!   their personal library and the libraries of their Spaces.
//!
//! Nothing is embedded, indexed, sent to a model or cached; no file bytes
//! are read. Every row's visibility (the caller's own Chat or library, or
//! current Space membership) is checked in the same SQL statement that
//! returns it. Text carrying a Metro credential prefix is never matched,
//! so it is excluded before anything is clipped or returned.
//!
//! Scopes never widen: `chat` is the personal Chat's messages only,
//! `personal` the personal library only, `space` one Space's rooms, threads
//! and library, and `all` all of these. `mode` narrows to `conversations`
//! (messages) or `library` (files); `kind` narrows files to a category.
//!
//! Results are bounded and deterministic: newest first, at most
//! [`MAX_RESULTS`] together, split between messages and files when both
//! match. Message text is clipped to a snippet around the first match.

use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::api::ApiError;
use crate::auth::{DEVICE_TOKEN_PREFIX, Human, RUNNER_TOKEN_PREFIX, valid_id};
use crate::db::Db;
use crate::library::{self, Asset, AssetKind};

/// Longest query, in characters.
pub const MAX_QUERY_CHARS: usize = 200;
/// Results one lookup returns, messages and files together.
pub const MAX_RESULTS: usize = 8;
/// Results of one kind kept for the other when both match.
pub const MIN_SHARE: usize = MAX_RESULTS / 2;
/// Characters of message text returned around the first match.
pub const SNIPPET_CHARS: usize = 160;
/// Characters of context kept before the match in a snippet.
const SNIPPET_LEAD: usize = 60;
/// Most recent events per table a lookup looks at, newest first.
pub const SCAN_EVENTS: i64 = 100_000;
/// Metro credential prefixes: text containing one never matches.
/// (`metro_rt_` covers registered runner tokens, `metro_rt_v2_`.)
pub const CREDENTIAL_PREFIXES: [&str; 3] = [
    RUNNER_TOKEN_PREFIX,
    DEVICE_TOKEN_PREFIX,
    crate::admin_users::MANAGED_TOKEN_PREFIX,
];

/// `POST /v1/search/quick`.
pub fn router() -> Router<AppState> {
    Router::new().route("/v1/search/quick", post(quick))
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    All,
    Chat,
    Personal,
    Space,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    All,
    Conversations,
    Library,
}

/// A file category, as the library's media search names them.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FileKind {
    Document,
    Image,
    Audio,
    Video,
}

impl FileKind {
    pub fn kinds(self) -> &'static [AssetKind] {
        match self {
            FileKind::Document => &[
                AssetKind::Text,
                AssetKind::Markdown,
                AssetKind::Pdf,
                AssetKind::Docx,
            ],
            FileKind::Image => &[AssetKind::Png, AssetKind::Jpeg],
            FileKind::Audio => &[AssetKind::Wav, AssetKind::Mp3],
            FileKind::Video => &[AssetKind::Mp4, AssetKind::Webm],
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuickBody {
    pub query: String,
    pub scope: Scope,
    #[serde(default)]
    pub space_id: Option<String>,
    pub mode: Mode,
    #[serde(default)]
    pub kind: Option<FileKind>,
}

/// One message: where it is, who wrote it, and a snippet of its text.
/// `space_id` and `room_id` are both null for the personal Chat.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct QuickMessage {
    pub space_id: Option<String>,
    pub room_id: Option<String>,
    pub event_id: i64,
    pub actor: String,
    pub text: String,
    /// Unix milliseconds; orders Chat and Space messages together.
    #[serde(skip)]
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct QuickResponse {
    /// The library's usual asset metadata (never bytes).
    pub assets: Vec<Asset>,
    pub messages: Vec<QuickMessage>,
}

/// The trimmed query, or why it is refused.
pub fn check_query(query: &str) -> Result<String, ApiError> {
    let q = query.trim();
    if q.is_empty() {
        return Err(ApiError::bad_request("query must not be empty"));
    }
    if q.chars().count() > MAX_QUERY_CHARS {
        return Err(ApiError::bad_request(format!(
            "query is longer than {MAX_QUERY_CHARS} characters"
        )));
    }
    Ok(q.to_owned())
}

/// `query` as a `LIKE … ESCAPE '\'` pattern that matches it literally anywhere.
pub fn like_pattern(query: &str) -> String {
    let mut p = String::with_capacity(query.len() + 2);
    p.push('%');
    for c in query.chars() {
        if matches!(c, '%' | '_' | '\\') {
            p.push('\\');
        }
        p.push(c);
    }
    p.push('%');
    p
}

/// Up to [`SNIPPET_CHARS`] of `text` around the first (ASCII
/// case-insensitive) occurrence of `query`, on one line, with `…` where it
/// was cut.
pub fn snippet(text: &str, query: &str) -> String {
    let chars: Vec<char> = text
        .chars()
        .map(|c| {
            if c == '\n' || c == '\r' || c == '\t' {
                ' '
            } else {
                c
            }
        })
        .collect();
    let q: Vec<char> = query.chars().collect();
    let at = (0..=chars.len().saturating_sub(q.len()))
        .find(|&i| {
            chars.len() >= q.len()
                && chars[i..i + q.len()]
                    .iter()
                    .zip(&q)
                    .all(|(a, b)| a.eq_ignore_ascii_case(b))
        })
        .unwrap_or(0);
    let mut start = at.saturating_sub(SNIPPET_LEAD);
    let end = (start + SNIPPET_CHARS).min(chars.len());
    start = end.saturating_sub(SNIPPET_CHARS).min(start);
    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    out.extend(&chars[start..end]);
    if end < chars.len() {
        out.push('…');
    }
    out.trim().to_owned()
}

/// How many of each to keep when both may be shown: each kind gets at
/// least [`MIN_SHARE`] when it has that many, and the rest goes to the
/// other, never more than [`MAX_RESULTS`] together.
pub fn split(messages: usize, assets: usize) -> (usize, usize) {
    let (m, a) = (messages.min(MAX_RESULTS), assets.min(MAX_RESULTS));
    if m + a <= MAX_RESULTS {
        return (m, a);
    }
    // Over the cap, so `a > MAX_RESULTS - m`: files keep their share, or
    // whatever messages leave, and messages take the rest.
    let keep_a = a.min(MIN_SHARE.min(a).max(MAX_RESULTS - m));
    (MAX_RESULTS - keep_a, keep_a)
}

/// The SQL that keeps credential-carrying text out, for column `col`
/// (parameters `?10`, `?11`, `?12`).
fn no_credentials(col: &str) -> String {
    format!("instr({col}, ?10) = 0 AND instr({col}, ?11) = 0 AND instr({col}, ?12) = 0")
}

/// Newest-first personal Chat messages of `user` containing `pattern`.
fn chat_messages(
    db: &Db,
    user: &str,
    pattern: &str,
    limit: usize,
) -> anyhow::Result<Vec<QuickMessage>> {
    let conn = db.lock();
    let sql = format!(
        "SELECT id, actor, text, created_at FROM personal_events
         WHERE user_id = ?1 AND kind IN ('message', 'agent')
           AND id > (SELECT COALESCE(MAX(id), 0) FROM personal_events) - ?4
           AND text LIKE ?2 ESCAPE '\\' AND {}
         ORDER BY id DESC LIMIT ?3",
        no_credentials("text")
    );
    let [c1, c2, c3] = CREDENTIAL_PREFIXES;
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map(
        params![
            user,
            pattern,
            limit as i64,
            SCAN_EVENTS,
            None::<i64>,
            None::<i64>,
            None::<i64>,
            None::<i64>,
            None::<i64>,
            c1,
            c2,
            c3
        ],
        |r| {
            Ok(QuickMessage {
                space_id: None,
                room_id: None,
                event_id: r.get(0)?,
                actor: r.get(1)?,
                text: r.get(2)?,
                created_at: r.get(3)?,
            })
        },
    )?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Newest-first messages in rooms and threads of `space` (or of every Space
/// `user` belongs to, when `None`) containing `pattern`; membership is
/// checked in the same statement.
fn space_messages(
    db: &Db,
    user: &str,
    space: Option<&str>,
    pattern: &str,
    limit: usize,
) -> anyhow::Result<Vec<QuickMessage>> {
    let conn = db.lock();
    let which = if space.is_some() {
        "e.space_id = ?5 AND EXISTS (SELECT 1 FROM space_members m
             WHERE m.space_id = e.space_id AND m.user_id = ?1)"
    } else {
        "e.space_id IN (SELECT m.space_id FROM space_members m WHERE m.user_id = ?1)"
    };
    let sql = format!(
        "SELECT e.id, e.space_id, e.room_id, e.actor, e.text, e.created_at FROM space_events e
         WHERE {which} AND e.kind IN ('message', 'agent')
           AND e.id > (SELECT COALESCE(MAX(id), 0) FROM space_events) - ?4
           AND e.text LIKE ?2 ESCAPE '\\' AND {}
         ORDER BY e.id DESC LIMIT ?3",
        no_credentials("e.text")
    );
    let [c1, c2, c3] = CREDENTIAL_PREFIXES;
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map(
        params![
            user,
            pattern,
            limit as i64,
            SCAN_EVENTS,
            space,
            None::<i64>,
            None::<i64>,
            None::<i64>,
            None::<i64>,
            c1,
            c2,
            c3
        ],
        |r| {
            Ok(QuickMessage {
                event_id: r.get(0)?,
                space_id: Some(r.get(1)?),
                room_id: Some(r.get(2)?),
                actor: r.get(3)?,
                text: r.get(4)?,
                created_at: r.get(5)?,
            })
        },
    )?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// The lookup itself, inside one `Db::call`.
pub fn lookup(
    db: &Db,
    user: &str,
    query: &str,
    scope: Scope,
    space: Option<&str>,
    mode: Mode,
    kind: Option<FileKind>,
) -> anyhow::Result<QuickResponse> {
    let pattern = like_pattern(query);
    let want_messages = mode != Mode::Library;
    let want_assets = mode != Mode::Conversations;

    let mut messages = Vec::new();
    if want_messages {
        if matches!(scope, Scope::All | Scope::Chat) {
            messages.extend(chat_messages(db, user, &pattern, MAX_RESULTS)?);
        }
        match scope {
            Scope::All => messages.extend(space_messages(db, user, None, &pattern, MAX_RESULTS)?),
            Scope::Space => {
                messages.extend(space_messages(db, user, space, &pattern, MAX_RESULTS)?)
            }
            Scope::Chat | Scope::Personal => {}
        }
        // Chat and Space events are separate id sequences: merge newest
        // first by the time each was written, ties broken by place and id,
        // so the same data always gives the same order.
        messages.sort_by(|x, y| {
            y.created_at
                .cmp(&x.created_at)
                .then_with(|| x.space_id.cmp(&y.space_id))
                .then_with(|| y.event_id.cmp(&x.event_id))
        });
        messages.truncate(MAX_RESULTS);
    }

    let mut assets = Vec::new();
    if want_assets {
        let lib_scope = match scope {
            Scope::All => Some(library::Scope::All),
            Scope::Personal => Some(library::Scope::Personal),
            Scope::Space => space.map(|s| library::Scope::Space(s.to_owned())),
            Scope::Chat => None,
        };
        if let Some(lib_scope) = lib_scope {
            assets = library::find_by_name(
                db,
                user,
                &lib_scope,
                &pattern,
                kind.map(FileKind::kinds),
                MAX_RESULTS + 4,
            )?
            .into_iter()
            .filter(|a| !CREDENTIAL_PREFIXES.iter().any(|p| a.name.contains(p)))
            .take(MAX_RESULTS)
            .collect();
        }
    }

    let (m, a) = split(messages.len(), assets.len());
    messages.truncate(m);
    assets.truncate(a);
    for msg in &mut messages {
        msg.text = snippet(&msg.text, query);
    }
    Ok(QuickResponse { assets, messages })
}

async fn quick(
    State(state): State<AppState>,
    who: Human,
    body: Result<Json<QuickBody>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(body) = body?;
    let query = check_query(&body.query)?;
    if body.mode == Mode::Conversations && body.kind.is_some() {
        return Err(ApiError::bad_request("kind is only for library results"));
    }
    let space = match (body.scope, body.space_id) {
        (Scope::Space, None) => {
            return Err(ApiError::bad_request("scope \"space\" needs a space_id"));
        }
        (Scope::Space, Some(sid)) => {
            if !valid_id(&sid) {
                return Err(ApiError::not_found("space not found"));
            }
            Some(sid)
        }
        (_, Some(_)) => {
            return Err(ApiError::bad_request(
                "space_id is only allowed with scope \"space\"",
            ));
        }
        (_, None) => None,
    };
    let user = who.id.clone();
    let (scope, mode, kind) = (body.scope, body.mode, body.kind);
    let result = state
        .db
        .call(move |db| {
            if let Some(sid) = space.as_deref() {
                // Outsiders and unknown ids look the same.
                if db.membership(sid, &user)?.is_none() {
                    return Ok(None);
                }
            }
            lookup(db, &user, &query, scope, space.as_deref(), mode, kind).map(Some)
        })
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("space not found"))?;
    let mut response = Json(result).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns_are_literal() {
        assert_eq!(like_pattern("50%_off\\x"), "%50\\%\\_off\\\\x%");
        assert_eq!(like_pattern(".pdf"), "%.pdf%");
    }

    #[test]
    fn snippets_clip_around_the_first_match() {
        let long = format!("{}needle{}", "a".repeat(300), "b".repeat(300));
        let s = snippet(&long, "NEEDLE");
        assert!(s.starts_with('…') && s.ends_with('…'), "{s}");
        assert!(s.contains("needle"));
        assert_eq!(s.chars().filter(|c| *c != '…').count(), SNIPPET_CHARS);
        assert_eq!(snippet("short\ntext", "text"), "short text");
        // Near the end: the window shifts back instead of shrinking.
        let tail = format!("{}end", "x".repeat(400));
        assert_eq!(
            snippet(&tail, "end").chars().filter(|c| *c != '…').count(),
            SNIPPET_CHARS
        );
    }

    #[test]
    fn mixed_results_are_shared_and_capped() {
        assert_eq!(split(3, 2), (3, 2));
        assert_eq!(split(8, 8), (4, 4));
        assert_eq!(split(8, 1), (7, 1));
        assert_eq!(split(1, 8), (1, 7));
        assert_eq!(split(8, 0), (8, 0));
        assert_eq!(split(0, 8), (0, 8));
        assert_eq!(split(6, 6), (4, 4));
        for m in 0..=9 {
            for a in 0..=9 {
                let (km, ka) = split(m, a);
                assert!(km + ka <= MAX_RESULTS && km <= m && ka <= a);
                if m > 0 && a > 0 {
                    assert!(km > 0 && ka > 0, "{m} {a}");
                }
                // Nothing is dropped while there is room.
                assert_eq!(km + ka, (m.min(8) + a.min(8)).min(MAX_RESULTS), "{m} {a}");
            }
        }
    }
}
