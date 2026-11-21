//! Unit tests. OIDC tests run a local wiremock provider over plain HTTP via
//! the test-only `Transport::AllowHttp`; signing keys are generated per test
//! run so no private key material lives in the repository.

use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::Duration;

use aws_lc_rs::encoding::AsDer;
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use jsonwebtoken::jwk::Jwk;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::oidc::{OidcLimits, Transport};

// ---------------------------------------------------------------- helpers

fn lookup(vars: &[(&str, &str)]) -> impl Fn(&str) -> Result<Option<String>, ()> + use<> {
    let map: HashMap<String, String> = vars
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    move |k| Ok(map.get(k).cloned())
}

async fn build(vars: &[(&str, &str)]) -> Result<Authenticator, ConfigError> {
    Authenticator::from_lookup_with(lookup(vars), Transport::AllowHttp).await
}

async fn build_https_only(vars: &[(&str, &str)]) -> Result<Authenticator, ConfigError> {
    Authenticator::from_lookup(lookup(vars)).await
}

fn pem(tag: &str, der: &[u8]) -> String {
    let b64 = STANDARD.encode(der);
    let body: Vec<&str> = b64
        .as_bytes()
        .chunks(64)
        .map(|c| std::str::from_utf8(c).unwrap())
        .collect();
    format!(
        "-----BEGIN {tag}-----\n{}\n-----END {tag}-----\n",
        body.join("\n")
    )
}

struct TestKey {
    enc: EncodingKey,
    jwk: Value,
}

fn make_jwk(enc: &EncodingKey, alg: Algorithm, kid: &str) -> Value {
    let mut jwk = serde_json::to_value(Jwk::from_encoding_key(enc, alg).unwrap()).unwrap();
    jwk["kid"] = json!(kid);
    jwk["use"] = json!("sig");
    jwk
}

fn rsa_key(kid: &str) -> (TestKey, String) {
    let kp = aws_lc_rs::rsa::KeyPair::generate(aws_lc_rs::rsa::KeySize::Rsa2048).unwrap();
    let pkcs8 = AsDer::<aws_lc_rs::encoding::Pkcs8V1Der>::as_der(&kp).unwrap();
    let enc = EncodingKey::from_rsa_pem(pem("PRIVATE KEY", pkcs8.as_ref()).as_bytes()).unwrap();
    let public_pem = {
        use aws_lc_rs::signature::KeyPair as _;
        let spki = AsDer::<aws_lc_rs::encoding::PublicKeyX509Der>::as_der(kp.public_key()).unwrap();
        pem("PUBLIC KEY", spki.as_ref())
    };
    let jwk = make_jwk(&enc, Algorithm::RS256, kid);
    (TestKey { enc, jwk }, public_pem)
}

fn ec_key(kid: &str) -> TestKey {
    let rng = aws_lc_rs::rand::SystemRandom::new();
    let doc = aws_lc_rs::signature::EcdsaKeyPair::generate_pkcs8(
        &aws_lc_rs::signature::ECDSA_P256_SHA256_FIXED_SIGNING,
        &rng,
    )
    .unwrap();
    let enc = EncodingKey::from_ec_der(doc.as_ref());
    let jwk = make_jwk(&enc, Algorithm::ES256, kid);
    TestKey { enc, jwk }
}

fn ed_key(kid: &str) -> TestKey {
    // PKCS#8 v1: jsonwebtoken's JWK helper only recognises the 48-byte form.
    let doc = aws_lc_rs::signature::Ed25519KeyPair::generate()
        .unwrap()
        .to_pkcs8v1()
        .unwrap();
    let enc = EncodingKey::from_ed_der(doc.as_ref());
    let jwk = make_jwk(&enc, Algorithm::EdDSA, kid);
    TestKey { enc, jwk }
}

struct Keys {
    rsa_a: TestKey,
    rsa_a_public_pem: String,
    rsa_b: TestKey,
    /// Same kid as rsa_a but a different key: an attacker's key.
    rsa_evil: TestKey,
    ec: TestKey,
    ed: TestKey,
}

fn keys() -> &'static Keys {
    static KEYS: OnceLock<Keys> = OnceLock::new();
    KEYS.get_or_init(|| {
        let (rsa_a, rsa_a_public_pem) = rsa_key("rsa-a");
        let (rsa_b, _) = rsa_key("rsa-b");
        let (rsa_evil, _) = rsa_key("rsa-a");
        Keys {
            rsa_a,
            rsa_a_public_pem,
            rsa_b,
            rsa_evil,
            ec: ec_key("ec-1"),
            ed: ed_key("ed-1"),
        }
    })
}

fn now() -> i64 {
    jsonwebtoken::get_current_timestamp() as i64
}

const AUD: &str = "metro-api";

