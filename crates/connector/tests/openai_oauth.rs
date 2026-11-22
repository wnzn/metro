//! Inert end-to-end tests for the Sign in with ChatGPT connector.
//!
//! A local axum fixture plays OpenAI's issuer (discovery, JWKS, token,
//! revocation) and API (`/v1/models`, `/v1/responses` SSE), all on
//! 127.0.0.1. A scripted "browser" follows the one-time start redirect and
//! calls the loopback callback. Nothing here contacts OpenAI, runs a model
//! CLI, or reads any real credentials. The signing key is generated per run.

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
use metro_connector::openai_oauth::{self as oa, Endpoints, LoginOptions};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const RESOURCE: &str = "https://api.openai.com/v1";
const ISSUED: &str = "oaiapp_fixture_client";
const PLAN_SCOPES: &str =
    "chatgpt.tokens.use.direct email offline_access openid profile resource.invoke";

// ------------------------------------------------------------------ fixture

#[derive(Clone)]
enum Browser {
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

struct Pending {
    client_id: String,
    challenge: String,
    redirect_uri: String,
    nonce: String,
    sub: String,
    scope: String,
    bad_nonce: bool,
}

struct Fx {
    issuer: String,
    enc: EncodingKey,
    jwk: Value,
    browser: Browser,
    codes: HashMap<String, Pending>,
    /// Authorize query parameters seen by the browser, per sign-in.
    authorize: Vec<HashMap<String, String>>,
    first_expires_in: u64,
    access: Option<String>,
    refresh: Option<String>,
    refresh_count: u32,
    refresh_error: Option<&'static str>,
    revoked: Vec<HashMap<String, String>>,
    responses_bodies: Vec<Value>,
    sse_mode: &'static str,
    sub_for_refresh: String,
}

type S = Arc<Mutex<Fx>>;

fn rsa() -> (EncodingKey, Value) {
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

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn form(body: &str) -> HashMap<String, String> {
    url::form_urlencoded::parse(body.as_bytes())
        .into_owned()
        .collect()
}

fn fake_access(issuer: &str, client_id: &str, n: u32) -> String {
    let h = URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#);
    let p = URL_SAFE_NO_PAD.encode(
        json!({"aud": RESOURCE, "client_id": client_id, "iss": issuer, "sub": "opaque", "n": n, "exp": now() + 3600})
            .to_string(),
    );
    format!("{h}.{p}.sig")
}

impl Fx {
    fn id_token(&self, client_id: &str, sub: &str, nonce: Option<&str>) -> String {
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

async fn discovery(State(s): State<S>) -> Response {
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

async fn jwks(State(s): State<S>) -> Response {
    axum::Json(json!({ "keys": [s.lock().unwrap().jwk.clone()] })).into_response()
}

fn oauth_err(code: &str) -> Response {
    (StatusCode::BAD_REQUEST, axum::Json(json!({"error": code}))).into_response()
}

async fn token(State(s): State<S>, body: String) -> Response {
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
            let sub = fx.sub_for_refresh.clone();
            let id = fx.id_token(ISSUED, &sub, None);
            axum::Json(json!({
                "access_token": access, "refresh_token": rt, "id_token": id, "token_type": "Bearer",
                "expires_in": 3600, "scope": PLAN_SCOPES,
            }))
            .into_response()
        }
        _ => oauth_err("unsupported_grant_type"),
    }
}

async fn revoke(State(s): State<S>, body: String) -> Response {
    s.lock().unwrap().revoked.push(form(&body));
    StatusCode::OK.into_response()
}

fn bearer_ok(fx: &Fx, h: &HeaderMap) -> bool {
    let want = fx.access.as_ref().map(|a| format!("Bearer {a}"));
    h.get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
        == want
}

async fn models(State(s): State<S>, h: HeaderMap) -> Response {
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

fn sse(events: &[Value]) -> String {
    events
        .iter()
        .map(|e| format!("event: {}\ndata: {}\n\n", e["type"].as_str().unwrap(), e))
        .collect()
}

async fn responses(State(s): State<S>, h: HeaderMap, body: String) -> Response {
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
        "limit" => vec![
            json!({"type": "response.created", "response": {"id": "resp_1"}}),
            json!({"type": "response.output_text.delta", "delta": "par"}),
            json!({"type": "response.failed", "response": {"id": "resp_SENSITIVE", "error": {"code": "subscription_sharing_usage_limit_exceeded", "message": "SENSITIVE limit text"}}}),
        ],
        "truncated" => vec![json!({"type": "response.output_text.delta", "delta": "Hel"})],
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

struct Fixture {
    s: S,
    ep: Endpoints,
    _dir: TempDir,
    profile: PathBuf,
}

async fn fixture(browser: Browser) -> Fixture {
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
        revoked: Vec::new(),
        responses_bodies: Vec::new(),
        sse_mode: "",
        sub_for_refresh: String::new(),
    }));
    let app = Router::new()
        .route("/.well-known/openid-configuration", get(discovery))
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
async fn browse(s: S, start: String) {
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let r = http.get(&start).send().await.unwrap();
    assert_eq!(r.status(), 302, "start link must redirect");
    let loc = r.headers()[header::LOCATION].to_str().unwrap().to_owned();
    // The start link is single-use.
    assert_eq!(http.get(&start).send().await.unwrap().status(), 410);

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

fn rand_hex() -> String {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{n:x}")
}

fn opts(f: &Fixture) -> LoginOptions {
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

struct TempDir(PathBuf);
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

fn mode(p: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).unwrap().permissions().mode() & 0o777
}

fn tool_request() -> Value {
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

// -------------------------------------------------------------------- tests

#[tokio::test]
async fn sign_in_list_models_tool_round_and_sign_out() {
    let f = fixture(Browser::Approve {
        sub: "user-1",
        scope: PLAN_SCOPES,
    })
    .await;
    oa::login_with(&f.profile, opts(&f)).await.expect("login");

    // Authorize request: first-time dynamic registration.
    let q = f.s.lock().unwrap().authorize[0].clone();
    assert_eq!(q["client_id"], "dynamic_agent_client");
    assert_eq!(q["agent_name_hint"], "Metro");
    assert!(q["ext_agent_host_id"].starts_with("urn:uuid:"));
    assert_eq!(q["response_type"], "code");
    assert_eq!(
        q["scope"],
        "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct"
    );
    assert_eq!(q["resource"], RESOURCE);
    assert_eq!(q["code_challenge_method"], "S256");
    assert!(q["state"].len() >= 43 && q["nonce"].len() >= 43);
    assert!(
        q["redirect_uri"].starts_with("http://127.0.0.1:")
            && q["redirect_uri"].ends_with("/auth/callback")
    );
    assert!(!q.contains_key("id_token_hint") && !q.contains_key("prompt"));

    // Storage: 0700 dir, 0600 files, nothing secret in account.json.
    assert_eq!(mode(&f.profile), 0o700);
    for file in ["host.json", "account.json", "tokens.json"] {
        assert_eq!(mode(&f.profile.join(file)), 0o600, "{file}");
    }
    let account = std::fs::read_to_string(f.profile.join("account.json")).unwrap();
    assert!(
        account.contains(ISSUED)
            && !account.contains("dynamic_agent_client")
            && !account.contains("rt-0")
    );

    let st = oa::status(&f.profile).await.unwrap();
    assert!(st.signed_in && st.registered && st.plan_usage_enabled);
    assert_eq!(st.subject.as_deref(), Some("user-1"));
    let host_id = st.ext_agent_host_id.clone().unwrap();
    assert!(!format!("{st:?}").contains("rt-0"));

    // Only models the account lists for display, in server order.
    assert_eq!(
        oa::models_with(&f.profile, &f.ep).await.unwrap(),
        vec!["gpt-fixture-a", "gpt-fixture-b"]
    );

    // Round 1: the model calls two tools.
    let r1 = oa::complete_with(&f.profile, &f.ep, tool_request())
        .await
        .unwrap();
    assert_eq!(r1["object"], "chat.completion");
    assert_eq!(r1["choices"][0]["finish_reason"], "tool_calls");
    let calls = r1["choices"][0]["message"]["tool_calls"]
        .as_array()
        .unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0]["id"], "call_A");
    assert_eq!(calls[0]["function"]["name"], "workspace_list");
    assert_eq!(calls[0]["function"]["arguments"], "{\"path\":\".\"}");
    assert_eq!(calls[1]["id"], "call_B");
    assert_eq!(r1["usage"]["total_tokens"], 15);

    let sent = f.s.lock().unwrap().responses_bodies[0].clone();
    assert_eq!(sent["store"], false);
    assert_eq!(sent["stream"], true);
    assert_eq!(
        sent["input"][0]["role"], "developer",
        "system becomes developer"
    );
    assert_eq!(sent["tools"][0]["type"], "namespace");
    assert_eq!(sent["tools"][0]["name"], "metro");
    assert_eq!(sent["tools"][0]["tools"][0]["name"], "workspace_list");
    assert!(sent.get("temperature").is_none() && sent.get("max_output_tokens").is_none());

    // Round 2: history includes the calls and their results under the same IDs.
    let mut req = tool_request();
    let msgs = req["messages"].as_array_mut().unwrap();
    msgs.push(r1["choices"][0]["message"].clone());
    msgs.push(json!({"role": "tool", "tool_call_id": "call_A", "content": "README.md"}));
    msgs.push(json!({"role": "tool", "tool_call_id": "call_B", "content": [{"type": "text", "text": "# Metro"}]}));
    let r2 = oa::complete_with(&f.profile, &f.ep, req).await.unwrap();
    assert_eq!(r2["choices"][0]["message"]["content"], "It is sunny.");
    assert_eq!(r2["choices"][0]["finish_reason"], "stop");
    assert!(
        !r2.to_string().contains("SECRET-REASONING"),
        "reasoning never forwarded"
    );
    let sent = f.s.lock().unwrap().responses_bodies[1].clone();
    let input = sent["input"].as_array().unwrap();
    let fc: Vec<&Value> = input
        .iter()
        .filter(|i| i["type"] == "function_call")
        .collect();
    assert_eq!(fc[0]["call_id"], "call_A");
    assert_eq!(fc[0]["namespace"], "metro");
    assert_eq!(fc[0]["name"], "workspace_list");
    let out: Vec<&Value> = input
        .iter()
        .filter(|i| i["type"] == "function_call_output")
        .collect();
    assert_eq!(out[0]["call_id"], "call_A");
    assert_eq!(out[1]["output"], "# Metro");

    // Sign-out revokes the refresh token with the issued client ID.
    let o = oa::logout_outcome(&f.profile, &f.ep).await.unwrap();
    assert!(o.signed_out && o.remote_revoked);
    let rev = f.s.lock().unwrap().revoked[0].clone();
    assert_eq!(rev["token"], "rt-0");
    assert_eq!(rev["token_type_hint"], "refresh_token");
    assert_eq!(rev["client_id"], ISSUED);
    let st = oa::status(&f.profile).await.unwrap();
    assert!(!st.signed_in && st.registered);
    assert_eq!(
        st.ext_agent_host_id.as_deref(),
        Some(host_id.as_str()),
        "host id kept"
    );
    assert!(!f.profile.join("tokens.json").exists());
    let e = oa::complete_with(&f.profile, &f.ep, tool_request())
        .await
        .unwrap_err();
    assert!(e.to_string().contains("not signed in"), "{e}");

    // Re-sign-in reuses the issued client and host. There's no name hint and
    // no id_token_hint after sign-out, but login_hint is sent.
    oa::login_with(&f.profile, opts(&f))
        .await
        .expect("re-login");
    let q2 = f.s.lock().unwrap().authorize[1].clone();
    assert_eq!(q2["client_id"], ISSUED);
    assert_eq!(q2["ext_agent_host_id"], host_id);
    assert!(!q2.contains_key("agent_name_hint"));
    assert!(!q2.contains_key("id_token_hint"));
    assert_eq!(q2["login_hint"], "person@example.test");
    assert_ne!(q2["state"], q["state"]);
    assert_ne!(q2["code_challenge"], q["code_challenge"]);

    // Reauthorizing while tokens are retained sends id_token_hint.
    oa::login_with(&f.profile, opts(&f)).await.expect("reauth");
    let q3 = f.s.lock().unwrap().authorize[2].clone();
    assert!(q3["id_token_hint"].split('.').count() == 3);
}

#[tokio::test]
async fn expiry_refresh_is_serialized_and_rotates() {
    let f = fixture(Browser::Approve {
        sub: "user-1",
        scope: PLAN_SCOPES,
    })
    .await;
    f.s.lock().unwrap().first_expires_in = 1;
    oa::login_with(&f.profile, opts(&f)).await.unwrap();

    // Eight concurrent callers against an expired token produce exactly one
    // refresh. The others reuse the rotated token after taking the lock.
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let (p, ep) = (f.profile.clone(), f.ep.clone());
        tasks.push(tokio::spawn(async move { oa::models_with(&p, &ep).await }));
    }
    for t in tasks {
        assert_eq!(t.await.unwrap().unwrap().len(), 2);
    }
    let fx = f.s.lock().unwrap();
    assert_eq!(fx.refresh_count, 1);
    assert_eq!(fx.refresh.as_deref(), Some("rt-1"));
    drop(fx);
    let tokens = std::fs::read_to_string(f.profile.join("tokens.json")).unwrap();
    assert!(tokens.contains("rt-1") && !tokens.contains("\"rt-0\""));
}

#[tokio::test]
async fn unusable_refresh_clears_tokens_but_keeps_registration() {
    let f = fixture(Browser::Approve {
        sub: "user-1",
        scope: PLAN_SCOPES,
    })
    .await;
    f.s.lock().unwrap().first_expires_in = 1;
    oa::login_with(&f.profile, opts(&f)).await.unwrap();
    f.s.lock().unwrap().refresh_error = Some("invalid_grant");
    let e = oa::models_with(&f.profile, &f.ep).await.unwrap_err();
    assert!(format!("{e:#}").contains("sign in again"), "{e:#}");
    let st = oa::status(&f.profile).await.unwrap();
    assert!(!st.signed_in && st.registered);
}

#[tokio::test]
async fn transient_refresh_failure_keeps_tokens() {
    let f = fixture(Browser::Approve {
        sub: "user-1",
        scope: PLAN_SCOPES,
    })
    .await;
    f.s.lock().unwrap().first_expires_in = 1;
    oa::login_with(&f.profile, opts(&f)).await.unwrap();
    f.s.lock().unwrap().refresh_error = Some("temporarily_unavailable");
    assert!(oa::models_with(&f.profile, &f.ep).await.is_err());
    assert!(oa::status(&f.profile).await.unwrap().signed_in);
}

#[tokio::test]
async fn forged_state_is_ignored_then_real_callback_completes() {
    let f = fixture(Browser::ForgedStateFirst).await;
    oa::login_with(&f.profile, opts(&f))
        .await
        .expect("login despite forged callback");
    let account = std::fs::read_to_string(f.profile.join("account.json")).unwrap();
    assert!(!account.contains("oaiapp_evil"));
}

#[tokio::test]
async fn declined_consent_stores_nothing() {
    let f = fixture(Browser::Deny).await;
    let e = oa::login_with(&f.profile, opts(&f)).await.unwrap_err();
    assert!(e.to_string().contains("declined"), "{e}");
    assert!(!f.profile.join("tokens.json").exists());
    assert!(!f.profile.join("account.json").exists());
}

#[tokio::test]
async fn bad_nonce_is_rejected() {
    let f = fixture(Browser::BadNonce).await;
    let e = oa::login_with(&f.profile, opts(&f)).await.unwrap_err();
    assert!(e.to_string().contains("nonce"), "{e}");
    assert!(!f.profile.join("tokens.json").exists());
}

#[tokio::test]
async fn registration_without_issued_client_id_is_incomplete() {
    let f = fixture(Browser::NoClientId).await;
    let e = oa::login_with(&f.profile, opts(&f)).await.unwrap_err();
    assert!(e.to_string().contains("registration incomplete"), "{e}");
}

#[tokio::test]
async fn missing_plan_scope_signs_in_but_never_calls_inference() {
    let f = fixture(Browser::Approve {
        sub: "user-1",
        scope: "email offline_access openid profile",
    })
    .await;
    let e = oa::login_with(&f.profile, opts(&f)).await.unwrap_err();
    assert!(e.to_string().contains("chatgpt.tokens.use.direct"), "{e}");
    let st = oa::status(&f.profile).await.unwrap();
    assert!(st.signed_in && !st.plan_usage_enabled);
    assert!(
        oa::complete_with(&f.profile, &f.ep, tool_request())
            .await
            .is_err()
    );
    assert!(
        f.s.lock().unwrap().responses_bodies.is_empty(),
        "no inference without plan scope"
    );
    // The next sign-in asks for consent with the full scope set.
    f.s.lock().unwrap().browser = Browser::Approve {
        sub: "user-1",
        scope: PLAN_SCOPES,
    };
    oa::login_with(&f.profile, opts(&f)).await.unwrap();
    let q = f.s.lock().unwrap().authorize[1].clone();
    assert_eq!(q["prompt"], "consent");
    assert!(q["scope"].contains("chatgpt.tokens.use.direct"));
    assert!(oa::status(&f.profile).await.unwrap().plan_usage_enabled);
}

#[tokio::test]
async fn different_account_cannot_overwrite_registration() {
    let f = fixture(Browser::Approve {
        sub: "user-1",
        scope: PLAN_SCOPES,
    })
    .await;
    oa::login_with(&f.profile, opts(&f)).await.unwrap();
    f.s.lock().unwrap().browser = Browser::Approve {
        sub: "user-2",
        scope: PLAN_SCOPES,
    };
    let e = oa::login_with(&f.profile, opts(&f)).await.unwrap_err();
    assert!(e.to_string().contains("different ChatGPT account"), "{e}");
    assert_eq!(
        oa::status(&f.profile).await.unwrap().subject.as_deref(),
        Some("user-1")
    );
}

#[tokio::test]
async fn stream_failures_are_visible() {
    let f = fixture(Browser::Approve {
        sub: "user-1",
        scope: PLAN_SCOPES,
    })
    .await;
    oa::login_with(&f.profile, opts(&f)).await.unwrap();

    f.s.lock().unwrap().sse_mode = "limit";
    let e = oa::complete_with(&f.profile, &f.ep, tool_request())
        .await
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("subscription_sharing_usage_limit_exceeded")
            && e.contains("chatgpt.com/settings/usage"),
        "{e}"
    );

    f.s.lock().unwrap().sse_mode = "truncated";
    let e = oa::complete_with(&f.profile, &f.ep, tool_request())
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("without response.completed"), "{e}");

