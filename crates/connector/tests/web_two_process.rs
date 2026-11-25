//! Consent and credentials across real processes: one `metro-connector web
//! run` is serving a search while a second `metro-connector` process turns
//! web search off, or deletes a provider key. The first provider is paused
//! at that moment and then answers 429; what happens next must follow the
//! files as they are now. Everything is loopback: a fake Metro and fake
//! vendors in this test.

use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, Uri};
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
    /// The Authorization header of each Firecrawl request.
    firecrawl_auth: Vec<Option<String>>,
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
    // Paused until the test has changed something in another process, then
    // limited (one waiter: a stored permit means no wake-up is missed).
    c.release.notified().await;
    (StatusCode::TOO_MANY_REQUESTS, axum::Json(json!({}))).into_response()
}

async fn firecrawl(State(c): State<Ctx>, h: HeaderMap) -> Response {
    let mut f = c.fake.lock().unwrap();
    f.firecrawl_hits += 1;
    f.firecrawl_auth.push(
        h.get("authorization")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned),
    );
    drop(f);
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

fn checked_stderr(mut c: Command, expected: i32, private: &[&str]) -> String {
    let out = c.stderr(Stdio::piped()).output().unwrap();
    let stderr = String::from_utf8(out.stderr).unwrap();
    for value in private {
        assert!(!stderr.contains(value), "a credential was logged");
    }
    assert_eq!(out.status.code(), Some(expected));
    stderr
}

#[test]
fn web_setup_and_status_accept_canonical_http_metro_bindings() {
    let t = tempfile::tempdir().unwrap();
    let token_file = t.path().join("device-token");
    metro_connector::fsutil::write_private(&token_file, TOKEN.as_bytes()).unwrap();
    for (raw, canonical, warns) in [
        (
            " HTTP://100.101.102.103:8787/metro/// ",
            "http://100.101.102.103:8787/metro",
            false,
        ),
        (
            "http://[FD7A:115C:A1E0::9]:8787/",
            "http://[fd7a:115c:a1e0::9]:8787",
            false,
        ),
        (
            "http://Metro.Fixture.ts.net:80/",
            "http://metro.fixture.ts.net",
            false,
        ),
        ("http://192.0.2.9:8787/", "http://192.0.2.9:8787", true),
        ("http://127.0.0.1:8787/", "http://127.0.0.1:8787", false),
        ("https://metro.invalid/", "https://metro.invalid", false),
    ] {
        // Device-token setup writes the local binding without contacting the URL.
        let home = tempfile::tempdir_in(t.path()).unwrap();
        let metro_cmd = |args: &[&str]| cmd(home.path(), "http://127.0.0.1:1", args);
        let setup = checked_stderr(
            metro_cmd(&[
                "web",
                "setup",
                "--server-url",
                raw,
                "--device-token-file",
                token_file.to_str().unwrap(),
            ]),
            0,
            &[TOKEN],
        );
        assert_eq!(
            setup.matches("metro-connector: warning:").count(),
            usize::from(warns)
        );
        if warns {
            assert!(setup.contains("isn't encrypted") && setup.contains("web device token"));
        }
        let status = checked_stderr(metro_cmd(&["web", "status"]), 0, &[TOKEN]);
        assert!(
            status
                .lines()
                .any(|line| line == format!("server: {canonical}"))
        );
        assert!(!status.contains("metro-connector: warning:"));
    }
}

#[test]
fn invalid_web_server_url_does_not_print_embedded_credentials() {
    let t = tempfile::tempdir().unwrap();
    let canary = "synthetic-password-canary";
    let malformed = format!("http://user:{canary}@[broken");
    let bad = checked_stderr(
        cmd(
            t.path(),
            "http://127.0.0.1:1",
            &["web", "setup", "--server-url", &malformed],
        ),
        1,
        &[canary],
    );
    assert!(bad.contains("--server-url"));
}

