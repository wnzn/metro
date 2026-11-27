//! `workspace.shell` and `workspace.write` over real HTTP: shell policy
//! storage and access, the built-in marker, approvals (who may decide, first
//! decision wins, stop and runner changes cancel), classify on the runner
//! owner's System One connection, and the job heartbeat. A scripted model
//! drives the real Rig tool loop.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Json;
use axum::routing::post;
use metro_model_protocol::Capability;
use metro_server::auth::{AuthFailure, AuthFuture, HumanAuth, UserIdentity};
use metro_server::db::Db;
use metro_server::model_routing::{BoxFuture, Routing, RoutingBackend};
use metro_server::providers::{SelectedModel, UseError};
use metro_server::{AppState, Config, Limits, ModelConfig, router};
use reqwest::StatusCode;
use serde_json::{Value, json};

const ALICE: &str = "alice-token";
const BOB: &str = "bob-token";
/// Carol is an organization admin.
const CAROL: &str = "carol-token";
/// Dave is in no Space.
const DAVE: &str = "dave-token";
const PROTOCOL: &str = "X-Metro-Runner-Protocol";
const CLASSIFIER: &str = "pc_11111111111111111111111111111111";

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

/// System One for Alice's classifier only: commands containing `rm` are
/// unsafe, `maybe` is a weak safe, `boom` fails; anything else is safe.
#[derive(Default)]
struct FakeSystemOne {
    calls: Mutex<Vec<(String, Value)>>,
}

impl RoutingBackend for FakeSystemOne {
    fn system_one<'a>(
        &'a self,
        _state: &'a AppState,
        user: &'a str,
        _space: Option<&'a str>,
        id: &'a str,
        payload: Value,
        questions: Value,
    ) -> BoxFuture<'a, anyhow::Result<Value>> {
        Box::pin(async move {
            self.calls
                .lock()
                .unwrap()
                .push((user.to_owned(), payload.clone()));
            anyhow::ensure!(user == "alice" && id == CLASSIFIER, "not usable");
            assert!(questions.get("shell_risk").is_some(), "{questions}");
            let command = payload["command"].as_str().unwrap_or_default();
            anyhow::ensure!(!command.contains("boom"), "upstream failed");
            let (choice, p) = if command.contains("rm") {
                ("unsafe", 0.9)
            } else if command.contains("maybe") {
                ("safe", 0.55)
            } else {
                ("safe", 0.95)
            };
            Ok(
                json!({"model": "jev-test", "usage": {}, "answers": {"shell_risk":
                {"type": "choice", "choice": choice, "confidence": 0.8,
                 "probabilities": {choice: p}}}}),
            )
        })
    }

    fn visible_capability<'a>(
        &'a self,
        _state: &'a AppState,
        user: &'a str,
        id: &'a str,
    ) -> BoxFuture<'a, anyhow::Result<Option<Capability>>> {
        Box::pin(async move {
            Ok((user == "alice" && id == CLASSIFIER).then_some(Capability::SystemOne))
        })
    }

    fn chat_model<'a>(
        &'a self,
        _state: &'a AppState,
        _user: &'a str,
        _space: Option<&'a str>,
        _id: &'a str,
    ) -> BoxFuture<'a, Result<SelectedModel, UseError>> {
        Box::pin(async { Err(UseError::NotFound) })
    }
}

struct Server {
    base: String,
    http: reqwest::Client,
    model: Arc<Mutex<Vec<Value>>>,
    system_one: Arc<FakeSystemOne>,
}