    f.s.lock().unwrap().sse_mode = "hosted";
    let e = oa::complete_with(&f.profile, &f.ep, tool_request())
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("web_search_call"), "{e}");
}

#[tokio::test]
async fn profile_must_be_private_and_endpoints_bound() {
    use std::os::unix::fs::PermissionsExt;
    let f = fixture(Browser::Approve {
        sub: "user-1",
        scope: PLAN_SCOPES,
    })
    .await;
    std::fs::create_dir(&f.profile).unwrap();
    std::fs::set_permissions(&f.profile, std::fs::Permissions::from_mode(0o755)).unwrap();
    let e = oa::login_with(&f.profile, opts(&f)).await.unwrap_err();
    assert!(e.to_string().contains("mode 700"), "{e}");
    std::fs::set_permissions(&f.profile, std::fs::Permissions::from_mode(0o700)).unwrap();
    oa::login_with(&f.profile, opts(&f)).await.unwrap();
    // A signed-in fixture profile refuses production endpoints, and the other
    // way round.
    let e = oa::models(&f.profile).await.unwrap_err();
    assert!(e.to_string().contains("different OpenAI issuer"), "{e}");
}

#[tokio::test]
async fn loopback_rejects_foreign_host_header() {
    let f = fixture(Browser::Approve {
        sub: "user-1",
        scope: PLAN_SCOPES,
    })
    .await;
    let (tx, rx) = tokio::sync::oneshot::channel::<String>();
    let mut o = opts(&f);
    o.timeout = Duration::from_secs(3);
    o.open_browser = Box::new(move |url: &str| {
        let _ = tx.send(url.to_owned());
        Ok(())
    });
    let login = tokio::spawn({
        let p = f.profile.clone();
        async move { oa::login_with(&p, o).await }
    });
    let start = rx.await.unwrap();
    let u = url::Url::parse(&start).unwrap();
    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", u.port().unwrap()))
        .await
        .unwrap();
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let req = format!(
        "GET {}?{} HTTP/1.1\r\nHost: evil.example:80\r\n\r\n",
        u.path(),
        u.query().unwrap()
    );
    s.write_all(req.as_bytes()).await.unwrap();
    let mut buf = String::new();
    s.read_to_string(&mut buf).await.unwrap();
    assert!(buf.starts_with("HTTP/1.1 421"), "{buf}");
    let e = login.await.unwrap().unwrap_err();
    assert!(e.to_string().contains("timed out"), "{e}");
}

