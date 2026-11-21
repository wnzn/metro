//! Bearer extraction for people and for Space runners.
//!
//! People are authenticated by a [`HumanAuth`] (in production
//! `metro_auth::Authenticator`), which yields a stable id for access control
//! and a display name. Runners are authenticated by a per-Space runner
//! token, which the server generates, shows once, and stores only as a
//! SHA-256 hash. Runner tokens carry a fixed prefix, and anything with that
//! prefix is refused on human endpoints before the human authenticator sees
//! it; a human token never matches a runner token hash.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::{HeaderMap, header};
use sha2::{Digest, Sha256};

use crate::AppState;
use crate::api::ApiError;
use crate::db::Author;

/// Bounds a slow identity provider (such as a JWKS fetch).
const AUTH_TIMEOUT: Duration = Duration::from_secs(10);
/// Longest bearer token considered at all.
const MAX_TOKEN_LEN: usize = 16 * 1024;
/// Every runner token starts with this.
pub const RUNNER_TOKEN_PREFIX: &str = "metro_rt_";

/// Who a verified human bearer token belongs to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserIdentity {
    /// Stable id (dev username or OIDC `sub`). The only key for access control.
    pub id: String,
    /// Display name; may change and is never used for access control.
    pub actor: String,
}

/// Why a human bearer token was not accepted. The detail is for server logs
/// only and is never sent to clients.
#[derive(Debug)]
pub enum AuthFailure {
    /// Missing or invalid token: 401.
    Rejected(String),
    /// The identity provider could not be reached and cached keys are stale: 503.
    Unavailable(String),
}

pub type AuthFuture<'a> =
    Pin<Box<dyn Future<Output = Result<UserIdentity, AuthFailure>> + Send + 'a>>;

/// Resolves a human bearer token to an identity.
pub trait HumanAuth: Send + Sync + 'static {
    fn identity<'a>(&'a self, bearer: &'a str) -> AuthFuture<'a>;
    /// The namespace in which identity ids are unique (`dev`, `oidc:<issuer>`).
    fn namespace(&self) -> &str;
}

impl HumanAuth for metro_auth::Authenticator {
    fn identity<'a>(&'a self, bearer: &'a str) -> AuthFuture<'a> {
        Box::pin(async move {
            match metro_auth::Authenticator::identity(self, bearer).await {
                Ok(identity) => Ok(UserIdentity {
                    id: identity.id,
                    actor: identity.actor,
                }),
                Err(e) if e.is_unavailable() => Err(AuthFailure::Unavailable(e.to_string())),
                Err(e) => Err(AuthFailure::Rejected(e.to_string())),
            }
        })
    }

    fn namespace(&self) -> &str {
        metro_auth::Authenticator::namespace(self)
    }
}

/// The bearer token of a request, if it carries exactly one well-formed one.
fn bearer(headers: &HeaderMap) -> Option<&str> {
    let mut values = headers.get_all(header::AUTHORIZATION).iter();
    let value = values.next()?;
    if values.next().is_some() {
        return None;
    }
    let value = value.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let token = token.trim();
    if token.is_empty() || token.len() > MAX_TOKEN_LEN {
        return None;
    }
    Some(token)
}

/// A fresh runner token: the prefix and 256 random bits as hex.
pub fn new_runner_token() -> anyhow::Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| anyhow::anyhow!("no system randomness: {e}"))?;
    let mut token = String::with_capacity(RUNNER_TOKEN_PREFIX.len() + 64);
    token.push_str(RUNNER_TOKEN_PREFIX);
    for b in bytes {
        token.push_str(&format!("{b:02x}"));
    }
    Ok(token)
}

