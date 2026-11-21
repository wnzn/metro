//! Bearer-token authentication for Metro.
//!
//! Two mutually exclusive modes, chosen from the environment:
//!
//! * **Dev tokens** (`METRO_DEV_USERS`): an explicit JSON map of
//!   `actor -> token`. Meant for local development only.
//! * **OIDC** (`METRO_OIDC_ISSUER` + `METRO_OIDC_AUDIENCE`): JWT access
//!   tokens issued by an external provider (Ory Hydra, Authelia, authentik,
//!   ...) and verified against the provider's JWKS found through OIDC
//!   discovery.
//!
//! There is no implicit bypass: if neither mode is configured, or both are,
//! or a setting is malformed, [`Authenticator::from_env`] returns an error.
//! See `README.md` in this crate for the full environment contract.

mod dev;
mod http;
mod oidc;

use std::fmt;

/// Environment variable names read by [`Authenticator::from_env`].
pub mod env {
    /// JSON object mapping actor id to dev bearer token.
    pub const DEV_USERS: &str = "METRO_DEV_USERS";
    /// Legacy runner token. If still present in the server's environment, dev
    /// tokens are checked against it so the two can't collide. It is never
    /// accepted as a credential, and its absence is fine.
    pub const RUNNER_TOKEN: &str = "METRO_RUNNER_TOKEN";
    /// HTTPS issuer URL; must equal the `iss` claim exactly.
    pub const OIDC_ISSUER: &str = "METRO_OIDC_ISSUER";
    /// Required audience value in the `aud` claim.
    pub const OIDC_AUDIENCE: &str = "METRO_OIDC_AUDIENCE";
    /// Claim used as the *display* actor (default `sub`). The stable id is
    /// always `sub`.
    pub const OIDC_ACTOR_CLAIM: &str = "METRO_OIDC_ACTOR_CLAIM";
    /// Optional required JOSE `typ` header, e.g. `at+jwt`.
    pub const OIDC_REQUIRED_TYP: &str = "METRO_OIDC_REQUIRED_TYP";
    /// Optional clock-skew leeway in seconds (default 60, max 300).
    pub const OIDC_LEEWAY_SECS: &str = "METRO_OIDC_LEEWAY_SECS";

    /// Every `METRO_OIDC_*` variable; any of them conflicts with dev mode.
    pub const ALL_OIDC: &[&str] = &[
        OIDC_ISSUER,
        OIDC_AUDIENCE,
        OIDC_ACTOR_CLAIM,
        OIDC_REQUIRED_TYP,
        OIDC_LEEWAY_SECS,
    ];
}

/// Configuration error. Messages never contain token values.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigError {
    #[error(
        "no authentication configured: set either {dev} or {iss} + {aud}",
        dev = env::DEV_USERS,
        iss = env::OIDC_ISSUER,
        aud = env::OIDC_AUDIENCE
    )]
    NotConfigured,
    #[error("{dev} and METRO_OIDC_* settings are mutually exclusive; found {0}", dev = env::DEV_USERS)]
    ConflictingModes(String),
    #[error("{var} is not valid unicode")]
    NotUnicode { var: &'static str },
    #[error("{var} is invalid: {reason}")]
    Invalid { var: &'static str, reason: String },
    #[error("{var} is required when {because} is set")]
    Missing {
        var: &'static str,
        because: &'static str,
    },
    #[error("OIDC discovery failed: {0}")]
    Discovery(String),
}

/// Authentication failure. Map [`AuthError::Unavailable`] to 503 and every
/// other variant to 401; do not echo the reason to clients.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AuthError {
    /// No token was supplied.
    #[error("missing bearer token")]
    Missing,
    /// The token was rejected. The reason is for server logs only.
    #[error("invalid bearer token: {0}")]
    Invalid(&'static str),
    /// The identity provider's keys could not be obtained and the cached
    /// keys are too old to use. Fail closed.
    #[error("identity provider unavailable: {0}")]
    Unavailable(String),
}

impl AuthError {
    /// True when the failure is on the provider side (retryable), not the token.
    pub fn is_unavailable(&self) -> bool {
        matches!(self, AuthError::Unavailable(_))
    }
}

/// Which mode an [`Authenticator`] runs in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMode {
    Dev,
    Oidc,
}

/// Who a verified bearer token belongs to.
///
/// * `id` is the stable key for memberships and ownership. In dev mode it is
///   the username; in OIDC mode it is the token's `sub`, which is only unique
///   *within one issuer* (see [`Authenticator::namespace`]).
/// * `actor` is for display. In OIDC mode it comes from
///   `METRO_OIDC_ACTOR_CLAIM` (default `sub`) and may change over time; never
///   key data on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub id: String,
    pub actor: String,
}

/// Verifies user bearer tokens and returns an [`Identity`].
pub struct Authenticator {
    inner: Inner,
    namespace: String,
}

enum Inner {
    Dev(dev::DevUsers),
    Oidc(Box<oidc::OidcVerifier>),
}

impl fmt::Debug for Authenticator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never print tokens or key material.
        f.debug_struct("Authenticator")
            .field("mode", &self.mode())
            .finish_non_exhaustive()
    }
}