#[test]
fn translation_is_strict() {
    for (field, v) in [
        ("temperature", json!(0.2)),
        ("max_tokens", json!(10)),
        ("stop", json!(["x"])),
        ("n", json!(2)),
        ("stream", json!(true)),
        ("user", json!("u")),
    ] {
        let mut r = tool_request();
        r[field] = v;
        let e = oa::chat_to_responses(&r).unwrap_err().to_string();
        assert!(e.contains(field), "{field}: {e}");
    }
    let mut r = tool_request();
    r["n"] = json!(1);
    r["temperature"] = Value::Null;
    assert!(oa::chat_to_responses(&r).is_ok());

    let mut r = tool_request();
    r["tools"] = json!([{"type": "web_search"}]);
    assert!(
        oa::chat_to_responses(&r)
            .unwrap_err()
            .to_string()
            .contains("web_search")
    );

    let mut r = tool_request();
    r["tool_choice"] = json!({"type": "function", "function": {"name": "workspace_list"}});
    assert!(oa::chat_to_responses(&r).is_err());

    let mut r = tool_request();
    r["messages"]
        .as_array_mut()
        .unwrap()
        .push(json!({"role": "tool", "tool_call_id": "nope", "content": "x"}));
    assert!(
        oa::chat_to_responses(&r)
            .unwrap_err()
            .to_string()
            .contains("unknown call id")
    );

    let mut r = tool_request();
    r["messages"][1]["content"] = json!([{"type": "input_audio", "input_audio": {}}]);
    assert!(
        oa::chat_to_responses(&r)
            .unwrap_err()
            .to_string()
            .contains("audio")
    );

    // Images, json_schema, reasoning effort and custom tools translate.
    let r = json!({
        "model": "m",
        "reasoning_effort": "low",
        "response_format": {"type": "json_schema", "json_schema": {"name": "out", "schema": {"type": "object"}, "strict": true}},
        "tools": [{"type": "custom", "custom": {"name": "apply_patch", "description": "patch"}}],
        "messages": [
            {"role": "developer", "content": "d"},
            {"role": "user", "content": [{"type": "text", "text": "see"}, {"type": "image_url", "image_url": {"url": "data:image/png;base64,AA", "detail": "low"}}]},
            {"role": "assistant", "content": null, "reasoning_content": "hidden", "tool_calls": [{"id": "c1", "type": "custom", "custom": {"name": "apply_patch", "input": "*** patch"}}]},
            {"role": "tool", "tool_call_id": "c1", "content": "ok"},
        ],
    });
    let b = oa::chat_to_responses(&r).unwrap();
    assert_eq!(b["reasoning"]["effort"], "low");
    assert_eq!(b["text"]["format"]["type"], "json_schema");
    assert_eq!(b["text"]["format"]["name"], "out");
    assert_eq!(b["tools"][0]["tools"][0]["type"], "custom");
    assert_eq!(b["input"][1]["content"][1]["type"], "input_image");
    assert_eq!(b["input"][2]["type"], "custom_tool_call");
    assert_eq!(b["input"][3]["type"], "custom_tool_call_output");
    assert!(!b.to_string().contains("hidden"));

    // Normalizing back: custom calls, tools outside the namespace, status.
    let c = oa::responses_to_chat(&json!({"status": "completed", "output": [{"type": "custom_tool_call", "call_id": "c2", "namespace": "metro", "name": "apply_patch", "input": "x"}]})).unwrap();
    assert_eq!(
        c["choices"][0]["message"]["tool_calls"][0]["custom"]["name"],
        "apply_patch"
    );
    assert!(oa::responses_to_chat(&json!({"status": "completed", "output": [{"type": "function_call", "call_id": "c", "namespace": "other", "name": "x"}]})).is_err());
    assert!(oa::responses_to_chat(&json!({"status": "incomplete", "output": []})).is_err());
}

