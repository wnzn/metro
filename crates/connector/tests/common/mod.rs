//! A deterministic loopback stand-in for OpenAI's issuer (discovery, JWKS,
//! token, revocation) and API (`/v1/models`, `/v1/responses` SSE) on
//! 127.0.0.1, and a scripted "browser" that follows Metro's start link and
//! calls the loopback callback. Shared by the Sign in with ChatGPT tests and
//! the desktop bridge tests. Nothing contacts OpenAI; the signing key is
//! generated per run.

#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aws_lc_rs::encoding::AsDer;
use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use jsonwebtoken::jwk::Jwk;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use metro_connector::openai_oauth::{Endpoints, LoginOptions};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;

pub const RESOURCE: &str = "https://api.openai.com/v1";
pub const ISSUED: &str = "oaiapp_fixture_client";
pub const PLAN_SCOPES: &str =
    "chatgpt.tokens.use.direct email offline_access openid profile resource.invoke";

// ------------------------------------------------------------------ fixture

#[derive(Clone)]
pub enum Browser {
    /// Approve, and sign the ID token for `sub` with the given scope.
    Approve {
        sub: &'static str,
        scope: &'static str,
    },
    /// Send a forged-state callback first, then approve.
    ForgedStateFirst,
    Deny,
    /// Approve, but sign the ID token with a different nonce.
    BadNonce,
    /// New registration whose callback omits client_id.
    NoClientId,
}

pub struct Pending {
    pub client_id: String,
    pub challenge: String,
    pub redirect_uri: String,
    pub nonce: String,
    pub sub: String,
    pub scope: String,
    pub bad_nonce: bool,
}

pub struct Fx {
    pub issuer: String,
    pub enc: EncodingKey,
    pub jwk: Value,
    pub browser: Browser,
    pub codes: HashMap<String, Pending>,
    /// Authorize query parameters seen by the browser, per sign-in.
    pub authorize: Vec<HashMap<String, String>>,
    pub first_expires_in: u64,
    pub access: Option<String>,
    pub refresh: Option<String>,
    pub refresh_count: u32,
    pub refresh_error: Option<&'static str>,
    pub refresh_scope: Option<&'static str>,
    pub revoked: Vec<HashMap<String, String>>,
    pub responses_bodies: Vec<Value>,
    pub sse_mode: &'static str,
    pub sub_for_refresh: String,
}

pub type S = Arc<Mutex<Fx>>;

