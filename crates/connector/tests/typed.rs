//! End-to-end tests of the real `metro-connector` binary for the typed
//! kinds (`device-systemone`, `device-embeddings`) against a fake Metro and
//! a fake System One / embeddings server on loopback. No real vendor.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path as UrlPath, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde_json::{Value, json};

const DEVICE_TOKEN: &str = "metro_pc_TYPEDdeviceTOKEN0123456789abcdef";
const PROVIDER_KEY: &str = "typesafe-test-key-DO-NOT-LEAK-7";

#[derive(Default)]
struct MetroState {
    jobs: VecDeque<Value>,
    results: Vec<(String, Value)>,
    seen: Vec<String>,
    chat: bool,
}
type Metro = Arc<Mutex<MetroState>>;

fn auth(h: &HeaderMap) -> Option<String> {
    h.get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

async fn claim(State(s): State<Metro>, h: HeaderMap, body: Bytes) -> Response {
    let mut s = s.lock().unwrap();
    s.seen.push(auth(&h).unwrap_or_default());
    if auth(&h).as_deref() != Some(&format!("Bearer {DEVICE_TOKEN}")) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let expected: &[u8] = if s.chat {
        br#"{"approved_models":["jev-latest"]}"#
    } else {
        b"{}"
    };
    if body.as_ref() != expected {
        return StatusCode::UNPROCESSABLE_ENTITY.into_response();
    }
    match s.jobs.pop_front() {
        Some(j) => (StatusCode::OK, axum::Json(j)).into_response(),
        None => StatusCode::NO_CONTENT.into_response(),
    }
}

async fn result(State(s): State<Metro>, UrlPath(id): UrlPath<String>, body: Bytes) -> Response {
    let mut s = s.lock().unwrap();
    s.seen.push(String::from_utf8_lossy(&body).into());
    s.results.push((id, serde_json::from_slice(&body).unwrap()));
    StatusCode::NO_CONTENT.into_response()
}

#[derive(Default)]
struct ModelState {
    /// (path, Authorization, body)
    calls: Vec<(String, Option<String>, Value)>,
}
type Model = Arc<Mutex<ModelState>>;

async fn systemone(State(s): State<Model>, h: HeaderMap, body: Bytes) -> Response {
    let req: Value = serde_json::from_slice(&body).unwrap();
    s.lock()
        .unwrap()
        .calls
        .push(("systemone".into(), auth(&h), req.clone()));
    // A TypeSafe-shaped answer plus fields the connector must drop.
    axum::Json(json!({
        "id": "shim-1",
        "model": "jev-latest",
        "debug": {"echo": PROVIDER_KEY},
        "answers": {"billing": {"type": "noul", "noul": 0.93, "trace": "x"}},
        "usage": {"input_tokens": 9, "output_tokens": 0}
    }))
    .into_response()
}

async fn embeddings(State(s): State<Model>, h: HeaderMap, body: Bytes) -> Response {
    let req: Value = serde_json::from_slice(&body).unwrap();
    s.lock()
        .unwrap()
        .calls
        .push(("embeddings".into(), auth(&h), req.clone()));
    let n = req["input"].as_array().unwrap().len();
    let data: Vec<Value> = (0..n)
        .rev()
        .map(|i| json!({"object": "embedding", "index": i, "embedding": [0.5, i as f64 + 1.0]}))
        .collect();
    axum::Json(json!({"object": "list", "model": "nomic-embed", "data": data})).into_response()
}

async fn models(State(s): State<Model>, h: HeaderMap) -> Response {
    s.lock()
        .unwrap()
        .calls
        .push(("models".into(), auth(&h), Value::Null));
    axum::Json(json!({
        "models": [{"name": "jev-latest", "description": "d", "release_date": "2026-09-15"}],
        "data": [{"id": "nomic-embed"}]
    }))
    .into_response()
}

async fn chat(State(s): State<Model>, body: Bytes) -> Response {
    s.lock().unwrap().calls.push((
        "chat".into(),
        None,
        serde_json::from_slice(&body).unwrap_or(Value::Null),
    ));
    StatusCode::INTERNAL_SERVER_ERROR.into_response()
}

async fn serve(app: Router) -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    format!("http://127.0.0.1:{}", addr.port())
}

