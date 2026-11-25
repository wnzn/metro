//! Guarded (conditional) dedicated runner pairing.
//!
//! The existing owner routes `POST|DELETE /v1/spaces/{sid}/runner-token`
//! keep their unconditional meaning: issue (or rotate) and revoke. A client
//! that must never replace or revoke someone else's runner, such as the
//! desktop app's local workspace tools, sends a precondition instead:
//!
//! - `POST` with `If-None-Match: *` issues a token only while the Space has
//!   no runner at all (no dedicated token and no attached registered runner).
//!   The check and the write are one SQL statement. On success the response
//!   carries `ETag: "rtg-<generation>"` (also as `etag` in the body), an
//!   opaque tag for exactly this pairing. Otherwise `412`.
//! - `DELETE` with `If-Match: "rtg-<generation>"` revokes the dedicated token
//!   only if the Space's runner generation still equals that tag, which holds
//!   only while that same token is the current one: every issue, rotation,
//!   revocation, attachment or detachment bumps the generation. The check and
//!   the write are again one statement. Otherwise `412`.
//!
//! Both are owner-only, like the unconditional routes. Tokens never appear in
//! URLs, headers other than the response body, or logs.

use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use rusqlite::{OptionalExtension, params};
use serde_json::json;

use crate::AppState;
use crate::api::{ApiError, ApiResult, db, empty_object, no_store, owner_of};
use crate::auth::{Human, new_runner_token, runner_token_hash};
use crate::db::Db;

const ETAG_PREFIX: &str = "rtg-";

/// What the request asked for, from its precondition headers.
enum Precondition {
    /// No precondition header: the unconditional route applies.
    None,
    /// `If-None-Match: *`.
    Unpaired,
    /// `If-Match: "rtg-<generation>"`.
    Generation(u64),
}

/// The tag for `generation`, quoted as an HTTP entity tag.
pub fn etag(generation: u64) -> String {
    format!("\"{ETAG_PREFIX}{generation}\"")
}

/// Parse one strong entity tag of our shape.
fn parse_etag(raw: &str) -> Option<u64> {
    let inner = raw.trim().strip_prefix('"')?.strip_suffix('"')?;
    let digits = inner.strip_prefix(ETAG_PREFIX)?;
    if digits.is_empty() || digits.len() > 19 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

fn single<'a>(headers: &'a HeaderMap, name: &header::HeaderName) -> ApiResult<Option<&'a str>> {
    let mut values = headers.get_all(name).iter();
    let Some(first) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(ApiError::bad_request(format!(
            "send at most one {name} header"
        )));
    }
    first
        .to_str()
        .map(Some)
        .map_err(|_| ApiError::bad_request(format!("{name} is not valid")))
}

fn precondition(headers: &HeaderMap, allow: &header::HeaderName) -> ApiResult<Precondition> {
    let none_match = single(headers, &header::IF_NONE_MATCH)?;
    let if_match = single(headers, &header::IF_MATCH)?;
    match (none_match, if_match) {
        (None, None) => Ok(Precondition::None),
        (Some(_), Some(_)) => Err(ApiError::bad_request(
            "send either If-None-Match or If-Match, not both",
        )),
        (Some(v), None) if allow == header::IF_NONE_MATCH && v.trim() == "*" => {
            Ok(Precondition::Unpaired)
        }
        (None, Some(v)) if allow == header::IF_MATCH => parse_etag(v)
            .map(Precondition::Generation)
            .ok_or_else(|| ApiError::bad_request("If-Match must be an ETag this route returned")),
        _ => Err(ApiError::bad_request(format!(
            "this route accepts only the {allow} precondition"
        ))),
    }
}

fn precondition_failed(message: &str) -> ApiError {
    ApiError::new(StatusCode::PRECONDITION_FAILED, message)
}

