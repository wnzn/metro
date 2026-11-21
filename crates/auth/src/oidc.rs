//! OIDC mode: discovery + JWKS + JWT access-token verification.

use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use jsonwebtoken::errors::ErrorKind;
use jsonwebtoken::jwk::{AlgorithmParameters, EllipticCurve, Jwk, KeyOperations, PublicKeyUse};
use jsonwebtoken::{Algorithm, AlgorithmFamily, DecodingKey, Header, Validation};
use serde::Deserialize;
use serde_json::{Map, Value};
use tokio::sync::{Mutex, RwLock};
use url::Url;

use crate::{AuthError, ConfigError, Identity, env, http, valid_display, valid_subject};

/// Largest bearer token accepted in OIDC mode.
pub(crate) const MAX_JWT_LEN: usize = 16 * 1024;
/// At most this many keys are taken from a JWKS document.
pub(crate) const MAX_JWKS_KEYS: usize = 32;
pub(crate) const DEFAULT_LEEWAY_SECS: u64 = 60;
pub(crate) const MAX_LEEWAY_SECS: u64 = 300;

/// Asymmetric algorithms only. HMAC (`HS*`) and `none` are never accepted.
const ALLOWED_ALGS: &[Algorithm] = &[
    Algorithm::RS256,
    Algorithm::RS384,
    Algorithm::RS512,
    Algorithm::PS256,
    Algorithm::PS384,
    Algorithm::PS512,
    Algorithm::ES256,
    Algorithm::ES384,
    Algorithm::EdDSA,
];

/// JWKS cache timing.
#[derive(Debug, Clone, Copy)]
pub(crate) struct OidcLimits {
    /// Keys older than this are refreshed on the next request.
    pub(crate) refresh_interval: Duration,
    /// Minimum gap between two JWKS fetches (bounds unknown-`kid` refetches).
    pub(crate) min_refetch_interval: Duration,
    /// If refreshing fails, cached keys are used until they are this old;
    /// after that every request fails with [`AuthError::Unavailable`].
    pub(crate) max_stale: Duration,
}