struct Env {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    metro: Metro,
    model: Model,
    metro_url: String,
    model_url: String,
}

async fn env() -> Env {
    let metro: Metro = Default::default();
    let metro_url = serve(
        Router::new()
            .route("/v1/device/claim", post(claim))
            .route("/v1/device/jobs/{id}/result", post(result))
            .with_state(metro.clone()),
    )
    .await;
    let model: Model = Default::default();
    let model_url = serve(
        Router::new()
            .route("/v1/systemone", post(systemone))
            .route("/v1/embeddings", post(embeddings))
            .route("/v1/models", get(models))
            .route("/v1/chat/completions", post(chat))
            .with_state(model.clone()),
    )
    .await;
    let tmp = tempfile::tempdir().unwrap();
    Env {
        home: tmp.path().join("connector"),
        metro,
        model,
        metro_url,
        model_url: format!("{model_url}/v1"),
        _tmp: tmp,
    }
}

async fn connector(home: &Path, args: &[&str], stdin: Option<&str>) -> (i32, String) {
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_metro-connector"))
        .args(args)
        .env_clear()
        .env("METRO_CONNECTOR_HOME", home)
        .env("HOME", home.parent().unwrap())
        .env("METRO_CONNECTOR_POLL_MS", "50")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    {
        use tokio::io::AsyncWriteExt;
        let mut si = child.stdin.take().unwrap();
        if let Some(s) = stdin {
            si.write_all(s.as_bytes()).await.unwrap();
        }
    }
    let out = tokio::time::timeout(std::time::Duration::from_secs(60), child.wait_with_output())
        .await
        .expect("connector timed out")
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    for s in [DEVICE_TOKEN, PROVIDER_KEY] {
        assert!(!text.contains(s), "secret in output: {text}");
    }
    (out.status.code().unwrap_or(-1), text)
}