/// The scripted model: the first turn calls `workspace_shell` (or the tool
/// named in the message as `tool=<name> args=<json>`), the next one quotes
/// the tool's answer.
async fn mock_model(seen: Arc<Mutex<Vec<Value>>>) -> ModelConfig {
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        post(move |Json(body): Json<Value>| {
            let seen = seen.clone();
            async move {
                seen.lock().unwrap().push(body.clone());
                let messages = body["messages"].as_array().cloned().unwrap_or_default();
                let tool_reply = messages.iter().rev().find(|m| m["role"] == "tool");
                let message = match tool_reply {
                    Some(reply) => {
                        let text = match &reply["content"] {
                            Value::String(s) => s.clone(),
                            other => other.to_string(),
                        };
                        json!({"role": "assistant", "content": format!("tool said: {text}")})
                    }
                    None => {
                        // Only the newest person's turn picks the command.
                        let all = messages
                            .iter()
                            .rev()
                            .find(|m| m["role"] == "user")
                            .map(|m| m["content"].to_string())
                            .unwrap_or_default();
                        let (name, args) = if all.contains("please write") {
                            (
                                "workspace_write",
                                json!({"path": "notes/a.md", "content": "hi"}),
                            )
                        } else {
                            let cmd = ["rm -rf build", "maybe ls", "boom", "sleep"]
                                .into_iter()
                                .find(|c| all.contains(c))
                                .unwrap_or("ls -la");
                            ("workspace_shell", json!({"command": cmd}))
                        };
                        json!({"role": "assistant", "content": null, "tool_calls": [{
                            "id": "call_1", "type": "function",
                            "function": {"name": name, "arguments": args.to_string()}}]})
                    }
                };
                let finish = if message["tool_calls"].is_array() {
                    "tool_calls"
                } else {
                    "stop"
                };
                Json(json!({
                    "id": "chatcmpl-1", "object": "chat.completion", "created": 0, "model": "mock",
                    "choices": [{"index": 0, "message": message, "finish_reason": finish}],
                    "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                }))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    ModelConfig {
        base_url,
        model: "mock".into(),
        api_key: "test-key".into(),
    }
}

impl Server {
    async fn start() -> Self {
        Self::start_with(Db::open_in_memory().unwrap()).await
    }

    async fn start_with(db: Db) -> Self {
        let user = |id: &str, name: &str| UserIdentity {
            id: id.to_owned(),
            actor: name.to_owned(),
        };
        let auth = Arc::new(FakeAuth(HashMap::from([
            (ALICE.to_owned(), user("alice", "Alice")),
            (BOB.to_owned(), user("bob", "Bob")),
            (CAROL.to_owned(), user("carol", "Carol")),
            (DAVE.to_owned(), user("dave", "Dave")),
        ])));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let config = Config {
            bind: "127.0.0.1:0".parse().unwrap(),
            db_path: ":memory:".into(),
            model: Some(mock_model(seen.clone()).await),
            limits: Limits {
                claim_timeout: Duration::from_secs(5),
                run_timeout: Duration::from_secs(5),
                agent_timeout: Duration::from_secs(30),
                ..Limits::default()
            },
            org_admins: vec!["carol".to_owned()],
        };
        let mut state = AppState::new(config, db, auth).await.unwrap();
        let system_one = Arc::new(FakeSystemOne::default());
        state.routing = Routing::new(system_one.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, router(state)).await.unwrap() });
        Self {
            base,
            http: reqwest::Client::new(),
            model: seen,
            system_one,
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

    async fn space(&self, token: &str, name: &str) -> String {
        let (status, body) = self.post(token, "/v1/spaces", json!({"name": name})).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        body["id"].as_str().unwrap().to_owned()
    }

    async fn add(&self, owner: &str, sid: &str, user: &str) {
        let (status, body) = self
            .post(
                owner,
                &format!("/v1/spaces/{sid}/members"),
                json!({"user_id": user}),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }

    async fn dedicated(&self, owner: &str, sid: &str) -> String {
        let (status, body) = self
            .post(owner, &format!("/v1/spaces/{sid}/runner-token"), json!({}))
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body["token"].as_str().unwrap().to_owned()
    }

    async fn register(&self, token: &str, body: Value) -> (String, String) {
        let (status, body) = self.post(token, "/v1/runners", body).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        (
            body["runner"]["id"].as_str().unwrap().to_owned(),
            body["token"].as_str().unwrap().to_owned(),
        )
    }

    async fn attach(&self, owner: &str, sid: &str, rid: &str) -> StatusCode {
        self.put(
            owner,
            &format!("/v1/spaces/{sid}/runner"),
            json!({"runner_id": rid}),
        )
        .await
        .0
    }

    /// Give the Space's default agent the shell and write tools.
    async fn allow_tools(&self, sid: &str) {
        let (_, body) = self.get(ALICE, &format!("/v1/spaces/{sid}/agents")).await;
        assert!(
            !body["default_tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t == "workspace.shell" || t == "workspace.write"),
            "{body}"
        );
        let catalog = body["tools"].as_array().unwrap();
        assert!(catalog.iter().any(|t| t == "workspace.shell"));
        let aid = body["agents"][0]["id"].as_str().unwrap().to_owned();
        let (status, body) = self
            .send(
                reqwest::Method::PATCH,
                ALICE,
                &format!("/v1/spaces/{sid}/agents/{aid}"),
                Some(json!({"tools": ["workspace.list", "workspace.shell", "workspace.write"]})),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    async fn ask(&self, token: &str, sid: &str, text: &str) {
        let (status, body) = self
            .post(
                token,
                &format!("/v1/spaces/{sid}/rooms/lobby/messages"),
                json!({"text": text, "invoke_agent": true}),
            )
            .await;
        assert!(status.is_success(), "{body}");
    }

    async fn events(&self, token: &str, sid: &str) -> Vec<Value> {
        let (status, body) = self
            .get(token, &format!("/v1/spaces/{sid}/rooms/lobby/events"))
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body["events"].as_array().unwrap().clone()
    }

    async fn wait_for(&self, sid: &str, pred: impl Fn(&Value) -> bool) -> Vec<Value> {
        for _ in 0..400 {
            let events = self.events(ALICE, sid).await;
            if events.iter().any(&pred) {
                return events;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("no matching event: {:#?}", self.events(ALICE, sid).await);
    }

    async fn approval(&self, sid: &str) -> Value {
        let events = self.wait_for(sid, |e| e["kind"] == "shell_approval").await;
        let e = events
            .iter()
            .rev()
            .find(|e| e["kind"] == "shell_approval")
            .unwrap();
        assert_eq!(e["actor"], "system");
        assert!(e["activity"]["id"].is_string(), "{e}");
        serde_json::from_str(e["text"].as_str().unwrap()).unwrap()
    }

    async fn resolved(&self, sid: &str, id: &str) -> Value {
        let events = self
            .wait_for(sid, |e| {
                e["kind"] == "shell_approval_resolved" && e["text"].as_str().unwrap().contains(id)
            })
            .await;
        let e = events
            .iter()
            .find(|e| {
                e["kind"] == "shell_approval_resolved" && e["text"].as_str().unwrap().contains(id)
            })
            .unwrap();
        serde_json::from_str(e["text"].as_str().unwrap()).unwrap()
    }

    async fn decide(
        &self,
        token: &str,
        sid: &str,
        id: &str,
        decision: &str,
    ) -> (StatusCode, Value) {
        self.post(
            token,
            &format!("/v1/spaces/{sid}/shell-approvals/{id}"),
            json!({"decision": decision}),
        )
        .await
    }

    async fn runner_post(&self, token: &str, path: &str, body: Value) -> (StatusCode, Value) {
        let r = self
            .http
            .post(format!("{}{path}", self.base))
            .bearer_auth(token)
            .header(PROTOCOL, "2")
            .json(&body)
            .send()
            .await
            .unwrap();
        let status = r.status();
        (status, r.json().await.unwrap_or(Value::Null))
    }

    async fn claim_once(&self, token: &str) -> (StatusCode, Value) {
        self.runner_post(token, "/v1/runner/claim", json!({})).await
    }

    async fn claim(&self, token: &str) -> Value {
        for _ in 0..400 {
            match self.claim_once(token).await {
                (StatusCode::OK, job) => return job,
                (StatusCode::NO_CONTENT, _) => tokio::time::sleep(Duration::from_millis(20)).await,
                (other, body) => panic!("claim returned {other}: {body}"),
            }
        }
        panic!("no job to claim");
    }

    async fn result(&self, token: &str, job: &Value, ok: bool, output: &str) -> StatusCode {
        let id = job["id"].as_str().unwrap();
        self.runner_post(
            token,
            &format!("/v1/runner/jobs/{id}/result"),
            json!({"ok": ok, "output": output}),
        )
        .await
        .0
    }

    async fn heartbeat(&self, token: &str, job: &Value) -> StatusCode {
        let id = job["id"].as_str().unwrap();
        self.runner_post(token, &format!("/v1/runner/jobs/{id}/heartbeat"), json!({}))
            .await
            .0
    }

    async fn completed(&self, sid: &str) -> String {
        let events = self
            .wait_for(sid, |e| {
                e["kind"] == "agent"
                    || e["activity"]["phase"] == "cancelled"
                    || e["activity"]["phase"] == "failed"
            })
            .await;
        events
            .iter()
            .rev()
            .find(|e| e["kind"] == "agent")
            .map(|e| e["text"].as_str().unwrap().to_owned())
            .unwrap_or_default()
    }

    /// Wait until no run is running, so the next message starts its own.
    async fn idle(&self) {
        for _ in 0..400 {
            let (_, runs) = self.get(ALICE, "/v1/runs").await;
            if runs["runs"]
                .as_array()
                .unwrap()
                .iter()
                .all(|r| r["status"] != "running")
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("a run is still running");
    }

    async fn agent_replies(&self, sid: &str) -> Vec<String> {
        self.events(ALICE, sid)
            .await
            .iter()
            .filter(|e| e["kind"] == "agent")
            .map(|e| e["text"].as_str().unwrap().to_owned())
            .collect()
    }

    /// The text of the reply after the first `before` replies.
    async fn reply_after(&self, sid: &str, before: usize) -> String {
        for _ in 0..400 {
            let replies = self.agent_replies(sid).await;
            if replies.len() > before {
                return replies[before].clone();
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("no new reply: {:#?}", self.events(ALICE, sid).await);
    }

    fn model_saw(&self, needle: &str) -> bool {
        self.model
            .lock()
            .unwrap()
            .iter()
            .any(|r| r["messages"].to_string().contains(needle))
    }
}

/// A Space owned by Alice with Bob as a member and a dedicated runner.
async fn dedicated_space(s: &Server) -> (String, String) {
    let sid = s.space(ALICE, "Team").await;
    s.add(ALICE, &sid, "bob").await;
    let token = s.dedicated(ALICE, &sid).await;
    s.allow_tools(&sid).await;
    (sid, token)
}

async fn set_dedicated(s: &Server, sid: &str, mode: &str, approver: &str) {
    let (status, body) = s
        .put(
            ALICE,
            &format!("/v1/spaces/{sid}/shell-policy"),
            json!({"shell_mode": mode, "approver": approver}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["shell"]["shell_mode"], mode);
}

#[tokio::test]
async fn approve_queues_only_after_an_eligible_member_decides() {
    let s = Server::start().await;
    let (sid, token) = dedicated_space(&s).await;
    let (_, runner) = s.get(BOB, &format!("/v1/spaces/{sid}/runner")).await;
    assert_eq!(
        runner["shell"],
        json!({"shell_mode": "approve", "approver": "owner", "effective_mode": "approve",
               "downgraded_reason": null, "runner_name": "Dedicated Space runner", "source": "dedicated"})
    );

    s.ask(BOB, &sid, "@agent list the files").await;
    let a = s.approval(&sid).await;
    let id = a["approval_id"].as_str().unwrap().to_owned();
    assert_eq!(a["command"], "ls -la");
    assert_eq!(a["requested_by"], json!({"id": "bob", "name": "Bob"}));
    assert_eq!(a["reason_code"], "policy");
    assert_eq!(a["approver"], "owner");
    assert!(a["expires_at"].is_string());
    // Nothing reaches the runner before a decision.
    assert_eq!(s.claim_once(&token).await.0, StatusCode::NO_CONTENT);

    let (_, list) = s
        .get(BOB, &format!("/v1/spaces/{sid}/shell-approvals"))
        .await;
    assert_eq!(list["approvals"][0]["approval_id"], id.as_str());
    assert_eq!(list["approvals"][0]["can_decide"], false);
    let (_, list) = s
        .get(ALICE, &format!("/v1/spaces/{sid}/shell-approvals"))
        .await;
    assert_eq!(list["approvals"][0]["can_decide"], true);
    assert_eq!(list["approvals"][0]["room_id"], "lobby");
    assert_eq!(
        s.get(DAVE, &format!("/v1/spaces/{sid}/shell-approvals"))
            .await
            .0,
        StatusCode::NOT_FOUND
    );

    // Outsiders and unknown ids: 404; a member who isn't the approver: 403.
    assert_eq!(
        s.decide(DAVE, &sid, &id, "approve").await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        s.decide(CAROL, &sid, &id, "approve").await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        s.decide(ALICE, &sid, "sa-0000000000000000", "approve")
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        s.decide(ALICE, &sid, "nope", "approve").await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        s.decide(BOB, &sid, &id, "approve").await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        s.post(
            ALICE,
            &format!("/v1/spaces/{sid}/shell-approvals/{id}"),
            json!({"decision": "maybe"})
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );

    // Two decisions at once: exactly one wins.
    let (first, second) = tokio::join!(
        s.decide(ALICE, &sid, &id, "approve"),
        s.decide(ALICE, &sid, &id, "deny")
    );
    let mut statuses = [first.0, second.0];
    statuses.sort();
    assert_eq!(statuses, [StatusCode::OK, StatusCode::CONFLICT]);
    let won = if first.0 == StatusCode::OK {
        first.1
    } else {
        second.1
    };
    assert_eq!(won["by"], json!({"id": "alice", "name": "Alice"}));
    let r = s.resolved(&sid, &id).await;
    assert_eq!(r["decision"], won["decision"]);
    assert_eq!(
        s.decide(ALICE, &sid, &id, "approve").await.0,
        StatusCode::CONFLICT
    );

    if won["decision"] == "approve" {
        let job = s.claim(&token).await;
        assert_eq!(job["tool"], "workspace.shell");
        assert_eq!(
            job["arguments"],
            json!({"command": "ls -la", "timeout_secs": 120})
        );
        assert_eq!(s.heartbeat(&token, &job).await, StatusCode::OK);
        assert_eq!(
            s.result(&token, &job, true, "exit: 0\n--- stdout ---\nfile\n")
                .await,
            StatusCode::OK
        );
        assert!(s.completed(&sid).await.contains("file"));
        assert_eq!(s.heartbeat(&token, &job).await, StatusCode::GONE);
    } else {
        assert!(s.completed(&sid).await.contains("denied by Alice"));
    }
    let (_, list) = s
        .get(ALICE, &format!("/v1/spaces/{sid}/shell-approvals"))
        .await;
    assert_eq!(list["approvals"], json!([]));
    // Approval events are run activity, never model history.
    assert!(!s.model_saw("shell_approval"));
    assert!(!s.model_saw(&id));
}

#[tokio::test]
async fn members_approver_lets_any_member_deny_and_the_model_hears_it() {
    let s = Server::start().await;
    let (sid, token) = dedicated_space(&s).await;
    // Only the Space owner sets a dedicated runner's policy.
    assert_eq!(
        s.put(
            BOB,
            &format!("/v1/spaces/{sid}/shell-policy"),
            json!({"shell_mode": "allow", "approver": "owner"})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        s.put(
            DAVE,
            &format!("/v1/spaces/{sid}/shell-policy"),
            json!({"shell_mode": "allow", "approver": "owner"})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    set_dedicated(&s, &sid, "approve", "members").await;
    s.ask(ALICE, &sid, "@agent clean up: rm -rf build").await;
    let a = s.approval(&sid).await;
    let id = a["approval_id"].as_str().unwrap();
    assert_eq!(s.decide(BOB, &sid, id, "deny").await.0, StatusCode::OK);
    assert!(
        s.completed(&sid)
            .await
            .contains("the command was denied by Bob")
    );
    assert_eq!(s.resolved(&sid, id).await["decision"], "deny");
    assert_eq!(s.claim_once(&token).await.0, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn off_refuses_without_a_job_and_allow_queues_at_once() {
    let s = Server::start().await;
    let (sid, token) = dedicated_space(&s).await;
    set_dedicated(&s, &sid, "off", "owner").await;
    s.ask(ALICE, &sid, "@agent list").await;
    assert!(s.completed(&sid).await.contains("turned off"));
    assert_eq!(s.claim_once(&token).await.0, StatusCode::NO_CONTENT);
    assert!(
        !s.events(ALICE, &sid)
            .await
            .iter()
            .any(|e| e["kind"] == "shell_approval")
    );
    s.idle().await;

    set_dedicated(&s, &sid, "allow", "owner").await;
    s.ask(ALICE, &sid, "@agent sleep a while").await;
    let job = s.claim(&token).await;
    assert_eq!(job["arguments"]["command"], "sleep");
    // Stopping the run makes the heartbeat tell the runner to stop.
    let run = s
        .wait_for(&sid, |e| {
            e["kind"] == "tool_call" && e["text"].as_str().unwrap().contains("sleep")
        })
        .await;
    let run = run.iter().rev().find(|e| e["kind"] == "tool_call").unwrap()["activity"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(s.heartbeat(&token, &job).await, StatusCode::OK);
    assert_eq!(
        s.post(ALICE, &format!("/v1/runs/{run}/stop"), json!({}))
            .await
            .0,
        StatusCode::OK
    );
    let mut gone = false;
    for _ in 0..100 {
        if s.heartbeat(&token, &job).await == StatusCode::GONE {
            gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(gone, "the stopped run's job is still live");
    assert_eq!(s.result(&token, &job, true, "late").await, StatusCode::GONE);
}

#[tokio::test]
async fn stopping_the_run_or_changing_the_runner_cancels_a_pending_approval() {
    let s = Server::start().await;
    let (sid, _token) = dedicated_space(&s).await;
    s.ask(ALICE, &sid, "@agent list").await;
    let a = s.approval(&sid).await;
    let id = a["approval_id"].as_str().unwrap().to_owned();
    let run = a["run_id"].as_str().unwrap();
    assert_eq!(
        s.post(ALICE, &format!("/v1/runs/{run}/stop"), json!({}))
            .await
            .0,
        StatusCode::OK
    );
    let r = s.resolved(&sid, &id).await;
    assert_eq!(
        r,
        json!({"approval_id": id, "decision": "cancelled", "by": null})
    );
    assert_eq!(
        s.decide(ALICE, &sid, &id, "approve").await.0,
        StatusCode::CONFLICT
    );
    s.idle().await;

    // A registered runner replaces the dedicated token while one waits.
    s.ask(ALICE, &sid, "@agent list again").await;
    let events = s
        .wait_for(&sid, |e| {
            e["kind"] == "shell_approval" && !e["text"].as_str().unwrap().contains(&id)
        })
        .await;
    let second: Value = serde_json::from_str(
        events
            .iter()
            .rev()
            .find(|e| e["kind"] == "shell_approval")
            .unwrap()["text"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    let second = second["approval_id"].as_str().unwrap().to_owned();
    assert_eq!(
        s.send(
            reqwest::Method::DELETE,
            ALICE,
            &format!("/v1/spaces/{sid}/runner"),
            Some(json!({}))
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    let (rid, rtoken) = s
        .register(ALICE, json!({"name": "laptop", "visibility": "personal"}))
        .await;
    assert_eq!(s.attach(ALICE, &sid, &rid).await, StatusCode::OK);
    assert_eq!(s.resolved(&sid, &second).await["decision"], "cancelled");
    assert_eq!(
        s.decide(ALICE, &sid, &second, "approve").await.0,
        StatusCode::CONFLICT
    );
    assert!(s.completed(&sid).await.contains("cancelled"));
    assert_eq!(s.claim_once(&rtoken).await.0, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn classify_uses_the_runner_owners_system_one_and_falls_back_to_approval() {
    let s = Server::start().await;
    let sid = s.space(ALICE, "Rated").await;
    s.add(ALICE, &sid, "bob").await;
    let (rid, token) = s
        .register(ALICE, json!({"name": "laptop", "visibility": "personal"}))
        .await;
    assert_eq!(s.attach(ALICE, &sid, &rid).await, StatusCode::OK);
    s.allow_tools(&sid).await;
    let (status, body) = s
        .put(
            ALICE,
            &format!("/v1/runners/{rid}/shell-policy"),
            json!({"shell_mode": "classify", "approver": "owner"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // No System One connection yet: an approval, with the reason.
    s.ask(BOB, &sid, "@agent list").await;
    let a = s.approval(&sid).await;
    assert_eq!(a["reason_code"], "classify_failed");
    assert!(
        a["reason"]
            .as_str()
            .unwrap()
            .contains("no System One connection"),
        "{a}"
    );
    assert_eq!(
        s.decide(ALICE, &sid, a["approval_id"].as_str().unwrap(), "deny")
            .await
            .0,
        StatusCode::OK
    );
    assert!(s.reply_after(&sid, 0).await.contains("denied"));
    s.idle().await;

    let (status, body) = s
        .put(
            ALICE,
            "/v1/model-routing",
            json!({"classifier_id": CLASSIFIER}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // Safe: queued without an approval. Bob asked, Alice's connection rated it.
    s.ask(BOB, &sid, "@agent show me what's there").await;
    let job = s.claim(&token).await;
    assert_eq!(job["arguments"]["command"], "ls -la");
    assert_eq!(
        s.result(&token, &job, true, "exit: 0").await,
        StatusCode::OK
    );
    assert!(s.reply_after(&sid, 1).await.contains("exit: 0"));
    s.idle().await;
    {
        let calls = s.system_one.calls.lock().unwrap();
        let (user, payload) = calls.last().unwrap();
        assert_eq!(user, "alice");
        let keys: Vec<&String> = payload.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["command", "request", "space"]);
        assert_eq!(payload["space"], "Rated");
        assert!(payload["request"].as_str().unwrap().contains("show me"));
    }

    for (text, code) in [
        ("@agent rm -rf build", "classify_unsafe"),
        ("@agent maybe ls", "classify_unsure"),
        ("@agent boom", "classify_failed"),
    ] {
        let before = s.agent_replies(&sid).await.len();
        s.ask(BOB, &sid, text).await;
        let command = format!("\"command\":\"{}\"", text.trim_start_matches("@agent "));
        let this = |e: &Value| {
            let t = e["text"].as_str().unwrap();
            e["kind"] == "shell_approval" && t.contains(code) && t.contains(&command)
        };
        let events = s.wait_for(&sid, this).await;
        let a: Value = serde_json::from_str(
            events.iter().find(|e| this(e)).unwrap()["text"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        let id = a["approval_id"].as_str().unwrap();
        assert_eq!(
            s.claim_once(&token).await.0,
            StatusCode::NO_CONTENT,
            "{text}"
        );
        assert_eq!(s.decide(ALICE, &sid, id, "deny").await.0, StatusCode::OK);
        assert!(
            s.reply_after(&sid, before)
                .await
                .contains("denied by Alice"),
            "{text}"
        );
        s.idle().await;
    }
}

#[tokio::test]
async fn runner_policies_are_for_their_managers_and_built_in_allow_needs_one_space() {
    let s = Server::start().await;
    let a = s.space(ALICE, "A").await;
    let b = s.space(ALICE, "B").await;
    s.add(ALICE, &a, "bob").await;
    // Only personal runners can be built in.
    assert_eq!(
        s.post(
            CAROL,
            "/v1/runners",
            json!({"name": "x", "visibility": "organization", "built_in": true})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let (rid, _) = s
        .register(
            ALICE,
            json!({"name": "This computer", "visibility": "personal", "built_in": true}),
        )
        .await;
    let (_, list) = s.get(ALICE, "/v1/runners").await;
    assert_eq!(list["runners"][0]["built_in"], true);
    let path = format!("/v1/runners/{rid}/shell-policy");
    let (status, view) = s.get(ALICE, &path).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(
        view["default"],
        json!({"shell_mode": "approve", "approver": "owner"})
    );
    assert_eq!(view["spaces"], json!([]));
    assert_eq!(s.get(BOB, &path).await.0, StatusCode::NOT_FOUND);

    let (status, view) = s
        .put(
            ALICE,
            &path,
            json!({"shell_mode": "allow", "approver": "members"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(
        s.put(
            ALICE,
            &path,
            json!({"shell_mode": "sometimes", "approver": "owner"})
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        s.put(
            BOB,
            &path,
            json!({"shell_mode": "off", "approver": "owner"})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );

    assert_eq!(s.attach(ALICE, &a, &rid).await, StatusCode::OK);
    let (_, runner) = s.get(BOB, &format!("/v1/spaces/{a}/runner")).await;
    assert_eq!(runner["shell"]["effective_mode"], "allow");
    assert_eq!(runner["shell"]["approver"], "members");
    assert_eq!(runner["shell"]["source"], "runner_default");
    assert_eq!(runner["shell"]["runner_name"], "This computer");
    // A second Space: allow stops applying to both.
    assert_eq!(s.attach(ALICE, &b, &rid).await, StatusCode::OK);
    let (_, runner) = s.get(BOB, &format!("/v1/spaces/{a}/runner")).await;
    assert_eq!(runner["shell"]["shell_mode"], "allow");
    assert_eq!(runner["shell"]["effective_mode"], "approve");
    assert_eq!(
        runner["shell"]["downgraded_reason"],
        "built_in_multiple_spaces"
    );

    // Overrides: per attached Space, null inherits; Bob can't see names he can't read.
    let over = format!("/v1/runners/{rid}/spaces/{b}/shell-policy");
    let (status, view) = s
        .put(ALICE, &over, json!({"shell_mode": "off", "approver": null}))
        .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    let spaces = view["spaces"].as_array().unwrap();
    let vb = spaces.iter().find(|x| x["space_id"] == b.as_str()).unwrap();
    assert_eq!(vb["shell_mode"], "off");
    assert_eq!(vb["approver"], Value::Null);
    assert_eq!(vb["effective_mode"], "off");
    assert_eq!(vb["effective_approver"], "members");
    assert_eq!(vb["space_name"], "B");
    let other = s.space(ALICE, "C").await;
    assert_eq!(
        s.put(
            ALICE,
            &format!("/v1/runners/{rid}/spaces/{other}/shell-policy"),
            json!({"shell_mode": "off", "approver": null})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        s.put(ALICE, &over, json!({"shell_mode": "off"})).await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    // Detaching drops the override.
    assert_eq!(
        s.send(
            reqwest::Method::DELETE,
            ALICE,
            &format!("/v1/spaces/{b}/runner"),
            Some(json!({}))
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(s.attach(ALICE, &b, &rid).await, StatusCode::OK);
    let (_, runner) = s.get(ALICE, &format!("/v1/spaces/{b}/runner")).await;
    assert_eq!(runner["shell"]["source"], "runner_default");
    // Back to one Space: allow applies again.
    assert_eq!(
        s.send(
            reqwest::Method::DELETE,
            ALICE,
            &format!("/v1/spaces/{b}/runner"),
            Some(json!({}))
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    let (_, runner) = s.get(BOB, &format!("/v1/spaces/{a}/runner")).await;
    assert_eq!(runner["shell"]["effective_mode"], "allow");
    // A dedicated policy can't be set while a registered runner serves the Space.
    assert_eq!(
        s.put(
            ALICE,
            &format!("/v1/spaces/{a}/shell-policy"),
            json!({"shell_mode": "allow", "approver": "owner"})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );

    // Reconnect can set or clear the marker; an absent field keeps it.
    let token_path = format!("/v1/runners/{rid}/token");
    assert_eq!(
        s.post(ALICE, &token_path, json!({"built_in": false}))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        s.get(ALICE, "/v1/runners").await.1["runners"][0]["built_in"],
        false
    );
    assert_eq!(
        s.post(ALICE, &token_path, json!({})).await.0,
        StatusCode::OK
    );
    assert_eq!(
        s.get(ALICE, "/v1/runners").await.1["runners"][0]["built_in"],
        false
    );
    assert_eq!(
        s.post(ALICE, &token_path, json!({"built_in": true}))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        s.get(ALICE, "/v1/runners").await.1["runners"][0]["built_in"],
        true
    );
    assert_eq!(
        s.post(ALICE, &token_path, json!({"x": 1})).await.0,
        StatusCode::BAD_REQUEST
    );

    // Organization runners: admins manage the policy; classify is refused.
    let (org, _) = s
        .register(CAROL, json!({"name": "farm", "visibility": "organization"}))
        .await;
    let org_path = format!("/v1/runners/{org}/shell-policy");
    assert_eq!(s.get(ALICE, &org_path).await.0, StatusCode::FORBIDDEN);
    assert_eq!(
        s.put(
            CAROL,
            &org_path,
            json!({"shell_mode": "classify", "approver": "owner"})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        s.put(
            CAROL,
            &org_path,
            json!({"shell_mode": "approve", "approver": "owner"})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        s.post(
            CAROL,
            &format!("/v1/runners/{org}/token"),
            json!({"built_in": true})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let d = s.space(ALICE, "D").await;
    assert_eq!(s.attach(ALICE, &d, &org).await, StatusCode::OK);
    let (_, view) = s.get(CAROL, &org_path).await;
    // Carol isn't a member of D: no name.
    assert_eq!(view["spaces"][0]["space_id"], d.as_str());
    assert_eq!(view["spaces"][0]["space_name"], Value::Null);
    assert_eq!(
        s.put(
            CAROL,
            &format!("/v1/runners/{org}/spaces/{d}/shell-policy"),
            json!({"shell_mode": "classify", "approver": null})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn org_admins_approve_only_in_spaces_they_belong_to() {
    let s = Server::start().await;
    let sid = s.space(ALICE, "Org").await;
    s.add(ALICE, &sid, "bob").await;
    let (org, token) = s
        .register(CAROL, json!({"name": "farm", "visibility": "organization"}))
        .await;
    assert_eq!(s.attach(ALICE, &sid, &org).await, StatusCode::OK);
    s.allow_tools(&sid).await;
    s.ask(BOB, &sid, "@agent list").await;
    let a = s.approval(&sid).await;
    let id = a["approval_id"].as_str().unwrap();
    // Not a member: no admin bypass. The Space owner isn't the approver.
    assert_eq!(
        s.decide(CAROL, &sid, id, "approve").await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        s.decide(ALICE, &sid, id, "approve").await.0,
        StatusCode::FORBIDDEN
    );
    s.add(ALICE, &sid, "carol").await;
    assert_eq!(s.decide(CAROL, &sid, id, "approve").await.0, StatusCode::OK);
    let job = s.claim(&token).await;
    assert_eq!(job["tool"], "workspace.shell");
    assert_eq!(job["space_id"], sid.as_str());
    assert_eq!(
        s.result(&token, &job, true, "exit: 0").await,
        StatusCode::OK
    );
    s.completed(&sid).await;
}

#[tokio::test]
async fn write_needs_no_approval_and_both_tools_are_opt_in() {
    let s = Server::start().await;
    let sid = s.space(ALICE, "Docs").await;
    let token = s.dedicated(ALICE, &sid).await;
    // Without the tools on the agent, the model is never offered them.
    s.ask(ALICE, &sid, "@agent please write").await;
    s.completed(&sid).await;
    s.idle().await;
    {
        let reqs = s.model.lock().unwrap();
        let names = reqs[0]["tools"].to_string();
        assert!(
            names.contains("workspace_list")
                && !names.contains("workspace_shell")
                && !names.contains("workspace_write"),
            "{names}"
        );
    }
    s.allow_tools(&sid).await;
    s.ask(ALICE, &sid, "@agent please write").await;
    let job = s.claim(&token).await;
    assert_eq!(job["tool"], "workspace.write");
    assert_eq!(
        job["arguments"],
        json!({"path": "notes/a.md", "content": "hi"})
    );
    assert_eq!(
        s.result(&token, &job, true, "wrote 2 bytes to notes/a.md")
            .await,
        StatusCode::OK
    );
    s.wait_for(&sid, |e| {
        e["kind"] == "agent" && e["text"].as_str().unwrap().contains("wrote 2 bytes")
    })
    .await;
    assert!(
        !s.events(ALICE, &sid)
            .await
            .iter()
            .any(|e| e["kind"] == "shell_approval")
    );
}

#[tokio::test]
async fn pending_approvals_expire_at_restart_and_go_with_their_space() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("metro.sqlite3");
    let (sid, id) = {
        let s = Server::start_with(Db::open(&path).unwrap()).await;
        let (sid, _) = dedicated_space(&s).await;
        s.ask(ALICE, &sid, "@agent list").await;
        let a = s.approval(&sid).await;
        (sid, a["approval_id"].as_str().unwrap().to_owned())
    };
    let s = Server::start_with(Db::open(&path).unwrap()).await;
    let r = s.resolved(&sid, &id).await;
    assert_eq!(r["decision"], "expired");
    assert_eq!(
        s.decide(ALICE, &sid, &id, "approve").await.0,
        StatusCode::CONFLICT
    );
    // Once only.
    drop(s);
    let s = Server::start_with(Db::open(&path).unwrap()).await;
    let n = s
        .events(ALICE, &sid)
        .await
        .iter()
        .filter(|e| e["kind"] == "shell_approval_resolved")
        .count();
    assert_eq!(n, 1);
    let (status, _) = s
        .send(
            reqwest::Method::DELETE,
            ALICE,
            &format!("/v1/spaces/{sid}"),
            Some(json!({"confirm_name": "Team"})),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}