pub fn rsa() -> (EncodingKey, Value) {
    let kp = aws_lc_rs::rsa::KeyPair::generate(aws_lc_rs::rsa::KeySize::Rsa2048).unwrap();
    let der = AsDer::<aws_lc_rs::encoding::Pkcs8V1Der>::as_der(&kp).unwrap();
    let b64 = STANDARD.encode(der.as_ref());
    let lines: Vec<&str> = b64
        .as_bytes()
        .chunks(64)
        .map(|c| std::str::from_utf8(c).unwrap())
        .collect();
    let pem = format!(
        "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
        lines.join("\n")
    );
    let enc = EncodingKey::from_rsa_pem(pem.as_bytes()).unwrap();
    let mut jwk =
        serde_json::to_value(Jwk::from_encoding_key(&enc, Algorithm::RS256).unwrap()).unwrap();
    jwk["kid"] = json!("k1");
    jwk["use"] = json!("sig");
    (enc, jwk)
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

pub fn form(body: &str) -> HashMap<String, String> {
    url::form_urlencoded::parse(body.as_bytes())
        .into_owned()
        .collect()
}

pub fn fake_access(issuer: &str, client_id: &str, n: u32) -> String {
    let h = URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#);
    let p = URL_SAFE_NO_PAD.encode(
        json!({"aud": RESOURCE, "client_id": client_id, "iss": issuer, "sub": "opaque", "n": n, "exp": now() + 3600})
            .to_string(),
    );
    format!("{h}.{p}.sig")
}

impl Fx {
    pub fn id_token(&self, client_id: &str, sub: &str, nonce: Option<&str>) -> String {
        let mut claims = json!({
            "iss": self.issuer, "aud": [client_id], "sub": sub, "email": "person@example.test",
            "email_verified": true, "iat": now(), "exp": now() + 3600,
        });
        if let Some(n) = nonce {
            claims["nonce"] = json!(n);
        }
        let mut h = Header::new(Algorithm::RS256);
        h.kid = Some("k1".into());
        jsonwebtoken::encode(&h, &claims, &self.enc).unwrap()
    }
}

pub async fn discovery(State(s): State<S>) -> Response {
    let i = s.lock().unwrap().issuer.clone();
    axum::Json(json!({
        "issuer": i,
        "authorization_endpoint": format!("{i}/api/accounts/authorize"),
        "token_endpoint": format!("{i}/api/accounts/oauth/token"),
        "jwks_uri": format!("{i}/.well-known/jwks.json"),
        "revocation_endpoint": format!("{i}/oauth/revoke"),
    }))
    .into_response()
}

pub async fn jwks(State(s): State<S>) -> Response {
    axum::Json(json!({ "keys": [s.lock().unwrap().jwk.clone()] })).into_response()
}

pub fn oauth_err(code: &str) -> Response {
    (StatusCode::BAD_REQUEST, axum::Json(json!({"error": code}))).into_response()
}

pub async fn token(State(s): State<S>, body: String) -> Response {
    let f = form(&body);
    let mut fx = s.lock().unwrap();
    match f.get("grant_type").map(String::as_str) {
        Some("authorization_code") => {
            let Some(p) = fx
                .codes
                .remove(f.get("code").map(String::as_str).unwrap_or(""))
            else {
                return oauth_err("invalid_grant");
            };
            let verifier = f.get("code_verifier").cloned().unwrap_or_default();
            let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
            if challenge != p.challenge
                || f.get("client_id") != Some(&p.client_id)
                || f.get("redirect_uri") != Some(&p.redirect_uri)
                || f.get("resource").map(String::as_str) != Some(RESOURCE)
                || f.contains_key("client_secret")
            {
                return oauth_err("invalid_grant");
            }
            let nonce = if p.bad_nonce {
                "not-the-nonce".to_owned()
            } else {
                p.nonce.clone()
            };
            let id = fx.id_token(&p.client_id, &p.sub, Some(&nonce));
            let access = fake_access(&fx.issuer, &p.client_id, 0);
            fx.access = Some(access.clone());
            fx.refresh = Some("rt-0".into());
            fx.sub_for_refresh = p.sub.clone();
            axum::Json(json!({
                "access_token": access, "refresh_token": "rt-0", "id_token": id, "token_type": "Bearer",
                "expires_in": fx.first_expires_in, "scope": p.scope,
            }))
            .into_response()
        }
        Some("refresh_token") => {
            if let Some(code) = fx.refresh_error {
                return oauth_err(code);
            }
            if f.get("client_id").map(String::as_str) != Some(ISSUED)
                || f.get("resource").map(String::as_str) != Some(RESOURCE)
                || f.contains_key("scope")
                || f.get("refresh_token") != fx.refresh.as_ref()
            {
                // A refresh with a superseded token behaves like reuse.
                return oauth_err("refresh_token_reused");
            }
            fx.refresh_count += 1;
            let n = fx.refresh_count;
            let access = fake_access(&fx.issuer, ISSUED, n);
            let rt = format!("rt-{n}");
            fx.access = Some(access.clone());
            fx.refresh = Some(rt.clone());
            // Common OAuth refresh shape: no new ID token and no scope
            // unless the grant changes. Still rotate the refresh token.
            let mut result = json!({
                "access_token": access, "refresh_token": rt, "token_type": "Bearer",
                "expires_in": 3600,
            });
            if let Some(scope) = fx.refresh_scope {
                result["scope"] = json!(scope);
            }
            axum::Json(result).into_response()
        }
        _ => oauth_err("unsupported_grant_type"),
    }
}

pub async fn revoke(State(s): State<S>, body: String) -> Response {
    s.lock().unwrap().revoked.push(form(&body));
    StatusCode::OK.into_response()
}

pub fn bearer_ok(fx: &Fx, h: &HeaderMap) -> bool {
    let want = fx.access.as_ref().map(|a| format!("Bearer {a}"));
    h.get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
        == want
}

pub async fn models(State(s): State<S>, h: HeaderMap) -> Response {
    let fx = s.lock().unwrap();
    if !bearer_ok(&fx, &h) {
        return (
            StatusCode::UNAUTHORIZED,
            axum::Json(json!({"detail": "bad token"})),
        )
            .into_response();
    }
    axum::Json(json!({"models": [
        {"slug": "gpt-fixture-a", "display_name": "Fixture A", "visibility": "list"},
        {"slug": "gpt-hidden", "display_name": "Hidden", "visibility": "hide"},
        {"slug": "gpt-fixture-b", "display_name": "Fixture B", "visibility": "list"},
    ]}))
    .into_response()
}

pub fn sse(events: &[Value]) -> String {
    events
        .iter()
        .map(|e| format!("event: {}\ndata: {}\n\n", e["type"].as_str().unwrap(), e))
        .collect()
}

fn streamed(parts: Vec<String>) -> Response {
    use std::pin::Pin;
    use std::task::{Context, Poll};

    struct Chunks {
        parts: std::vec::IntoIter<String>,
        pause: Pin<Box<tokio::time::Sleep>>,
    }
    impl hyper::body::Body for Chunks {
        type Data = axum::body::Bytes;
        type Error = std::convert::Infallible;

        fn poll_frame(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
        ) -> Poll<Option<Result<hyper::body::Frame<Self::Data>, Self::Error>>> {
            std::task::ready!(self.pause.as_mut().poll(cx));
            let next = self.parts.next();
            self.pause
                .as_mut()
                .reset(tokio::time::Instant::now() + Duration::from_millis(5));
            Poll::Ready(next.map(|s| Ok(hyper::body::Frame::data(s.into()))))
        }
    }
    (
        [(header::CONTENT_TYPE, "text/event-stream; charset=UTF-8")],
        axum::body::Body::new(Chunks {
            parts: parts.into_iter(),
            pause: Box::pin(tokio::time::sleep(Duration::ZERO)),
        }),
    )
        .into_response()
}

pub async fn responses(State(s): State<S>, h: HeaderMap, body: String) -> Response {
    let mut fx = s.lock().unwrap();
    if !bearer_ok(&fx, &h) {
        return (
            StatusCode::UNAUTHORIZED,
            axum::Json(json!({"detail": "bad token"})),
        )
            .into_response();
    }
    let body: Value = serde_json::from_str(&body).unwrap();
    fx.responses_bodies.push(body.clone());
    if body["store"] != json!(false) || body["stream"] != json!(true) {
        return (
            StatusCode::BAD_REQUEST,
            axum::Json(json!({"error": {"code": "bad", "message": "store/stream"}})),
        )
            .into_response();
    }
    let leak = |status: StatusCode, body: Value| {
        (
            status,
            [("x-request-id", "req_SENSITIVE_ID")],
            axum::Json(body),
        )
            .into_response()
    };
    match fx.sse_mode {
        "http400" => {
            return leak(
                StatusCode::BAD_REQUEST,
                json!({"error": {"code": "weird_code", "message": "SENSITIVE-ECHO of the user's prompt"}}),
            );
        }
        "http503" => {
            return leak(
                StatusCode::SERVICE_UNAVAILABLE,
                json!({"detail": "SENSITIVE-DETAIL"}),
            );
        }
        "http401" => {
            return leak(
                StatusCode::UNAUTHORIZED,
                json!({"detail": "fixture.person@example.test"}),
            );
        }
        "model_missing" => {
            return leak(
                StatusCode::NOT_FOUND,
                json!({"error": {"code": "model_not_found", "param": "model", "message": "SENSITIVE-MODEL"}}),
            );
        }
        // Drop the connection before any response: a transport failure.
        "drop" => {
            drop(fx);
            panic!("fixture drops the connection");
        }
        "non_sse" => return ([(header::CONTENT_TYPE, "text/html")], "SENSITIVE").into_response(),
        "reasoning" => {
            return leak(
                StatusCode::BAD_REQUEST,
                json!({"error": {"type": "invalid_request_error", "code": "unsupported_value",
                    "param": "reasoning.effort", "message": "SENSITIVE max at https://example.test"}}),
            );
        }
        "http403" => {
            return leak(
                StatusCode::FORBIDDEN,
                json!({"detail": "SENSITIVE region text"}),
            );
        }
        "capability" => {
            return leak(
                StatusCode::BAD_REQUEST,
                json!({"error": {"code": "subscription_sharing_unsupported_capability", "param": "SENSITIVE-PARAM", "message": "SENSITIVE-MSG"}}),
            );
        }
        _ => {}
    }
    let has_result = body["input"]
        .as_array()
        .unwrap()
        .iter()
        .any(|i| i["type"] == "function_call_output");
    let resp = |output: Value| json!({"id": "resp_1", "object": "response", "created_at": 1_790_000_000, "model": body["model"], "status": "completed", "output": output, "usage": {"input_tokens": 10, "output_tokens": 5, "total_tokens": 15}});
    let events = match fx.sse_mode {
        "first_chat" | "eof_completed" => {
            let mut in_progress = resp(json!([]));
            in_progress["status"] = json!("in_progress");
            in_progress["usage"] = Value::Null;
            let item = json!({"type": "message", "id": "msg_1", "role": "assistant",
            "status": "completed", "content": [
                {"type": "output_text", "text": "Hello, world!", "annotations": [], "logprobs": []}
            ]});
            let events = sse(&[
                json!({"type": "response.created", "sequence_number": 0, "response": in_progress}),
                json!({"type": "response.output_text.delta", "sequence_number": 1,
                    "item_id": "msg_1", "output_index": 0, "content_index": 0, "delta": "Hello, "}),
                json!({"type": "response.output_text.delta", "sequence_number": 2,
                    "item_id": "msg_1", "output_index": 0, "content_index": 0, "delta": "world!"}),
                json!({"type": "response.output_item.done", "sequence_number": 3, "output_index": 0,
                    "item": item}),
            ]);
            let terminal = sse(&[json!({"type": "response.completed", "sequence_number": 4,
                "response": resp(json!([item]))})]);
            let (a, b) = terminal.split_at(50);
            return streamed(vec![
                events.replace('\n', "\r\n"),
                a.into(),
                if fx.sse_mode == "eof_completed" {
                    b.trim_end().into()
                } else {
                    b.into()
                },
            ]);
        }
        "limit" => vec![
            json!({"type": "response.created", "response": {"id": "resp_1"}}),
            json!({"type": "response.output_text.delta", "delta": "par"}),
            json!({"type": "response.failed", "response": {"id": "resp_SENSITIVE", "error": {"code": "subscription_sharing_usage_limit_exceeded", "message": "SENSITIVE limit text"}}}),
        ],
        "truncated" => vec![json!({"type": "response.output_text.delta", "delta": "Hel"})],
        "stream_unknown" => vec![
            json!({"type": "response.failed", "response": {"id": "resp_SENSITIVE",
            "error": {"code": "SENSITIVE_unknown_code", "param": "SENSITIVE", "message": "SENSITIVE"}}}),
        ],
        "flat_error" => vec![json!({"type": "error", "sequence_number": 7,
            "code": "context_length_exceeded", "param": "input", "message": "SENSITIVE prompt text"})],
        "incomplete" => vec![json!({"type": "response.incomplete", "response": {
            "id": "resp_SENSITIVE", "status": "incomplete",
            "incomplete_details": {"reason": "max_output_tokens"}}})],
        "filtered" => vec![json!({"type": "response.incomplete", "response": {
            "status": "incomplete", "incomplete_details": {"reason": "content_filter"}}})],
        "bad_completion" => vec![
            json!({"type": "response.output_item.done", "output_index": 0, "item": {"type": "reasoning"}}),
            json!({"type": "response.completed", "response": "SENSITIVE not an object"}),
        ],
        "malformed" => {
            return (
                [(header::CONTENT_TYPE, "text/event-stream")],
                "data: {SENSITIVE\n\n",
            )
                .into_response();
        }
        "hosted" => vec![
            json!({"type": "response.completed", "response": resp(json!([{"type": "web_search_call", "id": "ws"}]))}),
        ],
        _ if has_result => {
            let item = json!({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "It is sunny."}]});
            vec![
                json!({"type": "response.output_text.delta", "delta": "It is sunny."}),
                json!({"type": "response.output_item.done", "output_index": 1, "item": item}),
                json!({"type": "response.output_item.done", "output_index": 0, "item": {"type": "reasoning", "id": "rs", "summary": [], "encrypted_content": "SECRET-REASONING"}}),
                // `output` left empty to check that output_item.done is used.
                json!({"type": "response.completed", "response": resp(json!([]))}),
            ]
        }
        _ => {
            let out = json!([
                {"type": "reasoning", "id": "rs", "summary": []},
                {"type": "function_call", "id": "fc_1", "call_id": "call_A", "namespace": "metro", "name": "workspace_list", "arguments": "{\"path\":\".\"}"},
                {"type": "function_call", "id": "fc_2", "call_id": "call_B", "namespace": "metro", "name": "workspace_read", "arguments": "{\"path\":\"README.md\"}"},
            ]);
            vec![json!({"type": "response.completed", "response": resp(out)})]
        }
    };
    ([(header::CONTENT_TYPE, "text/event-stream")], sse(&events)).into_response()
}

