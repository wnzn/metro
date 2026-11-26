//! The desktop's installer-requested personal registered runner
//! (`metro_connector::desktop_tools::runner`) against this real server over
//! HTTP, with throwaway accounts: created once and resumed, never attached
//! by Metro, Space isolation across two Spaces, revocation and deletion not
//! undone automatically, explicit reconnect, interrupted creation, account
//! switch, and nothing at all without the installer's marker. No model is
//! involved, and no token is ever printed.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use metro_connector::Secret;
use metro_connector::desktop_tools::runner::{self, Marker, Record, RecordState};
use metro_connector::desktop_tools::{DesktopTools, HostedSession, store};
use metro_server::auth::{AuthFailure, AuthFuture, HumanAuth, UserIdentity};
use metro_server::db::Db;
use metro_server::{AppState, Config, Limits, router};
use reqwest::StatusCode;
use serde_json::{Value, json};

const ALICE: &str = "alice-token";
const BOB: &str = "bob-token";

struct FakeAuth(HashMap<String, UserIdentity>);

impl HumanAuth for FakeAuth {
    fn identity<'a>(&'a self, bearer: &'a str) -> AuthFuture<'a> {
        Box::pin(async move {
            self.0
                .get(bearer)
                .cloned()
                .ok_or_else(|| AuthFailure::Rejected("unknown token".to_owned()))
        })
    }
    fn namespace(&self) -> &str {
        "dev"
    }
}

struct Server {
    base: String,
    http: reqwest::Client,
}

