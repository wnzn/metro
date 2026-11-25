//! End-to-end tests over real HTTP: people in private Spaces, per-Space
//! runners, personal Chat, mentions, reactions, runs, threads, and a mock
//! OpenAI-compatible model endpoint driving the real Rig tool loop.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Json;
use axum::routing::post;
use metro_server::auth::{AuthFailure, AuthFuture, HumanAuth, UserIdentity};
use metro_server::db::Db;
use metro_server::{AppState, Config, Limits, ModelConfig, router};
use reqwest::StatusCode;
use serde_json::{Value, json};

const ALICE: &str = "alice-token";
const BOB: &str = "bob-token";
const CAROL: &str = "carol-token";

struct FakeAuth {
    users: HashMap<String, UserIdentity>,
    namespace: String,
}

impl HumanAuth for FakeAuth {
    fn identity<'a>(&'a self, bearer: &'a str) -> AuthFuture<'a> {
        Box::pin(async move {
            if bearer == "idp-down" {
                return Err(AuthFailure::Unavailable("jwks fetch failed".to_owned()));
            }
            self.users
                .get(bearer)
                .cloned()
                .ok_or_else(|| AuthFailure::Rejected("unknown token".to_owned()))
        })
    }
    fn namespace(&self) -> &str {
        &self.namespace
    }
}

fn fake_auth(namespace: &str) -> Arc<FakeAuth> {
    let user = |id: &str, actor: &str| UserIdentity {
        id: id.to_owned(),
        actor: actor.to_owned(),
    };
    Arc::new(FakeAuth {
        users: HashMap::from([
            (ALICE.to_owned(), user("alice", "Alice")),
            (BOB.to_owned(), user("bob", "Bob")),
            (CAROL.to_owned(), user("carol", "Carol")),
        ]),
        namespace: namespace.to_owned(),
    })
}

fn config(model: Option<ModelConfig>) -> Config {
    Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        db_path: ":memory:".into(),
        model,
        limits: Limits {
            claim_timeout: Duration::from_secs(5),
            run_timeout: Duration::from_secs(5),
            agent_timeout: Duration::from_secs(30),
            ..Limits::default()
        },
        org_admins: Vec::new(),
    }
}

struct Server {
    base: String,
    http: reqwest::Client,
}

