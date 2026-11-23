//! OIDC discovery, JWKS, ID-token validation, PKCE and random values.

use std::str::FromStr;

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use jsonwebtoken::jwk::{AlgorithmParameters, EllipticCurve, Jwk, KeyOperations, PublicKeyUse};
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use super::http::{self, Failure, MAX_JSON};
use super::store::random_bytes;

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
const MAX_JWT: usize = 16 * 1024;
const LEEWAY: u64 = 5;

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct Discovery {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub jwks_uri: String,
    #[serde(default)]
    pub revocation_endpoint: Option<String>,
}

/// Fetches `{issuer}/.well-known/openid-configuration` and requires an exact
/// issuer match. Every endpoint must share the issuer's origin.
pub(crate) async fn discover(client: &reqwest::Client, issuer: &str) -> Result<Discovery> {
    let base = http::check_url(issuer)?;
    let url = format!(
        "{}/.well-known/openid-configuration",
        issuer.trim_end_matches('/')
    );
    let resp = client.get(&url).send().await.map_err(http::net)?;
    let status = resp.status();
    if !status.is_success() {
        let body = http::read_bounded(resp, MAX_JSON).await.unwrap_or_default();
        return Err(Failure::parse(status.as_u16(), &body).error("OIDC discovery"));
    }
    let body = http::read_bounded(resp, MAX_JSON).await?;
    let d: Discovery =
        serde_json::from_slice(&body).context("OIDC discovery document is malformed")?;
    if d.issuer != issuer {
        bail!("OIDC discovery issuer mismatch");
    }
    let mut endpoints = vec![&d.authorization_endpoint, &d.token_endpoint, &d.jwks_uri];
    if let Some(r) = &d.revocation_endpoint {
        endpoints.push(r);
    }
    for e in endpoints {
        let u = http::check_url(e)?;
        if u.origin() != base.origin() {
            bail!("OIDC discovery names an endpoint outside the issuer origin");
        }
    }
    Ok(d)
}

pub(crate) struct KeyEntry {
    kid: Option<String>,
    key: DecodingKey,
    algs: Vec<Algorithm>,
}

pub(crate) async fn fetch_jwks(client: &reqwest::Client, uri: &str) -> Result<Vec<KeyEntry>> {
    install_crypto_provider();
    let resp = client.get(uri).send().await.map_err(http::net)?;
    let status = resp.status();
    if !status.is_success() {
        let body = http::read_bounded(resp, MAX_JSON).await.unwrap_or_default();
        return Err(Failure::parse(status.as_u16(), &body).error("JWKS fetch"));
    }
    let body = http::read_bounded(resp, MAX_JSON).await?;
    #[derive(Deserialize)]
    struct Raw {
        keys: Vec<Value>,
    }
    let raw: Raw = serde_json::from_slice(&body).context("JWKS is malformed")?;
    let keys: Vec<KeyEntry> = raw
        .keys
        .into_iter()
        .take(64)
        .filter_map(|v| serde_json::from_value::<Jwk>(v).ok())
        .filter_map(|j| key_entry(&j))
        .collect();
    if keys.is_empty() {
        bail!("JWKS contains no usable signing keys");
    }
    Ok(keys)
}

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
        AlgorithmParameters::RSA(_) => vec![
            Algorithm::RS256,
            Algorithm::RS384,
            Algorithm::RS512,
            Algorithm::PS256,
            Algorithm::PS384,
            Algorithm::PS512,
        ],
        AlgorithmParameters::EllipticCurve(p) => match p.curve {
            EllipticCurve::P256 => vec![Algorithm::ES256],
            EllipticCurve::P384 => vec![Algorithm::ES384],
            _ => return None,
        },
        AlgorithmParameters::OctetKeyPair(p) => match p.curve {
            EllipticCurve::Ed25519 => vec![Algorithm::EdDSA],
            _ => return None,
        },
        _ => return None,
    };
    let algs = match jwk.common.key_algorithm {
        None => by_type,
        Some(ka) => {
            let a = Algorithm::from_str(&ka.to_string()).ok()?;
            if !by_type.contains(&a) {
                return None;
            }
            vec![a]
        }
    };
    let algs: Vec<Algorithm> = algs
        .into_iter()
        .filter(|a| ALLOWED_ALGS.contains(a))
        .collect();
    if algs.is_empty() {
        return None;
    }
    Some(KeyEntry {
        kid: jwk.common.key_id.clone(),
        key: DecodingKey::from_jwk(jwk).ok()?,
        algs,
    })
}

fn install_crypto_provider() {
    if jsonwebtoken::crypto::CryptoProvider::try_get_default().is_none() {
        let _ = jsonwebtoken::crypto::aws_lc::DEFAULT_PROVIDER.install_default();
    }
}

#[derive(Clone, Debug)]
pub(crate) struct IdClaims {
    pub sub: String,
    pub email: Option<String>,
}

