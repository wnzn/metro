//! The desktop app's local workspace tools (`metro_connector::desktop_tools`)
//! against this real server over HTTP: guarded pairing, Space isolation,
//! no replacement of anyone else's runner, disable, restart and the solo
//! Demo. No model is involved.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use metro_connector::Secret;
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
        headers: &[(&str, &str)],
        body: Option<Value>,
    ) -> (StatusCode, reqwest::header::HeaderMap, Value) {
        let mut req = self
            .http
            .request(method, format!("{}{path}", self.base))
            .bearer_auth(token);
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        if let Some(b) = body {
            req = req.json(&b);
        }
        let r = req.send().await.unwrap();
        let (s, h) = (r.status(), r.headers().clone());
        (s, h, r.json().await.unwrap_or(Value::Null))
    }

    async fn get(&self, token: &str, path: &str) -> (StatusCode, Value) {
        let (s, _, v) = self
            .send(reqwest::Method::GET, token, path, &[], None)
            .await;
        (s, v)
    }

    async fn post(&self, token: &str, path: &str, body: Value) -> (StatusCode, Value) {
        let (s, _, v) = self
            .send(reqwest::Method::POST, token, path, &[], Some(body))
            .await;
        (s, v)
    }

    async fn space(&self, token: &str, name: &str) -> String {
        let (s, v) = self.post(token, "/v1/spaces", json!({"name": name})).await;
        assert_eq!(s, StatusCode::CREATED, "{v}");
        v["id"].as_str().unwrap().to_owned()
    }

    async fn add(&self, owner: &str, sid: &str, user: &str) {
        let (s, v) = self
            .post(
                owner,
                &format!("/v1/spaces/{sid}/members"),
                json!({"user_id": user}),
            )
            .await;
        assert_eq!(s, StatusCode::CREATED, "{v}");
    }

    /// The unguarded owner route: issue or rotate.
    async fn issue(&self, owner: &str, sid: &str) -> String {
        let (s, v) = self
            .post(owner, &format!("/v1/spaces/{sid}/runner-token"), json!({}))
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
        assert!(
            v.get("etag").is_none(),
            "the unguarded route is unchanged: {v}"
        );
        v["token"].as_str().unwrap().to_owned()
    }

    async fn runner(&self, token: &str, sid: &str) -> Value {
        let (s, v) = self.get(token, &format!("/v1/spaces/{sid}/runner")).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        v["runner"].clone()
    }

    async fn claim(&self, runner_token: &str) -> StatusCode {
        self.send(
            reqwest::Method::POST,
            runner_token,
            "/v1/runner/claim",
            &[("X-Metro-Runner-Protocol", "2")],
            Some(json!({})),
        )
        .await
        .0
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

    /// What the launcher does: the local server's URL and bearer are the
    /// session's, so verification succeeds.
    fn hosted(&self, bearer: &str, account: &str) -> HostedSession {
        let t = Secret::new(bearer);
        HostedSession::verify(Some(&self.base), Some(&t), &self.base, &t, account).unwrap()
    }
}

async fn enable(tools: &DesktopTools, h: &HostedSession, sid: &str) -> (u16, Value) {
    tools
        .handle(
            "POST",
            &format!("/v1/local-tools/spaces/{sid}/enable"),
            b"{}",
            Some(h.clone()),
        )
        .await
}

async fn disable(tools: &DesktopTools, h: &HostedSession, sid: &str) -> (u16, Value) {
    tools
        .handle(
            "POST",
            &format!("/v1/local-tools/spaces/{sid}/disable"),
            b"{}",
            Some(h.clone()),
        )
        .await
}

async fn status(tools: &DesktopTools, h: Option<&HostedSession>) -> Value {
    let (s, v) = tools
        .handle("GET", "/v1/local-tools", b"", h.cloned())
        .await;
    assert_eq!(s, 200, "{v}");
    v
}