async fn setup(e: &Env, kind: &str, model: &str, with_key: bool) {
    let key_file = e.home.parent().unwrap().join("key.txt");
    std::fs::write(&key_file, format!("{PROVIDER_KEY}\n")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&key_file, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let kf = key_file.to_str().unwrap().to_string();
    let mut args = vec![
        "setup",
        "--profile",
        "typed",
        "--server-url",
        &e.metro_url,
        "--kind",
        kind,
        "--base-url",
        &e.model_url,
        "--device-token-file",
        "-",
        "--model",
        model,
        "--yes",
    ];
    if with_key {
        args.extend(["--api-key-file", &kf]);
    } else {
        args.push("--no-api-key");
    }
    let (code, out) = connector(&e.home, &args, Some(&format!("{DEVICE_TOKEN}\n"))).await;
    assert_eq!(code, 0, "{out}");
}

/// Push `job`, run the connector once, return the posted result body.
async fn run_job(e: &Env, job: Value) -> Value {
    e.metro.lock().unwrap().jobs.push_back(job);
    let (code, out) = connector(&e.home, &["run", "--profile", "typed", "--once"], None).await;
    assert_eq!(code, 0, "{out}");
    e.metro.lock().unwrap().results.last().unwrap().1.clone()
}

fn decide_request(model: &str) -> Value {
    json!({
        "model": model,
        "state": "I was charged twice.",
        "questions": {"billing": {"type": "noul", "instructions": "Is this about billing?"}}
    })
}

fn metro_never_saw_key(e: &Env) {
    for s in &e.metro.lock().unwrap().seen {
        assert!(!s.contains(PROVIDER_KEY), "provider key reached Metro: {s}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn systemone_profile_runs_only_decide() {
    let e = env().await;
    setup(&e, "device-systemone", "jev-latest", true).await;
    let cfg: Value =
        serde_json::from_slice(&std::fs::read(e.home.join("profiles/typed/profile.json")).unwrap())
            .unwrap();
    assert_eq!(cfg["kind"], "device-systemone");
    assert_eq!(cfg["base_url"], e.model_url);

    let body = run_job(
        &e,
        json!({"id": "d1", "operation": "decide", "request": decide_request("jev-latest")}),
    )
    .await;
    assert_eq!(body["ok"], true, "{body}");
    assert_eq!(
        body["response"],
        json!({"model": "jev-latest",
               "answers": {"billing": {"type": "noul", "noul": 0.93}},
               "usage": {"input_tokens": 9, "output_tokens": 0}})
    );
    {
        let m = e.model.lock().unwrap();
        assert_eq!(m.calls.len(), 1);
        assert_eq!(m.calls[0].0, "systemone");
        assert_eq!(
            m.calls[0].1.as_deref(),
            Some(format!("Bearer {PROVIDER_KEY}").as_str())
        );
        assert_eq!(m.calls[0].2, decide_request("jev-latest"));
    }

    let models = run_job(
        &e,
        json!({"id": "m1", "operation": "models", "request": {}}),
    )
    .await;
    assert_eq!(models["response"], json!({"models": ["jev-latest"]}));

    let mut boxq = decide_request("jev-latest");
    boxq["questions"]["where"] = json!({"type": "box", "instructions": "find it"});
    let mut assets = decide_request("jev-latest");
    assets["assets"] = json!({"img": {"type": "image", "format": "png", "data_base64": "AA=="}});
    let mut two = decide_request("jev-latest");
    two["questions"]["tone"] = json!({"type": "choice", "instructions": "Tone?", "criteria": {"calm": null, "angry": null}});
    let refused = [
        json!({"id": "r6", "operation": "decide", "request": two}),
        json!({"id": "r1", "operation": "decide", "request": decide_request("jev-unapproved")}),
        json!({"id": "r2", "operation": "decide", "request": boxq}),
        json!({"id": "r3", "operation": "decide", "request": assets}),
        json!({"id": "r4", "operation": "embed", "request": {"model": "jev-latest", "input": ["x"]}}),
        json!({"id": "r5", "operation": "complete", "request": {"model": "jev-latest", "messages": [{"role": "user", "content": "hi"}]}}),
    ];
    let calls_before = e.model.lock().unwrap().calls.len();
    for job in refused {
        let id = job["id"].clone();
        let body = run_job(&e, job).await;
        assert_eq!(body["ok"], false, "{id}: {body}");
        let err = body["error"].as_str().unwrap();
        assert!(
            !err.is_empty() && err.len() <= 200 && !err.contains("127.0.0.1"),
            "{id}: {err}"
        );
    }
    assert_eq!(
        e.model.lock().unwrap().calls.len(),
        calls_before,
        "the endpoint was called for a refused job"
    );
    metro_never_saw_key(&e);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn embeddings_profile_runs_only_embed_without_a_key() {
    let e = env().await;
    setup(&e, "device-embeddings", "nomic-embed", false).await;
    let body = run_job(
        &e,
        json!({"id": "e1", "operation": "embed", "request": {"model": "nomic-embed", "input": ["a", "b"]}}),
    )
    .await;
    assert_eq!(body["ok"], true, "{body}");
    assert_eq!(body["response"]["data"][0]["index"], 0);
    assert_eq!(body["response"]["data"][0]["embedding"], json!([0.5, 1.0]));
    assert_eq!(body["response"]["data"][1]["embedding"], json!([0.5, 2.0]));
    {
        let m = e.model.lock().unwrap();
        assert_eq!(m.calls[0].0, "embeddings");
        assert_eq!(m.calls[0].1, None, "no key: no Authorization header");
    }
    let body = run_job(
        &e,
        json!({"id": "e2", "operation": "decide", "request": decide_request("nomic-embed")}),
    )
    .await;
    assert_eq!(body["ok"], false);
    assert!(
        body["error"].as_str().unwrap().contains("embeddings only"),
        "{body}"
    );
    assert_eq!(e.model.lock().unwrap().calls.len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chat_profiles_refuse_typed_jobs() {
    let e = env().await;
    e.metro.lock().unwrap().chat = true;
    setup(&e, "device-openai-compatible", "jev-latest", true).await;
    for job in [
        json!({"id": "c1", "operation": "decide", "request": decide_request("jev-latest")}),
        json!({"id": "c2", "operation": "embed", "request": {"model": "jev-latest", "input": ["x"]}}),
    ] {
        let body = run_job(&e, job).await;
        assert_eq!(body["ok"], false, "{body}");
        assert!(
            body["error"]
                .as_str()
                .unwrap()
                .contains("chat completions only"),
            "{body}"
        );
    }
    assert!(e.model.lock().unwrap().calls.is_empty());
}