fn sign(
    key: &TestKey,
    alg: Algorithm,
    kid: Option<&str>,
    typ: Option<&str>,
    claims: &Value,
) -> String {
    let mut h = Header::new(alg);
    h.kid = kid.map(str::to_owned);
    h.typ = typ.map(str::to_owned);
    jsonwebtoken::encode(&h, claims, &key.enc).unwrap()
}

/// A provider serving discovery + JWKS. `issuer` is the server URI (no
/// trailing slash) unless `discovery_issuer` overrides what it advertises.
struct Provider {
    server: MockServer,
}

impl Provider {
    async fn start(jwks: Value) -> Self {
        let server = MockServer::start().await;
        let p = Self { server };
        let iss = p.issuer();
        p.mount(&iss, jwks).await;
        p
    }

    fn issuer(&self) -> String {
        self.server.uri()
    }

    async fn mount(&self, advertised_issuer: &str, jwks: Value) {
        Mock::given(method("GET"))
            .and(path("/.well-known/openid-configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "issuer": advertised_issuer,
                "jwks_uri": format!("{}/jwks", self.server.uri()),
                "id_token_signing_alg_values_supported": ["RS256"],
            })))
            .mount(&self.server)
            .await;
        self.set_jwks(jwks).await;
    }

    async fn set_jwks(&self, jwks: Value) {
        Mock::given(method("GET"))
            .and(path("/jwks"))
            .respond_with(ResponseTemplate::new(200).set_body_json(jwks))
            .with_priority(1)
            .mount(&self.server)
            .await;
    }

    async fn jwks_hits(&self) -> usize {
        self.server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.url.path() == "/jwks")
            .count()
    }

    async fn auth(&self, extra: &[(&str, &str)]) -> Authenticator {
        let iss = self.issuer();
        let mut vars = vec![(env::OIDC_ISSUER, iss.as_str()), (env::OIDC_AUDIENCE, AUD)];
        vars.extend_from_slice(extra);
        build(&vars).await.expect("authenticator")
    }

    fn claims(&self) -> Value {
        json!({
            "iss": self.issuer(),
            "aud": AUD,
            "sub": "user-123",
            "exp": now() + 300,
            "iat": now(),
        })
    }
}

fn default_jwks() -> Value {
    let k = keys();
    json!({ "keys": [k.rsa_a.jwk, k.ec.jwk, k.ed.jwk] })
}

fn set_limits(auth: &mut Authenticator, limits: OidcLimits) {
    match &mut auth.inner {
        Inner::Oidc(v) => v.set_limits(limits),
        Inner::Dev(_) => panic!("not oidc"),
    }
}

fn reason(r: Result<String, AuthError>) -> &'static str {
    match r {
        Err(AuthError::Invalid(why)) => why,
        other => panic!("expected Invalid, got {other:?}"),
    }
}

// ---------------------------------------------------------------- config

const DEV: &str = r#"{"alice":"alice-long-dev-token","bob":"bob-long-dev-token-x"}"#;

#[tokio::test]
async fn nothing_configured_fails_closed() {
    assert!(matches!(build(&[]).await, Err(ConfigError::NotConfigured)));
    // The runner token alone does not enable anything.
    let r = build(&[(env::RUNNER_TOKEN, "runner-token-0123456789")]).await;
    assert!(matches!(r, Err(ConfigError::NotConfigured)));
}

#[tokio::test]
async fn dev_and_oidc_are_mutually_exclusive() {
    for var in env::ALL_OIDC {
        let r = build(&[(env::DEV_USERS, DEV), (var, "x")]).await;
        assert!(matches!(r, Err(ConfigError::ConflictingModes(_))), "{var}");
    }
}

#[tokio::test]
async fn oidc_requires_issuer_and_audience() {
    let r = build(&[(env::OIDC_ISSUER, "https://id.example")]).await;
    assert!(matches!(r, Err(ConfigError::Missing { var, .. }) if var == env::OIDC_AUDIENCE));
    let r = build(&[(env::OIDC_AUDIENCE, AUD)]).await;
    assert!(matches!(r, Err(ConfigError::Missing { var, .. }) if var == env::OIDC_ISSUER));
    let r = build(&[
        (env::OIDC_ISSUER, "https://id.example"),
        (env::OIDC_AUDIENCE, " "),
    ])
    .await;
    assert!(matches!(r, Err(ConfigError::Missing { var, .. }) if var == env::OIDC_AUDIENCE));
}

#[tokio::test]
async fn oidc_issuer_must_be_https_without_extras() {
    for bad in [
        "http://id.example",
        "id.example",
        "https://user:pw@id.example",
        "https://id.example/?x=1",
        "https://id.example/#f",
        " https://id.example",
    ] {
        let r = build_https_only(&[(env::OIDC_ISSUER, bad), (env::OIDC_AUDIENCE, AUD)]).await;
        assert!(
            matches!(r, Err(ConfigError::Invalid { var, .. }) if var == env::OIDC_ISSUER),
            "{bad}: {r:?}"
        );
    }
}