/// Checks the signature against the JWKS, and checks `iss` exactly, `aud`
/// containing the issued client ID, `exp`/`iat`, `sub`, and `nonce` when one
/// is expected. A refresh response's ID token may carry no nonce.
pub(crate) fn verify_id_token(
    token: &str,
    keys: &[KeyEntry],
    issuer: &str,
    client_id: &str,
    nonce: Option<&str>,
) -> Result<IdClaims> {
    install_crypto_provider();
    if token.len() > MAX_JWT {
        bail!("ID token too long");
    }
    let header =
        jsonwebtoken::decode_header(token).map_err(|_| anyhow!("ID token is malformed"))?;
    if !ALLOWED_ALGS.contains(&header.alg) {
        bail!("ID token algorithm not allowed");
    }
    let mut v = Validation::new(header.alg);
    v.algorithms = vec![header.alg];
    v.leeway = LEEWAY;
    v.validate_exp = true;
    v.validate_nbf = true;
    v.validate_aud = true;
    v.set_issuer(&[issuer]);
    v.set_audience(&[client_id]);
    v.set_required_spec_claims(&["exp", "iat", "iss", "aud", "sub"]);

    let mut last = anyhow!("ID token: no matching signing key");
    let mut claims = None;
    for k in keys.iter().filter(|k| {
        k.algs.contains(&header.alg)
            && match &header.kid {
                Some(kid) => k.kid.as_deref() == Some(kid.as_str()),
                None => true,
            }
    }) {
        match jsonwebtoken::decode::<Map<String, Value>>(token, &k.key, &v) {
            Ok(d) => {
                claims = Some(d.claims);
                break;
            }
            Err(e) => {
                use jsonwebtoken::errors::ErrorKind as K;
                let what = match e.kind() {
                    K::InvalidSignature => {
                        last = anyhow!("ID token signature is invalid");
                        continue;
                    }
                    K::ExpiredSignature => "expired",
                    K::ImmatureSignature => "not yet valid",
                    K::InvalidIssuer => "wrong issuer",
                    K::InvalidAudience => "wrong audience",
                    K::MissingRequiredClaim(_) => "missing a required claim",
                    _ => "invalid",
                };
                bail!("ID token is {what}");
            }
        }
    }
    let claims = claims.ok_or(last)?;
    if claims.get("iss").and_then(Value::as_str) != Some(issuer) {
        bail!("ID token is wrong issuer");
    }
    if let Some(expected) = nonce {
        let got = claims.get("nonce").and_then(Value::as_str).unwrap_or("");
        if !bool::from(got.as_bytes().ct_eq(expected.as_bytes())) {
            bail!("ID token nonce does not match this sign-in attempt");
        }
    }
    let sub = match claims.get("sub").and_then(Value::as_str) {
        Some(s) if !s.is_empty() && s.len() <= 255 && !s.chars().any(char::is_control) => {
            s.to_owned()
        }
        _ => bail!("ID token subject is invalid"),
    };
    if let Some(sid) = claims.get("sid")
        && !sid.as_str().is_some_and(|s| !s.trim().is_empty())
    {
        bail!("ID token session identifier is invalid");
    }
    let email = claims
        .get("email")
        .and_then(Value::as_str)
        .filter(|e| !e.is_empty() && e.len() <= 320 && !e.chars().any(char::is_control))
        .map(str::to_owned);
    Ok(IdClaims { sub, email })
}

/// Checks that the access token is bound to this resource and issued client.
/// The access token is a bearer for OpenAI, so it's decoded here without
/// signature verification. This is a consistency check, not trust: it stops a
/// mixed-up token set from being stored.
pub(crate) fn check_access_token(
    token: &str,
    issuer: &str,
    resource: &str,
    client_id: &str,
) -> Result<Option<String>> {
    if token.len() > MAX_JWT * 4
        || token.is_empty()
        || token.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        bail!("access token is malformed");
    }
    let mut parts = token.split('.');
    let (Some(_), Some(payload), Some(_), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        bail!("access token is not a JWT; cannot confirm its resource binding");
    };
    let bytes = URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .map_err(|_| anyhow!("access token payload is malformed"))?;
    let c: Map<String, Value> =
        serde_json::from_slice(&bytes).map_err(|_| anyhow!("access token payload is malformed"))?;
    let aud_ok = match c.get("aud") {
        Some(Value::String(a)) => a == resource,
        Some(Value::Array(a)) => a.iter().any(|x| x.as_str() == Some(resource)),
        _ => false,
    };
    if !aud_ok {
        bail!("access token audience is not {resource}");
    }
    if c.get("iss")
        .and_then(Value::as_str)
        .is_some_and(|i| i != issuer)
    {
        bail!("access token issuer mismatch");
    }
    if c.get("client_id")
        .and_then(Value::as_str)
        .is_some_and(|i| i != client_id)
    {
        bail!("access token was issued to a different client");
    }
    Ok(c.get("scope").and_then(Value::as_str).map(str::to_owned))
}

pub(crate) fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

pub(crate) fn random_token() -> String {
    b64(&random_bytes::<32>())
}

pub(crate) struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

pub(crate) fn pkce() -> Pkce {
    // 64 random bytes give an 86-character verifier (RFC 7636 allows 43–128).
    let verifier = b64(&random_bytes::<64>());
    let challenge = b64(&Sha256::digest(verifier.as_bytes()));
    Pkce {
        verifier,
        challenge,
    }
}
