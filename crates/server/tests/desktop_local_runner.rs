//! Test runner end to end through the desktop's real local wiring: the
//! launcher's `LocalServer` starts this crate's real `metro-server` binary
//! (Unix: inherited fd; Windows: the socket description on stdin), the real
//! bridge binds the session, and `DesktopTools` serves the Space either with
//! its per-Space local tools or with this computer's installed (registered)
//! runner. Each case presses Test runner, restarts the app on the same data
//! and presses it again. No model is involved.
//!
//! Native Windows CI runs this file (release.yml, desktop-windows), because
//! that is where "Use locally" runs for most people.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use metro_connector::desktop::bridge::{self, Bridge};
use metro_connector::desktop::core::{Desktop, DesktopConfig};
use metro_connector::desktop::local::LocalWorkspaceHost;
use metro_connector::desktop_local::LocalServer;
use metro_connector::desktop_tools::DesktopTools;
use metro_connector::desktop_tools::runner::Marker;
use serde_json::{Value, json};

const SERVER: &str = env!("CARGO_BIN_EXE_metro-server");

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap()
}

async fn call(
    base: &str,
    bearer: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let m = reqwest::Method::from_bytes(method.as_bytes()).unwrap();
    let mut rb = http()
        .request(m, format!("{base}{path}"))
        .bearer_auth(bearer);
    if let Some(b) = body {
        rb = rb
            .header("content-type", "application/json")
            .body(serde_json::to_vec(&b).unwrap());
    }
    let r = rb
        .send()
        .await
        .unwrap_or_else(|e| panic!("{method} {path}: {e}"));
    let status = r.status().as_u16();
    let text = r.text().await.unwrap();
    (status, serde_json::from_str(&text).unwrap_or(Value::Null))
}

/// One launch of the app: the local server, the bridge and its bound session.
struct App {
    desktop: Arc<Desktop>,
    bridge: Bridge,
    url: String,
    token: String,
}

impl App {
    async fn launch(root: &Path, marker: Marker) -> App {
        let data = root.join("local");
        std::fs::create_dir_all(&data).unwrap();
        let local: Arc<dyn LocalWorkspaceHost> = Arc::new(LocalServer::new(SERVER.into(), data));
        let mut cfg = DesktopConfig::new(root.to_path_buf(), true, Some(local));
        cfg.open_browser = Arc::new(|_| Ok(()));
        cfg.tools = Some(Arc::new(
            DesktopTools::new(root.join("local-tools")).with_local_runner(marker),
        ));
        let desktop = Desktop::new(cfg);
        let bridge = bridge::start(desktop.clone()).await.unwrap();
        let mut app = App {
            desktop,
            bridge,
            url: String::new(),
            token: String::new(),
        };
        let (s, w) = app
            .ui("POST", "/v1/local-workspace/open", Some(json!({})))
            .await;
        assert_eq!(s, 200, "open the local workspace: {}", w["error"]);
        app.url = w["server_url"].as_str().unwrap().to_owned();
        app.token = w["token"].as_str().unwrap().to_owned();
        let body = json!({"server_url": app.url, "session_token": app.token});
        let (s, v) = app.ui("POST", "/v1/session", Some(body)).await;
        assert_eq!(s, 200, "bind the local session: {}", v["error"]);
        app
    }

    async fn ui(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        call(
            &self.bridge.url,
            self.bridge.token.expose(),
            method,
            path,
            body,
        )
        .await
    }

    async fn server(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        call(&self.url, &self.token, method, path, body).await
    }