#[tokio::test]
async fn oidc_optional_settings_are_validated() {
    let base = [
        (env::OIDC_ISSUER, "https://id.example"),
        (env::OIDC_AUDIENCE, AUD),
    ];
    for (var, val) in [
        (env::OIDC_LEEWAY_SECS, "301"),
        (env::OIDC_LEEWAY_SECS, "-1"),
        (env::OIDC_ACTOR_CLAIM, "a b"),
    ] {
        let mut vars = base.to_vec();
        vars.push((var, val));
        let r = build_https_only(&vars).await;
        assert!(
            matches!(r, Err(ConfigError::Invalid { var: v, .. }) if v == var),
            "{var}={val:?}: {r:?}"
        );
    }
}

#[tokio::test]
async fn blank_values_count_as_unset() {
    // compose.yaml passes `METRO_DEV_USERS:` etc. through as empty strings.
    let all_blank: Vec<(&str, &str)> = std::iter::once(env::DEV_USERS)
        .chain(env::ALL_OIDC.iter().copied())
        .map(|v| (v, ""))
        .collect();
    assert!(matches!(
        build(&all_blank).await,
        Err(ConfigError::NotConfigured)
    ));

    let mut dev_with_blank_oidc: Vec<(&str, &str)> =
        env::ALL_OIDC.iter().map(|v| (*v, "  ")).collect();
    dev_with_blank_oidc.push((env::DEV_USERS, DEV));
    let auth = build(&dev_with_blank_oidc).await.unwrap();
    assert_eq!(auth.mode(), AuthMode::Dev);
}

#[tokio::test]
async fn non_unicode_variable_is_an_error() {
    let r = Authenticator::from_lookup(|k| {
        if k == env::DEV_USERS {
            Err(())
        } else {
            Ok(None)
        }
    })
    .await;
    assert!(matches!(r, Err(ConfigError::NotUnicode { .. })));
}

// ---------------------------------------------------------------- dev mode

#[tokio::test]
async fn dev_tokens_map_to_their_user() {
    let auth = build(&[
        (env::DEV_USERS, DEV),
        (env::RUNNER_TOKEN, "runner-token-0123456789"),
    ])
    .await
    .unwrap();
    assert_eq!(auth.mode(), AuthMode::Dev);
    assert_eq!(
        auth.authenticate("alice-long-dev-token").await.unwrap(),
        "alice"
    );
    assert_eq!(
        auth.authenticate("Bearer bob-long-dev-token-x")
            .await
            .unwrap(),
        "bob"
    );
    assert_eq!(
        auth.authenticate("bearer  alice-long-dev-token ")
            .await
            .unwrap(),
        "alice"
    );
}

#[tokio::test]
async fn dev_rejects_unknown_empty_and_runner_tokens() {
    let auth = build(&[
        (env::DEV_USERS, DEV),
        (env::RUNNER_TOKEN, "runner-token-0123456789"),
    ])
    .await
    .unwrap();
    assert!(matches!(
        auth.authenticate("").await,
        Err(AuthError::Missing)
    ));
    assert!(matches!(
        auth.authenticate("Bearer ").await,
        Err(AuthError::Missing)
    ));
    assert_eq!(
        reason(auth.authenticate("alice-long-dev-tokeN").await),
        "unknown dev token"
    );
    assert_eq!(
        reason(auth.authenticate("alice").await),
        "unknown dev token"
    );
    // The runner token is never a user credential.
    assert_eq!(
        reason(auth.authenticate("runner-token-0123456789").await),
        "unknown dev token"
    );
    let long = "a".repeat(10_000);
    assert!(auth.authenticate(&long).await.is_err());
}

