//! End-to-end tests of the real `metro-connector` binary against a fake
//! Metro server and a fake OpenAI-compatible model server, both on loopback.
//! No external accounts, no vendor CLIs.

#[path = "common/cli_stdin.rs"]
mod cli_stdin;

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path as UrlPath, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use metro_connector::Secret;
use metro_connector::metro::{Claim, ErrorKind, MetroClient};
use metro_connector::profile::{MAX_MODEL_LEN, MAX_MODELS};
use serde_json::{Value, json};

const DEVICE_TOKEN: &str = "metro_pc_TESTdeviceTOKEN0123456789abcdef";
const PROVIDER_KEY: &str = "byok-test-key-DO-NOT-LEAK-42";

// ---------------------------------------------------------------- fake Metro

#[derive(Default)]
struct MetroState {
    jobs: VecDeque<Value>,
    /// (job id, result body)
    results: Vec<(String, Value)>,
    /// Every Authorization header and body Metro received.
    seen_auth: Vec<String>,
    seen_bodies: Vec<String>,
    /// Fail this many result posts with 500 before accepting.
    fail_results: usize,
    claim_overrides: VecDeque<Response>,
    claims: usize,
    /// The token this server accepts (default `DEVICE_TOKEN`).
    token: Option<String>,
}

type Metro = Arc<Mutex<MetroState>>;