#[test]
fn eligibility_is_explicit() {
    let e = oa::eligibility();
    assert!(e.eligible.contains("open-source") && e.not_covered.contains("remotely hosted"));
    assert!(
        e.docs
            .iter()
            .all(|d| d.starts_with("https://developers.openai.com/siwc/"))
    );
}

/// What `run.rs` sends to Metro for an error, and what it logs locally.
fn metro_and_local(e: &anyhow::Error) -> (String, String) {
    (
        metro_connector::secret::metro_error_text(e),
        metro_connector::secret::log_text(e, &[]),
    )
}

#[tokio::test]
async fn metro_sees_only_fixed_error_categories() {
    let generic =
        "the connector could not complete this request (see the connector log on the device)";
    let f = fixture(Browser::Approve {
        sub: "user-1",
        scope: PLAN_SCOPES,
    })
    .await;

    // Not signed in: sign-in category, before any network call.
    let e = oa::complete_with(&f.profile, &f.ep, tool_request())
        .await
        .unwrap_err();
    let (metro, _) = metro_and_local(&e);
    assert!(metro.contains("Sign in again on the device"), "{metro}");

    oa::login_with(&f.profile, opts(&f)).await.unwrap();

    // Unsupported option: fixed text. The request's own text never appears.
    let mut r = tool_request();
    r["temperature"] = json!(0.3);
    r["messages"][1]["content"] = json!("SENSITIVE-USER-TEXT");
    let e = oa::complete_with(&f.profile, &f.ep, r).await.unwrap_err();
    let (metro, local) = metro_and_local(&e);
    assert!(metro.contains("doesn't support"), "{metro}");
    assert!(local.contains("temperature"), "detail stays local: {local}");

    let cases: &[(&str, Option<&str>)] = &[
        ("limit", Some("usage limit reached")),
        ("capability", Some("doesn't support")),
        ("http503", Some("temporarily unavailable")),
        ("http403", Some("isn't available for this connection")),
        ("truncated", Some("temporarily unavailable")),
        ("http400", None),
    ];
    for (mode, want) in cases {
        f.s.lock().unwrap().sse_mode = mode;
        let e = oa::complete_with(&f.profile, &f.ep, tool_request())
            .await
            .unwrap_err();
        let (metro, local) = metro_and_local(&e);
        match want {
            Some(w) => assert!(metro.contains(w), "{mode}: {metro}"),
            None => assert_eq!(metro, generic, "{mode}"),
        }
        for leak in [
            "SENSITIVE",
            "req_",
            "resp_",
            "weird_code",
            "HTTP",
            "{",
            "\"",
        ] {
            assert!(
                !metro.contains(leak),
                "{mode}: {leak:?} reached Metro: {metro}"
            );
        }
        assert!(metro.chars().count() <= metro_connector::secret::MAX_SAFE_ERROR);
        if *mode != "truncated" {
            assert!(
                local.contains("SENSITIVE"),
                "{mode}: local log keeps the detail: {local}"
            );
        }
    }

    // Model listing uses the same boundary.
    f.s.lock().unwrap().sse_mode = "";
    let mut o = f.ep.clone();
    o.api_base = format!("{}/missing", f.ep.issuer);
    let e = oa::models_with(&f.profile, &o).await.unwrap_err();
    assert!(!metro_and_local(&e).0.contains("missing"));
}