impl Authenticator {
    /// Build from the process environment. For OIDC this performs discovery
    /// and an initial JWKS fetch, so it fails if the provider is unreachable.
    pub async fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|name| match std::env::var(name) {
            Ok(v) => Ok(Some(v)),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(std::env::VarError::NotUnicode(_)) => Err(()),
        })
        .await
    }

    /// Like [`from_env`](Self::from_env) but reads variables through `lookup`
    /// (`Ok(None)` = unset, `Err(())` = not unicode). Useful for tests and for
    /// callers that load configuration from elsewhere.
    pub async fn from_lookup<F>(lookup: F) -> Result<Self, ConfigError>
    where
        F: Fn(&str) -> Result<Option<String>, ()>,
    {
        Self::from_lookup_with(lookup, oidc::Transport::HttpsOnly).await
    }

    async fn from_lookup_with<F>(lookup: F, transport: oidc::Transport) -> Result<Self, ConfigError>
    where
        F: Fn(&str) -> Result<Option<String>, ()>,
    {
        // Blank values count as unset: compose files commonly pass `VAR:`
        // through as an empty string. Fail-closed still holds because
        // "nothing set" is an error.
        let get = |var: &'static str| -> Result<Option<String>, ConfigError> {
            let value = lookup(var).map_err(|()| ConfigError::NotUnicode { var })?;
            Ok(value.filter(|v| !v.trim().is_empty()))
        };

        let dev_users = get(env::DEV_USERS)?;
        let mut oidc_present = Vec::new();
        for var in env::ALL_OIDC {
            if get(var)?.is_some() {
                oidc_present.push(*var);
            }
        }

        match (dev_users, oidc_present.is_empty()) {
            (Some(_), false) => Err(ConfigError::ConflictingModes(oidc_present.join(", "))),
            (Some(raw), true) => {
                let runner = get(env::RUNNER_TOKEN)?;
                let users = dev::DevUsers::parse(&raw, runner.as_deref())?;
                Ok(Self {
                    inner: Inner::Dev(users),
                    namespace: "dev".to_owned(),
                })
            }
            (None, false) => {
                let settings = oidc::OidcSettings::from_lookup(&get, transport)?;
                let namespace = format!("oidc:{}", settings.issuer);
                let verifier = Box::new(oidc::OidcVerifier::discover(settings).await?);
                Ok(Self {
                    inner: Inner::Oidc(verifier),
                    namespace,
                })
            }
            (None, true) => Err(ConfigError::NotConfigured),
        }
    }

    pub fn mode(&self) -> AuthMode {
        match self.inner {
            Inner::Dev(_) => AuthMode::Dev,
            Inner::Oidc(_) => AuthMode::Oidc,
        }
    }

    /// The space in which [`Identity::id`] is unique: `"dev"` for dev
    /// tokens, `"oidc:<METRO_OIDC_ISSUER>"` for OIDC. Ids from different
    /// namespaces must never be treated as the same person. Store it next to
    /// persisted ids, or refuse to start when it differs from the stored one.
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    /// The configured issuer in OIDC mode.
    pub fn issuer(&self) -> Option<&str> {
        match &self.inner {
            Inner::Dev(_) => None,
            Inner::Oidc(v) => Some(v.issuer()),
        }
    }

    /// Verify a bearer token and return who it belongs to.
    ///
    /// `bearer` may be the raw token or a full `Authorization` header value
    /// (`Bearer <token>`, scheme matched case-insensitively).
    pub async fn identity(&self, bearer: &str) -> Result<Identity, AuthError> {
        let token = strip_scheme(bearer);
        if token.is_empty() {
            return Err(AuthError::Missing);
        }
        match &self.inner {
            Inner::Dev(users) => users.identity(token),
            Inner::Oidc(verifier) => verifier.identity(token).await,
        }
    }

    /// Compatibility wrapper: [`identity`](Self::identity) projected to its
    /// display `actor`. Same verification, nothing checked twice. New code
    /// should key data on [`Identity::id`] instead.
    pub async fn authenticate(&self, bearer: &str) -> Result<String, AuthError> {
        self.identity(bearer).await.map(|i| i.actor)
    }
}

fn strip_scheme(value: &str) -> &str {
    let v = value.trim();
    if v.eq_ignore_ascii_case("bearer") {
        return "";
    }
    match v.split_once(' ') {
        Some((scheme, rest)) if scheme.eq_ignore_ascii_case("bearer") => rest.trim(),
        _ => v,
    }
}

/// OIDC Core 1.0 §2: `sub` "MUST NOT exceed 255 ASCII characters".
pub(crate) const MAX_SUBJECT_LEN: usize = 255;
/// Display values (names, emails) may be longer than a subject.
pub(crate) const MAX_DISPLAY_LEN: usize = 512;

/// Invisible characters that let one string pose as another: zero-width
/// characters, bidi embeddings/overrides/isolates, and the BOM.
fn is_deceptive(c: char) -> bool {
    matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}')
}

fn valid_text(s: &str, max: usize) -> bool {
    !s.is_empty()
        && s.len() <= max
        && s.trim() == s
        && !s.chars().any(|c| c.is_control() || is_deceptive(c))
}

/// Stable subject: 1-255 bytes, no control or invisible characters, no
/// surrounding whitespace. Any other punctuation is fine (`auth0|123`,
/// `user@host`, `CgVhbGljZRIEbGRhcA`, UUIDs, URLs).
pub(crate) fn valid_subject(s: &str) -> bool {
    valid_text(s, MAX_SUBJECT_LEN)
}

/// Display value: like a subject but up to 512 bytes.
pub(crate) fn valid_display(s: &str) -> bool {
    valid_text(s, MAX_DISPLAY_LEN)
}

#[cfg(test)]
mod tests;