#[tokio::test]
async fn dev_rejects_bad_mappings() {
    let cases = [
        ("{}", "no users"),
        ("[]", "not an object"),
        (r#"{"alice": 5}"#, "non-string token"),
        (r#"{"alice":"short"}"#, "short token"),
        (r#"{"alice":"has a space in token"}"#, "whitespace"),
        (r#"{"al ice":"alice-long-dev-token"}"#, "bad name"),
        (r#"{"":"alice-long-dev-token"}"#, "empty name"),
        (
            r#"{"alice":"alice-long-dev-token","alice":"alice-other-dev-token"}"#,
            "duplicate user",
        ),
        (
            r#"{"alice":"same-long-dev-token","bob":"same-long-dev-token"}"#,
            "shared token",
        ),
        (r#"{"alice":"alice-long-dev-token"} x"#, "trailing data"),
    ];
    for (raw, what) in cases {
        let r = build(&[(env::DEV_USERS, raw)]).await;
        assert!(
            matches!(r, Err(ConfigError::Invalid { var, .. }) if var == env::DEV_USERS),
            "{what}: {r:?}"
        );
    }
}

#[tokio::test]
async fn dev_rejects_runner_token_collision() {
    let r = build(&[
        (env::DEV_USERS, r#"{"alice":"runner-token-0123456789"}"#),
        (env::RUNNER_TOKEN, "runner-token-0123456789"),
    ])
    .await;
    let err = r.unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains(env::RUNNER_TOKEN), "{msg}");
    // Error messages never echo token values.
    assert!(!msg.contains("runner-token-0123456789"), "{msg}");
}

#[tokio::test]
async fn debug_output_does_not_leak_tokens() {
    let auth = build(&[(env::DEV_USERS, DEV)]).await.unwrap();
    let dbg = format!("{auth:?}");
    assert!(!dbg.contains("alice-long-dev-token"), "{dbg}");
}

// ---------------------------------------------------------------- oidc: happy paths

#[tokio::test]
async fn oidc_accepts_valid_rs256_es256_eddsa() {
    let p = Provider::start(default_jwks()).await;
    let auth = p.auth(&[]).await;
    assert_eq!(auth.mode(), AuthMode::Oidc);
    let k = keys();
    let c = p.claims();
    let t = sign(&k.rsa_a, Algorithm::RS256, Some("rsa-a"), Some("JWT"), &c);
    assert_eq!(
        auth.authenticate(&format!("Bearer {t}")).await.unwrap(),
        "user-123"
    );
    let t = sign(&k.ec, Algorithm::ES256, Some("ec-1"), None, &c);
    assert_eq!(auth.authenticate(&t).await.unwrap(), "user-123");
    let t = sign(&k.ed, Algorithm::EdDSA, Some("ed-1"), None, &c);
    assert_eq!(auth.authenticate(&t).await.unwrap(), "user-123");
    // No kid: falls back to keys whose type permits the algorithm.
    let t = sign(&k.ec, Algorithm::ES256, None, None, &c);
    assert_eq!(auth.authenticate(&t).await.unwrap(), "user-123");
}

#[tokio::test]
async fn oidc_accepts_audience_array() {
    let p = Provider::start(default_jwks()).await;
    let auth = p.auth(&[]).await;
    let mut c = p.claims();
    c["aud"] = json!(["other-api", AUD]);
    let t = sign(&keys().rsa_a, Algorithm::RS256, Some("rsa-a"), None, &c);
    assert_eq!(auth.authenticate(&t).await.unwrap(), "user-123");
}

#[tokio::test]
async fn oidc_custom_actor_claim() {
    let p = Provider::start(default_jwks()).await;
    let auth = p
        .auth(&[(env::OIDC_ACTOR_CLAIM, "preferred_username")])
        .await;
    let mut c = p.claims();
    c["preferred_username"] = json!("alice");
    let t = sign(&keys().rsa_a, Algorithm::RS256, Some("rsa-a"), None, &c);
    assert_eq!(auth.authenticate(&t).await.unwrap(), "alice");

    let c = p.claims(); // no preferred_username
    let t = sign(&keys().rsa_a, Algorithm::RS256, Some("rsa-a"), None, &c);
    assert_eq!(reason(auth.authenticate(&t).await), "actor claim missing");

    let mut c = p.claims();
    c["preferred_username"] = json!("bad\nname");
    let t = sign(&keys().rsa_a, Algorithm::RS256, Some("rsa-a"), None, &c);
    assert_eq!(
        reason(auth.authenticate(&t).await),
        "actor claim has an invalid value"
    );
}

#[tokio::test]
async fn oidc_required_typ() {
    let p = Provider::start(default_jwks()).await;
    let auth = p.auth(&[(env::OIDC_REQUIRED_TYP, "at+jwt")]).await;
    let c = p.claims();
    let k = &keys().rsa_a;
    for ok in ["at+jwt", "AT+JWT", "application/at+jwt"] {
        let t = sign(k, Algorithm::RS256, Some("rsa-a"), Some(ok), &c);
        assert_eq!(auth.authenticate(&t).await.unwrap(), "user-123", "{ok}");
    }
    for bad in [Some("JWT"), None] {
        let t = sign(k, Algorithm::RS256, Some("rsa-a"), bad, &c);
        assert_eq!(
            reason(auth.authenticate(&t).await),
            "unexpected token type",
            "{bad:?}"
        );
    }
}

// ---------------------------------------------------------------- oidc: wrong claims

#[tokio::test]
async fn oidc_rejects_wrong_claims() {
    let p = Provider::start(default_jwks()).await;
    let auth = p.auth(&[]).await;
    let k = &keys().rsa_a;
    let base = p.claims();

    let mut cases: Vec<(&str, Value, &str)> = Vec::new();
    let mut c = base.clone();
    c["aud"] = json!("someone-else");
    cases.push(("wrong aud", c, "wrong audience"));
    let mut c = base.clone();
    c["aud"] = json!(["a", "b"]);
    cases.push(("aud array without us", c, "wrong audience"));
    let mut c = base.clone();
    c.as_object_mut().unwrap().remove("aud");
    cases.push(("missing aud", c, "missing required claim"));
    let mut c = base.clone();
    c["iss"] = json!("https://evil.example");
    cases.push(("wrong iss", c, "wrong issuer"));
    let mut c = base.clone();
    c["iss"] = json!(format!("{}/", p.issuer()));
    cases.push(("iss trailing slash", c, "wrong issuer"));
    let mut c = base.clone();
    c["iss"] = json!([p.issuer()]);
    cases.push(("iss array", c, "wrong issuer"));
    let mut c = base.clone();
    c.as_object_mut().unwrap().remove("iss");
    cases.push(("missing iss", c, "missing required claim"));
    let mut c = base.clone();
    c["exp"] = json!(now() - 3600);
    cases.push(("expired", c, "expired"));
    let mut c = base.clone();
    c["exp"] = json!(now() - 120); // beyond the default 60s leeway
    cases.push(("expired past leeway", c, "expired"));
    let mut c = base.clone();
    c.as_object_mut().unwrap().remove("exp");
    cases.push(("missing exp", c, "missing required claim"));
    let mut c = base.clone();
    c["exp"] = json!("tomorrow");
    cases.push(("string exp", c, "missing required claim"));
    let mut c = base.clone();
    c["nbf"] = json!(now() + 3600);
    cases.push(("nbf in future", c, "not yet valid"));
    let mut c = base.clone();
    c.as_object_mut().unwrap().remove("sub");
    cases.push(("missing sub", c, "missing required claim"));
    let mut c = base.clone();
    c["sub"] = json!("");
    cases.push(("empty sub", c, "sub has an invalid value"));

    for (what, claims, want) in cases {
        let t = sign(k, Algorithm::RS256, Some("rsa-a"), None, &claims);
        let got = auth.authenticate(&t).await;
        match got {
            Err(AuthError::Invalid(why)) => assert_eq!(why, want, "{what}"),
            other => panic!("{what}: expected Invalid({want}), got {other:?}"),
        }
    }
}

#[tokio::test]
async fn oidc_leeway_tolerates_small_skew() {
    let p = Provider::start(default_jwks()).await;
    let auth = p.auth(&[]).await;
    let mut c = p.claims();
    c["exp"] = json!(now() - 10);
    c["nbf"] = json!(now() + 10);
    let t = sign(&keys().rsa_a, Algorithm::RS256, Some("rsa-a"), None, &c);
    assert_eq!(auth.authenticate(&t).await.unwrap(), "user-123");

    let strict = p.auth(&[(env::OIDC_LEEWAY_SECS, "0")]).await;
    assert_eq!(reason(strict.authenticate(&t).await), "expired");
}

// ---------------------------------------------------------------- oidc: signatures & algorithms

#[tokio::test]
async fn oidc_rejects_untrusted_signer_and_tampering() {
    let p = Provider::start(default_jwks()).await;
    let auth = p.auth(&[]).await;
    let k = keys();
    let c = p.claims();

    // Attacker key re-using a trusted kid.
    let t = sign(&k.rsa_evil, Algorithm::RS256, Some("rsa-a"), None, &c);
    assert_eq!(reason(auth.authenticate(&t).await), "bad signature");

    // Valid token with the payload swapped.
    let t = sign(&k.rsa_a, Algorithm::RS256, Some("rsa-a"), None, &c);
    let mut parts: Vec<String> = t.split('.').map(str::to_owned).collect();
    let mut forged = c.clone();
    forged["sub"] = json!("admin");
    parts[1] = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&forged).unwrap());
    assert_eq!(
        reason(auth.authenticate(&parts.join(".")).await),
        "bad signature"
    );

    // Signature stripped.
    let stripped = format!("{}.{}.", parts[0], parts[1]);
    assert!(auth.authenticate(&stripped).await.is_err());

    // Garbage.
    assert_eq!(
        reason(auth.authenticate("not-a-jwt").await),
        "malformed token"
    );
}

#[tokio::test]
async fn oidc_rejects_alg_confusion() {
    let p = Provider::start(default_jwks()).await;
    let auth = p.auth(&[]).await;
    let k = keys();
    let c = p.claims();

    // Classic RS256 -> HS256 confusion: HMAC keyed with the RSA public key.
    let hmac = EncodingKey::from_secret(k.rsa_a_public_pem.as_bytes()).unwrap();
    let mut h = Header::new(Algorithm::HS256);
    h.kid = Some("rsa-a".into());
    let t = jsonwebtoken::encode(&h, &c, &hmac).unwrap();
    assert_eq!(reason(auth.authenticate(&t).await), "algorithm not allowed");

    // alg=none.
    let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"none","typ":"JWT","kid":"rsa-a"}"#);
    let body = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&c).unwrap());
    let t = format!("{header}.{body}.");
    assert!(matches!(
        auth.authenticate(&t).await,
        Err(AuthError::Invalid(_))
    ));

    // Header says ES256 but kid points at an RSA key: the key type decides.
    let t = sign(&k.ec, Algorithm::ES256, Some("rsa-a"), None, &c);
    assert_eq!(
        reason(auth.authenticate(&t).await),
        "no matching signing key"
    );

    // JWK pins RS256; a PS256 token from the same key is refused.
    let t = sign(&k.rsa_a, Algorithm::PS256, Some("rsa-a"), None, &c);
    assert_eq!(
        reason(auth.authenticate(&t).await),
        "no matching signing key"
    );
}

#[tokio::test]
async fn oidc_ignores_symmetric_and_encryption_keys_in_jwks() {
    let k = keys();
    let mut enc_only = k.rsa_b.jwk.clone();
    enc_only["use"] = json!("enc");
    enc_only["kid"] = json!("enc-key");
    let jwks = json!({ "keys": [
        { "kty": "oct", "k": URL_SAFE_NO_PAD.encode(b"0123456789abcdef0123456789abcdef"), "kid": "hmac", "alg": "HS256" },
        enc_only,
        { "kty": "unknown-type", "kid": "x" },
        k.rsa_a.jwk,
    ]});
    let p = Provider::start(jwks).await;
    let auth = p.auth(&[]).await;
    let c = p.claims();

    let hmac = EncodingKey::from_secret(b"0123456789abcdef0123456789abcdef").unwrap();
    let mut h = Header::new(Algorithm::HS256);
    h.kid = Some("hmac".into());
    let t = jsonwebtoken::encode(&h, &c, &hmac).unwrap();
    assert!(auth.authenticate(&t).await.is_err());

    let t = sign(&k.rsa_b, Algorithm::RS256, Some("enc-key"), None, &c);
    assert_eq!(
        reason(auth.authenticate(&t).await),
        "no matching signing key"
    );

    let t = sign(&k.rsa_a, Algorithm::RS256, Some("rsa-a"), None, &c);
    assert_eq!(auth.authenticate(&t).await.unwrap(), "user-123");
}

#[tokio::test]
async fn oidc_rejects_oversized_token() {
    let p = Provider::start(default_jwks()).await;
    let auth = p.auth(&[]).await;
    let mut c = p.claims();
    c["pad"] = json!("x".repeat(20_000));
    let t = sign(&keys().rsa_a, Algorithm::RS256, Some("rsa-a"), None, &c);
    assert_eq!(reason(auth.authenticate(&t).await), "token too long");
}

// ---------------------------------------------------------------- oidc: discovery & JWKS lifecycle

#[tokio::test]
async fn discovery_issuer_must_match_exactly() {
    let server = MockServer::start().await;
    let p = Provider { server };
    let advertised = format!("{}/", p.issuer()); // trailing slash differs
    p.mount(&advertised, default_jwks()).await;
    let iss = p.issuer();
    let r = build(&[(env::OIDC_ISSUER, iss.as_str()), (env::OIDC_AUDIENCE, AUD)]).await;
    assert!(
        matches!(r, Err(ConfigError::Discovery(ref m)) if m.contains("does not exactly match")),
        "{r:?}"
    );
}

#[tokio::test]
async fn discovery_failure_is_a_config_error() {
    let server = MockServer::start().await; // serves 404 for everything
    let iss = server.uri();
    let r = build(&[(env::OIDC_ISSUER, iss.as_str()), (env::OIDC_AUDIENCE, AUD)]).await;
    assert!(matches!(r, Err(ConfigError::Discovery(_))), "{r:?}");
}

#[tokio::test]
async fn jwks_without_usable_keys_is_a_config_error() {
    let p = Provider::start(json!({ "keys": [{ "kty": "oct", "k": "AAAA" }] })).await;
    let iss = p.issuer();
    let r = build(&[(env::OIDC_ISSUER, iss.as_str()), (env::OIDC_AUDIENCE, AUD)]).await;
    assert!(
        matches!(r, Err(ConfigError::Discovery(ref m)) if m.contains("no usable")),
        "{r:?}"
    );
}

#[tokio::test]
async fn oversized_jwks_is_refused() {
    let mut big = default_jwks();
    big["padding"] = json!("x".repeat(crate::http::MAX_BODY_BYTES + 1));
    let p = Provider::start(big).await;
    let iss = p.issuer();
    let r = build(&[(env::OIDC_ISSUER, iss.as_str()), (env::OIDC_AUDIENCE, AUD)]).await;
    assert!(
        matches!(r, Err(ConfigError::Discovery(ref m)) if m.contains("larger than")),
        "{r:?}"
    );
}

#[test]
fn jwks_url_must_be_https_in_production() {
    let u = url::Url::parse("http://id.example/jwks").unwrap();
    assert!(crate::oidc::check_url(&u, Transport::HttpsOnly).is_err());
    let u = url::Url::parse("https://id.example/jwks").unwrap();
    assert!(crate::oidc::check_url(&u, Transport::HttpsOnly).is_ok());
}

#[test]
fn jwks_key_count_is_bounded() {
    let k = keys();
    let many: Vec<Value> = (0..100)
        .map(|i| {
            let mut j = k.ec.jwk.clone();
            j["kid"] = json!(format!("k{i}"));
            j
        })
        .collect();
    let body = serde_json::to_vec(&json!({ "keys": many })).unwrap();
    assert_eq!(
        crate::oidc::parse_jwks_for_test(&body).unwrap(),
        crate::oidc::MAX_JWKS_KEYS
    );
}

#[tokio::test]
async fn key_rotation_is_picked_up_on_unknown_kid() {
    let k = keys();
    let p = Provider::start(json!({ "keys": [k.rsa_a.jwk] })).await;
    let mut auth = p.auth(&[]).await;
    set_limits(
        &mut auth,
        OidcLimits {
            refresh_interval: Duration::from_secs(3600),
            min_refetch_interval: Duration::ZERO,
            max_stale: Duration::from_secs(3600),
        },
    );
    let c = p.claims();
    let t_b = sign(&k.rsa_b, Algorithm::RS256, Some("rsa-b"), None, &c);
    assert_eq!(
        reason(auth.authenticate(&t_b).await),
        "no matching signing key"
    );

    // Provider rotates to key B (A retired).
    p.server.reset().await;
    let iss = p.issuer();
    p.mount(&iss, json!({ "keys": [k.rsa_b.jwk] })).await;

    assert_eq!(auth.authenticate(&t_b).await.unwrap(), "user-123");
    let t_a = sign(&k.rsa_a, Algorithm::RS256, Some("rsa-a"), None, &c);
    assert!(
        auth.authenticate(&t_a).await.is_err(),
        "retired key must stop working"
    );
}

#[tokio::test]
async fn unknown_kid_refetch_is_rate_limited() {
    let p = Provider::start(default_jwks()).await;
    let auth = p.auth(&[]).await; // default limits: 30s between fetches
    assert_eq!(p.jwks_hits().await, 1);
    let c = p.claims();
    for i in 0..20 {
        let t = sign(
            &keys().rsa_b,
            Algorithm::RS256,
            Some(&format!("nope-{i}")),
            None,
            &c,
        );
        assert!(auth.authenticate(&t).await.is_err());
    }
    // Startup fetch only: the forced refetch is blocked because the startup
    // fetch happened less than min_refetch_interval ago.
    assert_eq!(p.jwks_hits().await, 1);
}

#[tokio::test]
async fn stale_keys_survive_short_outage_then_fail_closed() {
    let k = keys();
    let p = Provider::start(default_jwks()).await;
    let mut auth = p.auth(&[]).await;
    let c = p.claims();
    let t = sign(&k.rsa_a, Algorithm::RS256, Some("rsa-a"), None, &c);

    // Provider goes down.
    p.server.reset().await;

    set_limits(
        &mut auth,
        OidcLimits {
            refresh_interval: Duration::ZERO,
            min_refetch_interval: Duration::ZERO,
            max_stale: Duration::from_secs(3600),
        },
    );
    assert_eq!(auth.authenticate(&t).await.unwrap(), "user-123");

    set_limits(
        &mut auth,
        OidcLimits {
            refresh_interval: Duration::ZERO,
            min_refetch_interval: Duration::ZERO,
            max_stale: Duration::ZERO,
        },
    );
    let r = auth.authenticate(&t).await;
    assert!(matches!(&r, Err(e) if e.is_unavailable()), "{r:?}");
}

// ---------------------------------------------------------------- identity

#[tokio::test]
async fn dev_identity_is_username_for_both_fields() {
    let auth = build(&[(env::DEV_USERS, DEV)]).await.unwrap();
    let who = auth.identity("alice-long-dev-token").await.unwrap();
    assert_eq!(
        who,
        Identity {
            id: "alice".into(),
            actor: "alice".into()
        }
    );
    assert_eq!(auth.namespace(), "dev");
    assert_eq!(auth.issuer(), None);
    // No METRO_RUNNER_TOKEN in the environment is fine (legacy, optional).
}

#[tokio::test]
async fn oidc_identity_defaults_actor_to_sub() {
    let p = Provider::start(default_jwks()).await;
    let auth = p.auth(&[]).await;
    let t = sign(
        &keys().rsa_a,
        Algorithm::RS256,
        Some("rsa-a"),
        None,
        &p.claims(),
    );
    let who = auth.identity(&t).await.unwrap();
    assert_eq!(who.id, "user-123");
    assert_eq!(who.actor, "user-123");
    assert_eq!(auth.authenticate(&t).await.unwrap(), who.actor);
    assert_eq!(auth.namespace(), format!("oidc:{}", p.issuer()));
    assert_eq!(auth.issuer(), Some(p.issuer().as_str()));
}

#[tokio::test]
async fn two_subjects_sharing_a_display_name_stay_distinct() {
    let p = Provider::start(default_jwks()).await;
    let auth = p
        .auth(&[(env::OIDC_ACTOR_CLAIM, "preferred_username")])
        .await;
    let k = &keys().rsa_a;
    let mut a = p.claims();
    a["sub"] = json!("248289761001");
    a["preferred_username"] = json!("alice");
    let mut b = p.claims();
    b["sub"] = json!("auth0|5f7c8ec7c33c6c004bbafe82");
    b["preferred_username"] = json!("alice");

    let ia = auth
        .identity(&sign(k, Algorithm::RS256, Some("rsa-a"), None, &a))
        .await
        .unwrap();
    let ib = auth
        .identity(&sign(k, Algorithm::RS256, Some("rsa-a"), None, &b))
        .await
        .unwrap();
    assert_eq!(ia.actor, "alice");
    assert_eq!(ib.actor, "alice");
    assert_eq!(ia.id, "248289761001");
    assert_eq!(ib.id, "auth0|5f7c8ec7c33c6c004bbafe82");
    assert_ne!(ia.id, ib.id);
    // The compatibility projection returns the display value only.
    let t = sign(k, Algorithm::RS256, Some("rsa-a"), None, &b);
    assert_eq!(auth.authenticate(&t).await.unwrap(), "alice");
}

#[tokio::test]
async fn sub_is_required_even_when_display_claim_is_present() {
    let p = Provider::start(default_jwks()).await;
    let auth = p
        .auth(&[(env::OIDC_ACTOR_CLAIM, "preferred_username")])
        .await;
    let k = &keys().rsa_a;

    let mut c = p.claims();
    c.as_object_mut().unwrap().remove("sub");
    c["preferred_username"] = json!("alice");
    let t = sign(k, Algorithm::RS256, Some("rsa-a"), None, &c);
    assert_eq!(
        reason(auth.identity(&t).await.map(|i| i.id)),
        "missing required claim"
    );
    assert_eq!(
        reason(auth.authenticate(&t).await),
        "missing required claim"
    );

    c["sub"] = json!("");
    let t = sign(k, Algorithm::RS256, Some("rsa-a"), None, &c);
    assert_eq!(
        reason(auth.authenticate(&t).await),
        "sub has an invalid value"
    );

    c["sub"] = json!(12345);
    let t = sign(k, Algorithm::RS256, Some("rsa-a"), None, &c);
    assert!(matches!(
        auth.identity(&t).await,
        Err(AuthError::Invalid(_))
    ));
}

#[tokio::test]
async fn subject_string_bounds() {
    let p = Provider::start(default_jwks()).await;
    let auth = p.auth(&[]).await;
    let k = &keys().rsa_a;
    let max = "s".repeat(crate::MAX_SUBJECT_LEN);
    let accepted = [
        "auth0|5f7c8ec7c33c6c004bbafe82",
        "CgVhbGljZRIEbGRhcA",
        "8b8f6a52-7f8a-4b5e-9f53-3f1c1c2d9e10",
        "user@example.com",
        "https://idp.example/users/42#main",
        "github:octo cat",
        "ユーザー42",
        max.as_str(),
    ];
    for sub in accepted {
        let mut c = p.claims();
        c["sub"] = json!(sub);
        let t = sign(k, Algorithm::RS256, Some("rsa-a"), None, &c);
        assert_eq!(auth.identity(&t).await.unwrap().id, sub, "{sub:?}");
    }
    let too_long = "s".repeat(crate::MAX_SUBJECT_LEN + 1);
    let rejected = [
        too_long.as_str(),
        "tab\there",
        "new\nline",
        "nul\u{0}",
        " leading",
        "trailing ",
        "bidi\u{202E}gnp.exe",
        "zero\u{200B}width",
    ];
    for sub in rejected {
        let mut c = p.claims();
        c["sub"] = json!(sub);
        let t = sign(k, Algorithm::RS256, Some("rsa-a"), None, &c);
        assert_eq!(
            reason(auth.authenticate(&t).await),
            "sub has an invalid value",
            "{sub:?}"
        );
    }
}

#[tokio::test]
async fn display_claim_bounds() {
    let p = Provider::start(default_jwks()).await;
    let auth = p.auth(&[(env::OIDC_ACTOR_CLAIM, "name")]).await;
    let k = &keys().rsa_a;
    let mut c = p.claims();
    c["name"] = json!("x".repeat(crate::MAX_DISPLAY_LEN));
    let t = sign(k, Algorithm::RS256, Some("rsa-a"), None, &c);
    assert!(auth.identity(&t).await.is_ok());
    for bad in [
        json!("x".repeat(crate::MAX_DISPLAY_LEN + 1)),
        json!("a\u{202E}b"),
        json!(["alice"]),
        json!(""),
    ] {
        c["name"] = bad.clone();
        let t = sign(k, Algorithm::RS256, Some("rsa-a"), None, &c);
        assert_eq!(
            reason(auth.authenticate(&t).await),
            "actor claim has an invalid value",
            "{bad}"
        );
    }
}