impl Db {
    /// Set `space_id`'s dedicated token only if it has no runner of any kind;
    /// returns the new generation. One statement, so nothing can pair the
    /// Space between the check and the write.
    pub fn set_runner_token_if_unpaired(
        &self,
        space_id: &str,
        hash: &[u8],
    ) -> anyhow::Result<Option<u64>> {
        let conn = self.lock();
        Ok(conn
            .query_row(
                "UPDATE spaces
                     SET runner_token_hash = ?2, runner_generation = runner_generation + 1,
                         runner_drop_before = runner_generation + 1
                  WHERE id = ?1
                    AND runner_token_hash IS NULL
                    AND NOT EXISTS (SELECT 1 FROM space_runners WHERE space_id = ?1)
                  RETURNING runner_generation",
                params![space_id, hash],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            .map(|g| g as u64))
    }

    /// Revoke `space_id`'s dedicated token only while its runner generation
    /// is still `generation`; returns the new generation. One statement.
    pub fn clear_runner_token_if_generation(
        &self,
        space_id: &str,
        generation: u64,
    ) -> anyhow::Result<Option<u64>> {
        let Ok(generation) = i64::try_from(generation) else {
            return Ok(None);
        };
        let conn = self.lock();
        Ok(conn
            .query_row(
                "UPDATE spaces
                     SET runner_token_hash = NULL, runner_generation = runner_generation + 1,
                         runner_drop_before = runner_generation + 1
                  WHERE id = ?1
                    AND runner_generation = ?2
                    AND runner_token_hash IS NOT NULL
                    AND NOT EXISTS (SELECT 1 FROM space_runners WHERE space_id = ?1)
                  RETURNING runner_generation",
                params![space_id, generation],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            .map(|g| g as u64))
    }
}

/// `POST /v1/spaces/{sid}/runner-token` with `If-None-Match: *`. `None` when
/// the request has no precondition (the caller then runs the unconditional
/// route unchanged).
pub(crate) async fn issue_if_requested(
    state: &AppState,
    who: &Human,
    sid: &str,
    headers: &HeaderMap,
    body: &[u8],
) -> Option<ApiResult<Response>> {
    match precondition(headers, &header::IF_NONE_MATCH) {
        Ok(Precondition::None) => None,
        Ok(Precondition::Unpaired) => Some(issue_unpaired(state, who, sid, body).await),
        Ok(Precondition::Generation(_)) => unreachable!("rejected by precondition()"),
        Err(e) => Some(Err(e)),
    }
}

async fn issue_unpaired(
    state: &AppState,
    who: &Human,
    sid: &str,
    body: &[u8],
) -> ApiResult<Response> {
    empty_object(body)?;
    let space = owner_of(state, sid, who).await?;
    let token = new_runner_token().map_err(ApiError::internal)?;
    let hash = runner_token_hash(&token);
    let space_id = space.id.clone();
    let Some(generation) = db(state, move |db| {
        db.set_runner_token_if_unpaired(&space_id, &hash)
    })
    .await?
    else {
        return Err(precondition_failed(
            "this Space already has a runner; nothing was changed",
        ));
    };
    state.jobs.rotate(&space.id, generation);
    tracing::info!(space = %space.id, generation, "runner token issued (guarded)");
    let tag = etag(generation);
    let mut response = no_store(StatusCode::OK, json!({"token": token, "etag": tag}));
    if let Ok(v) = HeaderValue::from_str(&tag) {
        response.headers_mut().insert(header::ETAG, v);
    }
    Ok(response)
}

/// `DELETE /v1/spaces/{sid}/runner-token` with `If-Match`. `None` without a
/// precondition.
pub(crate) async fn revoke_if_requested(
    state: &AppState,
    who: &Human,
    sid: &str,
    headers: &HeaderMap,
) -> Option<ApiResult<Response>> {
    match precondition(headers, &header::IF_MATCH) {
        Ok(Precondition::None) => None,
        Ok(Precondition::Generation(g)) => Some(revoke_generation(state, who, sid, g).await),
        Ok(Precondition::Unpaired) => unreachable!("rejected by precondition()"),
        Err(e) => Some(Err(e)),
    }
}

async fn revoke_generation(
    state: &AppState,
    who: &Human,
    sid: &str,
    generation: u64,
) -> ApiResult<Response> {
    let space = owner_of(state, sid, who).await?;
    let space_id = space.id.clone();
    let Some(new_generation) = db(state, move |db| {
        db.clear_runner_token_if_generation(&space_id, generation)
    })
    .await?
    else {
        return Err(precondition_failed(
            "this Space's runner changed since that pairing; nothing was revoked",
        ));
    };
    state.jobs.revoke(&space.id, new_generation);
    tracing::info!(space = %space.id, generation = new_generation, "runner token revoked (guarded)");
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn etags_round_trip() {
        assert_eq!(parse_etag(&etag(7)), Some(7));
        assert_eq!(parse_etag(" \"rtg-12\" "), Some(12));
        for bad in [
            "rtg-1",
            "W/\"rtg-1\"",
            "\"rtg-\"",
            "\"rtg-1a\"",
            "\"x-1\"",
            "*",
            "\"rtg-99999999999999999999\"",
        ] {
            assert_eq!(parse_etag(bad), None, "{bad}");
        }
    }

    #[test]
    fn preconditions() {
        let h = |pairs: &[(&header::HeaderName, &str)]| {
            let mut m = HeaderMap::new();
            for (k, v) in pairs {
                m.append((*k).clone(), HeaderValue::from_str(v).unwrap());
            }
            m
        };
        let inm = &header::IF_NONE_MATCH;
        let im = &header::IF_MATCH;
        assert!(matches!(precondition(&h(&[]), inm), Ok(Precondition::None)));
        assert!(matches!(
            precondition(&h(&[(inm, "*")]), inm),
            Ok(Precondition::Unpaired)
        ));
        assert!(precondition(&h(&[(inm, "\"rtg-1\"")]), inm).is_err());
        assert!(precondition(&h(&[(im, "\"rtg-1\"")]), inm).is_err());
        assert!(precondition(&h(&[(inm, "*"), (inm, "*")]), inm).is_err());
        assert!(matches!(
            precondition(&h(&[(im, "\"rtg-3\"")]), im),
            Ok(Precondition::Generation(3))
        ));
        assert!(precondition(&h(&[(im, "*")]), im).is_err());
        assert!(precondition(&h(&[(inm, "*")]), im).is_err());
        assert!(precondition(&h(&[(im, "\"rtg-3\""), (inm, "*")]), im).is_err());
    }
}