#[tokio::test]
async fn web_session_setup_and_restart_use_the_saved_http_metro_server() {
    // The reserved hostname is never resolved: this loopback proxy is the
    // entire fixture. Only origin-specific, authenticated requests succeed.
    let app = Router::new()
        .route("/metro/v1/me", axum::routing::get(me))
        .route("/metro/v1/web-devices", post(create))
        .route(
            "/metro/v1/device/claim",
            post(|uri: Uri, h: HeaderMap| async move {
                if uri.host() == Some("metro.invalid")
                    && h.get("authorization").and_then(|v| v.to_str().ok())
                        == Some(&format!("Bearer {TOKEN}"))
                {
                    StatusCode::NO_CONTENT
                } else {
                    StatusCode::UNAUTHORIZED
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    tokio::task::spawn_blocking(move || {
        let t = tempfile::tempdir().unwrap();
        let session = t.path().join("session");
        metro_connector::fsutil::write_private(&session, b"tok-alice\n").unwrap();
        let metro_cmd = |args: &[&str]| {
            let mut c = cmd(t.path(), &proxy, args);
            c.env("HTTP_PROXY", &proxy);
            c
        };
        let setup = checked_stderr(
            metro_cmd(&[
                "web",
                "setup",
                "--server-url",
                "http://METRO.invalid:80/metro/",
                "--session-token-file",
                session.to_str().unwrap(),
            ]),
            0,
            &[TOKEN, "tok-alice"],
        );
        assert_eq!(setup.matches("metro-connector: warning:").count(), 1);
        assert!(setup.contains("isn't encrypted") && setup.contains("Metro session token"));
        std::fs::remove_file(session).unwrap();
        let run = checked_stderr(
            metro_cmd(&["web", "run", "--once"]),
            0,
            &[TOKEN, "tok-alice"],
        );
        assert_eq!(run.matches("metro-connector: warning:").count(), 1);
        assert!(run.contains("isn't encrypted") && run.contains("web device token"));
        assert!(run.contains("serving web search for http://metro.invalid/metro "));
    })
    .await
    .unwrap();
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

const FIRECRAWL_KEY: &str = "fc-two-process-key-123456";

struct Harness {
    ctx: Ctx,
    base: String,
    home: std::path::PathBuf,
    _t: tempfile::TempDir,
}

impl Harness {
    async fn start(firecrawl_key: bool) -> Self {
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
        let key = home.join("fc-key");
        metro_connector::fsutil::write_private(&key, format!("{FIRECRAWL_KEY}\n").as_bytes())
            .unwrap();
        let (h, b) = (home.clone(), base.clone());
        let (s, k) = (
            session.to_string_lossy().into_owned(),
            key.to_string_lossy().into_owned(),
        );
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
            if firecrawl_key {
                run_ok(
                    cmd(
                        &h,
                        &b,
                        &["web", "key", "set", "firecrawl", "--from-file", &k],
                    ),
                    "key set",
                );
            }
        })
        .await
        .unwrap();
        Self {
            ctx,
            base,
            home,
            _t: t,
        }
    }

    fn worker(&self) -> Kill {
        Kill(
            cmd(&self.home, &self.base, &["web", "run"])
                .spawn()
                .unwrap(),
        )
    }

    fn queue(&self, id: &str) {
        self.ctx.fake.lock().unwrap().job = Some(json!({
            "id": id, "operation": "web_search", "request": {"v": 1, "query": "q"}
        }));
    }

    /// Another `metro-connector` process, run to completion.
    async fn other_process(&self, args: &'static [&'static str]) {
        let (h, b) = (self.home.clone(), self.base.clone());
        tokio::task::spawn_blocking(move || run_ok(cmd(&h, &b, args), "other process"))
            .await
            .unwrap();
    }

    async fn result(&self, n: usize) -> Value {
        let f = &self.ctx.fake;
        wait("a job result", || f.lock().unwrap().results.len() >= n).await;
        f.lock().unwrap().results[n - 1].clone()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_disable_from_another_process_stops_rotation_before_the_next_provider() {
    let h = Harness::start(false).await;
    let _worker = h.worker();
    h.queue("job-1");
    wait("the first provider is contacted", || {
        h.ctx.fake.lock().unwrap().tavily_hits == 1
    })
    .await;
    // Process 2: the person turns web search off; it returns first.
    h.other_process(&["web", "disable"]).await;
    // Only now does the first provider answer (429).
    h.ctx.release.notify_one();
    assert_eq!(
        h.result(1).await,
        json!({"ok": false, "error": "web search is turned off on this computer"})
    );
    assert_eq!(
        h.ctx.fake.lock().unwrap().firecrawl_hits,
        0,
        "the second provider was contacted after disable"
    );
    // The first provider's 429 was kept although the job ended early: after
    // turning web search on again it is still cooling down.
    h.other_process(&["web", "enable"]).await;
    h.queue("job-2");
    let r = h.result(2).await;
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(r["response"]["attempts"][0]["provider"], "tavily");
    assert_eq!(r["response"]["attempts"][0]["outcome"], "skipped_cooldown");
    assert_eq!(r["response"]["provider"], "firecrawl");
    assert_eq!(
        h.ctx.fake.lock().unwrap().tavily_hits,
        1,
        "contacted during its cooldown"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_key_deleted_by_another_process_during_rotation_is_not_used() {
    let h = Harness::start(true).await;
    let _worker = h.worker();
    h.queue("job-1");
    wait("the first provider is contacted", || {
        h.ctx.fake.lock().unwrap().tavily_hits == 1
    })
    .await;
    // Process 2: the person deletes the Firecrawl key; it returns first.
    h.other_process(&["web", "key", "delete", "firecrawl"])
        .await;
    h.ctx.release.notify_one();
    let r = h.result(1).await;
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(r["response"]["provider"], "firecrawl");
    assert_eq!(
        r["response"]["mode"], "keyless",
        "the credential actually used"
    );
    assert_eq!(r["response"]["attempts"][1]["mode"], "keyless");
    assert_eq!(
        h.ctx.fake.lock().unwrap().firecrawl_auth,
        vec![None],
        "the deleted key was sent"
    );
}