async fn claim(State(s): State<Metro>, headers: HeaderMap, body: Bytes) -> Response {
    let mut s = s.lock().unwrap();
    s.claims += 1;
    s.seen_auth.push(auth(&headers));
    s.seen_bodies.push(String::from_utf8_lossy(&body).into());
    let expected = s.token.clone().unwrap_or_else(|| DEVICE_TOKEN.to_string());
    if auth(&headers) != format!("Bearer {expected}") {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if let Some(r) = s.claim_overrides.pop_front() {
        return r;
    }
    match s.jobs.pop_front() {
        Some(j) => (StatusCode::OK, axum::Json(j)).into_response(),
        None => StatusCode::NO_CONTENT.into_response(),
    }
}

async fn result(
    State(s): State<Metro>,
    UrlPath(id): UrlPath<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let mut s = s.lock().unwrap();
    s.seen_auth.push(auth(&headers));
    s.seen_bodies.push(String::from_utf8_lossy(&body).into());
    if s.fail_results > 0 {
        s.fail_results -= 1;
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    let v: Value = serde_json::from_slice(&body).unwrap();
    s.results.push((id, v));
    StatusCode::NO_CONTENT.into_response()
}

fn auth(h: &HeaderMap) -> String {
    h.get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string()
}

// ---------------------------------------------------------------- fake model

#[derive(Default)]
struct ModelState {
    seen_auth: Vec<Option<String>>,
    completions: Vec<Value>,
    /// Return a body this large instead of a normal answer.
    huge: Option<usize>,
    redirect: bool,
    /// Answer only after this long.
    delay: Option<std::time::Duration>,
}

type Model = Arc<Mutex<ModelState>>;

async fn models(State(s): State<Model>, headers: HeaderMap) -> Response {
    let mut s = s.lock().unwrap();
    s.seen_auth.push(
        headers
            .get("authorization")
            .map(|v| v.to_str().unwrap().into()),
    );
    axum::Json(json!({"object": "list", "data": [
        {"id": "local-small", "object": "model"},
        {"id": "local-big", "object": "model"},
        {"id": "not-approved", "object": "model"}
    ]}))
    .into_response()
}

async fn completions(State(state): State<Model>, headers: HeaderMap, body: Bytes) -> Response {
    let delay = state.lock().unwrap().delay;
    if let Some(d) = delay {
        tokio::time::sleep(d).await;
    }
    let mut s = state.lock().unwrap();
    s.seen_auth.push(
        headers
            .get("authorization")
            .map(|v| v.to_str().unwrap().into()),
    );
    let req: Value = serde_json::from_slice(&body).unwrap();
    s.completions.push(req.clone());
    if s.redirect {
        return (
            StatusCode::FOUND,
            [("location", "http://127.0.0.1:1/steal")],
        )
            .into_response();
    }
    if let Some(n) = s.huge {
        return (StatusCode::OK, "x".repeat(n)).into_response();
    }
    axum::Json(json!({
        "id": "chatcmpl-test", "object": "chat.completion", "created": 7, "model": req["model"],
        "system_fingerprint": "vendor-internal",
        "x_vendor_debug": {"echo_key": PROVIDER_KEY},
        "choices": [{"index": 0, "finish_reason": "tool_calls", "logprobs": null, "message": {
            "role": "assistant", "content": null,
            "tool_calls": [{"id": "call_9", "type": "function", "function": {"name": "read_file", "arguments": "{\"path\":\"README.md\"}"}}]
        }}],
        "usage": {"prompt_tokens": 11, "completion_tokens": 5, "total_tokens": 16}
    }))
    .into_response()
}

// ---------------------------------------------------------------- harness

async fn serve(app: Router) -> SocketAddr {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    addr
}

struct Env {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    metro: Metro,
    model: Model,
    metro_url: String,
    model_url: String,
}

async fn spawn_metro(token: Option<&str>) -> (Metro, String) {
    let metro: Metro = Default::default();
    metro.lock().unwrap().token = token.map(str::to_string);
    let m = serve(
        Router::new()
            .route("/v1/device/claim", post(claim))
            .route("/v1/device/jobs/{id}/result", post(result))
            .with_state(metro.clone()),
    )
    .await;
    (metro, format!("http://127.0.0.1:{}", m.port()))
}

async fn spawn_model() -> (Model, String) {
    let model: Model = Default::default();
    let p = serve(
        Router::new()
            .route("/v1/models", get(models))
            .route("/v1/chat/completions", post(completions))
            .with_state(model.clone()),
    )
    .await;
    (model, format!("http://127.0.0.1:{}/v1", p.port()))
}

async fn env() -> Env {
    let (metro, metro_url) = spawn_metro(None).await;
    let (model, model_url) = spawn_model().await;
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("connector");
    Env {
        home,
        metro,
        model,
        metro_url,
        model_url,
        _tmp: tmp,
    }
}

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

async fn connector(home: &Path, args: &[&str], stdin: Option<&str>) -> Out {
    let mut cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_metro-connector"));
    cmd.args(args)
        .env_clear()
        .env("METRO_CONNECTOR_HOME", home)
        .env("HOME", home.parent().unwrap())
        .env("METRO_CONNECTOR_POLL_MS", "50")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd.spawn().unwrap();
    cli_stdin::send_stdin(child.stdin.take().unwrap(), stdin).await;
    let out = tokio::time::timeout(std::time::Duration::from_secs(60), child.wait_with_output())
        .await
        .expect("connector timed out")
        .unwrap();
    Out {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into(),
        stderr: String::from_utf8_lossy(&out.stderr).into(),
    }
}

fn write_private(path: &Path, s: &str) {
    std::fs::write(path, s).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
}

fn assert_no_secrets(out: &Out) {
    for s in [DEVICE_TOKEN, PROVIDER_KEY] {
        assert!(!out.stdout.contains(s), "secret on stdout: {}", out.stdout);
        assert!(!out.stderr.contains(s), "secret on stderr: {}", out.stderr);
    }
}

/// Set up an openai-compatible profile: token from stdin, key from a file.
async fn setup(e: &Env, with_key: bool) {
    let key_file = e.home.parent().unwrap().join("key.txt");
    write_private(&key_file, &format!("{PROVIDER_KEY}\n"));
    let mut args = vec![
        "setup",
        "--profile",
        "work",
        "--server-url",
        &e.metro_url,
        "--kind",
        "device-openai-compatible",
        "--base-url",
        &e.model_url,
        "--device-token-file",
        "-",
        "--model",
        "local-small",
        "--model",
        "local-big",
        "--yes",
    ];
    let kf = key_file.to_str().unwrap().to_string();
    if with_key {
        args.extend(["--api-key-file", &kf]);
    } else {
        args.push("--no-api-key");
    }
    let out = connector(&e.home, &args, Some(&format!("{DEVICE_TOKEN}\n"))).await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_no_secrets(&out);
}

async fn run_once(e: &Env) -> Out {
    let out = connector(&e.home, &["run", "--profile", "work", "--once"], None).await;
    assert_no_secrets(&out);
    out
}

fn push_job(e: &Env, job: Value) {
    e.metro.lock().unwrap().jobs.push_back(job);
}

fn last_result(e: &Env) -> (String, Value) {
    e.metro
        .lock()
        .unwrap()
        .results
        .last()
        .cloned()
        .expect("no result posted")
}

fn metro_never_saw_key(e: &Env) {
    let s = e.metro.lock().unwrap();
    for a in &s.seen_auth {
        assert_eq!(a, &format!("Bearer {DEVICE_TOKEN}"));
    }
    for b in &s.seen_bodies {
        assert!(!b.contains(PROVIDER_KEY), "provider key reached Metro: {b}");
    }
}

fn claim_client(e: &Env) -> MetroClient {
    MetroClient::new(
        &e.metro_url,
        Secret::new(DEVICE_TOKEN),
        std::time::Duration::from_secs(5),
    )
    .unwrap()
}

fn claim_bodies(e: &Env) -> Vec<Value> {
    e.metro
        .lock()
        .unwrap()
        .seen_bodies
        .iter()
        .map(|b| serde_json::from_str(b).unwrap())
        .collect()
}

fn chat(model: &str) -> Value {
    json!({
        "model": model,
        "messages": [
            {"role": "system", "content": "You are Metro's agent."},
            {"role": "user", "content": "Read the README"}
        ],
        "tools": [{"type": "function", "function": {"name": "read_file", "description": "Read a file",
            "parameters": {"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]}}}],
        "tool_choice": "auto",
        "temperature": 0.1,
        "max_tokens": 256
    })
}

// ---------------------------------------------------------------- tests

#[tokio::test]
async fn claim_legacy_retry_is_authenticated_and_local_to_the_client() {
    for old_response in [
        StatusCode::BAD_REQUEST.into_response(),
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            axum::Json(json!({"error": "the body must be {}"})),
        )
            .into_response(),
    ] {
        let e = env().await;
        e.metro
            .lock()
            .unwrap()
            .claim_overrides
            .push_back(old_response);
        let models = ["local-small".into()];
        let client = claim_client(&e);
        for _ in 0..2 {
            assert_eq!(
                client.claim_with_inventory(Some(&models)).await.unwrap(),
                Claim::Idle
            );
        }
        assert_eq!(
            claim_client(&e)
                .claim_with_inventory(Some(&models))
                .await
                .unwrap(),
            Claim::Idle
        );
        let extended = json!({"approved_models": models});
        assert_eq!(
            claim_bodies(&e),
            vec![extended.clone(), json!({}), json!({}), extended]
        );
        metro_never_saw_key(&e);
        assert!(e.model.lock().unwrap().seen_auth.is_empty());
    }
}

#[tokio::test]
async fn claim_errors_do_not_downgrade_or_mask_revocation() {
    let models = ["local-small".into()];
    let extended = json!({"approved_models": models});
    for (status, kind) in [
        (StatusCode::UNAUTHORIZED, ErrorKind::Unauthorized),
        (StatusCode::FORBIDDEN, ErrorKind::Unauthorized),
        (StatusCode::UNPROCESSABLE_ENTITY, ErrorKind::Fatal),
        (StatusCode::TOO_MANY_REQUESTS, ErrorKind::Retryable),
        (StatusCode::INTERNAL_SERVER_ERROR, ErrorKind::Retryable),
    ] {
        let e = env().await;
        e.metro
            .lock()
            .unwrap()
            .claim_overrides
            .push_back(status.into_response());
        let err = claim_client(&e)
            .claim_with_inventory(Some(&models))
            .await
            .unwrap_err();
        assert_eq!(err.kind, kind);
        assert_eq!(claim_bodies(&e), vec![extended.clone()]);
        metro_never_saw_key(&e);
    }

    let e = env().await;
    e.metro.lock().unwrap().claim_overrides.push_back(
        (
            StatusCode::BAD_REQUEST,
            axum::Json(json!({"error": "some other invalid request"})),
        )
            .into_response(),
    );
    assert_eq!(
        claim_client(&e)
            .claim_with_inventory(Some(&models))
            .await
            .unwrap_err()
            .message,
        "claim: HTTP 400",
    );
    assert_eq!(claim_bodies(&e), vec![extended.clone()]);
    metro_never_saw_key(&e);

    let e = env().await;
    let client = claim_client(&e);
    e.metro.lock().unwrap().claim_overrides.extend([
        StatusCode::BAD_REQUEST.into_response(),
        StatusCode::UNAUTHORIZED.into_response(),
        StatusCode::BAD_REQUEST.into_response(),
    ]);
    assert_eq!(
        client
            .claim_with_inventory(Some(&models))
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Unauthorized,
    );
    assert_eq!(
        client
            .claim_with_inventory(Some(&models))
            .await
            .unwrap_err()
            .message,
        "claim: HTTP 400",
    );
    assert_eq!(
        claim_bodies(&e),
        vec![extended.clone(), json!({}), extended.clone()]
    );
    metro_never_saw_key(&e);

    let e = env().await;
    let client = claim_client(&e);
    assert_eq!(
        client.claim_with_inventory(Some(&models)).await.unwrap(),
        Claim::Idle
    );
    e.metro
        .lock()
        .unwrap()
        .claim_overrides
        .push_back(StatusCode::BAD_REQUEST.into_response());
    assert_eq!(
        client
            .claim_with_inventory(Some(&models))
            .await
            .unwrap_err()
            .message,
        "claim: HTTP 400",
    );
    assert_eq!(claim_bodies(&e), vec![extended.clone(), extended]);
    metro_never_saw_key(&e);
}

#[tokio::test]
async fn claim_checks_profile_limits_without_changing_the_list() {
    let e = env().await;
    let client = claim_client(&e);
    let mut models = (0..MAX_MODELS)
        .map(|i| format!("{i:02}{}", "é".repeat((MAX_MODEL_LEN - 2) / 2)))
        .collect::<Vec<_>>();
    for bad in [
        vec!["a".into(), "a".into()],
        vec!["".into()],
        vec!["a\u{a0}b".into()],
        vec!["a\u{7f}b".into()],
        vec![format!("a{}", "é".repeat(MAX_MODEL_LEN / 2))],
    ] {
        let err = client.claim_with_inventory(Some(&bad)).await.unwrap_err();
        assert_eq!(err.kind, ErrorKind::Fatal);
        assert_eq!(err.message, "claim: invalid approved models");
    }
    models.push("extra".into());
    assert_eq!(
        client
            .claim_with_inventory(Some(&models))
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Fatal
    );
    assert!(claim_bodies(&e).is_empty());
    models.pop();
    assert_eq!(
        client.claim_with_inventory(Some(&models)).await.unwrap(),
        Claim::Idle
    );
    assert_eq!(
        client.claim_with_inventory(Some(&[])).await.unwrap(),
        Claim::Idle
    );
    assert_eq!(client.claim().await.unwrap(), Claim::Idle);
    assert_eq!(
        claim_bodies(&e),
        vec![
            json!({"approved_models": models}),
            json!({"approved_models": []}),
            json!({})
        ]
    );
    metro_never_saw_key(&e);
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn setup_writes_private_profile() {
    use std::os::unix::fs::PermissionsExt;
    let e = env().await;
    setup(&e, true).await;
    let dir = e.home.join("profiles/work");
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&e.home), 0o700);
    assert_eq!(mode(&dir), 0o700);
    assert_eq!(mode(&dir.join("vendor")), 0o700);
    for f in ["profile.json", "device-token", "provider-key"] {
        assert_eq!(mode(&dir.join(f)), 0o600, "{f}");
    }
    let cfg: Value =
        serde_json::from_slice(&std::fs::read(dir.join("profile.json")).unwrap()).unwrap();
    assert_eq!(cfg["kind"], "device-openai-compatible");
    assert_eq!(cfg["models"], json!(["local-small", "local-big"]));
    let raw = std::fs::read_to_string(dir.join("profile.json")).unwrap();
    assert!(!raw.contains(DEVICE_TOKEN) && !raw.contains(PROVIDER_KEY));

    let status = connector(&e.home, &["status", "--profile", "work"], None).await;
    assert_eq!(status.code, 0, "{}", status.stderr);
    assert!(status.stdout.contains("device token: saved"));
    assert_no_secrets(&status);

    // Setup never contacted Metro (a claim could consume a job).
    assert_eq!(e.metro.lock().unwrap().claims, 0);

    // logout removes the key but keeps the device token; forget removes all.
    let out = connector(&e.home, &["logout", "--profile", "work"], None).await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(!dir.join("provider-key").exists());
    assert!(dir.join("device-token").exists());
    let out = connector(&e.home, &["forget", "--profile", "work", "--yes"], None).await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(!dir.exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn models_and_complete_roundtrip() {
    let e = env().await;
    setup(&e, true).await;

    assert_eq!(run_once(&e).await.code, 0);
    let approved = json!({"approved_models": ["local-small", "local-big"]});
    assert_eq!(claim_bodies(&e), vec![approved.clone()]);
    // An idle poll is driven only by local approval, never by vendor discovery.
    assert!(e.model.lock().unwrap().seen_auth.is_empty());

    push_job(
        &e,
        json!({"id": "dj_1", "operation": "models", "request": {}}),
    );
    let out = run_once(&e).await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    let (id, body) = last_result(&e);
    assert_eq!(id, "dj_1");
    // Only approved models, in the approved order.
    assert_eq!(
        body,
        json!({"ok": true, "response": {"models": ["local-small", "local-big"]}})
    );

    push_job(
        &e,
        json!({"id": "dj_2", "operation": "complete", "request": chat("local-small")}),
    );
    let out = run_once(&e).await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    let (id, body) = last_result(&e);
    assert_eq!(id, "dj_2");
    assert_eq!(body["ok"], true, "{body}");
    let resp = &body["response"];
    assert_eq!(resp["choices"][0]["finish_reason"], "tool_calls");
    assert_eq!(
        resp["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
        "read_file"
    );
    assert_eq!(resp["usage"]["total_tokens"], 16);
    let s = resp.to_string();
    assert!(
        !s.contains("vendor-internal")
            && !s.contains("x_vendor_debug")
            && !s.contains(PROVIDER_KEY)
    );

    let m = e.model.lock().unwrap();
    // The key went to the model endpoint, as a bearer token, and only there.
    assert!(
        m.seen_auth
            .iter()
            .all(|a| a.as_deref() == Some(&format!("Bearer {PROVIDER_KEY}")[..]))
    );
    let sent = &m.completions[0];
    assert_eq!(sent["model"], "local-small");
    assert_eq!(sent["stream"], false);
    assert!(sent.get("approved_models").is_none());
    drop(m);
    let bodies = claim_bodies(&e);
    for i in [0, 1, 3] {
        assert_eq!(bodies[i], approved);
    }
    assert_eq!(bodies.len(), 5);
    metro_never_saw_key(&e);
}

/// Metro sends a self-hosted model's thinking option on the wire the owner
/// chose for it; the connector forwards it to the endpoint unchanged.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn custom_thinking_reaches_the_endpoint() {
    let e = env().await;
    setup(&e, true).await;
    let mut top = chat("local-small");
    top["reasoning_effort"] = json!("think-hard");
    let mut nested = chat("local-big");
    nested["reasoning"] = json!({"effort": "low"});
    for (id, req) in [("dj_t", top), ("dj_n", nested)] {
        push_job(
            &e,
            json!({"id": id, "operation": "complete", "request": req}),
        );
        let out = run_once(&e).await;
        assert_eq!(out.code, 0, "{}", out.stderr);
        let (got, body) = last_result(&e);
        assert_eq!(got, id);
        assert_eq!(body["ok"], true, "{body}");
    }
    let m = e.model.lock().unwrap();
    assert_eq!(m.completions.len(), 2);
    assert_eq!(m.completions[0]["reasoning_effort"], "think-hard");
    assert!(m.completions[0].get("reasoning").is_none());
    assert_eq!(m.completions[1]["reasoning"], json!({"effort": "low"}));
    assert!(m.completions[1].get("reasoning_effort").is_none());
    drop(m);
    metro_never_saw_key(&e);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_key_endpoint_sends_no_auth() {
    let e = env().await;
    setup(&e, false).await;
    push_job(
        &e,
        json!({"id": "dj_1", "operation": "complete", "request": chat("local-big")}),
    );
    let out = run_once(&e).await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(last_result(&e).1["ok"], true);
    assert_eq!(e.model.lock().unwrap().seen_auth, vec![None]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn refuses_unsafe_jobs_without_calling_the_model() {
    let e = env().await;
    setup(&e, true).await;
    let mut image = chat("local-small");
    image["messages"][1]["content"] =
        json!([{"type": "image_url", "image_url": {"url": "http://169.254.169.254/latest"}}]);
    let mut hosted = chat("local-small");
    hosted["tools"] = json!([{"type": "web_search"}]);
    let mut url_field = chat("local-small");
    url_field["base_url"] = json!("https://evil.example/v1");
    let mut streaming = chat("local-small");
    streaming["stream"] = json!(true);
    let cases = vec![
        json!({"id": "bad_op", "operation": "exec", "request": {"argv": ["sh", "-c", "id"]}}),
        json!({"id": "bad_login", "operation": "login", "request": {}}),
        json!({"id": "no_op", "request": {}}),
        json!({"id": "bad_model", "operation": "complete", "request": chat("not-approved")}),
        json!({"id": "image", "operation": "complete", "request": image}),
        json!({"id": "hosted", "operation": "complete", "request": hosted}),
        json!({"id": "url_field", "operation": "complete", "request": url_field}),
        json!({"id": "stream", "operation": "complete", "request": streaming}),
        json!({"id": "models_args", "operation": "models", "request": {"base_url": "https://evil"}}),
    ];
    let n = cases.len();
    for c in cases {
        push_job(&e, c);
        let out = run_once(&e).await;
        assert_eq!(out.code, 0, "{}", out.stderr);
        let (id, body) = last_result(&e);
        assert_eq!(body["ok"], false, "{id}: {body}");
        let err = body["error"].as_str().unwrap();
        assert!(!err.is_empty() && err.len() <= 200, "{id}: {err}");
    }
    assert_eq!(e.metro.lock().unwrap().results.len(), n);
    let m = e.model.lock().unwrap();
    assert!(
        m.completions.is_empty(),
        "model was called for an unsafe job"
    );
    drop(m);
    assert!(
        last_result(&e).1["error"]
            .as_str()
            .unwrap()
            .contains("invalid request")
    );
    metro_never_saw_key(&e);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn oversized_and_redirected_provider_responses_fail_safely() {
    let e = env().await;
    setup(&e, true).await;
    e.model.lock().unwrap().huge = Some(9 * 1024 * 1024);
    push_job(
        &e,
        json!({"id": "big", "operation": "complete", "request": chat("local-small")}),
    );
    let out = run_once(&e).await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    let body = last_result(&e).1;
    assert_eq!(body["ok"], false);
    assert!(
        body["error"].as_str().unwrap().contains("larger than"),
        "{body}"
    );

    {
        let mut m = e.model.lock().unwrap();
        m.huge = None;
        m.redirect = true;
    }
    push_job(
        &e,
        json!({"id": "redir", "operation": "complete", "request": chat("local-small")}),
    );
    let out = run_once(&e).await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    let body = last_result(&e).1;
    assert_eq!(body["ok"], false);
    assert!(
        body["error"].as_str().unwrap().contains("HTTP 302"),
        "{body}"
    );
    metro_never_saw_key(&e);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn result_retries_never_rerun_the_request() {
    let e = env().await;
    setup(&e, true).await;
    e.metro.lock().unwrap().fail_results = 2;
    push_job(
        &e,
        json!({"id": "dj_r", "operation": "complete", "request": chat("local-small")}),
    );
    let out = run_once(&e).await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(last_result(&e).0, "dj_r");
    assert_eq!(
        e.model.lock().unwrap().completions.len(),
        1,
        "request ran more than once"
    );
    assert!(out.stderr.contains("retrying"), "{}", out.stderr);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn revoked_token_stops_with_clear_message() {
    let e = env().await;
    setup(&e, true).await;
    e.metro
        .lock()
        .unwrap()
        .claim_overrides
        .push_back(StatusCode::UNAUTHORIZED.into_response());
    let out = run_once(&e).await;
    assert_eq!(out.code, 3, "{}", out.stderr);
    assert!(
        out.stderr.contains("rejected the device token"),
        "{}",
        out.stderr
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn metro_redirects_and_huge_claims_are_refused() {
    let e = env().await;
    setup(&e, true).await;
    e.metro.lock().unwrap().claim_overrides.push_back(
        (
            StatusCode::TEMPORARY_REDIRECT,
            [("location", e.model_url.clone() + "/chat/completions")],
        )
            .into_response(),
    );
    let out = run_once(&e).await;
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(
        out.stderr.contains("redirects are not followed"),
        "{}",
        out.stderr
    );

    let huge = json!({"id": "huge", "operation": "complete", "request": {"pad": "x".repeat(5 * 1024 * 1024)}});
    e.metro
        .lock()
        .unwrap()
        .claim_overrides
        .push_back((StatusCode::OK, axum::Json(huge)).into_response());
    let out = run_once(&e).await;
    assert_ne!(out.code, 0);
    let m = e.model.lock().unwrap();
    assert!(
        m.completions.is_empty() && m.seen_auth.is_empty(),
        "the model endpoint was contacted"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cli_refuses_secrets_on_argv_and_bad_profiles() {
    let e = env().await;
    let out = connector(&e.home, &["setup", "--device-token", DEVICE_TOKEN], None).await;
    assert_eq!(out.code, 2);
    assert!(out.stderr.contains("not supported"));
    assert!(!out.stderr.contains(DEVICE_TOKEN));
    let out = connector(&e.home, &["status", "--profile", "../../etc"], None).await;
    assert_ne!(out.code, 0);
    assert!(out.stderr.contains("invalid"));
    // A wrong-looking token is refused and not echoed.
    let out = connector(
        &e.home,
        &[
            "setup",
            "--server-url",
            &e.metro_url,
            "--kind",
            "device-claude-code",
            "--device-token-file",
            "-",
            "--no-login",
            "--model",
            "m",
            "--yes",
        ],
        Some("sk-ant-not-a-device-token-123456\n"),
    )
    .await;
    assert_ne!(out.code, 0);
    assert!(!out.stderr.contains("sk-ant-not"), "{}", out.stderr);
    // Plain http to a non-loopback Metro is refused.
    let out = connector(
        &e.home,
        &[
            "setup",
            "--server-url",
            "http://metro.example.com",
            "--kind",
            "device-claude-code",
            "--device-token-file",
            "-",
            "--no-login",
            "--model",
            "m",
            "--yes",
        ],
        Some(&format!("{DEVICE_TOKEN}\n")),
    )
    .await;
    assert_ne!(out.code, 0);
    assert!(out.stderr.contains("plain http"), "{}", out.stderr);
    let help = connector(&e.home, &["--help"], None).await;
    assert_eq!(help.code, 0);
    assert!(help.stdout.contains("metro-connector setup"));
}

#[tokio::test]
async fn cli_stdin_tolerates_a_child_that_has_already_exited() {
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_metro-connector"))
        .arg("--help")
        .env_clear()
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let stdin = child.stdin.take().unwrap();
    let out = tokio::time::timeout(std::time::Duration::from_secs(60), child.wait_with_output())
        .await
        .expect("connector timed out")
        .unwrap();
    // Waiting first forces the broken pipe rather than racing the child.
    cli_stdin::send_stdin(stdin, Some("unused input\n")).await;
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("metro-connector setup"));
}

// ------------------------------------------------ re-setup and destinations

const DEVICE_TOKEN_B: &str = "metro_pc_SECONDserverTOKEN0123456789abcd";
const PROVIDER_KEY_B: &str = "byok-second-endpoint-key-77";

fn profile_dir(e: &Env) -> PathBuf {
    e.home.join("profiles/work")
}

/// Bytes of every file a re-setup could touch (missing files as None).
fn snapshot(e: &Env) -> Vec<(String, Option<Vec<u8>>)> {
    ["profile.json", "device-token", "provider-key"]
        .iter()
        .map(|f| (f.to_string(), std::fs::read(profile_dir(e).join(f)).ok()))
        .collect()
}

fn edit_profile(e: &Env, f: impl FnOnce(&mut Value)) {
    let path = profile_dir(e).join("profile.json");
    let mut v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    f(&mut v);
    std::fs::write(&path, v.to_string()).unwrap();
}

fn never_saw(seen: &[String], secret: &str) {
    for a in seen {
        assert!(
            !a.contains(secret),
            "old credential reached a new host: {a}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn changed_metro_server_needs_a_new_device_token() {
    let e = env().await;
    setup(&e, true).await;
    let (metro_b, url_b) = spawn_metro(Some(DEVICE_TOKEN_B)).await;
    let before = snapshot(&e);

    // New server, no token supplied: refused even with --yes, and nothing on
    // disk changes.
    let out = connector(
        &e.home,
        &[
            "setup",
            "--profile",
            "work",
            "--server-url",
            &url_b,
            "--yes",
        ],
        None,
    )
    .await;
    assert_ne!(out.code, 0, "{}", out.stderr);
    assert_no_secrets(&out);
    assert!(
        out.stderr.contains(&e.metro_url) && out.stderr.contains(&url_b),
        "{}",
        out.stderr
    );
    assert!(out.stderr.contains("device token"), "{}", out.stderr);
    assert_eq!(
        snapshot(&e),
        before,
        "a refused re-setup changed the profile"
    );
    assert_eq!(metro_b.lock().unwrap().claims, 0);

    // The old profile still works against the old server.
    push_job(
        &e,
        json!({"id": "still_a", "operation": "models", "request": {}}),
    );
    assert_eq!(run_once(&e).await.code, 0);
    assert_eq!(last_result(&e).0, "still_a");

    // With the new server's token it succeeds, and the new server only ever
    // sees the new token.
    let out = connector(
        &e.home,
        &[
            "setup",
            "--profile",
            "work",
            "--server-url",
            &url_b,
            "--device-token-file",
            "-",
            "--yes",
        ],
        Some(&format!("{DEVICE_TOKEN_B}\n")),
    )
    .await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(!out.stderr.contains(DEVICE_TOKEN_B));
    metro_b
        .lock()
        .unwrap()
        .jobs
        .push_back(json!({"id": "on_b", "operation": "models", "request": {}}));
    let out = run_once(&e).await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    let b = metro_b.lock().unwrap();
    assert_eq!(b.results.last().unwrap().0, "on_b");
    assert!(
        b.seen_auth
            .iter()
            .all(|a| a == &format!("Bearer {DEVICE_TOKEN_B}"))
    );
    never_saw(&b.seen_auth, DEVICE_TOKEN);
    never_saw(&b.seen_bodies, DEVICE_TOKEN);
    never_saw(&b.seen_bodies, PROVIDER_KEY);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn same_metro_server_keeps_the_saved_token() {
    let e = env().await;
    setup(&e, true).await;
    let token_before = std::fs::read(profile_dir(&e).join("device-token")).unwrap();
    let key_before = std::fs::read(profile_dir(&e).join("provider-key")).unwrap();
    // Same server (spelled with a trailing slash: compared normalized) and
    // same endpoint: kept without asking for anything.
    let url = format!("{}/", e.metro_url);
    let out = connector(
        &e.home,
        &["setup", "--profile", "work", "--server-url", &url, "--yes"],
        None,
    )
    .await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(
        std::fs::read(profile_dir(&e).join("device-token")).unwrap(),
        token_before
    );
    assert_eq!(
        std::fs::read(profile_dir(&e).join("provider-key")).unwrap(),
        key_before
    );
    push_job(
        &e,
        json!({"id": "kept", "operation": "complete", "request": chat("local-small")}),
    );
    assert_eq!(run_once(&e).await.code, 0);
    assert_eq!(last_result(&e).1["ok"], true);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn changed_model_endpoint_needs_a_new_key_or_no_key() {
    let e = env().await;
    setup(&e, true).await;
    let (model_b, url_b) = spawn_model().await;
    let before = snapshot(&e);

    let out = connector(
        &e.home,
        &["setup", "--profile", "work", "--base-url", &url_b, "--yes"],
        None,
    )
    .await;
    assert_ne!(out.code, 0, "{}", out.stderr);
    assert!(
        out.stderr.contains(&e.model_url) && out.stderr.contains(&url_b),
        "{}",
        out.stderr
    );
    assert_eq!(
        snapshot(&e),
        before,
        "a refused re-setup changed the profile"
    );

    // Explicitly keyless: accepted, the old key is deleted, and B gets no
    // Authorization header at all.
    let out = connector(
        &e.home,
        &[
            "setup",
            "--profile",
            "work",
            "--base-url",
            &url_b,
            "--no-api-key",
            "--yes",
        ],
        None,
    )
    .await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(!profile_dir(&e).join("provider-key").exists());
    push_job(
        &e,
        json!({"id": "b1", "operation": "complete", "request": chat("local-small")}),
    );
    assert_eq!(run_once(&e).await.code, 0);
    assert_eq!(last_result(&e).1["ok"], true);
    assert_eq!(model_b.lock().unwrap().seen_auth, vec![None]);

    // A fresh key for B: only that key is ever sent to B.
    let kf = e.home.parent().unwrap().join("key-b.txt");
    write_private(&kf, PROVIDER_KEY_B);
    let out = connector(
        &e.home,
        &[
            "setup",
            "--profile",
            "work",
            "--api-key-file",
            kf.to_str().unwrap(),
            "--yes",
        ],
        None,
    )
    .await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(!out.stderr.contains(PROVIDER_KEY_B));
    push_job(
        &e,
        json!({"id": "b2", "operation": "complete", "request": chat("local-small")}),
    );
    assert_eq!(run_once(&e).await.code, 0);
    let m = model_b.lock().unwrap();
    assert_eq!(
        m.seen_auth.last().unwrap().as_deref(),
        Some(&format!("Bearer {PROVIDER_KEY_B}")[..])
    );
    let seen: Vec<String> = m.seen_auth.iter().flatten().cloned().collect();
    never_saw(&seen, PROVIDER_KEY);
    drop(m);
    metro_never_saw_key(&e);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn secrets_are_bound_to_their_destination_even_after_partial_writes() {
    let e = env().await;
    setup(&e, true).await;
    let (metro_b, url_b) = spawn_metro(None).await;
    let (model_b, model_url_b) = spawn_model().await;

    // profile.json points at a new Metro server while the token on disk is
    // still the old server's (as if a write had stopped half-way): refused,
    // and the new server is never contacted.
    edit_profile(&e, |v| v["server_url"] = json!(url_b));
    let out = run_once(&e).await;
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(out.stderr.contains("belongs to"), "{}", out.stderr);
    assert_eq!(metro_b.lock().unwrap().claims, 0);
    let st = connector(&e.home, &["status", "--profile", "work"], None).await;
    assert!(st.stdout.contains("different server"), "{}", st.stdout);
    edit_profile(&e, |v| v["server_url"] = json!(e.metro_url));

    // Same for the model endpoint: the job fails, and the new endpoint never
    // gets a request (with or without the old key).
    edit_profile(&e, |v| v["base_url"] = json!(model_url_b));
    push_job(
        &e,
        json!({"id": "mismatch", "operation": "complete", "request": chat("local-small")}),
    );
    assert_eq!(run_once(&e).await.code, 0);
    let (_, body) = last_result(&e);
    assert_eq!(body["ok"], false);
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("different endpoint"),
        "{body}"
    );
    let m = model_b.lock().unwrap();
    assert!(m.seen_auth.is_empty() && m.completions.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn slow_provider_is_cut_off_and_the_failure_is_delivered() {
    let e = env().await;
    setup(&e, true).await;
    edit_profile(&e, |v| v["request_timeout_secs"] = json!(5));
    e.model.lock().unwrap().delay = Some(std::time::Duration::from_secs(30));
    push_job(
        &e,
        json!({"id": "slow", "operation": "complete", "request": chat("local-small")}),
    );
    let started = std::time::Instant::now();
    let out = run_once(&e).await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(15),
        "{:?}",
        started.elapsed()
    );
    let (id, body) = last_result(&e);
    assert_eq!(id, "slow");
    assert_eq!(body["ok"], false);
    assert!(
        body["error"].as_str().unwrap().contains("in time"),
        "{body}"
    );
}