impl Server {
    async fn start() -> Self {
        let user = |id: &str, actor: &str| UserIdentity {
            id: id.to_owned(),
            actor: actor.to_owned(),
        };
        let auth = Arc::new(FakeAuth(HashMap::from([
            (ALICE.to_owned(), user("alice", "Alice")),
            (BOB.to_owned(), user("bob", "Bob")),
        ])));
        let config = Config {
            bind: "127.0.0.1:0".parse().unwrap(),
            db_path: ":memory:".into(),
            model: None,
            limits: Limits {
                claim_timeout: Duration::from_secs(5),
                run_timeout: Duration::from_secs(5),
                agent_timeout: Duration::from_secs(30),
                ..Limits::default()
            },
            org_admins: Vec::new(),
        };
        let state = AppState::new(config, Db::open_in_memory().unwrap(), auth)
            .await
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, router(state)).await.unwrap() });
        Server {
            base,
            http: reqwest::Client::new(),
        }
    }

    async fn send(
        &self,
        method: reqwest::Method,
        token: &str,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut req = self
            .http
            .request(method, format!("{}{path}", self.base))
            .bearer_auth(token);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let r = req.send().await.unwrap();
        let s = r.status();
        let mut v: Value = r.json().await.unwrap_or(Value::Null);
        // Never let a token reach an assertion message.
        if let Some(o) = v.as_object_mut() {
            o.remove("token");
        }
        (s, v)
    }

    async fn get(&self, token: &str, path: &str) -> (StatusCode, Value) {
        self.send(reqwest::Method::GET, token, path, None).await
    }

    async fn post(&self, token: &str, path: &str, body: Value) -> (StatusCode, Value) {
        self.send(reqwest::Method::POST, token, path, Some(body))
            .await
    }

    async fn space(&self, token: &str, name: &str) -> String {
        let (s, v) = self.post(token, "/v1/spaces", json!({"name": name})).await;
        assert_eq!(s, StatusCode::CREATED, "{v}");
        v["id"].as_str().unwrap().to_owned()
    }

    /// The person's own personal runners, as the server lists them.
    async fn personal_runners(&self, token: &str) -> Vec<Value> {
        let (s, v) = self.get(token, "/v1/runners").await;
        assert_eq!(s, StatusCode::OK, "{v}");
        v["runners"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["visibility"] == "personal")
            .cloned()
            .collect()
    }

    async fn space_runner(&self, token: &str, sid: &str) -> Value {
        let (s, v) = self.get(token, &format!("/v1/spaces/{sid}/runner")).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        v["runner"].clone()
    }

    /// What the Space owner does in the existing Space runner settings.
    async fn attach(&self, token: &str, sid: &str, runner_id: &str) -> StatusCode {
        self.send(
            reqwest::Method::PUT,
            token,
            &format!("/v1/spaces/{sid}/runner"),
            Some(json!({"runner_id": runner_id})),
        )
        .await
        .0
    }

    /// A registered runner's claim: 204/200 while its token works, 401
    /// once rotated or revoked.
    async fn claim(&self, runner_token: &str) -> StatusCode {
        self.http
            .post(format!("{}/v1/runner/claim", self.base))
            .bearer_auth(runner_token)
            .header("X-Metro-Runner-Protocol", "2")
            .json(&json!({}))
            .send()
            .await
            .unwrap()
            .status()
    }

    /// Create a personal runner directly (as Settings would); its id and
    /// token. The token stays in the test, never in a message.
    async fn create_runner(&self, token: &str, name: &str) -> (String, String) {
        let r = self
            .http
            .post(format!("{}/v1/runners", self.base))
            .bearer_auth(token)
            .json(&json!({"name": name, "visibility": "personal"}))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::CREATED);
        let v: Value = r.json().await.unwrap();
        (
            v["runner"]["id"].as_str().unwrap().to_owned(),
            v["token"].as_str().unwrap().to_owned(),
        )
    }

    /// Rotate a runner's token directly (as Settings would).
    async fn rotate(&self, token: &str, id: &str) -> String {
        let r = self
            .http
            .post(format!("{}/v1/runners/{id}/token", self.base))
            .bearer_auth(token)
            .json(&json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        let v: Value = r.json().await.unwrap();
        v["token"].as_str().unwrap().to_owned()
    }

    async fn demo(&self, token: &str, sid: &str) -> (StatusCode, Value) {
        self.post(
            token,
            &format!("/v1/spaces/{sid}/rooms/lobby/demo"),
            json!({}),
        )
        .await
    }

    async fn wait_tool_result(&self, token: &str, sid: &str, n: usize) -> Value {
        for _ in 0..400 {
            let (_, v) = self
                .get(token, &format!("/v1/spaces/{sid}/rooms/lobby/events"))
                .await;
            let results: Vec<Value> = v["events"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|e| e["kind"] == "tool_result")
                .cloned()
                .collect();
            if results.len() >= n {
                return results[n - 1].clone();
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("no tool_result #{n} in {sid}");
    }

    fn hosted(&self, bearer: &str, account: &str) -> HostedSession {
        let t = Secret::new(bearer);
        HostedSession::verify(Some(&self.base), Some(&t), &self.base, &t, account).unwrap()
    }
}

fn tools(root: &Path, marker: Marker) -> DesktopTools {
    DesktopTools::new(root.to_path_buf()).with_local_runner(marker)
}

async fn runner_status(t: &DesktopTools, h: Option<&HostedSession>) -> Value {
    let (s, v) = t
        .handle("GET", "/v1/local-tools/runner", b"", h.cloned())
        .await;
    assert_eq!(s, 200, "{v}");
    assert!(!v.to_string().contains("metro_rt_"), "no token in status");
    v
}

async fn wait_state(t: &DesktopTools, h: &HostedSession, want: &str) -> Value {
    for _ in 0..400 {
        let v = runner_status(t, Some(h)).await;
        if v["state"] == want {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!(
        "runner never became {want}: {}",
        runner_status(t, Some(h)).await
    );
}

async fn reconnect(t: &DesktopTools, h: &HostedSession) -> (u16, Value) {
    t.handle(
        "POST",
        "/v1/local-tools/runner/reconnect",
        b"{}",
        Some(h.clone()),
    )
    .await
}

/// Reconnect to a runner the person picked by id.
async fn reconnect_to(t: &DesktopTools, h: &HostedSession, id: &str) -> (u16, Value) {
    let body = json!({"runner_id": id}).to_string();
    t.handle(
        "POST",
        "/v1/local-tools/runner/reconnect",
        body.as_bytes(),
        Some(h.clone()),
    )
    .await
}

/// A record as a crash right after the create call leaves it.
fn plant(root: &Path, h: &HostedSession, state: RecordState, name: &str, id: Option<&str>) {
    let r = store::open_root(root).unwrap();
    let rec = Record {
        server_url: h.server_url().to_owned(),
        account_id: h.account_id().to_owned(),
        name: name.into(),
        state,
        runner_id: id.map(str::to_owned),
        token: None,
    };
    runner::save(&r, h, &rec).unwrap();
}

async fn folder(t: &DesktopTools, h: &HostedSession, sid: &str) -> (u16, Value) {
    t.handle(
        "POST",
        &format!("/v1/local-tools/runner/spaces/{sid}/folder"),
        b"{}",
        Some(h.clone()),
    )
    .await
}

fn record(root: &Path, h: &HostedSession) -> Option<Record> {
    let r = store::open_root(root).unwrap();
    runner::load(&r, h).unwrap()
}

fn record_path(root: &Path, h: &HostedSession) -> PathBuf {
    root.join("runner")
        .join(runner::identity_dir(h.server_url(), h.account_id()))
        .join("runner.json")
}

/// A token fingerprint for comparing without showing it.
fn same_token(a: &Record, b: &Record) -> bool {
    a.token.as_ref().map(Secret::expose) == b.token.as_ref().map(Secret::expose)
}

#[tokio::test]
async fn created_once_resumed_and_serves_only_attached_spaces() {
    let s = Server::start().await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("local-tools");
    let alice = s.hosted(ALICE, "alice");
    let a = s.space(ALICE, "first").await;
    let b = s.space(ALICE, "second").await;

    let t = tools(&root, Marker::Accepted);
    t.resume(alice.clone()).await;
    let st = wait_state(&t, &alice, "online").await;
    assert_eq!(st["requested"], true);
    assert_eq!(st["available"], true);
    assert_eq!(st["tools"], json!(["workspace.list", "workspace.read"]));
    let runners = s.personal_runners(ALICE).await;
    assert_eq!(runners.len(), 1, "exactly one runner");
    let rid = runners[0]["id"].as_str().unwrap().to_owned();
    assert_eq!(st["runner"]["id"], rid.as_str());
    assert_eq!(st["runner"]["name"], runners[0]["name"]);
    assert_eq!(t.local_runner_id().await.as_deref(), Some(rid.as_str()));

    // Metro attaches nothing: both Spaces still have no runner.
    assert!(s.space_runner(ALICE, &a).await.is_null());
    assert!(s.space_runner(ALICE, &b).await.is_null());

    // The token is kept privately.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let m = std::fs::metadata(record_path(&root, &alice)).unwrap();
        assert_eq!(m.permissions().mode() & 0o777, 0o600);
    }
    let first = record(&root, &alice).unwrap();
    assert_eq!(first.state, RecordState::Active);

    // Binding again (token refresh) and restarting the app: same runner,
    // same token, nothing new on the server.
    t.resume(alice.clone()).await;
    t.stop_all().await;
    assert_eq!(t.local_runner_id().await, None, "stopped");
    drop(t);
    let t = tools(&root, Marker::Accepted);
    t.resume(alice.clone()).await;
    wait_state(&t, &alice, "online").await;
    assert_eq!(s.personal_runners(ALICE).await.len(), 1);
    assert_eq!(t.local_runner_id().await.as_deref(), Some(rid.as_str()));
    assert!(same_token(&first, &record(&root, &alice).unwrap()));

    // The owner attaches it to the first Space only. The second Space gets
    // a different runner with the very same display name: "attached" goes
    // by the stable id, so it is false there.
    assert_eq!(s.attach(ALICE, &a, &rid).await, StatusCode::OK);
    let name = st["runner"]["name"].as_str().unwrap().to_owned();
    let (twin, _twin_token) = s.create_runner(ALICE, &name).await;
    assert_ne!(twin, rid);
    assert_eq!(s.attach(ALICE, &b, &twin).await, StatusCode::OK);
    let (code, fa) = folder(&t, &alice, &a).await;
    assert_eq!(code, 200, "{fa}");
    assert_eq!(fa["attached"], true);
    let (code, fb) = folder(&t, &alice, &b).await;
    assert_eq!(code, 200, "{fb}");
    assert_eq!(fb["attached"], false, "same name, other runner: {fb}");
    assert_eq!(s.space_runner(ALICE, &b).await["name"], name.as_str());
    // Back to no runner on the second Space, and the twin removed.
    let (code, _) = s
        .send(
            reqwest::Method::DELETE,
            ALICE,
            &format!("/v1/runners/{twin}"),
            None,
        )
        .await;
    assert_eq!(code, StatusCode::NO_CONTENT);
    assert!(s.space_runner(ALICE, &b).await.is_null());
    let fa = PathBuf::from(fa["folder"].as_str().unwrap());
    let fb = PathBuf::from(fb["folder"].as_str().unwrap());
    assert!(fa.starts_with(&root) && fb.starts_with(&root));
    assert_ne!(fa, fb);
    std::fs::write(fa.join("hello.txt"), "hi").unwrap();
    std::fs::write(fb.join("b-secret.txt"), "b only").unwrap();

    let (st, v) = s.demo(ALICE, &a).await;
    assert_eq!(st, StatusCode::ACCEPTED, "{v}");
    let r = s.wait_tool_result(ALICE, &a, 1).await.to_string();
    assert!(r.contains("hello.txt"), "{r}");
    assert!(!r.contains("b-secret"), "{r}");

    // Attached to the second Space too: its own folder only.
    assert_eq!(s.attach(ALICE, &b, &rid).await, StatusCode::OK);
    assert_eq!(folder(&t, &alice, &b).await.1["attached"], true);
    let (st, v) = s.demo(ALICE, &b).await;
    assert_eq!(st, StatusCode::ACCEPTED, "{v}");
    let r = s.wait_tool_result(ALICE, &b, 1).await.to_string();
    assert!(r.contains("b-secret.txt"), "{r}");
    assert!(!r.contains("hello.txt"), "{r}");
    assert_eq!(s.personal_runners(ALICE).await.len(), 1);

    // Someone else neither sees it nor can attach it.
    assert!(s.personal_runners(BOB).await.is_empty());
    let c = s.space(BOB, "bobs").await;
    assert_eq!(s.attach(BOB, &c, &rid).await, StatusCode::NOT_FOUND);
    // And a Space you aren't in has no folder here.
    let (st, _) = folder(&t, &alice, &c).await;
    assert_eq!(st, 404);
    t.stop_all().await;
}

#[tokio::test]
async fn revocation_and_deletion_are_not_undone_automatically() {
    let s = Server::start().await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("local-tools");
    let alice = s.hosted(ALICE, "alice");
    let a = s.space(ALICE, "first").await;
    let t = tools(&root, Marker::Accepted);
    t.resume(alice.clone()).await;
    wait_state(&t, &alice, "online").await;
    let rid = t.local_runner_id().await.unwrap();
    assert_eq!(s.attach(ALICE, &a, &rid).await, StatusCode::OK);

    // The token is rotated elsewhere (Settings): the worker stops as
    // revoked and the record no longer holds a token.
    let (st, _) = s
        .post(ALICE, &format!("/v1/runners/{rid}/token"), json!({}))
        .await;
    assert_eq!(st, StatusCode::OK);
    let v = wait_state(&t, &alice, "revoked").await;
    assert_eq!(v["can_reconnect"], true);
    let rec = record(&root, &alice).unwrap();
    assert_eq!(rec.state, RecordState::Revoked);
    assert!(rec.token.is_none());

    // Binding again or restarting doesn't reissue anything.
    t.resume(alice.clone()).await;
    let t = {
        t.stop_all().await;
        tools(&root, Marker::Accepted)
    };
    t.resume(alice.clone()).await;
    assert_eq!(runner_status(&t, Some(&alice)).await["state"], "revoked");
    assert_eq!(t.local_runner_id().await, None);
    assert_eq!(s.personal_runners(ALICE).await.len(), 1);

    // Reconnecting explicitly: same runner, new token, still attached.
    let (st, v) = reconnect(&t, &alice).await;
    assert_eq!(st, 200, "{v}");
    wait_state(&t, &alice, "online").await;
    assert_eq!(t.local_runner_id().await.as_deref(), Some(rid.as_str()));
    assert_eq!(s.space_runner(ALICE, &a).await["id"], rid.as_str());
    assert_eq!(s.personal_runners(ALICE).await.len(), 1);
    // Reconnecting while it runs changes nothing.
    let before = record(&root, &alice).unwrap();
    let (st, _) = reconnect(&t, &alice).await;
    assert_eq!(st, 200);
    assert!(same_token(&before, &record(&root, &alice).unwrap()));

    // Deleted elsewhere: revoked now, deleted after a restart, never
    // recreated on its own.
    let (st, _) = s
        .send(
            reqwest::Method::DELETE,
            ALICE,
            &format!("/v1/runners/{rid}"),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    wait_state(&t, &alice, "revoked").await;
    t.stop_all().await;
    let t = tools(&root, Marker::Accepted);
    t.resume(alice.clone()).await;
    // Revoked records stay revoked until the person acts.
    assert_eq!(runner_status(&t, Some(&alice)).await["state"], "revoked");
    assert!(s.personal_runners(ALICE).await.is_empty());

    // Explicit reconnect after deletion: a new runner, attached to nothing.
    let (st, v) = reconnect(&t, &alice).await;
    assert_eq!(st, 200, "{v}");
    wait_state(&t, &alice, "online").await;
    let runners = s.personal_runners(ALICE).await;
    assert_eq!(runners.len(), 1);
    assert_ne!(runners[0]["id"], rid.as_str());
    assert!(s.space_runner(ALICE, &a).await.is_null());
    t.stop_all().await;
}

#[tokio::test]
async fn an_active_record_whose_runner_is_gone_is_marked_deleted() {
    let s = Server::start().await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("local-tools");
    let alice = s.hosted(ALICE, "alice");
    let t = tools(&root, Marker::Accepted);
    t.resume(alice.clone()).await;
    wait_state(&t, &alice, "online").await;
    let rid = t.local_runner_id().await.unwrap();
    // The app is closed, then the runner is deleted.
    t.stop_all().await;
    drop(t);
    let (st, _) = s
        .send(
            reqwest::Method::DELETE,
            ALICE,
            &format!("/v1/runners/{rid}"),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let t = tools(&root, Marker::Accepted);
    t.resume(alice.clone()).await;
    let v = runner_status(&t, Some(&alice)).await;
    assert_eq!(v["state"], "deleted", "{v}");
    assert!(s.personal_runners(ALICE).await.is_empty(), "not recreated");
    assert_eq!(record(&root, &alice).unwrap().state, RecordState::Deleted);
}

#[tokio::test]
async fn a_lost_create_is_never_rotated_by_name_even_after_the_owner_rotated() {
    let s = Server::start().await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("local-tools");
    let alice = s.hosted(ALICE, "alice");

    // The create reached the server but its answer was lost (crash). The
    // owner later found the runner in Settings and rotated its token.
    let name = "Metro desktop on this computer (aaaaaa)";
    plant(&root, &alice, RecordState::Creating, name, None);
    let (id, _lost) = s.create_runner(ALICE, name).await;
    let owners = s.rotate(ALICE, &id).await;

    // Start: interrupted, nothing rotated, nothing created.
    let t = tools(&root, Marker::Accepted);
    t.resume(alice.clone()).await;
    let v = runner_status(&t, Some(&alice)).await;
    assert_eq!(v["state"], "interrupted", "{v}");
    assert_eq!(v["unconfirmed_name"], name);
    assert!(v["runner"].is_null(), "{v}");
    assert_eq!(v["can_reconnect"], true);
    assert_eq!(t.local_runner_id().await, None);
    assert_eq!(
        s.claim(&owners).await,
        StatusCode::NO_CONTENT,
        "owner's token untouched"
    );
    assert_eq!(
        record(&root, &alice).unwrap().state,
        RecordState::Interrupted
    );
    // Again (token refresh, restart): still nothing.
    t.resume(alice.clone()).await;
    t.stop_all().await;
    let t = tools(&root, Marker::Accepted);
    t.resume(alice.clone()).await;
    assert_eq!(
        runner_status(&t, Some(&alice)).await["state"],
        "interrupted"
    );
    assert_eq!(s.claim(&owners).await, StatusCode::NO_CONTENT);
    assert_eq!(s.personal_runners(ALICE).await.len(), 1);

    // Explicit reconnect without a choice: no known id, so a new runner;
    // the same-named one is left alone.
    let (code, v) = reconnect(&t, &alice).await;
    assert_eq!(code, 200, "{v}");
    wait_state(&t, &alice, "online").await;
    let new_id = t.local_runner_id().await.unwrap();
    assert_ne!(new_id, id);
    assert_eq!(s.claim(&owners).await, StatusCode::NO_CONTENT);
    assert_eq!(s.personal_runners(ALICE).await.len(), 2);
    t.stop_all().await;
}

#[tokio::test]
async fn duplicate_names_are_never_picked_and_an_explicit_id_is() {
    let s = Server::start().await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("local-tools");
    let alice = s.hosted(ALICE, "alice");
    let name = "Metro desktop on this computer (dddddd)";
    plant(&root, &alice, RecordState::Creating, name, None);
    let (first, first_token) = s.create_runner(ALICE, name).await;
    let (second, second_token) = s.create_runner(ALICE, name).await;

    let t = tools(&root, Marker::Accepted);
    t.resume(alice.clone()).await;
    let v = runner_status(&t, Some(&alice)).await;
    assert_eq!(v["state"], "interrupted", "{v}");
    assert_eq!(s.claim(&first_token).await, StatusCode::NO_CONTENT);
    assert_eq!(s.claim(&second_token).await, StatusCode::NO_CONTENT);

    // A runner that isn't the person's own can't be chosen.
    let (bob_runner, _) = s.create_runner(BOB, name).await;
    let (code, v) = reconnect_to(&t, &alice, &bob_runner).await;
    assert_eq!(code, 404, "{v}");
    let (code, _) = t
        .handle(
            "POST",
            "/v1/local-tools/runner/reconnect",
            br#"{"runner_id":"../x"}"#,
            Some(alice.clone()),
        )
        .await;
    assert_eq!(code, 422);

    // The person picks the second one by id: exactly that one is rotated.
    let (code, v) = reconnect_to(&t, &alice, &second).await;
    assert_eq!(code, 200, "{v}");
    wait_state(&t, &alice, "online").await;
    assert_eq!(t.local_runner_id().await.as_deref(), Some(second.as_str()));
    assert_eq!(s.claim(&second_token).await, StatusCode::UNAUTHORIZED);
    assert_eq!(
        s.claim(&first_token).await,
        StatusCode::NO_CONTENT,
        "the other is untouched"
    );
    // While it runs, another choice is refused.
    let (code, _) = reconnect_to(&t, &alice, &first).await;
    assert_eq!(code, 409);
    assert_eq!(s.personal_runners(ALICE).await.len(), 2);
    t.stop_all().await;
}

#[tokio::test]
async fn a_known_id_is_kept_and_reconnect_rotates_only_that_runner() {
    let s = Server::start().await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("local-tools");
    let alice = s.hosted(ALICE, "alice");
    // Created and its id learned, but the token couldn't be saved; a
    // same-named runner also exists.
    let name = "Metro desktop on this computer (eeeeee)";
    let (known, known_token) = s.create_runner(ALICE, name).await;
    let (_twin, twin_token) = s.create_runner(ALICE, name).await;
    plant(&root, &alice, RecordState::Interrupted, name, Some(&known));

    let t = tools(&root, Marker::Accepted);
    t.resume(alice.clone()).await;
    let v = runner_status(&t, Some(&alice)).await;
    assert_eq!(v["state"], "interrupted", "{v}");
    assert_eq!(v["runner"]["id"], known.as_str(), "known id kept: {v}");
    assert_eq!(
        s.claim(&known_token).await,
        StatusCode::NO_CONTENT,
        "not rotated on its own"
    );

    let (code, v) = reconnect(&t, &alice).await;
    assert_eq!(code, 200, "{v}");
    wait_state(&t, &alice, "online").await;
    assert_eq!(t.local_runner_id().await.as_deref(), Some(known.as_str()));
    assert_eq!(s.claim(&known_token).await, StatusCode::UNAUTHORIZED);
    assert_eq!(s.claim(&twin_token).await, StatusCode::NO_CONTENT);
    assert_eq!(s.personal_runners(ALICE).await.len(), 2);
    t.stop_all().await;
}

#[tokio::test]
async fn nothing_without_the_marker_or_the_own_server_and_account_switch_stops() {
    let s = Server::start().await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("local-tools");
    let alice = s.hosted(ALICE, "alice");

    // No marker, or an untrusted one: nothing is created.
    for m in [Marker::Absent, Marker::Rejected("not empty".into())] {
        let t = tools(&root, m);
        t.resume(alice.clone()).await;
        let v = runner_status(&t, Some(&alice)).await;
        assert_eq!(v["state"], "not_requested", "{v}");
        assert_eq!(v["requested"], false);
        let (st, v) = reconnect(&t, &alice).await;
        assert_eq!(st, 409, "{v}");
        assert!(s.personal_runners(ALICE).await.is_empty());
    }
    assert!(record(&root, &alice).is_none());

    // Marker, but not this app's own server session: unavailable.
    let t = tools(&root, Marker::Accepted);
    let v = runner_status(&t, None).await;
    assert_eq!(v["state"], "unavailable", "{v}");
    let (st, v) = t
        .handle("POST", "/v1/local-tools/runner/reconnect", b"{}", None)
        .await;
    assert_eq!(st, 409, "{v}");
    assert_eq!(v["error"]["code"], "not_local");
    let (st, v) = t.handle("GET", "/v1/local-tools", b"", None).await;
    assert_eq!(st, 200);
    assert_eq!(v["local_runner"]["state"], "unavailable");
    assert!(s.personal_runners(ALICE).await.is_empty());

    // Own server: created. Then another account binds: Alice's worker
    // stops; Bob's is separate (own record and folders).
    t.resume(alice.clone()).await;
    wait_state(&t, &alice, "online").await;
    let alice_rid = t.local_runner_id().await.unwrap();
    let bob = s.hosted(BOB, "bob");
    t.resume(bob.clone()).await;
    wait_state(&t, &bob, "online").await;
    let bob_rid = t.local_runner_id().await.unwrap();
    assert_ne!(alice_rid, bob_rid);
    assert_ne!(record_path(&root, &alice), record_path(&root, &bob));
    assert_eq!(s.personal_runners(BOB).await.len(), 1);
    assert_eq!(s.personal_runners(ALICE).await.len(), 1);

    // Sign-out stops everything.
    t.stop_all().await;
    assert_eq!(t.local_runner_id().await, None);
    let (st, v) = t.handle("GET", "/v1/local-tools", b"", None).await;
    assert_eq!(st, 200);
    assert_eq!(v["local_runner"]["state"], "unavailable");
}

/// A start that can't get going says why, in both statuses, instead of
/// leaving nothing polling; the next session bind tries again.
#[tokio::test]
async fn a_folder_that_cannot_open_is_reported_and_retried_on_the_next_bind() {
    let s = Server::start().await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("local-tools");
    // Not a folder: Metro can't open (or create) it.
    std::fs::write(&root, b"not a folder").unwrap();
    let t = tools(&root, Marker::Accepted);
    let alice = s.hosted(ALICE, "alice");
    t.resume(alice.clone()).await;

    let v = runner_status(&t, Some(&alice)).await;
    assert_eq!(v["state"], "failed", "{v}");
    let detail = v["detail"].as_str().unwrap_or_default();
    assert!(
        detail.contains("couldn't open its local tools folder"),
        "{v}"
    );
    let (st, all) = t
        .handle("GET", "/v1/local-tools", b"", Some(alice.clone()))
        .await;
    assert_eq!(st, 200, "{all}");
    assert!(
        all["problem"]
            .as_str()
            .is_some_and(|p| p.contains("couldn't open its local tools folder")),
        "{all}"
    );
    assert_eq!(all["local_runner"]["state"], "failed", "{all}");

    // Fixed: the next bind starts it, and the reason is gone.
    std::fs::remove_file(&root).unwrap();
    t.resume(alice.clone()).await;
    let v = wait_state(&t, &alice, "online").await;
    assert!(v["detail"].is_null(), "{v}");
    let (_, all) = t
        .handle("GET", "/v1/local-tools", b"", Some(alice.clone()))
        .await;
    assert!(all["problem"].is_null(), "{all}");
    t.stop_all().await;
}