impl Default for OidcLimits {
    fn default() -> Self {
        Self {
            refresh_interval: Duration::from_secs(10 * 60),
            min_refetch_interval: Duration::from_secs(30),
            max_stale: Duration::from_secs(60 * 60),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Transport {
    HttpsOnly,
    /// Plain HTTP permitted. Only reachable from unit tests.
    #[cfg(test)]
    AllowHttp,
}

impl Transport {
    fn https_only(self) -> bool {
        match self {
            Transport::HttpsOnly => true,
            #[cfg(test)]
            Transport::AllowHttp => false,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct OidcSettings {
    /// Exactly as configured; compared byte-for-byte with discovery `issuer`
    /// and the token `iss`.
    pub(crate) issuer: String,
    pub(crate) audience: String,
    pub(crate) actor_claim: String,
    pub(crate) required_typ: Option<String>,
    pub(crate) leeway_secs: u64,
    pub(crate) transport: Transport,
    pub(crate) limits: OidcLimits,
}

fn invalid(var: &'static str, reason: impl Into<String>) -> ConfigError {
    ConfigError::Invalid {
        var,
        reason: reason.into(),
    }
}

pub(crate) fn check_url(url: &Url, transport: Transport) -> Result<(), String> {
    match url.scheme() {
        "https" => {}
        #[cfg(test)]
        "http" if transport == Transport::AllowHttp => {}
        other => return Err(format!("must use https, got {other:?}")),
    }
    let _ = transport;
    if url.host_str().is_none_or(str::is_empty) {
        return Err("must have a host".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("must not contain credentials".into());
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err("must not contain a query or fragment".into());
    }
    Ok(())
}

impl OidcSettings {
    pub(crate) fn from_lookup(
        get: &dyn Fn(&'static str) -> Result<Option<String>, ConfigError>,
        transport: Transport,
    ) -> Result<Self, ConfigError> {
        let issuer = get(env::OIDC_ISSUER)?.ok_or(ConfigError::Missing {
            var: env::OIDC_ISSUER,
            because: "any METRO_OIDC_* variable",
        })?;
        let audience = get(env::OIDC_AUDIENCE)?.ok_or(ConfigError::Missing {
            var: env::OIDC_AUDIENCE,
            because: env::OIDC_ISSUER,
        })?;

        if issuer.is_empty() || issuer.trim() != issuer {
            return Err(invalid(
                env::OIDC_ISSUER,
                "must be a non-empty URL without surrounding spaces",
            ));
        }
        let issuer_url =
            Url::parse(&issuer).map_err(|e| invalid(env::OIDC_ISSUER, e.to_string()))?;
        check_url(&issuer_url, transport).map_err(|r| invalid(env::OIDC_ISSUER, r))?;

        if audience.is_empty() || audience.trim() != audience || audience.len() > 512 {
            return Err(invalid(
                env::OIDC_AUDIENCE,
                "must be a non-empty value without surrounding spaces",
            ));
        }

        let actor_claim = match get(env::OIDC_ACTOR_CLAIM)? {
            None => "sub".to_owned(),
            Some(c) => {
                let ok = !c.is_empty()
                    && c.len() <= 64
                    && c.bytes().all(|b| {
                        b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b':')
                    });
                if !ok {
                    return Err(invalid(env::OIDC_ACTOR_CLAIM, "must be a plain claim name"));
                }
                c
            }
        };

        let required_typ = match get(env::OIDC_REQUIRED_TYP)? {
            None => None,
            Some(t) if t.trim().is_empty() => {
                return Err(invalid(
                    env::OIDC_REQUIRED_TYP,
                    "must not be empty when set",
                ));
            }
            Some(t) => Some(normalize_typ(&t)),
        };

        let leeway_secs = match get(env::OIDC_LEEWAY_SECS)? {
            None => DEFAULT_LEEWAY_SECS,
            Some(s) => {
                let n: u64 = s
                    .parse()
                    .map_err(|_| invalid(env::OIDC_LEEWAY_SECS, "must be an integer"))?;
                if n > MAX_LEEWAY_SECS {
                    return Err(invalid(
                        env::OIDC_LEEWAY_SECS,
                        format!("must be at most {MAX_LEEWAY_SECS}"),
                    ));
                }
                n
            }
        };

        Ok(Self {
            issuer,
            audience,
            actor_claim,
            required_typ,
            leeway_secs,
            transport,
            limits: OidcLimits::default(),
        })
    }

    fn discovery_url(&self) -> Result<Url, ConfigError> {
        let base = self.issuer.trim_end_matches('/');
        Url::parse(&format!("{base}/.well-known/openid-configuration"))
            .map_err(|e| invalid(env::OIDC_ISSUER, e.to_string()))
    }
}

/// `typ` comparison per RFC 7515 §4.1.9: case-insensitive, `application/`
/// prefix optional.
fn normalize_typ(t: &str) -> String {
    let t = t.trim().to_ascii_lowercase();
    match t.strip_prefix("application/") {
        Some(rest) => rest.to_owned(),
        None => t,
    }
}

struct KeyEntry {
    kid: Option<String>,
    key: DecodingKey,
    /// Algorithms this key may verify. Derived from the key type (and the
    /// JWK `alg` if present), never from the token header.
    algs: Vec<Algorithm>,
}

struct KeySet {
    keys: Vec<KeyEntry>,
    fetched_at: Instant,
}

enum RefreshError {
    RateLimited,
    Failed(String),
}

pub(crate) struct OidcVerifier {
    settings: OidcSettings,
    client: reqwest::Client,
    jwks_uri: Url,
    keys: RwLock<Arc<KeySet>>,
    /// Serialises refreshes and records when the last fetch was attempted.
    refresh: Mutex<Instant>,
}

#[derive(Deserialize)]
struct DiscoveryDoc {
    issuer: String,
    jwks_uri: String,
}

#[derive(Deserialize)]
struct RawJwks {
    keys: Vec<Value>,
}

impl OidcVerifier {
    pub(crate) async fn discover(settings: OidcSettings) -> Result<Self, ConfigError> {
        install_crypto_provider();
        let client =
            http::client(settings.transport.https_only()).map_err(ConfigError::Discovery)?;
        let discovery_url = settings.discovery_url()?;
        let body = http::get_bounded(&client, &discovery_url)
            .await
            .map_err(ConfigError::Discovery)?;
        let doc: DiscoveryDoc = serde_json::from_slice(&body).map_err(|e| {
            ConfigError::Discovery(format!("{discovery_url}: invalid document: {e}"))
        })?;

        // OpenID Connect Discovery 1.0 §4.3: issuer must match exactly.
        if doc.issuer != settings.issuer {
            return Err(ConfigError::Discovery(format!(
                "discovery issuer {:?} does not exactly match {} {:?} (check the trailing slash)",
                doc.issuer,
                env::OIDC_ISSUER,
                settings.issuer
            )));
        }
        let jwks_uri = Url::parse(&doc.jwks_uri)
            .map_err(|e| ConfigError::Discovery(format!("jwks_uri: {e}")))?;
        check_url(&jwks_uri, settings.transport)
            .map_err(|r| ConfigError::Discovery(format!("jwks_uri {r}")))?;

        let keys = fetch_jwks(&client, &jwks_uri)
            .await
            .map_err(ConfigError::Discovery)?;

        let now = Instant::now();
        Ok(Self {
            settings,
            client,
            jwks_uri,
            keys: RwLock::new(Arc::new(KeySet {
                keys,
                fetched_at: now,
            })),
            refresh: Mutex::new(now),
        })
    }

    #[cfg(test)]
    pub(crate) fn set_limits(&mut self, limits: OidcLimits) {
        self.settings.limits = limits;
    }

    pub(crate) fn issuer(&self) -> &str {
        &self.settings.issuer
    }

    pub(crate) async fn identity(&self, token: &str) -> Result<Identity, AuthError> {
        if token.len() > MAX_JWT_LEN {
            return Err(AuthError::Invalid("token too long"));
        }
        let header = jsonwebtoken::decode_header(token)
            .map_err(|_| AuthError::Invalid("malformed token"))?;
        if !ALLOWED_ALGS.contains(&header.alg) {
            return Err(AuthError::Invalid("algorithm not allowed"));
        }
        if let Some(expected) = &self.settings.required_typ {
            match &header.typ {
                Some(t) if normalize_typ(t) == *expected => {}
                _ => return Err(AuthError::Invalid("unexpected token type")),
            }
        }

        let mut keys = self.current_keys().await?;
        if !has_candidate(&keys, &header) {
            // Possibly a rotated-in key: refetch, rate-limited.
            match self.refresh_keys().await {
                Ok(fresh) => keys = fresh,
                Err(RefreshError::RateLimited) => {}
                Err(RefreshError::Failed(e)) => return Err(AuthError::Unavailable(e)),
            }
            if !has_candidate(&keys, &header) {
                return Err(AuthError::Invalid("no matching signing key"));
            }
        }

        let claims = self.verify(token, &header, &keys)?;
        self.identity_from_claims(&claims)
    }

    fn verify(
        &self,
        token: &str,
        header: &Header,
        keys: &KeySet,
    ) -> Result<Map<String, Value>, AuthError> {
        let s = &self.settings;
        let mut v = Validation::new(header.alg);
        v.algorithms = vec![header.alg];
        v.leeway = s.leeway_secs;
        v.validate_exp = true;
        v.validate_nbf = true;
        v.validate_aud = true;
        v.set_issuer(&[&s.issuer]);
        v.set_audience(&[&s.audience]);
        // `sub` is always required: it is the stable id, whatever claim is
        // used for display.
        v.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);

        let mut last = AuthError::Invalid("no matching signing key");
        for entry in candidates(keys, header) {
            match jsonwebtoken::decode::<Map<String, Value>>(token, &entry.key, &v) {
                Ok(data) => return Ok(data.claims),
                Err(e) => match e.kind() {
                    // Try the next candidate (only relevant without a kid).
                    ErrorKind::InvalidSignature => last = AuthError::Invalid("bad signature"),
                    other => return Err(AuthError::Invalid(describe(other))),
                },
            }
        }
        Err(last)
    }

    fn identity_from_claims(&self, claims: &Map<String, Value>) -> Result<Identity, AuthError> {
        // Belt and braces: the library accepts array-valued `iss`; we require
        // the single string form with an exact match.
        match claims.get("iss") {
            Some(Value::String(iss)) if *iss == self.settings.issuer => {}
            _ => return Err(AuthError::Invalid("wrong issuer")),
        }
        if !matches!(claims.get("exp"), Some(Value::Number(_))) {
            return Err(AuthError::Invalid("missing exp"));
        }
        let id = match claims.get("sub") {
            Some(Value::String(sub)) if valid_subject(sub) => sub.clone(),
            Some(_) => return Err(AuthError::Invalid("sub has an invalid value")),
            None => return Err(AuthError::Invalid("sub missing")),
        };
        let actor = if self.settings.actor_claim == "sub" {
            id.clone()
        } else {
            match claims.get(&self.settings.actor_claim) {
                Some(Value::String(a)) if valid_display(a) => a.clone(),
                Some(_) => return Err(AuthError::Invalid("actor claim has an invalid value")),
                None => return Err(AuthError::Invalid("actor claim missing")),
            }
        };
        Ok(Identity { id, actor })
    }

    /// Current keys, refreshing when they are older than `refresh_interval`.
    async fn current_keys(&self) -> Result<Arc<KeySet>, AuthError> {
        let keys = self.keys.read().await.clone();
        let age = keys.fetched_at.elapsed();
        if age < self.settings.limits.refresh_interval {
            return Ok(keys);
        }
        match self.refresh_keys().await {
            Ok(fresh) => Ok(fresh),
            Err(_) if age < self.settings.limits.max_stale => Ok(keys),
            Err(RefreshError::Failed(e)) => Err(AuthError::Unavailable(e)),
            Err(RefreshError::RateLimited) => Err(AuthError::Unavailable(
                "signing keys expired and refresh is rate limited".into(),
            )),
        }
    }

    /// Single-flight, rate-limited JWKS refetch.
    async fn refresh_keys(&self) -> Result<Arc<KeySet>, RefreshError> {
        let requested = Instant::now();
        let mut last_attempt = self.refresh.lock().await;
        {
            let current = self.keys.read().await.clone();
            if current.fetched_at >= requested {
                // Someone refreshed while we waited for the lock.
                return Ok(current);
            }
        }
        if last_attempt.elapsed() < self.settings.limits.min_refetch_interval {
            return Err(RefreshError::RateLimited);
        }
        *last_attempt = Instant::now();
        let keys = fetch_jwks(&self.client, &self.jwks_uri)
            .await
            .map_err(RefreshError::Failed)?;
        let set = Arc::new(KeySet {
            keys,
            fetched_at: Instant::now(),
        });
        *self.keys.write().await = set.clone();
        Ok(set)
    }
}

fn describe(kind: &ErrorKind) -> &'static str {
    match kind {
        ErrorKind::ExpiredSignature => "expired",
        ErrorKind::ImmatureSignature => "not yet valid",
        ErrorKind::InvalidIssuer => "wrong issuer",
        ErrorKind::InvalidAudience => "wrong audience",
        ErrorKind::MissingRequiredClaim(_) => "missing required claim",
        ErrorKind::InvalidClaimFormat(_) => "malformed claim",
        ErrorKind::InvalidAlgorithm => "algorithm not allowed",
        ErrorKind::InvalidSignature => "bad signature",
        _ => "invalid token",
    }
}

fn candidates<'a>(keys: &'a KeySet, header: &'a Header) -> impl Iterator<Item = &'a KeyEntry> + 'a {
    keys.keys.iter().filter(move |k| {
        k.algs.contains(&header.alg)
            && match &header.kid {
                Some(kid) => k.kid.as_deref() == Some(kid.as_str()),
                None => true,
            }
    })
}

fn has_candidate(keys: &KeySet, header: &Header) -> bool {
    candidates(keys, header).next().is_some()
}

async fn fetch_jwks(client: &reqwest::Client, uri: &Url) -> Result<Vec<KeyEntry>, String> {
    let body = http::get_bounded(client, uri).await?;
    let keys = parse_jwks(&body).map_err(|e| format!("{uri}: {e}"))?;
    Ok(keys)
}

fn parse_jwks(body: &[u8]) -> Result<Vec<KeyEntry>, String> {
    let raw: RawJwks = serde_json::from_slice(body).map_err(|e| format!("invalid JWKS: {e}"))?;
    let keys: Vec<KeyEntry> = raw
        .keys
        .into_iter()
        .take(MAX_JWKS_KEYS)
        .filter_map(|v| serde_json::from_value::<Jwk>(v).ok())
        .filter_map(|jwk| key_entry(&jwk))
        .collect();
    if keys.is_empty() {
        return Err("JWKS contains no usable asymmetric signing keys".into());
    }
    Ok(keys)
}

/// Turn a JWK into a verification key bound to the algorithms its key type
/// permits. Symmetric (`oct`) keys and encryption keys are discarded.
fn key_entry(jwk: &Jwk) -> Option<KeyEntry> {
    if matches!(
        jwk.common.public_key_use,
        Some(PublicKeyUse::Encryption | PublicKeyUse::Other(_))
    ) {
        return None;
    }
    if jwk
        .common
        .key_operations
        .as_ref()
        .is_some_and(|ops| !ops.contains(&KeyOperations::Verify))
    {
        return None;
    }

    let by_type: Vec<Algorithm> = match &jwk.algorithm {
        AlgorithmParameters::RSA(_) => AlgorithmFamily::Rsa.algorithms().to_vec(),
        AlgorithmParameters::EllipticCurve(p) => match p.curve {
            EllipticCurve::P256 => vec![Algorithm::ES256],
            EllipticCurve::P384 => vec![Algorithm::ES384],
            _ => return None,
        },
        AlgorithmParameters::OctetKeyPair(p) => match p.curve {
            EllipticCurve::Ed25519 => vec![Algorithm::EdDSA],
            _ => return None,
        },
        // Never accept symmetric keys from a JWKS, and ignore unknown types.
        _ => return None,
    };

    let algs = match jwk.common.key_algorithm {
        None => by_type,
        Some(ka) => {
            let alg = Algorithm::from_str(&ka.to_string()).ok()?;
            if !by_type.contains(&alg) {
                return None;
            }
            vec![alg]
        }
    };
    let algs: Vec<Algorithm> = algs
        .into_iter()
        .filter(|a| ALLOWED_ALGS.contains(a))
        .collect();
    if algs.is_empty() {
        return None;
    }

    let key = DecodingKey::from_jwk(jwk).ok()?;
    Some(KeyEntry {
        kid: jwk.common.key_id.clone(),
        key,
        algs,
    })
}

/// Select aws-lc-rs as jsonwebtoken's process-wide provider. Harmless if a
/// provider is already installed.
fn install_crypto_provider() {
    if jsonwebtoken::crypto::CryptoProvider::try_get_default().is_none() {
        let _ = jsonwebtoken::crypto::aws_lc::DEFAULT_PROVIDER.install_default();
    }
}

#[cfg(test)]
pub(crate) fn parse_jwks_for_test(body: &[u8]) -> Result<usize, String> {
    parse_jwks(body).map(|k| k.len())
}