/// What the database stores for a runner token. The token has 256 bits of
/// entropy, so a plain SHA-256 needs no salt or stretching.
pub fn runner_token_hash(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

/// An authenticated person.
#[derive(Clone, Debug)]
pub struct Human {
    pub id: String,
    /// Display name for events.
    pub name: String,
}

impl Human {
    pub fn author(&self) -> Author {
        Author {
            id: Some(self.id.clone()),
            name: self.name.clone(),
        }
    }
}

impl FromRequestParts<AppState> for Human {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let token = bearer(&parts.headers).ok_or_else(ApiError::unauthorized)?;
        if token.starts_with(RUNNER_TOKEN_PREFIX) {
            tracing::warn!("runner token presented on a human endpoint; refused");
            return Err(ApiError::unauthorized());
        }
        match tokio::time::timeout(AUTH_TIMEOUT, state.auth.identity(token)).await {
            Ok(Ok(identity)) if valid_id(&identity.id) => Ok(Human {
                name: display_name(&identity),
                id: identity.id,
            }),
            Ok(Ok(_)) => {
                tracing::warn!("authenticator returned an unusable user id");
                Err(ApiError::unauthorized())
            }
            Ok(Err(AuthFailure::Rejected(reason))) => {
                tracing::debug!(%reason, "human authentication failed");
                Err(ApiError::unauthorized())
            }
            Ok(Err(AuthFailure::Unavailable(reason))) => {
                tracing::warn!(%reason, "identity provider unavailable");
                Err(ApiError::unavailable(
                    "authentication is temporarily unavailable",
                ))
            }
            Err(_) => {
                tracing::warn!("human authentication timed out");
                Err(ApiError::unavailable(
                    "authentication is temporarily unavailable",
                ))
            }
        }
    }
}

/// User ids are what owners type to add members: printable, bounded.
pub fn valid_id(id: &str) -> bool {
    !id.trim().is_empty() && id == id.trim() && id.len() <= 256 && !id.chars().any(char::is_control)
}

/// The display name for events. A name that would impersonate the server's
/// own actors, or is unusable, falls back to the stable id, marked as a user.
fn display_name(identity: &UserIdentity) -> String {
    use crate::db::actor;
    let reserved = |n: &str| [actor::AGENT, actor::RUNNER, actor::SYSTEM].contains(&n.trim());
    let usable =
        |n: &str| !n.trim().is_empty() && n.len() <= 256 && !n.chars().any(char::is_control);
    if usable(&identity.actor) && !reserved(&identity.actor) {
        identity.actor.clone()
    } else if !reserved(&identity.id) {
        identity.id.clone()
    } else {
        format!("user:{}", identity.id)
    }
}

/// A Space's paired runner: the Space its token belongs to, and the
/// credential generation it was issued in.
pub struct Runner {
    pub space_id: String,
    pub generation: u64,
}

impl FromRequestParts<AppState> for Runner {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let token = bearer(&parts.headers)
            .filter(|t| t.starts_with(RUNNER_TOKEN_PREFIX))
            .ok_or_else(ApiError::unauthorized)?;
        let hash = runner_token_hash(token);
        let found = state
            .db
            .call(move |db| db.runner_by_token_hash(&hash))
            .await
            .map_err(ApiError::internal)?;
        match found {
            Some((space_id, generation)) => Ok(Runner {
                space_id,
                generation,
            }),
            None => Err(ApiError::unauthorized()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn parses_bearer() {
        let mut h = HeaderMap::new();
        assert_eq!(bearer(&h), None);
        h.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer abc"),
        );
        assert_eq!(bearer(&h), Some("abc"));
        h.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("bearer  abc "),
        );
        assert_eq!(bearer(&h), Some("abc"));
        h.insert(header::AUTHORIZATION, HeaderValue::from_static("Basic abc"));
        assert_eq!(bearer(&h), None);
        h.insert(header::AUTHORIZATION, HeaderValue::from_static("Bearer "));
        assert_eq!(bearer(&h), None);
        h.insert(header::AUTHORIZATION, HeaderValue::from_static("Bearer a"));
        h.append(header::AUTHORIZATION, HeaderValue::from_static("Bearer b"));
        assert_eq!(bearer(&h), None);
    }

    #[test]
    fn display_names_cannot_impersonate_the_server() {
        let id = |id: &str, actor: &str| UserIdentity {
            id: id.into(),
            actor: actor.into(),
        };
        assert_eq!(display_name(&id("u1", "Alice")), "Alice");
        assert_eq!(display_name(&id("u1", "agent")), "u1");
        assert_eq!(display_name(&id("system", "runner")), "user:system");
        assert_eq!(display_name(&id("u1", "a\nb")), "u1");
    }

    #[test]
    fn runner_tokens_are_random_and_prefixed() {
        let a = new_runner_token().unwrap();
        let b = new_runner_token().unwrap();
        assert_ne!(a, b);
        assert!(a.starts_with(RUNNER_TOKEN_PREFIX));
        assert_eq!(a.len(), RUNNER_TOKEN_PREFIX.len() + 64);
        assert_eq!(runner_token_hash(&a).len(), 32);
    }
}