pub struct Fixture {
    pub s: S,
    pub ep: Endpoints,
    pub _dir: TempDir,
    pub profile: PathBuf,
}

pub async fn fixture(browser: Browser) -> Fixture {
    fixture_with_discovery_gate(browser, None).await
}

pub async fn fixture_with_discovery_gate(
    browser: Browser,
    gate: Option<Arc<Semaphore>>,
) -> Fixture {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let issuer = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let (enc, jwk) = rsa();
    let s: S = Arc::new(Mutex::new(Fx {
        issuer: issuer.clone(),
        enc,
        jwk,
        browser,
        codes: HashMap::new(),
        authorize: Vec::new(),
        first_expires_in: 3600,
        access: None,
        refresh: None,
        refresh_count: 0,
        refresh_error: None,
        refresh_scope: None,
        revoked: Vec::new(),
        responses_bodies: Vec::new(),
        sse_mode: "",
        sub_for_refresh: String::new(),
    }));
    let app = Router::new()
        .route(
            "/.well-known/openid-configuration",
            get(move |state: State<S>| {
                let gate = gate.clone();
                async move {
                    let _permit = match gate.as_ref() {
                        Some(gate) => Some(gate.acquire().await.unwrap()),
                        None => None,
                    };
                    discovery(state).await
                }
            }),
        )
        .route("/.well-known/jwks.json", get(jwks))
        .route("/api/accounts/oauth/token", post(token))
        .route("/oauth/revoke", post(revoke))
        .route("/v1/models", get(models))
        .route("/v1/responses", post(responses))
        .with_state(s.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let dir = TempDir::new();
    let profile = dir.0.join("conn");
    Fixture {
        s,
        ep: Endpoints {
            issuer: issuer.clone(),
            api_base: format!("{issuer}/v1"),
            resource: RESOURCE.into(),
        },
        _dir: dir,
        profile,
    }
}

/// The browser half. It follows the start redirect, checks what a real
/// authorize endpoint would see, and calls the loopback callback.
pub async fn browse(s: S, start: String) {
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let r = http
        .get(&start)
        .send()
        .await
        .map_err(reqwest::Error::without_url)
        .unwrap();
    assert_eq!(r.status(), 302, "start link must redirect");
    let loc = r.headers()[header::LOCATION].to_str().unwrap().to_owned();
    // Reopening the start link continues the same attempt.
    let again = http
        .get(&start)
        .send()
        .await
        .map_err(reqwest::Error::without_url)
        .unwrap();
    assert_eq!(again.status(), 302);
    assert_eq!(again.headers()[header::LOCATION].to_str().unwrap(), loc);
    // Another key is refused.
    let forged = format!("{}{}", &start[..start.len() - 4], "AAAA");
    assert_eq!(
        http.get(&forged)
            .send()
            .await
            .map_err(reqwest::Error::without_url)
            .unwrap()
            .status(),
        410
    );

    let u = url::Url::parse(&loc).unwrap();
    let q: HashMap<String, String> = u.query_pairs().into_owned().collect();
    let redirect = q["redirect_uri"].clone();
    let (browser, issuer) = {
        let mut fx = s.lock().unwrap();
        fx.authorize.push(q.clone());
        (fx.browser.clone(), fx.issuer.clone())
    };
    assert!(loc.starts_with(&format!("{issuer}/api/accounts/authorize?")));
    let new_reg = q["client_id"] == "dynamic_agent_client";
    let code = format!("code-{}", rand_hex());
    let mut cb = url::Url::parse(&redirect).unwrap();
    let (sub, scope, bad_nonce) = match &browser {
        Browser::Approve { sub, scope } => (*sub, *scope, false),
        Browser::BadNonce => ("user-1", PLAN_SCOPES, true),
        _ => ("user-1", PLAN_SCOPES, false),
    };
    match browser {
        Browser::Deny => {
            cb.query_pairs_mut()
                .append_pair("error", "access_denied")
                .append_pair("state", &q["state"]);
        }
        _ => {
            if matches!(browser, Browser::ForgedStateFirst) {
                let mut forged = cb.clone();
                forged
                    .query_pairs_mut()
                    .append_pair("code", "evil")
                    .append_pair("state", "forged")
                    .append_pair("client_id", "oaiapp_evil");
                assert_eq!(
                    http.get(forged.as_str()).send().await.unwrap().status(),
                    400
                );
            }
            let client_id = if new_reg {
                ISSUED.to_owned()
            } else {
                q["client_id"].clone()
            };
            s.lock().unwrap().codes.insert(
                code.clone(),
                Pending {
                    client_id: client_id.clone(),
                    challenge: q["code_challenge"].clone(),
                    redirect_uri: redirect.clone(),
                    nonce: q["nonce"].clone(),
                    sub: sub.into(),
                    scope: scope.into(),
                    bad_nonce,
                },
            );
            let mut qp = cb.query_pairs_mut();
            qp.append_pair("code", &code)
                .append_pair("scope", scope)
                .append_pair("state", &q["state"]);
            if new_reg && !matches!(browser, Browser::NoClientId) {
                qp.append_pair("client_id", ISSUED);
            }
        }
    }
    let r = http.get(cb.as_str()).send().await.unwrap();
    assert_eq!(r.status(), 200);
}

/// The text a person would see for `e`.
pub fn safe(e: &anyhow::Error) -> String {
    e.downcast_ref::<metro_connector::SafeError>()
        .map(|s| s.as_str().to_owned())
        .unwrap_or_default()
}

pub fn rand_hex() -> String {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{n:x}")
}

pub fn opts(f: &Fixture) -> LoginOptions {
    let s = f.s.clone();
    LoginOptions {
        endpoints: f.ep.clone(),
        open_browser: Box::new(move |url: &str| {
            let url = url.to_owned();
            assert!(url.starts_with("http://127.0.0.1:") && url.contains("/auth/start?k="));
            tokio::spawn(browse(s, url));
            Ok(())
        }),
        preferred_port: 0,
        timeout: Duration::from_secs(20),
    }
}

pub struct TempDir(pub PathBuf);
impl TempDir {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "metro-oauth-test-{}-{}",
            std::process::id(),
            rand_hex()
        ));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(unix)]
pub fn mode(p: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).unwrap().permissions().mode() & 0o777
}

pub fn tool_request() -> Value {
    json!({
        "model": "gpt-fixture-a",
        "messages": [
            {"role": "system", "content": "You are Metro's agent."},
            {"role": "user", "content": "List the workspace."},
        ],
        "tools": [
            {"type": "function", "function": {"name": "workspace_list", "description": "List", "parameters": {"type": "object", "properties": {"path": {"type": "string"}}}}},
            {"type": "function", "function": {"name": "workspace_read", "description": "Read", "parameters": {"type": "object"}}},
        ],
        "tool_choice": "auto",
    })
}
