//! Registered (personal and organization) runners over real HTTP: who may
//! see, manage and attach them, one runner serving two Spaces, protocol 2,
//! detach and rotation with jobs in flight, and dedicated Space tokens
//! working as before.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use metro_server::auth::{AuthFailure, AuthFuture, HumanAuth, UserIdentity};
use metro_server::db::Db;
use metro_server::{AppState, Config, Limits, router};
use reqwest::StatusCode;
use serde_json::{Value, json};

const ALICE: &str = "alice-token";
const BOB: &str = "bob-token";
/// Carol is the organization admin.
const CAROL: &str = "carol-token";
const PROTOCOL: &str = "X-Metro-Runner-Protocol";

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

fn auth() -> Arc<FakeAuth> {
    let user = |id: &str, actor: &str| UserIdentity {
        id: id.to_owned(),
        actor: actor.to_owned(),
    };
    Arc::new(FakeAuth(HashMap::from([
        (ALICE.to_owned(), user("alice", "Alice")),
        (BOB.to_owned(), user("bob", "Bob")),
        // The display name matches an admin id; only the stable id counts.
        (CAROL.to_owned(), user("carol", "alice")),
        // Display name "carol", but a different stable id: not an admin.
        ("mallory-token".to_owned(), user("mallory", "carol")),
    ])))
}

struct Server {
    base: String,
    http: reqwest::Client,
}

impl Server {
    async fn start() -> Self {
        Self::start_with(Db::open_in_memory().unwrap()).await
    }