    /// Test runner (`POST …/demo`) in the lobby: the listing, or the room's
    /// error, of this press only.
    async fn test_runner(&self, sid: &str) -> Value {
        let room = format!("/v1/spaces/{sid}/rooms/lobby");
        let (s, ev) = self
            .server("POST", &format!("{room}/demo"), Some(json!({})))
            .await;
        assert_eq!(s, 202, "Test runner was refused: {ev}");
        let after = ev["id"].as_i64().unwrap();
        let start = Instant::now();
        loop {
            let (_, v) = self
                .server("GET", &format!("{room}/events?after={after}"), None)
                .await;
            if let Some(e) = v["events"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|e| e["kind"] == "tool_result" || e["kind"] == "error")
            {
                return e.clone();
            }
            assert!(
                start.elapsed() < Duration::from_secs(45),
                "Test runner got no answer in 45 s"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn close(self) {
        self.ui("DELETE", "/v1/session", None).await;
        self.desktop.shutdown().await;
        self.bridge.stop().await;
    }
}

fn assert_listing(event: &Value, marker: &str, what: &str) {
    let text = event["text"].as_str().unwrap_or_default();
    assert_eq!(event["kind"], "tool_result", "{what}: {text}");
    assert_eq!(event["actor"], "runner", "{what}: {event}");
    let (head, listing) = text.split_once('\n').unwrap_or((text, ""));
    assert_eq!(head, "workspace.list ok", "{what}: {text}");
    let listing: Value = serde_json::from_str(listing).unwrap();
    let names: Vec<&str> = listing["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["name"].as_str())
        .collect();
    assert_eq!(names, [marker], "{what}: {text}");
}

async fn new_space(app: &App, name: &str) -> String {
    let (s, v) = app
        .server("POST", "/v1/spaces", Some(json!({"name": name})))
        .await;
    assert_eq!(s, 201, "{v}");
    v["id"].as_str().unwrap().to_owned()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn local_tools_answer_test_runner_before_and_after_a_restart() {
    let t = tempfile::tempdir().unwrap();
    let app = App::launch(t.path(), Marker::Absent).await;
    let sid = new_space(&app, "Local tools").await;
    let (s, v) = app
        .ui(
            "POST",
            &format!("/v1/local-tools/spaces/{sid}/enable"),
            Some(json!({})),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    let folder = v["folder"].as_str().unwrap().to_owned();
    std::fs::write(Path::new(&folder).join("local-marker.txt"), b"x").unwrap();
    assert_listing(
        &app.test_runner(&sid).await,
        "local-marker.txt",
        "first launch",
    );
    app.close().await;

    // Restart: the pairing is resumed, nothing is re-enabled.
    let app = App::launch(t.path(), Marker::Absent).await;
    assert_listing(
        &app.test_runner(&sid).await,
        "local-marker.txt",
        "after restart",
    );
    app.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_installed_runner_answers_test_runner_before_and_after_a_restart() {
    let t = tempfile::tempdir().unwrap();
    let app = App::launch(t.path(), Marker::Accepted).await;
    // The installer asked for it: created once on the own hosted server.
    let start = Instant::now();
    let runner = loop {
        let (_, v) = app.ui("GET", "/v1/local-tools/runner", None).await;
        if v["state"] == "online" {
            break v["runner"]["id"].as_str().unwrap().to_owned();
        }
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "local runner: {v}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let sid = new_space(&app, "Installed runner").await;
    let (s, v) = app
        .server(
            "POST",
            &format!("/v1/spaces/{sid}/runner/attach-if-empty"),
            Some(json!({"runner_id": runner})),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    let (s, v) = app
        .ui(
            "POST",
            &format!("/v1/local-tools/runner/spaces/{sid}/folder"),
            Some(json!({})),
        )
        .await;
    assert_eq!((s, &v["attached"]), (200, &json!(true)), "{v}");
    let folder = v["folder"].as_str().unwrap().to_owned();
    std::fs::write(Path::new(&folder).join("runner-marker.txt"), b"x").unwrap();
    assert_listing(
        &app.test_runner(&sid).await,
        "runner-marker.txt",
        "first launch",
    );
    app.close().await;

    let app = App::launch(t.path(), Marker::Accepted).await;
    assert_listing(
        &app.test_runner(&sid).await,
        "runner-marker.txt",
        "after restart",
    );
    app.close().await;
}
