//! Consent across real processes: one `metro-connector web run` is serving
//! a search while a second `metro-connector web disable` turns web search
//! off. The first provider is paused at that moment and then answers 429;
//! the second provider must receive nothing, and Metro gets a refusal.
//! Everything is loopback: a fake Metro and fake vendors in this test.

use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde_json::{Value, json};
use tokio::sync::Notify;

#[derive(Default)]
struct Fake {
    job: Option<Value>,
    results: Vec<Value>,
    tavily_hits: u32,
    firecrawl_hits: u32,
}

#[derive(Clone)]
struct Ctx {
    fake: Arc<Mutex<Fake>>,
    release: Arc<Notify>,
}

const TOKEN: &str = "metro_pc_00000000000000000000000000000000000000000000000000000000000000aa";

async fn me(h: HeaderMap) -> Response {
    match h.get("authorization").and_then(|v| v.to_str().ok()) {
        Some("Bearer tok-alice") => axum::Json(json!({"id": "alice"})).into_response(),
        _ => StatusCode::UNAUTHORIZED.into_response(),
    }
}

async fn create() -> Response {
    (
        StatusCode::CREATED,
        axum::Json(json!({"id": format!("wd_{:032x}", 1), "token": TOKEN})),
    )
        .into_response()
}

async fn claim(State(c): State<Ctx>, h: HeaderMap) -> Response {
    if h.get("authorization").and_then(|v| v.to_str().ok()) != Some(&format!("Bearer {TOKEN}")) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match c.fake.lock().unwrap().job.take() {
        Some(j) => axum::Json(j).into_response(),
        None => StatusCode::NO_CONTENT.into_response(),
    }
}

async fn result(State(c): State<Ctx>, Path(_id): Path<String>, body: String) -> StatusCode {
    c.fake
        .lock()
        .unwrap()
        .results
        .push(serde_json::from_str(&body).unwrap());
    StatusCode::NO_CONTENT
}

async fn tavily(State(c): State<Ctx>) -> Response {
    c.fake.lock().unwrap().tavily_hits += 1;
    // Paused until the test has turned web search off, then limited.
    c.release.notified().await;
    (StatusCode::TOO_MANY_REQUESTS, axum::Json(json!({}))).into_response()
}

async fn firecrawl(State(c): State<Ctx>) -> Response {
    c.fake.lock().unwrap().firecrawl_hits += 1;
    axum::Json(json!({"data": {"web": [{"title": "t", "url": "https://f.example/"}]}}))
        .into_response()
}

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_metro-connector")
}

fn cmd(home: &std::path::Path, base: &str, args: &[&str]) -> Command {
    let mut c = Command::new(bin());
    c.args(args)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("METRO_CONNECTOR_HOME", home.join("connector"))
        .env("METRO_CONNECTOR_POLL_MS", "50")
        .env("METRO_WEB_TEST_BASE_TAVILY", format!("{base}/tavily"))
        .env("METRO_WEB_TEST_BASE_FIRECRAWL", format!("{base}/firecrawl"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    c
}

fn run_ok(mut c: Command, what: &str) {
    let status = c.status().unwrap();
    assert!(status.success(), "{what}: {status}");
}

struct Kill(Child);

impl Drop for Kill {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn wait(what: &str, mut f: impl FnMut() -> bool) {
    let t = Instant::now();
    while !f() {
        assert!(t.elapsed() < Duration::from_secs(20), "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_disable_from_another_process_stops_rotation_before_the_next_provider() {
    let ctx = Ctx {
        fake: Arc::default(),
        release: Arc::new(Notify::new()),
    };
    let app = Router::new()
        .route("/v1/me", axum::routing::get(me))
        .route("/v1/web-devices", post(create))
        .route("/v1/device/claim", post(claim))
        .route("/v1/device/jobs/{id}/result", post(result))
        .route("/tavily/search", post(tavily))
        .route("/firecrawl/search", post(firecrawl))
        .with_state(ctx.clone());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });

    let t = tempfile::tempdir().unwrap();
    let home = t.path().to_path_buf();
    let session = home.join("session");
    metro_connector::fsutil::write_private(&session, b"tok-alice\n").unwrap();
    let s = session.to_string_lossy().into_owned();
    let (h, b) = (home.clone(), base.clone());
    tokio::task::spawn_blocking(move || {
        run_ok(
            cmd(
                &h,
                &b,
                &[
                    "web",
                    "setup",
                    "--server-url",
                    &b,
                    "--session-token-file",
                    &s,
                ],
            ),
            "setup",
        );
        run_ok(cmd(&h, &b, &["web", "order", "tavily,firecrawl"]), "order");
    })
    .await
    .unwrap();

    // Process 1: the worker.
    let _worker = Kill(cmd(&home, &base, &["web", "run"]).spawn().unwrap());
    ctx.fake.lock().unwrap().job = Some(json!({
        "id": "job-1", "operation": "web_search", "request": {"v": 1, "query": "q"}
    }));
    wait("the first provider is contacted", || {
        ctx.fake.lock().unwrap().tavily_hits == 1
    })
    .await;

    // Process 2: the person turns web search off; it returns first.
    let (h, b) = (home.clone(), base.clone());
    tokio::task::spawn_blocking(move || run_ok(cmd(&h, &b, &["web", "disable"]), "disable"))
        .await
        .unwrap();
    // Only now does the first provider answer (429).
    ctx.release.notify_waiters();
    wait("the job's result", || {
        !ctx.fake.lock().unwrap().results.is_empty()
    })
    .await;
    let f = ctx.fake.lock().unwrap();
    assert_eq!(
        f.results[0],
        json!({"ok": false, "error": "web search is turned off on this computer"})
    );
    assert_eq!(
        f.firecrawl_hits, 0,
        "the second provider was contacted after disable"
    );
}