    async fn start_with(db: Db) -> Self {
        let config = Config {
            bind: "127.0.0.1:0".parse().unwrap(),
            db_path: ":memory:".into(),
            model: None,
            limits: Limits {
                claim_timeout: Duration::from_secs(5),
                run_timeout: Duration::from_secs(5),
                ..Limits::default()
            },
            org_admins: vec!["carol".to_owned()],
        };
        let state = AppState::new(config, db, auth()).await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, router(state)).await.unwrap() });
        Self {
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
        if let Some(body) = body {
            req = req.json(&body);
        }
        let r = req.send().await.unwrap();
        let status = r.status();
        (status, r.json().await.unwrap_or(Value::Null))
    }

    async fn get(&self, token: &str, path: &str) -> (StatusCode, Value) {
        self.send(reqwest::Method::GET, token, path, None).await
    }
    async fn post(&self, token: &str, path: &str, body: Value) -> (StatusCode, Value) {
        self.send(reqwest::Method::POST, token, path, Some(body))
            .await
    }
    async fn put(&self, token: &str, path: &str, body: Value) -> (StatusCode, Value) {
        self.send(reqwest::Method::PUT, token, path, Some(body))
            .await
    }
    async fn delete(&self, token: &str, path: &str) -> (StatusCode, Value) {
        self.send(reqwest::Method::DELETE, token, path, None).await
    }

    async fn space(&self, token: &str, name: &str) -> String {
        let (status, space) = self.post(token, "/v1/spaces", json!({"name": name})).await;
        assert_eq!(status, StatusCode::CREATED, "{space}");
        space["id"].as_str().unwrap().to_owned()
    }

    async fn add(&self, owner: &str, space: &str, user: &str) {
        let (status, body) = self
            .post(
                owner,
                &format!("/v1/spaces/{space}/members"),
                json!({"user_id": user}),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }

    /// Register a runner; returns (id, token).
    async fn register(&self, token: &str, name: &str, visibility: &str) -> (String, String) {
        let r = self
            .http
            .post(format!("{}/v1/runners", self.base))
            .bearer_auth(token)
            .json(&json!({"name": name, "visibility": visibility}))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::CREATED);
        assert_eq!(r.headers()["cache-control"], "no-store");
        let body: Value = r.json().await.unwrap();
        assert_eq!(body["runner"]["name"], name);
        assert_eq!(body["runner"]["visibility"], visibility);
        assert_eq!(body["runner"]["can_manage"], true);
        let token = body["token"].as_str().unwrap().to_owned();
        assert!(token.starts_with("metro_rt_v2_"));
        (body["runner"]["id"].as_str().unwrap().to_owned(), token)
    }

    async fn attach(&self, owner: &str, space: &str, runner: &str) -> (StatusCode, Value) {
        self.put(
            owner,
            &format!("/v1/spaces/{space}/runner"),
            json!({"runner_id": runner}),
        )
        .await
    }

    async fn dedicated_token(&self, owner: &str, space: &str) -> (StatusCode, Value) {
        self.post(
            owner,
            &format!("/v1/spaces/{space}/runner-token"),
            json!({}),
        )
        .await
    }

    async fn demo(&self, token: &str, space: &str) -> StatusCode {
        self.post(
            token,
            &format!("/v1/spaces/{space}/rooms/lobby/demo"),
            json!({}),
        )
        .await
        .0
    }

    async fn claim_once(&self, runner: &str, protocol: Option<&str>) -> (StatusCode, Value) {
        let mut req = self
            .http
            .post(format!("{}/v1/runner/claim", self.base))
            .bearer_auth(runner)
            .json(&json!({}));
        if let Some(p) = protocol {
            req = req.header(PROTOCOL, p);
        }
        let r = req.send().await.unwrap();
        let status = r.status();
        (status, r.json().await.unwrap_or(Value::Null))
    }

    async fn claim(&self, runner: &str, protocol: Option<&str>) -> Value {
        for _ in 0..400 {
            match self.claim_once(runner, protocol).await {
                (StatusCode::OK, job) => return job,
                (StatusCode::NO_CONTENT, _) => tokio::time::sleep(Duration::from_millis(20)).await,
                (other, body) => panic!("claim returned {other}: {body}"),
            }
        }
        panic!("no job to claim");
    }

    async fn result(
        &self,
        runner: &str,
        protocol: Option<&str>,
        job: &Value,
        output: &str,
    ) -> StatusCode {
        let id = job["id"].as_str().unwrap();
        let mut req = self
            .http
            .post(format!("{}/v1/runner/jobs/{id}/result", self.base))
            .bearer_auth(runner)
            .json(&json!({"ok": true, "output": output}));
        if let Some(p) = protocol {
            req = req.header(PROTOCOL, p);
        }
        req.send().await.unwrap().status()
    }

    async fn wait_for(
        &self,
        token: &str,
        space: &str,
        pred: impl Fn(&Value) -> bool,
    ) -> Vec<Value> {
        for _ in 0..400 {
            let (_, body) = self
                .get(token, &format!("/v1/spaces/{space}/rooms/lobby/events"))
                .await;
            let events = body["events"].as_array().cloned().unwrap_or_default();
            if events.iter().any(&pred) {
                return events;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("no matching event in {space}");
    }
}

fn ids(body: &Value) -> Vec<String> {
    body["runners"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn me_reports_org_admin_by_stable_id_only() {
    let s = Server::start().await;
    let (_, me) = s.get(CAROL, "/v1/me").await;
    assert_eq!(
        me,
        json!({"id": "carol", "actor": "alice", "is_org_admin": true})
    );
    let (_, me) = s.get(ALICE, "/v1/me").await;
    assert_eq!(me["is_org_admin"], false);
    let (_, me) = s.get("mallory-token", "/v1/me").await;
    assert_eq!(me["is_org_admin"], false);
}

#[tokio::test]
async fn personal_runners_are_private_to_their_owner() {
    let s = Server::start().await;
    let (mine, _) = s.register(ALICE, "alice laptop", "personal").await;
    let alice_space = s.space(ALICE, "a").await;
    let bob_space = s.space(BOB, "b").await;

    let (status, body) = s.get(ALICE, "/v1/runners").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["runners"][0]["can_manage"], true);
    assert_eq!(ids(&body), vec![mine.clone()]);
    for other in [BOB, CAROL] {
        let (_, body) = s.get(other, "/v1/runners").await;
        assert!(ids(&body).is_empty(), "{body}");
        // Not even an org admin can see, rotate, delete or attach it.
        assert_eq!(
            s.post(other, &format!("/v1/runners/{mine}/token"), json!({}))
                .await
                .0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            s.delete(other, &format!("/v1/runners/{mine}")).await.0,
            StatusCode::NOT_FOUND
        );
    }
    assert_eq!(
        s.attach(BOB, &bob_space, &mine).await.0,
        StatusCode::NOT_FOUND
    );
    // Alice cannot put her runner on a Space she does not own.
    assert_eq!(
        s.attach(ALICE, &bob_space, &mine).await.0,
        StatusCode::NOT_FOUND
    );
    s.add(BOB, &bob_space, "alice").await;
    assert_eq!(
        s.attach(ALICE, &bob_space, &mine).await.0,
        StatusCode::FORBIDDEN
    );
    // Her own Space: fine, and idempotent.
    let (status, body) = s.attach(ALICE, &alice_space, &mine).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["runner"]["id"], mine.as_str());
    assert_eq!(s.attach(ALICE, &alice_space, &mine).await.0, StatusCode::OK);
    // A member sees which runner serves the Space, but cannot change it.
    s.add(ALICE, &alice_space, "bob").await;
    let (status, body) = s
        .get(BOB, &format!("/v1/spaces/{alice_space}/runner"))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        without_update_fields(&body["runner"]),
        json!({"id": mine, "name": "alice laptop", "visibility": "personal", "kind": "registered"})
    );
    assert_eq!(
        s.delete(BOB, &format!("/v1/spaces/{alice_space}/runner"))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        s.get("mallory-token", &format!("/v1/spaces/{alice_space}/runner"))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let (_, spaces) = s.get(BOB, "/v1/spaces").await;
    let paired = spaces["spaces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|sp| sp["id"] == alice_space.as_str())
        .unwrap()["runner_paired"]
        .clone();
    assert_eq!(paired, true);
}

#[tokio::test]
async fn organization_runners_are_managed_by_admins_and_attachable_by_space_owners() {
    let s = Server::start().await;
    assert_eq!(
        s.post(
            ALICE,
            "/v1/runners",
            json!({"name": "ci", "visibility": "organization"})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        s.post(
            "mallory-token",
            "/v1/runners",
            json!({"name": "ci", "visibility": "organization"})
        )
        .await
        .0,
        StatusCode::FORBIDDEN,
        "display name never grants admin"
    );
    assert_eq!(
        s.post(
            ALICE,
            "/v1/runners",
            json!({"name": "x", "visibility": "public"})
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let (org, _) = s.register(CAROL, "build farm", "organization").await;
    let (_, body) = s.get(ALICE, "/v1/runners").await;
    assert_eq!(
        without_update_fields(&body["runners"]),
        json!([{"id": org, "name": "build farm", "visibility": "organization", "can_manage": false}])
    );
    assert_eq!(
        s.post(ALICE, &format!("/v1/runners/{org}/token"), json!({}))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        s.delete(ALICE, &format!("/v1/runners/{org}")).await.0,
        StatusCode::FORBIDDEN
    );
    let a = s.space(ALICE, "a").await;
    s.add(ALICE, &a, "bob").await;
    assert_eq!(s.attach(BOB, &a, &org).await.0, StatusCode::FORBIDDEN);
    assert_eq!(s.attach(ALICE, &a, &org).await.0, StatusCode::OK);
    // The admin manages the runner but is not an owner of Alice's Space.
    assert_eq!(s.attach(CAROL, &a, &org).await.0, StatusCode::NOT_FOUND);
    assert_eq!(
        s.attach(ALICE, &a, "rn-doesnotexist").await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn one_runner_serves_two_spaces_with_space_ids() {
    let s = Server::start().await;
    let (org, token) = s.register(CAROL, "shared", "organization").await;
    let a = s.space(ALICE, "a").await;
    let b = s.space(BOB, "b").await;
    let unattached = s.space(BOB, "c").await;
    assert_eq!(s.attach(ALICE, &a, &org).await.0, StatusCode::OK);
    assert_eq!(s.attach(BOB, &b, &org).await.0, StatusCode::OK);

    // An old runner binary (no protocol header) cannot use the new token.
    assert_eq!(
        s.claim_once(&token, None).await.0,
        StatusCode::UPGRADE_REQUIRED
    );
    assert_eq!(
        s.claim_once(&token, Some("1")).await.0,
        StatusCode::UPGRADE_REQUIRED
    );
    assert_eq!(
        s.claim_once(&token, Some("3")).await.0,
        StatusCode::UPGRADE_REQUIRED
    );
    // An unknown token is 401 whatever the header says.
    assert_eq!(
        s.claim_once("metro_rt_v2_00", Some("2")).await.0,
        StatusCode::UNAUTHORIZED
    );
    // Nothing queued yet.
    assert_eq!(
        s.claim_once(&token, Some("2")).await.0,
        StatusCode::NO_CONTENT
    );
    // A Space without a runner refuses Demo.
    assert_eq!(s.demo(BOB, &unattached).await, StatusCode::CONFLICT);

    assert_eq!(s.demo(ALICE, &a).await, StatusCode::ACCEPTED);
    let first = s.claim(&token, Some("2")).await;
    assert_eq!(s.demo(BOB, &b).await, StatusCode::ACCEPTED);
    let second = s.claim(&token, Some("2")).await;
    assert_eq!(first["space_id"], a.as_str());
    assert_eq!(second["space_id"], b.as_str());
    assert_eq!(first["tool"], "workspace.list");
    // The old header cannot answer either.
    assert_eq!(
        s.result(&token, None, &first, "x").await,
        StatusCode::UPGRADE_REQUIRED
    );
    assert_eq!(
        s.result(&token, Some("2"), &first, "in a").await,
        StatusCode::OK
    );
    assert_eq!(
        s.result(&token, Some("2"), &second, "in b").await,
        StatusCode::OK
    );
    let events = s.wait_for(ALICE, &a, |e| e["kind"] == "tool_result").await;
    assert!(
        events
            .iter()
            .any(|e| e["text"].as_str().unwrap().contains("in a"))
    );
    assert!(
        !events
            .iter()
            .any(|e| e["text"].as_str().unwrap().contains("in b"))
    );
    let events = s.wait_for(BOB, &b, |e| e["kind"] == "tool_result").await;
    assert!(
        events
            .iter()
            .any(|e| e["text"].as_str().unwrap().contains("in b"))
    );
    // Runner tokens never work on human endpoints.
    assert_eq!(s.get(&token, "/v1/me").await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(
        s.get("metro_pc_abc", "/v1/me").await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        s.claim_once("metro_pc_abc", Some("2")).await.0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn runner_cannot_answer_another_runners_job() {
    let s = Server::start().await;
    let (one, t1) = s.register(ALICE, "one", "personal").await;
    let (_two, t2) = s.register(ALICE, "two", "personal").await;
    let a = s.space(ALICE, "a").await;
    s.attach(ALICE, &a, &one).await;
    s.demo(ALICE, &a).await;
    let job = s.claim(&t1, Some("2")).await;
    // Runner two serves no Space: the job does not exist for it.
    assert_eq!(s.claim_once(&t2, Some("2")).await.0, StatusCode::NO_CONTENT);
    assert_eq!(
        s.result(&t2, Some("2"), &job, "forged").await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(s.result(&t1, Some("2"), &job, "real").await, StatusCode::OK);
}

#[tokio::test]
async fn detach_and_reassign_cancel_in_flight_jobs() {
    let s = Server::start().await;
    let (first, t1) = s.register(ALICE, "first", "personal").await;
    let (second, t2) = s.register(ALICE, "second", "personal").await;
    let a = s.space(ALICE, "a").await;
    s.attach(ALICE, &a, &first).await;
    s.demo(ALICE, &a).await;
    let job = s.claim(&t1, Some("2")).await;
    // Reassign while the job is claimed.
    assert_eq!(s.attach(ALICE, &a, &second).await.0, StatusCode::OK);
    s.wait_for(ALICE, &a, |e| e["kind"] == "error").await;
    // The late result goes nowhere, and does not reach the new runner either.
    assert_eq!(
        s.result(&t1, Some("2"), &job, "late").await,
        StatusCode::NOT_FOUND
    );
    // The new runner serves this Space now, so it learns only that the
    // job expired; it can never complete it.
    assert_eq!(
        s.result(&t2, Some("2"), &job, "late").await,
        StatusCode::GONE
    );
    assert_eq!(s.claim_once(&t2, Some("2")).await.0, StatusCode::NO_CONTENT);
    // The new runner serves new work.
    s.demo(ALICE, &a).await;
    let next = s.claim(&t2, Some("2")).await;
    assert_ne!(next["id"], job["id"]);
    assert_eq!(s.claim_once(&t1, Some("2")).await.0, StatusCode::NO_CONTENT);
    // Detach with a job claimed.
    assert_eq!(
        s.delete(ALICE, &format!("/v1/spaces/{a}/runner")).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        s.result(&t2, Some("2"), &next, "late").await,
        StatusCode::NOT_FOUND
    );
    let (_, body) = s.get(ALICE, &format!("/v1/spaces/{a}/runner")).await;
    assert_eq!(body, json!({"runner": null}));
    assert_eq!(s.demo(ALICE, &a).await, StatusCode::CONFLICT);
    // Detaching again is a no-op.
    assert_eq!(
        s.delete(ALICE, &format!("/v1/spaces/{a}/runner")).await.0,
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn rotation_and_deletion_revoke_the_token() {
    let s = Server::start().await;
    let (org, old) = s.register(CAROL, "ci", "organization").await;
    let a = s.space(ALICE, "a").await;
    s.attach(ALICE, &a, &org).await;
    s.demo(ALICE, &a).await;
    let job = s.claim(&old, Some("2")).await;
    let r = s
        .http
        .post(format!("{}/v1/runners/{org}/token", s.base))
        .bearer_auth(CAROL)
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(r.headers()["cache-control"], "no-store");
    let new = r.json::<Value>().await.unwrap()["token"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(new, old);
    assert_eq!(
        s.claim_once(&old, Some("2")).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        s.result(&old, Some("2"), &job, "late").await,
        StatusCode::UNAUTHORIZED
    );
    // The job claimed with the old token was cancelled, not handed over.
    assert_eq!(
        s.result(&new, Some("2"), &job, "late").await,
        StatusCode::GONE
    );
    assert_eq!(
        s.claim_once(&new, Some("2")).await.0,
        StatusCode::NO_CONTENT
    );
    // Still attached.
    let (_, body) = s.get(ALICE, &format!("/v1/spaces/{a}/runner")).await;
    assert_eq!(body["runner"]["id"], org.as_str());

    assert_eq!(
        s.delete(CAROL, &format!("/v1/runners/{org}")).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        s.claim_once(&new, Some("2")).await.0,
        StatusCode::UNAUTHORIZED
    );
    let (_, body) = s.get(ALICE, &format!("/v1/spaces/{a}/runner")).await;
    assert_eq!(body, json!({"runner": null}));
    let (_, body) = s.get(ALICE, "/v1/runners").await;
    assert!(ids(&body).is_empty());
    assert_eq!(
        s.delete(CAROL, &format!("/v1/runners/{org}")).await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn dedicated_tokens_keep_working_and_never_mix_with_registered_runners() {
    let s = Server::start().await;
    let a = s.space(ALICE, "a").await;
    let b = s.space(ALICE, "b").await;
    let (status, body) = s.dedicated_token(ALICE, &a).await;
    assert_eq!(status, StatusCode::OK);
    let dedicated = body["token"].as_str().unwrap().to_owned();
    assert!(dedicated.starts_with("metro_rt_") && !dedicated.starts_with("metro_rt_v2_"));
    let (_, body) = s.get(ALICE, &format!("/v1/spaces/{a}/runner")).await;
    assert_eq!(body["runner"]["kind"], "dedicated");
    assert_eq!(body["runner"]["id"], Value::Null);

    // The old wire format: no header, no space_id.
    s.demo(ALICE, &a).await;
    let job = s.claim(&dedicated, None).await;
    assert!(job.get("space_id").is_none(), "{job}");
    assert_eq!(
        s.result(&dedicated, None, &job, "old").await,
        StatusCode::OK
    );
    // A protocol 2 runner with a dedicated token also learns the Space.
    s.demo(ALICE, &a).await;
    let job = s.claim(&dedicated, Some("2")).await;
    assert_eq!(job["space_id"], a.as_str());
    assert_eq!(
        s.result(&dedicated, Some("2"), &job, "new").await,
        StatusCode::OK
    );
    // It never sees another Space's jobs.
    let (personal, rt) = s.register(ALICE, "p", "personal").await;
    s.attach(ALICE, &b, &personal).await;
    s.demo(ALICE, &b).await;
    assert_eq!(
        s.claim_once(&dedicated, None).await.0,
        StatusCode::NO_CONTENT
    );
    let job_b = s.claim(&rt, Some("2")).await;
    assert_eq!(
        s.result(&dedicated, None, &job_b, "x").await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(s.result(&rt, Some("2"), &job_b, "ok").await, StatusCode::OK);

    // A Space has one runner: neither kind replaces the other implicitly.
    assert_eq!(s.attach(ALICE, &a, &personal).await.0, StatusCode::CONFLICT);
    assert_eq!(s.dedicated_token(ALICE, &b).await.0, StatusCode::CONFLICT);
    assert_eq!(
        s.delete(ALICE, &format!("/v1/spaces/{b}/runner-token"))
            .await
            .0,
        StatusCode::CONFLICT
    );
    // DELETE .../runner revokes a dedicated token too.
    assert_eq!(
        s.delete(ALICE, &format!("/v1/spaces/{a}/runner")).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        s.claim_once(&dedicated, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(s.attach(ALICE, &a, &personal).await.0, StatusCode::OK);
}

#[tokio::test]
async fn attachments_survive_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("metro.sqlite3");
    let (a, token) = {
        let s = Server::start_with(Db::open(&path).unwrap()).await;
        let (rn, token) = s.register(ALICE, "p", "personal").await;
        let a = s.space(ALICE, "a").await;
        s.attach(ALICE, &a, &rn).await;
        (a, token)
    };
    let s = Server::start_with(Db::open(&path).unwrap()).await;
    s.demo(ALICE, &a).await;
    let job = s.claim(&token, Some("2")).await;
    assert_eq!(job["space_id"], a.as_str());
}

/// A runner view without the display-only `presence` and `update` objects
/// (covered in tests/updates.rs), which must be present on every view.
fn without_update_fields(v: &Value) -> Value {
    match v {
        Value::Array(items) => Value::Array(items.iter().map(without_update_fields).collect()),
        Value::Object(map) => {
            assert!(
                map["presence"].is_object() && map["update"].is_object(),
                "{v}"
            );
            let mut map = map.clone();
            map.remove("presence");
            map.remove("update");
            Value::Object(map)
        }
        other => other.clone(),
    }
}