async fn wait_worker(tools: &DesktopTools, h: &HostedSession, sid: &str, want: &str) -> Value {
    for _ in 0..400 {
        let v = status(tools, Some(h)).await;
        if let Some(e) = v["spaces"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["space_id"] == sid)
            && e["worker"] == want
        {
            return e.clone();
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!(
        "worker for {sid} never became {want}: {}",
        status(tools, Some(h)).await
    );
}

fn token_of(root: &Path, account: &str, sid: &str) -> String {
    let r = store::open_root(root).unwrap();
    store::load(&r, account, sid)
        .unwrap()
        .unwrap()
        .token
        .expose()
        .to_owned()
}

#[tokio::test]
async fn solo_demo_lists_only_the_spaces_own_folder() {
    let s = Server::start().await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("local-tools");
    let tools = DesktopTools::new(root.clone());
    let alice = s.hosted(ALICE, "alice");
    let a = s.space(ALICE, "solo").await;
    let b = s.space(ALICE, "other").await;

    let (st, e) = enable(&tools, &alice, &a).await;
    assert_eq!(st, 200, "{e}");
    assert_eq!(e["state"], "on");
    assert_eq!(e["tools"], json!(["workspace.list", "workspace.read"]));
    let folder = Path::new(e["folder"].as_str().unwrap()).to_path_buf();
    assert!(
        folder.starts_with(&root),
        "private folder under the desktop root"
    );
    std::fs::write(folder.join("hello.txt"), "hi").unwrap();
    assert_eq!(s.runner(ALICE, &a).await["kind"], "dedicated");

    // Another Space of the same person, enabled too, with its own folder.
    let (st, eb) = enable(&tools, &alice, &b).await;
    assert_eq!(st, 200, "{eb}");
    let folder_b = Path::new(eb["folder"].as_str().unwrap()).to_path_buf();
    assert_ne!(folder, folder_b);
    std::fs::write(folder_b.join("b-secret.txt"), "b only").unwrap();
    wait_worker(&tools, &alice, &a, "online").await;

    let (st, v) = s.demo(ALICE, &a).await;
    assert_eq!(st, StatusCode::ACCEPTED, "{v}");
    let result = s.wait_tool_result(ALICE, &a, 1).await;
    let text = result["text"].as_str().unwrap_or_default().to_owned() + &result.to_string();
    assert!(text.contains("hello.txt"), "{result}");
    assert!(!text.contains("b-secret"), "{result}");

    let (st, v) = s.demo(ALICE, &b).await;
    assert_eq!(st, StatusCode::ACCEPTED, "{v}");
    let result = s.wait_tool_result(ALICE, &b, 1).await;
    assert!(result.to_string().contains("b-secret.txt"), "{result}");
    assert!(!result.to_string().contains("hello.txt"), "{result}");

    // Enabling again is idempotent: same pairing, nothing re-issued.
    let token = token_of(&root, "alice", &a);
    let (st, _) = enable(&tools, &alice, &a).await;
    assert_eq!(st, 200);
    assert_eq!(token_of(&root, "alice", &a), token);
    tools.stop_all().await;
}

#[tokio::test]
async fn never_replaces_an_existing_runner() {
    let s = Server::start().await;
    let dir = tempfile::tempdir().unwrap();
    let tools = DesktopTools::new(dir.path().join("local-tools"));
    let alice = s.hosted(ALICE, "alice");

    // A dedicated runner someone already paired.
    let a = s.space(ALICE, "has-dedicated").await;
    let existing = s.issue(ALICE, &a).await;
    let (st, e) = enable(&tools, &alice, &a).await;
    assert_eq!(st, 409, "{e}");
    assert_eq!(e["error"]["code"], "runner_present");
    assert_eq!(
        s.claim(&existing).await,
        StatusCode::NO_CONTENT,
        "still valid"
    );

    // A registered runner attached to the Space.
    let b = s.space(ALICE, "has-registered").await;
    let (st, v) = s
        .post(
            ALICE,
            "/v1/runners",
            json!({"name": "laptop", "visibility": "personal"}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{v}");
    let rid = v["runner"]["id"].as_str().unwrap().to_owned();
    let rtoken = v["token"].as_str().unwrap().to_owned();
    let (st, _, v) = s
        .send(
            reqwest::Method::PUT,
            ALICE,
            &format!("/v1/spaces/{b}/runner"),
            &[],
            Some(json!({"runner_id": rid})),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let (st, e) = enable(&tools, &alice, &b).await;
    assert_eq!(st, 409, "{e}");
    assert!(
        e["error"]["message"].as_str().unwrap().contains("laptop"),
        "{e}"
    );
    assert_eq!(s.runner(ALICE, &b).await["kind"], "registered");
    assert_eq!(s.claim(&rtoken).await, StatusCode::NO_CONTENT);

    // Two desktops of the same owner: the second never overwrites the first.
    let c = s.space(ALICE, "two-desktops").await;
    let dir2 = tempfile::tempdir().unwrap();
    let root1 = dir.path().join("local-tools");
    let tools2 = DesktopTools::new(dir2.path().join("local-tools"));
    let (st, _) = enable(&tools, &alice, &c).await;
    assert_eq!(st, 200);
    let (st, e) = enable(&tools2, &alice, &c).await;
    assert_eq!(st, 409, "{e}");
    assert_eq!(e["error"]["code"], "runner_present");
    let first = token_of(&root1, "alice", &c);
    // The first desktop's worker holds the claim loop; a claim still works.
    assert_ne!(s.claim(&first).await, StatusCode::UNAUTHORIZED);
    tools.stop_all().await;
    tools2.stop_all().await;
}

#[tokio::test]
async fn old_disable_cannot_revoke_a_replacement() {
    let s = Server::start().await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("local-tools");
    let tools = DesktopTools::new(root.clone());
    let alice = s.hosted(ALICE, "alice");
    let a = s.space(ALICE, "replaced").await;
    let (st, _) = enable(&tools, &alice, &a).await;
    assert_eq!(st, 200);
    wait_worker(&tools, &alice, &a, "online").await;
    let ours = token_of(&root, "alice", &a);

    // The operator rotates the Space's token by hand (unguarded route).
    let replacement = s.issue(ALICE, &a).await;
    // The desktop notices the refusal and marks the pairing revoked.
    let e = wait_worker(&tools, &alice, &a, "revoked").await;
    assert_eq!(e["state"], "revoked");
    assert_eq!(s.claim(&ours).await, StatusCode::UNAUTHORIZED);

    // Turning local tools off now must not revoke the replacement.
    let (st, e) = disable(&tools, &alice, &a).await;
    assert_eq!(st, 200, "{e}");
    assert_eq!(e["state"], "off");
    assert_eq!(s.claim(&replacement).await, StatusCode::NO_CONTENT);
    assert_eq!(s.runner(ALICE, &a).await["kind"], "dedicated");

    // Same race before the worker noticed: the guarded DELETE refuses a
    // stale ETag atomically.
    let b = s.space(ALICE, "race").await;
    let (st, _) = enable(&tools, &alice, &b).await;
    assert_eq!(st, 200);
    tools.stop_all().await; // worker can't notice anything now
    let replacement = s.issue(ALICE, &b).await;
    let (st, e) = disable(&tools, &alice, &b).await;
    assert_eq!(st, 200, "{e}");
    assert!(
        e["detail"]
            .as_str()
            .unwrap()
            .contains("nothing was revoked"),
        "{e}"
    );
    assert_eq!(s.claim(&replacement).await, StatusCode::NO_CONTENT);

    // Re-enabling while the replacement exists is refused, not a takeover.
    let (st, e) = enable(&tools, &alice, &b).await;
    assert_eq!(st, 409, "{e}");
    assert_eq!(s.claim(&replacement).await, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn stale_local_confirmation_neither_stops_nor_revokes_the_new_pairing() {
    let s = Server::start().await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("local-tools");
    let tools = DesktopTools::new(root.clone());
    let alice = s.hosted(ALICE, "alice");
    let sid = s.space(ALICE, "replace-local").await;
    let (st, first) = enable(&tools, &alice, &sid).await;
    assert_eq!(st, 200);
    assert_eq!(
        s.send(
            reqwest::Method::DELETE,
            ALICE,
            &format!("/v1/spaces/{sid}/runner-token"),
            &[],
            None
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    wait_worker(&tools, &alice, &sid, "revoked").await;
    let (st, second) = enable(&tools, &alice, &sid).await;
    assert_eq!(st, 200);
    assert_ne!(first["pairing_revision"], second["pairing_revision"]);
    let token = token_of(&root, "alice", &sid);
    let path = format!("/v1/local-tools/spaces/{sid}/disable-if-current");
    for (account, revision) in [
        ("alice", &first["pairing_revision"]),
        ("bob", &second["pairing_revision"]),
    ] {
        let body =
            serde_json::to_vec(&json!({"account_id": account, "pairing_revision": revision}))
                .unwrap();
        let (st, v) = tools
            .handle("POST", &path, &body, Some(alice.clone()))
            .await;
        assert_eq!(st, 409);
        assert_eq!(v["error"]["code"], "pairing_changed");
    }
    assert_eq!(
        wait_worker(&tools, &alice, &sid, "online").await["pairing_revision"],
        second["pairing_revision"]
    );
    assert_eq!(s.claim(&token).await, StatusCode::NO_CONTENT);
    let body = serde_json::to_vec(
        &json!({"account_id": "alice", "pairing_revision": second["pairing_revision"]}),
    )
    .unwrap();
    let (st, v) = tools.handle("POST", &path, &body, Some(alice)).await;
    assert_eq!(st, 200);
    assert_eq!(v["state"], "off");
    assert_eq!(s.claim(&token).await, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn guarded_routes_are_atomic_preconditions() {
    let s = Server::start().await;
    let a = s.space(ALICE, "guarded").await;
    let path = format!("/v1/spaces/{a}/runner-token");
    let (st, h, v) = s
        .send(
            reqwest::Method::POST,
            ALICE,
            &path,
            &[("If-None-Match", "*")],
            Some(json!({})),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(h["cache-control"], "no-store");
    let tag = h["etag"].to_str().unwrap().to_owned();
    assert_eq!(v["etag"], tag.as_str());
    let token = v["token"].as_str().unwrap().to_owned();
    // A second guarded issue changes nothing.
    let (st, _, _) = s
        .send(
            reqwest::Method::POST,
            ALICE,
            &path,
            &[("If-None-Match", "*")],
            Some(json!({})),
        )
        .await;
    assert_eq!(st, StatusCode::PRECONDITION_FAILED);
    assert_eq!(s.claim(&token).await, StatusCode::NO_CONTENT);
    // Members can't use it; malformed preconditions are refused.
    s.add(ALICE, &a, "bob").await;
    let (st, _, _) = s
        .send(
            reqwest::Method::DELETE,
            BOB,
            &path,
            &[("If-Match", &tag)],
            None,
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    for bad in ["*", "rtg-1", "\"nope\""] {
        let (st, _, _) = s
            .send(
                reqwest::Method::DELETE,
                ALICE,
                &path,
                &[("If-Match", bad)],
                None,
            )
            .await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{bad}");
    }
    // Revoking with a tag from before a rotation fails; the current one works.
    let rotated = s.issue(ALICE, &a).await;
    let (st, _, _) = s
        .send(
            reqwest::Method::DELETE,
            ALICE,
            &path,
            &[("If-Match", &tag)],
            None,
        )
        .await;
    assert_eq!(st, StatusCode::PRECONDITION_FAILED);
    assert_eq!(s.claim(&rotated).await, StatusCode::NO_CONTENT);
    // The unguarded DELETE is unchanged.
    let (st, _, _) = s
        .send(reqwest::Method::DELETE, ALICE, &path, &[], None)
        .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    assert_eq!(s.claim(&rotated).await, StatusCode::UNAUTHORIZED);
    let (st, _, v) = s
        .send(
            reqwest::Method::POST,
            ALICE,
            &path,
            &[("If-None-Match", "*")],
            Some(json!({})),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let tag2 = v["etag"].as_str().unwrap().to_owned();
    let (st, _, _) = s
        .send(
            reqwest::Method::DELETE,
            ALICE,
            &path,
            &[("If-Match", &tag2)],
            None,
        )
        .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    assert!(s.runner(ALICE, &a).await.is_null());
}

#[tokio::test]
async fn owner_only_member_isolation_and_account_switch() {
    let s = Server::start().await;
    let dir = tempfile::tempdir().unwrap();
    let tools = DesktopTools::new(dir.path().join("local-tools"));
    let alice = s.hosted(ALICE, "alice");
    let bob = s.hosted(BOB, "bob");
    let a = s.space(ALICE, "alices").await;
    let private = s.space(ALICE, "private").await;
    s.add(ALICE, &a, "bob").await;

    // A member can't turn it on; an outsider can't even see the Space.
    let (st, e) = enable(&tools, &bob, &a).await;
    assert_eq!(st, 403, "{e}");
    assert_eq!(e["error"]["code"], "not_owner");
    let (st, e) = enable(&tools, &bob, &private).await;
    assert_eq!(st, 404, "{e}");
    assert!(s.runner(ALICE, &a).await.is_null());

    // A session that claims another account than the server reports.
    let liar = s.hosted(BOB, "alice");
    let (st, e) = enable(&tools, &liar, &a).await;
    assert_eq!(st, 422, "{e}");
    assert_eq!(e["error"]["code"], "session_rejected");

    // Alice's tools run only while Alice is bound.
    let (st, _) = enable(&tools, &alice, &a).await;
    assert_eq!(st, 200);
    wait_worker(&tools, &alice, &a, "online").await;
    let v = status(&tools, Some(&bob)).await;
    assert_eq!(v["spaces"], json!([]), "Bob sees none of Alice's pairings");
    // Switching to Bob stopped Alice's worker: a Demo job now waits.
    let v = status(&tools, Some(&alice)).await;
    assert_eq!(v["spaces"][0]["worker"], "stopped", "{v}");

    // A session that isn't this desktop's own server gets nothing.
    let t = Secret::new(ALICE);
    assert!(
        HostedSession::verify(Some("http://127.0.0.1:1"), Some(&t), &s.base, &t, "alice").is_none()
    );
    let v = status(&tools, None).await;
    assert_eq!(v["available"], false);
    let (st, e) = tools
        .handle(
            "POST",
            &format!("/v1/local-tools/spaces/{private}/enable"),
            b"{}",
            None,
        )
        .await;
    assert_eq!(st, 409, "{e}");
    assert!(s.runner(ALICE, &private).await.is_null(), "nothing issued");
}

#[tokio::test]
async fn disable_revokes_and_restart_resumes_without_reissuing() {
    let s = Server::start().await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("local-tools");
    let alice = s.hosted(ALICE, "alice");
    let a = s.space(ALICE, "lifecycle").await;

    let tools = DesktopTools::new(root.clone());
    let (st, e) = enable(&tools, &alice, &a).await;
    assert_eq!(st, 200, "{e}");
    let folder = Path::new(e["folder"].as_str().unwrap()).to_path_buf();
    std::fs::write(folder.join("notes.md"), "kept").unwrap();
    let token = token_of(&root, "alice", &a);
    // App exit: workers stop, the pairing stays.
    tools.stop_all().await;
    drop(tools);

    // Next launch: resume starts the worker with the same token.
    let tools = DesktopTools::new(root.clone());
    tools.resume(alice.clone()).await;
    wait_worker(&tools, &alice, &a, "online").await;
    assert_eq!(token_of(&root, "alice", &a), token);
    let (st, _) = s.demo(ALICE, &a).await;
    assert_eq!(st, StatusCode::ACCEPTED);
    assert!(
        s.wait_tool_result(ALICE, &a, 1)
            .await
            .to_string()
            .contains("notes.md")
    );

    // Turning it off revokes exactly our token and keeps the files.
    let (st, e) = disable(&tools, &alice, &a).await;
    assert_eq!(st, 200, "{e}");
    assert_eq!(e["state"], "off");
    assert!(s.runner(ALICE, &a).await.is_null());
    assert_eq!(s.claim(&token).await, StatusCode::UNAUTHORIZED);
    assert!(folder.join("notes.md").is_file());
    let r = store::open_root(&root).unwrap();
    assert!(store::load(&r, "alice", &a).unwrap().is_none());
    // No runner, no fallback: Demo is refused.
    let (st, _) = s.demo(ALICE, &a).await;
    assert_eq!(st, StatusCode::CONFLICT);
    // Disabling again is a no-op.
    let (st, e) = disable(&tools, &alice, &a).await;
    assert_eq!(st, 200, "{e}");
    tools.stop_all().await;
}

#[tokio::test]
async fn personal_chat_never_reaches_the_local_tools() {
    let s = Server::start().await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("local-tools");
    let tools = DesktopTools::new(root.clone());
    let alice = s.hosted(ALICE, "alice");
    let a = s.space(ALICE, "chat-check").await;
    let (st, _) = enable(&tools, &alice, &a).await;
    assert_eq!(st, 200);
    tools.stop_all().await; // claim by hand below
    let token = token_of(&root, "alice", &a);
    for text in ["@agent list my files", "read hello.txt"] {
        let (st, v) = s
            .post(ALICE, "/v1/chat/messages", json!({"text": text}))
            .await;
        assert!(st.is_success(), "{v}");
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(s.claim(&token).await, StatusCode::NO_CONTENT);
}