impl Server {
    async fn start(config: Config, db: Db) -> Self {
        let state = AppState::new(config, db, fake_auth("dev")).await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, router(state)).await.unwrap() });
        Self {
            base,
            http: reqwest::Client::new(),
        }
    }

    async fn plain() -> Self {
        Self::start(config(None), Db::open_in_memory().unwrap()).await
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    async fn send(
        &self,
        method: reqwest::Method,
        token: &str,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut req = self.http.request(method, self.url(path)).bearer_auth(token);
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

    async fn delete(&self, token: &str, path: &str, body: Value) -> (StatusCode, Value) {
        self.send(reqwest::Method::DELETE, token, path, Some(body))
            .await
    }

    /// A new Space owned by `token`'s user; returns its id.
    async fn space(&self, token: &str, name: &str) -> String {
        let (status, space) = self.post(token, "/v1/spaces", json!({"name": name})).await;
        assert_eq!(status, StatusCode::CREATED, "{space}");
        assert_eq!(space["role"], "owner");
        assert_eq!(space["runner_paired"], false);
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

    async fn runner_token(&self, owner: &str, space: &str) -> String {
        let r = self
            .http
            .post(self.url(&format!("/v1/spaces/{space}/runner-token")))
            .bearer_auth(owner)
            .json(&json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(r.headers()["cache-control"], "no-store");
        let body: Value = r.json().await.unwrap();
        body["token"].as_str().unwrap().to_owned()
    }

    async fn say(&self, token: &str, space: &str, room: &str, body: Value) -> (StatusCode, Value) {
        self.post(
            token,
            &format!("/v1/spaces/{space}/rooms/{room}/messages"),
            body,
        )
        .await
    }

    async fn events(&self, token: &str, space: &str, room: &str) -> Vec<Value> {
        let (status, body) = self
            .get(token, &format!("/v1/spaces/{space}/rooms/{room}/events"))
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body["events"].as_array().unwrap().clone()
    }

    /// Poll until a room has an event matching `pred`.
    async fn wait_for(
        &self,
        token: &str,
        space: &str,
        room: &str,
        pred: impl Fn(&Value) -> bool,
    ) -> Vec<Value> {
        for _ in 0..400 {
            let events = self.events(token, space, room).await;
            if events.iter().any(&pred) {
                return events;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!(
            "no matching event in {space}/{room}: {:#?}",
            self.events(token, space, room).await
        );
    }

    async fn claim_once(&self, runner: &str) -> (StatusCode, Option<Value>) {
        let r = self
            .http
            .post(self.url("/v1/runner/claim"))
            .bearer_auth(runner)
            .json(&json!({}))
            .send()
            .await
            .unwrap();
        let status = r.status();
        (
            status,
            if status == StatusCode::OK {
                Some(r.json().await.unwrap())
            } else {
                None
            },
        )
    }

    /// Act as a runner: claim one job, waiting for it to appear.
    async fn claim(&self, runner: &str) -> Value {
        for _ in 0..400 {
            match self.claim_once(runner).await {
                (StatusCode::OK, Some(job)) => return job,
                (StatusCode::NO_CONTENT, _) => tokio::time::sleep(Duration::from_millis(20)).await,
                (other, _) => panic!("claim returned {other}"),
            }
        }
        panic!("no job to claim");
    }

    async fn result(&self, runner: &str, job: &Value, output: &str) -> (StatusCode, Value) {
        let id = job["id"].as_str().unwrap();
        self.post(
            runner,
            &format!("/v1/runner/jobs/{id}/result"),
            json!({"ok": true, "output": output}),
        )
        .await
    }

    async fn runs(&self, token: &str) -> Vec<Value> {
        let (status, body) = self.get(token, "/v1/runs").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body["runs"].as_array().unwrap().clone()
    }
}

/// The kinds of `events`, without `activity` lifecycle events (checked
/// separately with [`lifecycle`]).
fn kinds(events: &[Value]) -> Vec<&str> {
    events
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .filter(|k| *k != "activity")
        .collect()
}

/// `(activity id, phase)` of each lifecycle event, oldest first.
fn lifecycle(events: &[Value]) -> Vec<(String, String)> {
    events
        .iter()
        .filter(|e| e["kind"] == "activity")
        .map(|e| {
            assert!(
                e["activity"]["name"]
                    .as_str()
                    .is_some_and(|n| !n.is_empty()),
                "{e}"
            );
            (
                e["activity"]["id"].as_str().unwrap().to_owned(),
                e["activity"]["phase"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

// ---- Mock model ----

enum Reply {
    Text(String),
    Tool(&'static str, Value),
    /// Never answer (until the client gives up).
    Hang,
}

type Script = Arc<dyn Fn(&Value) -> Reply + Send + Sync>;

struct MockModel {
    config: ModelConfig,
    requests: Arc<Mutex<Vec<Value>>>,
}

impl MockModel {
    async fn start(script: impl Fn(&Value) -> Reply + Send + Sync + 'static) -> Self {
        let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
        let seen = requests.clone();
        let script: Script = Arc::new(script);
        let app = axum::Router::new().route(
            "/v1/chat/completions",
            post(move |Json(body): Json<Value>| {
                let (seen, script) = (seen.clone(), script.clone());
                async move {
                    seen.lock().unwrap().push(body.clone());
                    let (message, finish) = match script(&body) {
                        Reply::Text(text) => {
                            (json!({"role": "assistant", "content": text}), "stop")
                        }
                        Reply::Tool(name, args) => (
                            json!({"role": "assistant", "content": null, "tool_calls": [{
                                "id": format!("call_{}", seen.lock().unwrap().len()),
                                "type": "function",
                                "function": {"name": name, "arguments": args.to_string()}
                            }]}),
                            "tool_calls",
                        ),
                        Reply::Hang => {
                            tokio::time::sleep(Duration::from_secs(120)).await;
                            (json!({"role": "assistant", "content": "too late"}), "stop")
                        }
                    };
                    Json(json!({
                        "id": "chatcmpl-1",
                        "object": "chat.completion",
                        "created": 0,
                        "model": "mock",
                        "choices": [{"index": 0, "message": message, "finish_reason": finish}],
                        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                    }))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            config: ModelConfig {
                base_url,
                model: "mock".into(),
                api_key: "test-key".into(),
            },
            requests,
        }
    }

    fn requests(&self) -> Vec<Value> {
        self.requests.lock().unwrap().clone()
    }
}

fn tool_names(request: &Value) -> Vec<String> {
    request["tools"]
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .map(|t| t["function"]["name"].as_str().unwrap().to_owned())
                .collect()
        })
        .unwrap_or_default()
}

fn has_tool_reply(request: &Value) -> bool {
    request["messages"]
        .as_array()
        .is_some_and(|m| m.iter().any(|m| m["role"] == "tool"))
}

fn last_tool_reply(request: &Value) -> String {
    let messages = request["messages"].as_array().unwrap();
    let reply = messages.iter().rev().find(|m| m["role"] == "tool").unwrap();
    match &reply["content"] {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

// ---- Authentication and the removed API ----

#[tokio::test]
async fn healthz_is_public_and_everything_else_needs_a_token() {
    let s = Server::plain().await;
    assert_eq!(
        s.http.get(s.url("/healthz")).send().await.unwrap().status(),
        StatusCode::OK
    );
    let r = s.http.get(s.url("/v1/spaces")).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(r.headers()["www-authenticate"], "Bearer");
    assert_eq!(s.get("nope", "/v1/me").await.0, StatusCode::UNAUTHORIZED);
    let (status, body) = s.get("idp-down", "/v1/spaces").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(!body.to_string().contains("jwks"), "{body}");
    let (status, me) = s.get(ALICE, "/v1/me").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me["id"], "alice");
}

#[tokio::test]
async fn the_unscoped_room_api_is_gone() {
    let s = Server::plain().await;
    s.space(ALICE, "home").await;
    for (method, path) in [
        (reqwest::Method::GET, "/v1/rooms"),
        (reqwest::Method::POST, "/v1/rooms"),
        (reqwest::Method::GET, "/v1/rooms/lobby/events"),
        (reqwest::Method::POST, "/v1/rooms/lobby/messages"),
        (reqwest::Method::POST, "/v1/rooms/lobby/demo"),
    ] {
        let (status, _) = s
            .send(
                method.clone(),
                ALICE,
                path,
                Some(json!({"text": "x", "name": "x"})),
            )
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {path}");
    }
}

#[tokio::test]
async fn startup_refuses_a_database_from_another_identity_provider() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("metro.sqlite3");
    AppState::new(config(None), Db::open(&path).unwrap(), fake_auth("dev"))
        .await
        .unwrap();
    let err = AppState::new(
        config(None),
        Db::open(&path).unwrap(),
        fake_auth("oidc:https://idp.example"),
    )
    .await
    .err()
    .unwrap();
    assert!(format!("{err:#}").contains("identity namespace"), "{err:#}");
}

// ---- Spaces and members ----

#[tokio::test]
async fn spaces_are_private_to_their_members() {
    let s = Server::plain().await;
    let a = s.space(ALICE, "alice's").await;
    let (_, spaces) = s.get(ALICE, "/v1/spaces").await;
    assert_eq!(spaces["spaces"][0]["id"], a);
    let (_, rooms) = s.get(ALICE, &format!("/v1/spaces/{a}/rooms")).await;
    assert_eq!(rooms, json!({"rooms": [{"id": "lobby", "name": "lobby"}]}));

    // Bob sees nothing, and outsiders can't tell the Space exists.
    let (_, spaces) = s.get(BOB, "/v1/spaces").await;
    assert_eq!(spaces, json!({"spaces": []}));
    for path in [
        format!("/v1/spaces/{a}/rooms"),
        format!("/v1/spaces/{a}/members"),
        format!("/v1/spaces/{a}/rooms/lobby/events"),
        "/v1/spaces/s-doesnotexist/rooms".to_owned(),
        "/v1/spaces/..%2F/rooms".to_owned(),
    ] {
        assert_eq!(s.get(BOB, &path).await.0, StatusCode::NOT_FOUND, "{path}");
    }
    assert_eq!(
        s.say(BOB, &a, "lobby", json!({"text": "hi"})).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        s.post(BOB, &format!("/v1/spaces/{a}/rooms/lobby/demo"), json!({}))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        s.post(
            BOB,
            &format!("/v1/spaces/{a}/members"),
            json!({"user_id": "bob"})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        s.post(BOB, &format!("/v1/spaces/{a}/runner-token"), json!({}))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn members_share_a_space_and_only_the_owner_manages_it() {
    let s = Server::plain().await;
    let a = s.space(ALICE, "team").await;
    let members = format!("/v1/spaces/{a}/members");
    s.add(ALICE, &a, "bob").await;
    // Adding again is idempotent; the owner is already a member.
    assert_eq!(
        s.post(ALICE, &members, json!({"user_id": "bob"})).await.0,
        StatusCode::OK
    );
    let (status, owner) = s.post(ALICE, &members, json!({"user_id": "alice"})).await;
    assert_eq!(
        (status, owner["role"].clone()),
        (StatusCode::OK, json!("owner"))
    );
    // Someone who has never signed in can be added by exact id.
    s.add(ALICE, &a, "auth0|5f7c00aa").await;
    assert_eq!(
        s.post(ALICE, &members, json!({"user_id": " bob"})).await.0,
        StatusCode::BAD_REQUEST
    );

    let (_, spaces) = s.get(BOB, "/v1/spaces").await;
    assert_eq!(
        spaces["spaces"][0],
        json!({"id": a, "name": "team", "description": "", "role": "member", "runner_paired": false})
    );
    // Members can read the roster (for @mention autocomplete) ...
    let (status, list) = s.get(BOB, &members).await;
    assert_eq!(status, StatusCode::OK);
    let ids: Vec<&str> = list["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["user_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["alice", "bob", "auth0|5f7c00aa"]);
    assert_eq!(list["members"][0]["role"], "owner");
    // ... but cannot manage it.
    assert_eq!(
        s.post(BOB, &members, json!({"user_id": "carol"})).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        s.delete(BOB, &members, json!({"user_id": "auth0|5f7c00aa"}))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        s.post(BOB, &format!("/v1/spaces/{a}/runner-token"), json!({}))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        s.delete(BOB, &format!("/v1/spaces/{a}/runner-token"), json!({}))
            .await
            .0,
        StatusCode::FORBIDDEN
    );

    // Bob shares the rooms.
    s.say(ALICE, &a, "lobby", json!({"text": "hello bob"}))
        .await;
    s.say(BOB, &a, "lobby", json!({"text": "hi alice"})).await;
    let seen_by_alice = s.events(ALICE, &a, "lobby").await;
    assert_eq!(seen_by_alice, s.events(BOB, &a, "lobby").await);
    assert_eq!(seen_by_alice[0]["actor"], "Alice");
    assert_eq!(seen_by_alice[0]["actor_id"], "alice");
    assert_eq!(seen_by_alice[1]["actor_id"], "bob");
    let first = seen_by_alice[0]["id"].as_i64().unwrap();
    let (_, after) = s
        .get(
            BOB,
            &format!("/v1/spaces/{a}/rooms/lobby/events?after={first}"),
        )
        .await;
    assert_eq!(after["events"].as_array().unwrap().len(), 1);

    // The owner stays; removing Bob takes his access away at once.
    assert_eq!(
        s.delete(ALICE, &members, json!({"user_id": "alice"}))
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        s.delete(ALICE, &members, json!({"user_id": "bob"})).await.0,
        StatusCode::OK
    );
    assert_eq!(
        s.delete(ALICE, &members, json!({"user_id": "bob"})).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        s.get(BOB, &format!("/v1/spaces/{a}/rooms/lobby/events"))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(s.get(BOB, "/v1/spaces").await.1, json!({"spaces": []}));
}

#[tokio::test]
async fn rooms_are_scoped_to_their_space() {
    let s = Server::plain().await;
    let a = s.space(ALICE, "a").await;
    let b = s.space(ALICE, "b").await;
    let (status, room) = s
        .post(
            ALICE,
            &format!("/v1/spaces/{a}/rooms"),
            json!({"name": "design"}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let design = room["id"].as_str().unwrap().to_owned();
    assert!(room.get("parent_room_id").is_none());
    assert_eq!(
        s.post(
            ALICE,
            &format!("/v1/spaces/{a}/rooms"),
            json!({"name": "Design"})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    // The same name is fine in another Space, and the room id is not reachable there.
    assert_eq!(
        s.post(
            ALICE,
            &format!("/v1/spaces/{b}/rooms"),
            json!({"name": "design"})
        )
        .await
        .0,
        StatusCode::CREATED
    );
    assert_eq!(
        s.get(ALICE, &format!("/v1/spaces/{b}/rooms/{design}/events"))
            .await
            .0,
        StatusCode::NOT_FOUND
    );

    s.say(ALICE, &a, "lobby", json!({"text": "in a"})).await;
    s.say(ALICE, &b, "lobby", json!({"text": "in b"})).await;
    assert_eq!(s.events(ALICE, &a, "lobby").await[0]["text"], "in a");
    assert_eq!(s.events(ALICE, &b, "lobby").await[0]["text"], "in b");
    assert_eq!(s.events(ALICE, &a, "lobby").await.len(), 1);
    assert_eq!(
        s.say(ALICE, &a, "lobby", json!({"text": ""})).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        s.get(
            ALICE,
            &format!("/v1/spaces/{a}/rooms/lobby/events?after=-1")
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
}

// ---- Runners ----

#[tokio::test]
async fn two_spaces_have_separate_runners_with_the_same_lobby_id() {
    let s = Server::plain().await;
    let a = s.space(ALICE, "alice's").await;
    let b = s.space(BOB, "bob's").await;
    let ta = s.runner_token(ALICE, &a).await;
    let tb = s.runner_token(BOB, &b).await;
    assert_ne!(ta, tb);
    assert_eq!(
        s.get(ALICE, "/v1/spaces").await.1["spaces"][0]["runner_paired"],
        true
    );

    // Runner tokens are never human tokens, and the reverse.
    assert_eq!(s.get(&ta, "/v1/spaces").await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(s.get(&ta, "/v1/me").await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(s.claim_once(ALICE).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(
        s.claim_once("metro_rt_forged").await.0,
        StatusCode::UNAUTHORIZED
    );

    let (status, demo) = s
        .post(
            ALICE,
            &format!("/v1/spaces/{a}/rooms/lobby/demo"),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{demo}");
    // Bob's runner never sees Alice's job, even though both serve a "lobby".
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(s.claim_once(&tb).await.0, StatusCode::NO_CONTENT);
    let job = s.claim(&ta).await;
    assert_eq!(job["tool"], "workspace.list");
    assert_eq!(job["arguments"], json!({"path": "."}));
    // Bob's runner cannot answer it either.
    assert_eq!(
        s.result(&tb, &job, "bob's files").await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        s.result(&ta, &job, "Cargo.toml\nsrc/").await.1["accepted"],
        true
    );
    assert_eq!(
        s.result(&ta, &job, "again").await.1,
        json!({"accepted": false, "reason": "duplicate"})
    );

    let events = s
        .wait_for(ALICE, &a, "lobby", |e| e["kind"] == "tool_result")
        .await;
    assert_eq!(kinds(&events), ["demo", "tool_call", "tool_result"]);
    let events: Vec<Value> = s
        .wait_for(ALICE, &a, "lobby", |e| {
            e["activity"]["phase"] == "completed"
        })
        .await;
    // Demo is its own `demo` activity, never an agent turn; the person's
    // Demo event is not part of it.
    assert!(events[0].get("activity").is_none());
    let demo = events[1]["activity"]["id"].as_str().unwrap().to_owned();
    assert!(demo.starts_with("demo_"), "{demo}");
    assert_eq!(
        lifecycle(&events),
        [
            (demo.clone(), "started".into()),
            (demo.clone(), "completed".into())
        ]
    );
    for e in &events[1..] {
        assert_eq!(e["activity"]["id"], demo.as_str(), "{e}");
        assert_eq!(e["activity"]["source"], "demo", "{e}");
        assert_eq!(e["activity"]["name"], "Demo", "{e}");
    }
    let events: Vec<Value> = events
        .into_iter()
        .filter(|e| e["kind"] != "activity")
        .collect();
    assert_eq!(events[2]["text"], "workspace.list ok\nCargo.toml\nsrc/");
    assert_eq!(events[2]["actor"], "runner");
    assert!(s.events(BOB, &b, "lobby").await.is_empty());
    // The token never appears in events or listings.
    let everything = format!(
        "{:?}{:?}",
        s.events(ALICE, &a, "lobby").await,
        s.get(ALICE, "/v1/spaces").await
    );
    assert!(!everything.contains(&ta));
}

#[tokio::test]
async fn rotation_and_revocation_cut_off_the_old_runner_at_once() {
    let s = Server::plain().await;
    let a = s.space(ALICE, "a").await;
    let demo = format!("/v1/spaces/{a}/rooms/lobby/demo");
    assert_eq!(
        s.post(ALICE, &demo, json!({})).await.0,
        StatusCode::CONFLICT,
        "unpaired Demo is refused"
    );

    let old = s.runner_token(ALICE, &a).await;
    s.post(ALICE, &demo, json!({})).await;
    let claimed = s.claim(&old).await;
    let new = s.runner_token(ALICE, &a).await;
    // The old token is dead for claims and results; its claimed job failed.
    assert_eq!(s.claim_once(&old).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(
        s.result(&old, &claimed, "late").await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(s.result(&new, &claimed, "late").await.0, StatusCode::GONE);
    let events = s
        .wait_for(ALICE, &a, "lobby", |e| e["kind"] == "error")
        .await;
    assert!(
        events
            .iter()
            .any(|e| e["text"].as_str().unwrap().contains("rotated or revoked")),
        "{events:?}"
    );

    // The new token works.
    s.post(ALICE, &demo, json!({})).await;
    let job = s.claim(&new).await;
    assert_eq!(s.result(&new, &job, "fresh").await.0, StatusCode::OK);

    // Revocation: the token stops working and Demo is refused again.
    let (status, _) = s
        .delete(ALICE, &format!("/v1/spaces/{a}/runner-token"), json!({}))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(s.claim_once(&new).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(
        s.post(ALICE, &demo, json!({})).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        s.get(ALICE, "/v1/spaces").await.1["spaces"][0]["runner_paired"],
        false
    );
}

// ---- Persistence ----

#[tokio::test]
async fn everything_survives_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("metro.sqlite3");
    let (a, token) = {
        let s = Server::start(config(None), Db::open(&path).unwrap()).await;
        let a = s.space(ALICE, "kept").await;
        s.add(ALICE, &a, "bob").await;
        s.say(BOB, &a, "lobby", json!({"text": "remember me"}))
            .await;
        s.post(
            ALICE,
            "/v1/chat/messages",
            json!({"text": "note to self", "save_note": true}),
        )
        .await;
        (a.clone(), s.runner_token(ALICE, &a).await)
    };
    let s = Server::start(config(None), Db::open(&path).unwrap()).await;
    let events = s.events(BOB, &a, "lobby").await;
    assert_eq!(events[0]["text"], "remember me");
    assert_eq!(
        s.get(BOB, "/v1/spaces").await.1["spaces"][0]["role"],
        "member"
    );
    assert_eq!(
        s.get(ALICE, "/v1/chat/events").await.1["events"][0]["text"],
        "note to self"
    );
    // The runner token is still valid (only its hash was stored).
    assert_eq!(s.claim_once(&token).await.0, StatusCode::NO_CONTENT);
}

// ---- Personal Chat ----

#[tokio::test]
async fn personal_chat_is_private_and_needs_no_space() {
    let s = Server::plain().await;
    let (status, event) = s
        .post(
            ALICE,
            "/v1/chat/messages",
            json!({"text": "just me", "invoke_agent": false}),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(event["audience"], "room");
    assert_eq!(
        s.get(ALICE, "/v1/chat/events").await.1["events"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(s.get(BOB, "/v1/chat/events").await.1, json!({"events": []}));
    // Bob can't react to Alice's Chat either.
    let eid = event["id"].as_i64().unwrap();
    assert_eq!(
        s.put(
            BOB,
            &format!("/v1/chat/events/{eid}/reactions"),
            json!({"emoji": "👍"})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );

    // Without a model, asking the agent says so in Alice's Chat only.
    s.post(
        ALICE,
        "/v1/chat/messages",
        json!({"text": "hello", "invoke_agent": true}),
    )
    .await;
    for _ in 0..200 {
        let events = s.get(ALICE, "/v1/chat/events").await.1["events"]
            .as_array()
            .unwrap()
            .clone();
        if events.iter().any(|e| e["kind"] == "error") {
            assert!(
                events.last().unwrap()["text"]
                    .as_str()
                    .unwrap()
                    .contains("METRO_MODEL")
            );
            assert_eq!(events[1]["audience"], "agent");
            assert!(
                events.iter().all(|e| e.get("activity").is_none()),
                "a turn that never started has no activity: {events:?}"
            );
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("no error event");
}

#[tokio::test]
async fn personal_chat_talks_to_the_model_without_tools() {
    let model = MockModel::start(|_| Reply::Text("hi from the model".into())).await;
    let s = Server::start(
        config(Some(model.config.clone())),
        Db::open_in_memory().unwrap(),
    )
    .await;
    s.post(
        ALICE,
        "/v1/chat/messages",
        json!({"text": "hello", "invoke_agent": true}),
    )
    .await;
    for _ in 0..200 {
        let events = s.get(ALICE, "/v1/chat/events").await.1["events"]
            .as_array()
            .unwrap()
            .clone();
        if let Some(reply) = events.iter().find(|e| e["kind"] == "agent") {
            assert_eq!(reply["text"], "hi from the model");
            let tools = tool_names(&model.requests()[0]);
            assert_eq!(
                tools,
                ["react_to_message"],
                "only the reaction tool in personal Chat"
            );
            assert!(
                s.runs(ALICE).await.is_empty(),
                "personal Chat runs are not Space runs"
            );
            // Its own `chat_` turn, started before the reply and ended after it.
            let turn = reply["activity"]["id"].as_str().unwrap().to_owned();
            assert!(turn.starts_with("chat_"), "{turn}");
            assert_eq!(reply["activity"]["name"], "Metro");
            let events = loop {
                let events = s.get(ALICE, "/v1/chat/events").await.1["events"]
                    .as_array()
                    .unwrap()
                    .clone();
                if events.iter().any(|e| e["activity"]["phase"] == "completed") {
                    break events;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            };
            assert_eq!(
                lifecycle(&events),
                [(turn.clone(), "started".into()), (turn, "completed".into())]
            );
            assert!(events[0].get("activity").is_none());
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("no reply");
}

// ---- Mentions ----

#[tokio::test]
async fn only_agent_mentions_wake_the_agent() {
    let model = MockModel::start(|_| Reply::Text("noted".into())).await;
    let s = Server::start(
        config(Some(model.config.clone())),
        Db::open_in_memory().unwrap(),
    )
    .await;
    let a = s.space(ALICE, "team").await;
    s.add(ALICE, &a, "bob").await;

    // Bob-only, by structured mention and by text: no run.
    let (status, e) = s
        .say(
            ALICE,
            &a,
            "lobby",
            json!({"text": "can you look?", "mentions": [{"kind": "user", "id": "bob"}]}),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(
        (e["audience"].clone(), e["mentions"].clone()),
        (json!("users"), json!([{"kind": "user", "id": "bob"}]))
    );
    let (_, e) = s
        .say(ALICE, &a, "lobby", json!({"text": "@bob lunch?"}))
        .await;
    assert_eq!(e["audience"], "users");
    let (_, e) = s
        .say(ALICE, &a, "lobby", json!({"text": "general news"}))
        .await;
    assert_eq!(
        (e["audience"].clone(), e["mentions"].clone()),
        (json!("room"), json!([]))
    );
    // An explicit unknown target is refused, and stores nothing.
    let (status, _) = s
        .say(
            ALICE,
            &a,
            "lobby",
            json!({"text": "hi", "mentions": [{"kind": "user", "id": "mallory"}]}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(
        model.requests().is_empty(),
        "no person-only message wakes the model"
    );
    assert_eq!(s.events(BOB, &a, "lobby").await.len(), 3);

    // `@agent` in the text wakes it without the flag; `@agent @bob` is mixed.
    let (_, e) = s
        .say(
            BOB,
            &a,
            "lobby",
            json!({"text": "@agent @bob what did alice ask me?", "invoke_agent": false}),
        )
        .await;
    assert_eq!(e["audience"], "mixed");
    let events = s
        .wait_for(ALICE, &a, "lobby", |e| e["kind"] == "agent")
        .await;
    assert_eq!(
        events.iter().rev().find(|e| e["kind"] == "agent").unwrap()["text"],
        "noted"
    );
    let requests = model.requests();
    assert_eq!(requests.len(), 1);
    // The model saw who earlier messages were addressed to, as Metro context
    // separate from the message text.
    let sent = requests[0]["messages"].to_string();
    assert!(sent.contains(r#"\"audience\":\"users\""#), "{sent}");
    assert!(sent.contains(r#"\"speaker_id\":\"alice\""#), "{sent}");
    assert!(sent.contains(r#"\"audience\":\"mixed\""#), "{sent}");
    assert!(sent.contains("can you look?"), "{sent}");
}

// ---- Agent, runner tools and threads ----

#[tokio::test]
async fn ask_agent_runs_the_rig_tool_loop_through_the_space_runner() {
    let model = MockModel::start(|req| {
        if has_tool_reply(req) {
            Reply::Text(format!("The runner says: {}", last_tool_reply(req)))
        } else {
            Reply::Tool("workspace_list", json!({"path": "src"}))
        }
    })
    .await;
    let s = Server::start(
        config(Some(model.config.clone())),
        Db::open_in_memory().unwrap(),
    )
    .await;
    let a = s.space(ALICE, "team").await;
    s.add(ALICE, &a, "bob").await;
    let token = s.runner_token(ALICE, &a).await;
    s.say(
        BOB,
        &a,
        "lobby",
        json!({"text": "what is in src?", "invoke_agent": true}),
    )
    .await;

    let job = s.claim(&token).await;
    assert_eq!(
        (job["tool"].clone(), job["arguments"].clone()),
        (json!("workspace.list"), json!({"path": "src"}))
    );
    assert_eq!(s.result(&token, &job, "main.rs").await.0, StatusCode::OK);

    let events = s
        .wait_for(ALICE, &a, "lobby", |e| {
            e["activity"]["phase"] == "completed"
        })
        .await;
    assert_eq!(events, s.events(BOB, &a, "lobby").await);
    assert_eq!(
        kinds(&events),
        ["message", "tool_call", "tool_result", "agent"]
    );
    let runs = s.runs(BOB).await;
    let run = runs[0]["id"].as_str().unwrap().to_owned();
    // One turn: started before its first tool, ended after its reply. The
    // person's message is not part of it; every server event is.
    let events = s
        .wait_for(ALICE, &a, "lobby", |e| {
            e["activity"]["phase"] == "completed"
        })
        .await;
    assert_eq!(
        events
            .iter()
            .map(|e| e["kind"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "message",
            "activity",
            "tool_call",
            "tool_result",
            "agent",
            "activity"
        ]
    );
    assert!(events[0].get("activity").is_none(), "{}", events[0]);
    for e in &events[1..] {
        assert_eq!(e["activity"]["id"].as_str(), Some(run.as_str()), "{e}");
        assert_eq!(e["activity"]["name"], "Metro", "{e}");
        assert_eq!(e["activity"]["source"], "agent", "{e}");
    }
    assert!(events[2]["activity"].get("phase").is_none());
    assert_eq!(
        lifecycle(&events),
        [(run.clone(), "started".into()), (run, "completed".into())]
    );
    let events: Vec<Value> = events
        .into_iter()
        .filter(|e| e["kind"] != "activity")
        .collect();
    assert!(events[3]["text"].as_str().unwrap().contains("main.rs"));
    // Lifecycle events never reach the model.
    for r in model.requests() {
        assert!(!r["messages"].to_string().contains("\"activity\""), "{r}");
    }
    let tools = tool_names(&model.requests()[0]);
    for name in [
        "workspace_list",
        "workspace_read",
        "workspace_check",
        "workspace_mcp_list",
        "workspace_mcp",
        "thread_create",
        "thread_send",
        "thread_status",
        "thread_stop",
        "react_to_message",
    ] {
        assert!(
            tools.iter().any(|t| t == name),
            "{name} missing from {tools:?}"
        );
    }
    let runs = s.runs(BOB).await;
    assert_eq!(runs[0]["status"], "completed");
    assert_eq!(runs[0]["actor"], "bob");
    assert_eq!(runs[0]["room_id"], "lobby");
}

#[tokio::test]
async fn an_unpaired_space_agent_has_no_workspace_tools() {
    let model = MockModel::start(|_| Reply::Text("no tools here".into())).await;
    let s = Server::start(
        config(Some(model.config.clone())),
        Db::open_in_memory().unwrap(),
    )
    .await;
    let a = s.space(ALICE, "plain").await;
    s.say(
        ALICE,
        &a,
        "lobby",
        json!({"text": "list files", "invoke_agent": true}),
    )
    .await;
    s.wait_for(ALICE, &a, "lobby", |e| e["kind"] == "agent")
        .await;
    let tools = tool_names(&model.requests()[0]);
    assert!(
        !tools.iter().any(|t| t.starts_with("workspace_")),
        "{tools:?}"
    );
    assert!(tools.iter().any(|t| t == "thread_create"), "{tools:?}");
}

#[tokio::test]
async fn a_parent_agent_delegates_to_a_thread_and_hears_back() {
    let model = MockModel::start(|req| {
        let tools = tool_names(req);
        let parent = tools.iter().any(|t| t == "thread_create");
        match (parent, has_tool_reply(req)) {
            (true, false) => Reply::Tool(
                "thread_create",
                json!({"name": "research", "task": "count the files"}),
            ),
            (true, true) => Reply::Text("I started a thread for that.".into()),
            (false, _) => Reply::Text("child: 3 files".into()),
        }
    })
    .await;
    let s = Server::start(
        config(Some(model.config.clone())),
        Db::open_in_memory().unwrap(),
    )
    .await;
    let a = s.space(ALICE, "team").await;
    s.add(ALICE, &a, "bob").await;
    s.say(
        ALICE,
        &a,
        "lobby",
        json!({"text": "please research", "invoke_agent": true}),
    )
    .await;

    let parent = s
        .wait_for(BOB, &a, "lobby", |e| {
            e["kind"] == "thread" && e["text"].as_str().unwrap().contains("completed")
        })
        .await;
    let mirror = parent
        .iter()
        .find(|e| e["text"].as_str().unwrap().contains("completed"))
        .unwrap();
    assert!(
        mirror["text"].as_str().unwrap().contains("child: 3 files"),
        "{mirror}"
    );
    let (_, rooms) = s.get(BOB, &format!("/v1/spaces/{a}/rooms")).await;
    let thread = rooms["rooms"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "research")
        .unwrap()
        .clone();
    assert_eq!(thread["parent_room_id"], "lobby");
    let tid = thread["id"].as_str().unwrap();

    // The thread holds the task, labelled as addressed to the agent, and the child's answer.
    let child = s.events(BOB, &a, tid).await;
    assert_eq!(kinds(&child), ["message", "agent"]);
    assert_eq!(child[0]["text"], "count the files");
    assert_eq!(child[0]["actor"], "agent");
    assert_eq!(child[0]["audience"], "agent");
    // The child cannot create threads of its own.
    let child_request = model
        .requests()
        .into_iter()
        .find(|r| !tool_names(r).iter().any(|t| t == "thread_create"))
        .unwrap();
    assert!(
        child_request["messages"]
            .to_string()
            .contains("count the files")
    );

    let runs = s.runs(ALICE).await;
    assert!(
        runs.iter()
            .any(|r| r["room_id"] == tid && r["status"] == "completed" && r["actor"] == "alice"),
        "{runs:?}"
    );
    // Outsiders see none of these runs.
    assert!(s.runs(CAROL).await.is_empty());

    // Each turn is its own activity: the child is shown by its thread's
    // name in its own room; the parent's marker belongs to the parent turn,
    // the mirrored result to the child's.
    let child_run = runs.iter().find(|r| r["room_id"] == tid).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let parent_run = runs.iter().find(|r| r["room_id"] == "lobby").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let child = s
        .wait_for(BOB, &a, tid, |e| e["activity"]["phase"] == "completed")
        .await;
    assert_eq!(
        lifecycle(&child),
        [
            (child_run.clone(), "started".into()),
            (child_run.clone(), "completed".into())
        ]
    );
    assert!(child[0].get("activity").is_none(), "the task is a message");
    for e in &child[1..] {
        assert_eq!(e["activity"]["name"], "research", "{e}");
        assert_eq!(e["activity"]["source"], "agent", "{e}");
    }
    let parent = s
        .wait_for(BOB, &a, "lobby", |e| e["activity"]["phase"] == "completed")
        .await;
    let started = parent
        .iter()
        .find(|e| e["text"].as_str().unwrap().starts_with("Started thread"))
        .unwrap();
    assert_eq!(started["activity"]["id"], parent_run.as_str());
    assert_eq!(started["activity"]["name"], "Metro");
    let mirror = parent
        .iter()
        .find(|e| e["kind"] == "thread" && e["text"].as_str().unwrap().contains("completed"))
        .unwrap();
    assert_eq!(mirror["activity"]["id"], child_run.as_str());
    assert!(mirror["activity"].get("phase").is_none());
    // Only the parent's own lifecycle is in the parent room.
    assert!(lifecycle(&parent).iter().all(|(id, _)| *id == parent_run));
}

#[tokio::test]
async fn interleaved_turns_in_one_room_keep_their_own_activity() {
    // Each run lists the directory its prompt names, then reports.
    let model = MockModel::start(|req| {
        if has_tool_reply(req) {
            Reply::Text(format!("done: {}", last_tool_reply(req)))
        } else {
            // The prompt is the last of the two in the request; the other
            // one, if present, is shared history.
            let sent = req["messages"].to_string();
            let at = |p: &str| sent.rfind(p).map_or(-1, |i| i as i64);
            let path = if at("list alpha") > at("list beta") {
                "alpha"
            } else {
                "beta"
            };
            Reply::Tool("workspace_list", json!({"path": path}))
        }
    })
    .await;
    let s = Server::start(
        config(Some(model.config.clone())),
        Db::open_in_memory().unwrap(),
    )
    .await;
    let a = s.space(ALICE, "team").await;
    s.add(ALICE, &a, "bob").await;
    let token = s.runner_token(ALICE, &a).await;
    s.say(
        ALICE,
        &a,
        "lobby",
        json!({"text": "list alpha", "invoke_agent": true}),
    )
    .await;
    s.say(
        BOB,
        &a,
        "lobby",
        json!({"text": "list beta", "invoke_agent": true}),
    )
    .await;
    // Both are waiting on the runner before either is answered.
    let first = s.claim(&token).await;
    let second = s.claim(&token).await;
    s.result(&token, &second, "second-out").await;
    s.result(&token, &first, "first-out").await;
    let events = loop {
        let events = s.events(ALICE, &a, "lobby").await;
        if lifecycle(&events)
            .iter()
            .filter(|(_, p)| p == "completed")
            .count()
            == 2
        {
            break events;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let runs = s.runs(ALICE).await;
    let run_of = |task: &str| {
        runs.iter().find(|r| r["task"] == task).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let (alpha, beta) = (run_of("list alpha"), run_of("list beta"));
    assert_ne!(alpha, beta);
    for e in &events {
        let text = e["text"].as_str().unwrap();
        match e["kind"].as_str().unwrap() {
            "message" => assert!(e.get("activity").is_none(), "{e}"),
            "tool_call" if text.contains("alpha") => {
                assert_eq!(e["activity"]["id"], alpha.as_str())
            }
            "tool_call" => assert_eq!(e["activity"]["id"], beta.as_str()),
            "tool_result" | "agent" => {
                // The output the runner gave for this run's own path.
                let path = if e["activity"]["id"] == alpha.as_str() {
                    "alpha"
                } else {
                    "beta"
                };
                assert!(
                    e["activity"]["id"] == alpha.as_str() || e["activity"]["id"] == beta.as_str(),
                    "{e}"
                );
                let out = if first["arguments"]["path"] == path {
                    "first-out"
                } else {
                    "second-out"
                };
                assert!(text.contains(out), "{e} should carry {out}");
            }
            "activity" => {}
            other => panic!("unexpected {other}: {e}"),
        }
    }
    for run in [&alpha, &beta] {
        let phases: Vec<_> = lifecycle(&events)
            .into_iter()
            .filter(|(id, _)| id == run)
            .map(|(_, p)| p)
            .collect();
        assert_eq!(phases, ["started", "completed"]);
    }
}

#[tokio::test]
async fn a_failed_turn_ends_failed_and_a_refused_one_never_starts() {
    // A model that is not there: the run starts, then fails.
    let dead = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}/v1", l.local_addr().unwrap())
    };
    let s = Server::start(
        config(Some(ModelConfig {
            base_url: dead,
            model: "mock".into(),
            api_key: "test-key".into(),
        })),
        Db::open_in_memory().unwrap(),
    )
    .await;
    let a = s.space(ALICE, "team").await;
    s.say(
        ALICE,
        &a,
        "lobby",
        json!({"text": "hi", "invoke_agent": true}),
    )
    .await;
    let events = s
        .wait_for(ALICE, &a, "lobby", |e| e["activity"]["phase"] == "failed")
        .await;
    let run = s.runs(ALICE).await[0]["id"].as_str().unwrap().to_owned();
    assert_eq!(
        lifecycle(&events),
        [
            (run.clone(), "started".into()),
            (run.clone(), "failed".into())
        ]
    );
    let error = events.iter().find(|e| e["kind"] == "error").unwrap();
    assert_eq!(error["activity"]["id"], run.as_str());

    // No model at all: the run is refused before it starts, so there is a
    // plain error and no lifecycle that could look live.
    let s = Server::plain().await;
    let a = s.space(ALICE, "team").await;
    s.say(
        ALICE,
        &a,
        "lobby",
        json!({"text": "hi", "invoke_agent": true}),
    )
    .await;
    let events = s
        .wait_for(ALICE, &a, "lobby", |e| e["kind"] == "error")
        .await;
    assert!(lifecycle(&events).is_empty(), "{events:?}");
    assert!(
        events.iter().all(|e| e.get("activity").is_none()),
        "{events:?}"
    );
}

// ---- Runs: listing and stopping ----

#[tokio::test]
async fn runs_can_be_stopped_by_their_initiator_or_the_owner_only() {
    let model = MockModel::start(|_| Reply::Hang).await;
    let s = Server::start(
        config(Some(model.config.clone())),
        Db::open_in_memory().unwrap(),
    )
    .await;
    let a = s.space(ALICE, "team").await;
    s.add(ALICE, &a, "bob").await;
    s.add(ALICE, &a, "carol").await;
    s.say(
        BOB,
        &a,
        "lobby",
        json!({"text": "slow job", "invoke_agent": true}),
    )
    .await;

    let run = loop {
        if let Some(run) = s.runs(CAROL).await.first().cloned() {
            break run;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(run["status"], "running");
    assert_eq!(run["space_id"], a.as_str());
    assert_eq!(run["task"], "slow job");
    let stop = format!("/v1/runs/{}/stop", run["id"].as_str().unwrap());
    // Another member may see it but not stop it; an outsider can't see it at all.
    assert_eq!(
        s.post(CAROL, &stop, json!({})).await.0,
        StatusCode::FORBIDDEN
    );
    let (s_out, _) = s.post("nope", &stop, json!({})).await;
    assert_eq!(s_out, StatusCode::UNAUTHORIZED);
    assert_eq!(
        s.post(CAROL, "/v1/runs/run_unknown/stop", json!({}))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    // The owner may.
    let (status, stopped) = s.post(ALICE, &stop, json!({})).await;
    assert_eq!(status, StatusCode::OK, "{stopped}");
    assert_eq!(stopped["status"], "cancelled");
    assert_eq!(
        s.post(ALICE, &stop, json!({})).await.0,
        StatusCode::CONFLICT
    );

    let events = s
        .wait_for(BOB, &a, "lobby", |e| e["activity"]["phase"] == "cancelled")
        .await;
    let status = events.iter().find(|e| e["kind"] == "status").unwrap();
    assert!(status["text"].as_str().unwrap().contains("cancelled"));
    let id = run["id"].as_str().unwrap().to_owned();
    assert_eq!(status["activity"]["id"], id.as_str());
    // A stopped turn ends `cancelled`, never left looking live.
    assert_eq!(
        lifecycle(&events),
        [(id.clone(), "started".into()), (id, "cancelled".into())]
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        !s.events(BOB, &a, "lobby")
            .await
            .iter()
            .any(|e| e["kind"] == "agent"),
        "no reply after a stop"
    );

    // The initiator may stop their own run.
    s.say(
        BOB,
        &a,
        "lobby",
        json!({"text": "another", "invoke_agent": true}),
    )
    .await;
    let run = loop {
        if let Some(run) = s
            .runs(BOB)
            .await
            .into_iter()
            .find(|r| r["status"] == "running")
        {
            break run;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let (status, _) = s
        .post(
            BOB,
            &format!("/v1/runs/{}/stop", run["id"].as_str().unwrap()),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn removing_a_member_stops_their_runs() {
    let model = MockModel::start(|_| Reply::Hang).await;
    let s = Server::start(
        config(Some(model.config.clone())),
        Db::open_in_memory().unwrap(),
    )
    .await;
    let a = s.space(ALICE, "team").await;
    s.add(ALICE, &a, "bob").await;
    s.say(
        BOB,
        &a,
        "lobby",
        json!({"text": "slow job", "invoke_agent": true}),
    )
    .await;
    while s.runs(ALICE).await.is_empty() {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let (status, removed) = s
        .delete(
            ALICE,
            &format!("/v1/spaces/{a}/members"),
            json!({"user_id": "bob"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(removed["stopped_runs"], 1);
    assert_eq!(s.runs(ALICE).await[0]["status"], "cancelled");
    assert!(
        s.runs(BOB).await.is_empty(),
        "Bob no longer sees the Space's runs"
    );
    s.wait_for(ALICE, &a, "lobby", |e| e["kind"] == "status")
        .await;
}

// ---- Reactions ----

#[tokio::test]
async fn people_react_to_messages_in_their_spaces() {
    let s = Server::plain().await;
    let a = s.space(ALICE, "team").await;
    s.add(ALICE, &a, "bob").await;
    let (_, msg) = s.say(ALICE, &a, "lobby", json!({"text": "shipped"})).await;
    let eid = msg["id"].as_i64().unwrap();
    let path = format!("/v1/spaces/{a}/rooms/lobby/events/{eid}/reactions");
    assert_eq!(
        s.put(BOB, &path, json!({"emoji": "🎉"})).await,
        (StatusCode::OK, json!({"changed": true}))
    );
    assert_eq!(
        s.put(BOB, &path, json!({"emoji": "🎉"})).await,
        (StatusCode::OK, json!({"changed": false}))
    );
    assert_eq!(
        s.delete(BOB, &path, json!({"emoji": "🎉"})).await,
        (StatusCode::OK, json!({"changed": true}))
    );
    assert_eq!(
        s.put(BOB, &path, json!({"emoji": "💩"})).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        s.put(CAROL, &path, json!({"emoji": "👍"})).await.0,
        StatusCode::NOT_FOUND
    );
    let missing = format!("/v1/spaces/{a}/rooms/lobby/events/999999/reactions");
    assert_eq!(
        s.put(BOB, &missing, json!({"emoji": "👍"})).await.0,
        StatusCode::NOT_FOUND
    );

    let events = s.events(ALICE, &a, "lobby").await;
    let reactions: Vec<Value> = events
        .iter()
        .filter(|e| e["kind"] == "reaction")
        .map(|e| serde_json::from_str(e["text"].as_str().unwrap()).unwrap())
        .collect();
    assert_eq!(
        reactions.len(),
        2,
        "only real changes are recorded: {events:?}"
    );
    assert_eq!(reactions[0]["target_event_id"], eid);
    assert_eq!(reactions[0]["present"], true);
    assert_eq!(reactions[0]["reactor_id"], "bob");
    assert_eq!(reactions[1]["present"], false);
}

#[tokio::test]
async fn the_agent_can_answer_with_a_reaction_alone() {
    let model = MockModel::start(|req| {
        if has_tool_reply(req) {
            Reply::Text("METRO_REACTION_ONLY".into())
        } else {
            Reply::Tool("react_to_message", json!({"emoji": "🙏"}))
        }
    })
    .await;
    let s = Server::start(
        config(Some(model.config.clone())),
        Db::open_in_memory().unwrap(),
    )
    .await;
    let a = s.space(ALICE, "team").await;
    let (_, msg) = s
        .say(ALICE, &a, "lobby", json!({"text": "@agent thanks!"}))
        .await;
    let events = s
        .wait_for(ALICE, &a, "lobby", |e| e["kind"] == "reaction")
        .await;
    let reaction = events
        .iter()
        .rev()
        .find(|e| e["kind"] == "reaction")
        .unwrap();
    assert!(reaction.get("activity").is_none(), "{reaction}");
    let reaction: Value = serde_json::from_str(reaction["text"].as_str().unwrap()).unwrap();
    assert_eq!(reaction["target_event_id"], msg["id"]);
    assert_eq!(reaction["emoji"], "🙏");
    assert_eq!(reaction["reactor_kind"], "agent");
    loop {
        if s.runs(ALICE).await[0]["status"] == "completed" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let events = s.events(ALICE, &a, "lobby").await;
    assert!(
        !events.iter().any(|e| e["kind"] == "agent"),
        "no redundant reply: {events:?}"
    );
    // A reaction-only turn still ends: its lifecycle is closed.
    let run = s.runs(ALICE).await[0]["id"].as_str().unwrap().to_owned();
    assert_eq!(
        lifecycle(&events),
        [(run.clone(), "started".into()), (run, "completed".into())]
    );
}
