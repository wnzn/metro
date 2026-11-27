//! Messages sent while an agent turn runs: queued Chat turns, and steering
//! into the running turn in Chat and in Space rooms ([`metro_server::steering`]).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Json;
use axum::routing::post;
use metro_server::auth::{AuthFailure, AuthFuture, HumanAuth, UserIdentity};
use metro_server::db::Db;
use metro_server::{AppState, Config, Limits, ModelConfig, router};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use tokio::sync::Semaphore;

struct FakeAuth;

impl HumanAuth for FakeAuth {
    fn identity<'a>(&'a self, bearer: &'a str) -> AuthFuture<'a> {
        Box::pin(async move {
            match bearer {
                "alice" | "bob" => Ok(UserIdentity {
                    id: bearer.to_owned(),
                    actor: bearer.to_owned(),
                }),
                _ => Err(AuthFailure::Rejected("unknown token".to_owned())),
            }
        })
    }
    fn namespace(&self) -> &str {
        "dev"
    }
}

/// The model: records every request. A request whose last message is a
/// person's (its last text) containing `[tool]` waits for the gate, then calls
/// `react_to_message`; one containing `[slow]` waits, then replies; anything
/// else replies at once. Replies are `reply-N`, N counting requests.
struct Model {
    calls: Mutex<Vec<Value>>,
    gate: Arc<Semaphore>,
}

impl Model {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::default(),
            gate: Arc::new(Semaphore::new(0)),
        })
    }

    fn calls(&self) -> Vec<Value> {
        self.calls.lock().unwrap().clone()
    }

    fn open(&self) {
        self.gate.add_permits(1);
    }

    async fn answer(&self, body: Value) -> Value {
        let n = {
            let mut calls = self.calls.lock().unwrap();
            calls.push(body.clone());
            calls.len()
        };
        let last = body["messages"].as_array().unwrap().last().unwrap().clone();
        // The last text part: the provider merges adjacent user messages.
        let said = match (&last["role"], &last["content"]) {
            (r, Value::Array(parts)) if r == "user" => parts.last().unwrap()["text"].to_string(),
            (r, text) if r == "user" => text.to_string(),
            _ => String::new(),
        };
        let message = if said.contains("[tool]") {
            self.gate.acquire().await.unwrap().forget();
            json!({"role": "assistant", "content": null, "tool_calls": [{
                "id": format!("call_{n}"), "type": "function",
                "function": {"name": "react_to_message", "arguments": "{\"emoji\":\"👍\"}"}}]})
        } else {
            if said.contains("[slow]") {
                self.gate.acquire().await.unwrap().forget();
            }
            json!({"role": "assistant", "content": format!("reply-{n}")})
        };
        let finish = if message.get("tool_calls").is_some() {
            "tool_calls"
        } else {
            "stop"
        };
        json!({
            "id": "c", "object": "chat.completion", "created": 0, "model": body["model"],
            "choices": [{"index": 0, "finish_reason": finish, "message": message}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        })
    }
}

struct H {
    base: String,
    http: reqwest::Client,
    model: Arc<Model>,
    state: AppState,
}

impl H {
    async fn start() -> Self {
        Self::start_on(None).await
    }

    /// On database `file` (a fresh in-memory one when `None`), with its own model.
    async fn start_on(file: Option<&std::path::Path>) -> Self {
        let model = Model::new();
        let served = model.clone();
        let app = axum::Router::new().route(
            "/v1/chat/completions",
            post(move |Json(body): Json<Value>| {
                let model = served.clone();
                async move { Json(model.answer(body).await) }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let config = Config {
            bind: "127.0.0.1:0".parse().unwrap(),
            db_path: ":memory:".into(),
            model: Some(ModelConfig {
                base_url: url,
                model: "env-model".into(),
                api_key: String::new(),
            }),
            limits: Limits {
                agent_timeout: Duration::from_secs(20),
                max_agent_runs: 16,
                max_space_runs: 16,
                ..Limits::default()
            },
            org_admins: vec![],
        };
        let db = match file {
            Some(f) => Db::open(f).unwrap(),
            None => Db::open_in_memory().unwrap(),
        };
        let state = AppState::new(config, db, Arc::new(FakeAuth)).await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = state.clone();
        tokio::spawn(async move { axum::serve(listener, router(app)).await.unwrap() });
        Self {
            base,
            http: reqwest::Client::new(),
            model,
            state,
        }
    }

    async fn req(
        &self,
        method: Method,
        who: &str,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut r = self
            .http
            .request(method, format!("{}{path}", self.base))
            .bearer_auth(who);
        if let Some(b) = body {
            r = r.json(&b);
        }
        let r = r.send().await.unwrap();
        let status = r.status();
        let text = r.text().await.unwrap();
        (status, serde_json::from_str(&text).unwrap_or(json!(text)))
    }

    async fn post(&self, who: &str, path: &str, body: Value) -> (StatusCode, Value) {
        self.req(Method::POST, who, path, Some(body)).await
    }

    /// A Chat message from alice; the POST's answer.
    async fn say(&self, text: &str, delivery: Option<&str>) -> Value {
        let mut body = json!({"text": text});
        if let Some(d) = delivery {
            body["delivery"] = json!(d);
        }
        let (s, v) = self.post("alice", "/v1/chat/messages", body).await;
        assert_eq!(s, StatusCode::ACCEPTED, "{v}");
        v
    }

    async fn chat(&self) -> Vec<Value> {
        let (s, v) = self
            .req(Method::GET, "alice", "/v1/chat/events", None)
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
        v["events"].as_array().unwrap().clone()
    }

    async fn room(&self, sid: &str) -> Vec<Value> {
        let (s, v) = self
            .req(
                Method::GET,
                "alice",
                &format!("/v1/spaces/{sid}/rooms/lobby/events"),
                None,
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
        v["events"].as_array().unwrap().clone()
    }

    /// Wait until `events()` satisfies `done`.
    async fn until<F, Fut>(&self, events: F, done: impl Fn(&[Value]) -> bool) -> Vec<Value>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = Vec<Value>>,
    {
        for _ in 0..400 {
            let now = events().await;
            if done(&now) {
                return now;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("never happened: {:?}", events().await);
    }

    async fn calls_reach(&self, n: usize) {
        for _ in 0..400 {
            if self.model.calls().len() >= n {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the model got {} calls, not {n}", self.model.calls().len());
    }

    /// A Space of alice with bob in it.
    async fn space(&self) -> String {
        let (s, v) = self.post("alice", "/v1/spaces", json!({"name": "S"})).await;
        assert_eq!(s, StatusCode::CREATED, "{v}");
        let sid = v["id"].as_str().unwrap().to_owned();
        let (s, v) = self
            .post(
                "alice",
                &format!("/v1/spaces/{sid}/members"),
                json!({"user_id": "bob"}),
            )
            .await;
        assert_eq!(s, StatusCode::CREATED, "{v}");
        sid
    }

    async fn sent(&self, who: &str, sid: &str, body: Value) -> Value {
        let (s, v) = self
            .post(who, &format!("/v1/spaces/{sid}/rooms/lobby/messages"), body)
            .await;
        assert_eq!(s, StatusCode::ACCEPTED, "{v}");
        v
    }

    fn pending(&self) -> i64 {
        metro_server::decisions::count(&self.state.db).unwrap()
    }
}

fn id(v: &Value) -> i64 {
    v["id"].as_i64().unwrap()
}

fn phase<'a>(events: &'a [Value], phase: &str) -> Vec<&'a Value> {
    events
        .iter()
        .filter(|e| e["kind"] == "activity" && e["activity"]["phase"] == phase)
        .collect()
}

fn replies(events: &[Value]) -> Vec<String> {
    events
        .iter()
        .filter(|e| e["kind"] == "agent")
        .map(|e| e["text"].as_str().unwrap().to_owned())
        .collect()
}

fn steered(events: &[Value]) -> Vec<i64> {
    events
        .iter()
        .filter(|e| e["kind"] == "steered")
        .map(|e| e["activity"]["steered"].as_i64().unwrap())
        .collect()
}

/// Where `needle` is in request `call`'s messages, as `(index, role)`.
fn position(call: &Value, needle: &str) -> Option<(usize, String)> {
    call["messages"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
        .find(|(_, m)| m["content"].to_string().contains(needle))
        .map(|(i, m)| (i, m["role"].as_str().unwrap().to_owned()))
}

fn tool_result_at(call: &Value) -> usize {
    call["messages"]
        .as_array()
        .unwrap()
        .iter()
        .position(|m| m["role"] == "tool")
        .expect("a tool result")
}

// ---- Personal Chat ----

#[tokio::test]
async fn queued_chat_messages_wait_for_the_running_turn_and_run_in_order() {
    let h = H::start().await;
    h.say("[slow] first", None).await;
    h.calls_reach(1).await;
    let second = h.say("q-second", Some("queue")).await;
    let third = h.say("q-third", None).await;
    assert!(second["turn_id"].is_string() && third["turn_id"].is_string());
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(h.model.calls().len(), 1, "nothing starts while a turn runs");
    assert_eq!(phase(&h.chat().await, "started").len(), 1);
    h.model.open();
    let chat = h.until(|| h.chat(), |e| replies(e).len() == 3).await;
    assert_eq!(replies(&chat), ["reply-1", "reply-2", "reply-3"]);
    // Each starts after the one before ended.
    let started = phase(&chat, "started");
    let completed = phase(&chat, "completed");
    assert!(id(started[1]) > id(completed[0]) && id(started[2]) > id(completed[1]));
    // A queued turn sees the reply it waited for, before its own message.
    let call = &h.model.calls()[1];
    let (reply, _) = position(call, "reply-1").expect("the earlier reply");
    let (own, role) = position(call, "q-second").unwrap();
    assert!(reply < own && role == "user");
    assert_eq!(h.pending(), 0);
}

#[tokio::test]
async fn a_steered_chat_message_joins_the_running_turn_at_its_next_model_call() {
    let h = H::start().await;
    h.say("[tool] look this up", None).await;
    h.calls_reach(1).await;
    let steer = h.say("also check the weekend", Some("steer")).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    h.model.open();
    let chat = h.until(|| h.chat(), |e| !replies(e).is_empty()).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let chat = {
        let _ = chat;
        h.chat().await
    };
    assert_eq!(replies(&chat), ["reply-2"], "one turn answered both");
    assert_eq!(
        phase(&chat, "started").len(),
        1,
        "the steered message started no turn"
    );
    assert_eq!(steered(&chat), [id(&steer)]);
    let calls = h.model.calls();
    assert_eq!(calls.len(), 2);
    let (at, role) = position(&calls[1], "also check the weekend").unwrap();
    assert_eq!(role, "user");
    assert!(
        at > tool_result_at(&calls[1]),
        "after what the model had already seen"
    );
    assert!(position(&calls[0], "also check the weekend").is_none());
    assert_eq!(h.pending(), 0);
    assert!(h.state.chat_runs.turns_of("alice").is_empty());
}

#[tokio::test]
async fn a_steered_chat_message_the_turn_never_took_becomes_the_next_turn() {
    let h = H::start().await;
    h.say("[slow] only one model call", None).await;
    h.calls_reach(1).await;
    h.say("and then this", Some("steer")).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    h.model.open();
    let chat = h.until(|| h.chat(), |e| replies(e).len() == 2).await;
    assert_eq!(phase(&chat, "started").len(), 2);
    assert!(steered(&chat).is_empty());
    // Its own turn, which sees the reply that ended the first.
    let call = &h.model.calls()[1];
    assert!(position(call, "reply-1").unwrap().0 < position(call, "and then this").unwrap().0);
    assert_eq!(h.pending(), 0);
}

#[tokio::test]
async fn stopping_the_running_turn_keeps_the_queue_and_a_stopped_queued_turn_never_runs() {
    let h = H::start().await;
    let first = h.say("[slow] first", None).await;
    h.calls_reach(1).await;
    let dropped = h.say("cancel me", None).await;
    h.say("keep me", None).await;
    for turn in [&first, &dropped] {
        let path = format!("/v1/chat/turns/{}/stop", turn["turn_id"].as_str().unwrap());
        let (s, v) = h.post("alice", &path, json!({})).await;
        assert_eq!(s, StatusCode::OK, "{v}");
    }
    h.model.open();
    let chat = h.until(|| h.chat(), |e| replies(e).len() == 1).await;
    assert_eq!(phase(&chat, "cancelled").len(), 1);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let calls = h.model.calls();
    assert_eq!(
        calls.len(),
        2,
        "the stopped queued turn never reached the model"
    );
    assert!(position(&calls[1], "keep me").is_some());
    assert_eq!(phase(&h.chat().await, "started").len(), 2);
    assert_eq!(h.pending(), 0);
}

#[tokio::test]
async fn deleted_queued_and_steered_messages_never_reach_the_model() {
    let h = H::start().await;
    h.say("[tool] first", None).await;
    h.calls_reach(1).await;
    let steer = h.say("secret steer", Some("steer")).await;
    let queued = h.say("secret queued", None).await;
    for m in [&steer, &queued] {
        let (s, _) = h
            .req(
                Method::DELETE,
                "alice",
                &format!("/v1/chat/events/{}", id(m)),
                None,
            )
            .await;
        assert_eq!(s, StatusCode::OK);
    }
    h.model.open();
    let chat = h.until(|| h.chat(), |e| !replies(e).is_empty()).await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(steered(&chat).is_empty());
    assert_eq!(phase(&h.chat().await, "started").len(), 1);
    let all = serde_json::to_string(&h.model.calls()).unwrap();
    assert!(!all.contains("secret"), "{all}");
    assert_eq!(h.pending(), 0);
}

// ---- Space rooms ----

#[tokio::test]
async fn in_a_room_the_senders_next_message_to_the_running_agent_steers_it() {
    let h = H::start().await;
    let sid = h.space().await;
    h.sent("alice", &sid, json!({"text": "@agent [tool] go"}))
        .await;
    h.calls_reach(1).await;
    // People only: posted, never steering.
    let people = h
        .sent(
            "alice",
            &sid,
            json!({"text": "fyi", "mentions": [{"kind": "user", "id": "bob"}],
                                    "infer_text_mentions": false}),
        )
        .await;
    let steer = h
        .sent("alice", &sid, json!({"text": "@agent also this"}))
        .await;
    // Someone else's message to the agent starts its own run on their model.
    h.sent("bob", &sid, json!({"text": "@agent from bob"}))
        .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    h.model.open();
    let room = h.until(|| h.room(&sid), |e| replies(e).len() == 2).await;
    assert_eq!(steered(&room), [id(&steer)]);
    assert_eq!(
        phase(&room, "started").len(),
        2,
        "alice's run and bob's, never a second of alice's"
    );
    let calls = h.model.calls();
    let second = calls
        .iter()
        .find(|c| position(c, "[tool] go").is_some() && c.to_string().contains("\"tool\""))
        .expect("alice's run's second model call");
    let (at, role) = position(second, "also this").unwrap();
    assert!(role == "user" && at > tool_result_at(second));
    assert!(position(second, "fyi").is_none() && position(second, "from bob").is_none());
    let _ = people;
    assert_eq!(h.pending(), 0);
}

#[tokio::test]
async fn in_a_room_a_message_the_run_never_took_starts_its_own_run() {
    let h = H::start().await;
    let sid = h.space().await;
    h.sent("alice", &sid, json!({"text": "@agent [slow] one"}))
        .await;
    h.calls_reach(1).await;
    h.sent("alice", &sid, json!({"text": "@agent msg-two"}))
        .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    h.model.open();
    let room = h.until(|| h.room(&sid), |e| replies(e).len() == 2).await;
    assert!(steered(&room).is_empty());
    assert_eq!(phase(&room, "started").len(), 2);
    let call = &h.model.calls()[1];
    assert!(position(call, "reply-1").unwrap().0 < position(call, "msg-two").unwrap().0);
}

#[tokio::test]
async fn a_handed_back_message_sees_only_its_agents_late_replies_to_its_sender() {
    let h = H::start().await;
    let sid = h.space().await;
    h.sent("alice", &sid, json!({"text": "@agent [slow] one"}))
        .await;
    h.calls_reach(1).await;
    h.sent("bob", &sid, json!({"text": "@agent [slow] bobs"}))
        .await;
    h.calls_reach(2).await;
    h.sent("alice", &sid, json!({"text": "@agent msg-two"}))
        .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    h.model.open();
    h.model.open();
    let room = h.until(|| h.room(&sid), |e| replies(e).len() == 3).await;
    assert!(steered(&room).is_empty());
    let alices_run = phase(&room, "started")[0]["activity"]["id"].clone();
    let reply_of = |run: &Value| {
        room.iter()
            .find(|e| e["kind"] == "agent" && e["activity"]["id"] == *run)
            .map(|e| e["text"].as_str().unwrap().to_owned())
            .unwrap()
    };
    let alices = reply_of(&alices_run);
    let bobs_run = phase(&room, "started")[1]["activity"]["id"].clone();
    let bobs = reply_of(&bobs_run);
    let call = h
        .model
        .calls()
        .into_iter()
        .find(|c| position(c, "msg-two").is_some())
        .unwrap();
    assert!(position(&call, &alices).is_some(), "{call}");
    assert!(position(&call, &bobs).is_none(), "{call}");
}

#[tokio::test]
async fn a_steer_not_delivered_before_a_restart_starts_its_run_once() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("metro.sqlite3");
    let before = H::start_on(Some(&file)).await;
    let sid = before.space().await;
    before
        .sent("alice", &sid, json!({"text": "@agent [tool] go"}))
        .await;
    before.calls_reach(1).await;
    before
        .sent("alice", &sid, json!({"text": "@agent left-waiting"}))
        .await;
    // Offered to the running turn, durable until it's delivered.
    assert_eq!(before.pending(), 1);

    // The process stops before the turn's next model call; the next one
    // starts the message's own run, once.
    let after = H::start_on(Some(&file)).await;
    assert_eq!(
        metro_server::decisions::resume(after.state.clone())
            .await
            .unwrap(),
        1
    );
    let room = after
        .until(|| after.room(&sid), |e| !replies(e).is_empty())
        .await;
    assert!(steered(&room).is_empty());
    let calls = after.model.calls();
    assert_eq!(calls.len(), 1);
    assert!(position(&calls[0], "left-waiting").is_some());
    assert_eq!(after.pending(), 0);
    assert_eq!(
        metro_server::decisions::resume(after.state.clone())
            .await
            .unwrap(),
        0
    );
    // The old turn goes on, but no longer holds the message: it neither
    // takes it nor starts it a second time.
    before.model.open();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let room = after.room(&sid).await;
    assert!(steered(&room).is_empty(), "{room:?}");
    assert_eq!(
        phase(&room, "started").len(),
        2,
        "the first run and the steer's own, never another: {room:?}"
    );
    let all = serde_json::to_string(&before.model.calls()).unwrap();
    assert!(!all.contains("left-waiting"), "{all}");
    assert_eq!(after.model.calls().len(), 1);
}

#[tokio::test]
async fn a_delivered_steer_never_starts_a_run_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("metro.sqlite3");
    let before = H::start_on(Some(&file)).await;
    let sid = before.space().await;
    before
        .sent("alice", &sid, json!({"text": "@agent [tool] go"}))
        .await;
    before.calls_reach(1).await;
    let steer = before
        .sent("alice", &sid, json!({"text": "@agent also this"}))
        .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    before.model.open();
    let room = before
        .until(|| before.room(&sid), |e| !replies(e).is_empty())
        .await;
    assert_eq!(steered(&room), [id(&steer)]);
    assert_eq!(before.pending(), 0, "delivery settles the planned run");
    let after = H::start_on(Some(&file)).await;
    assert_eq!(
        metro_server::decisions::resume(after.state.clone())
            .await
            .unwrap(),
        0
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(after.model.calls().is_empty());
    assert_eq!(phase(&after.room(&sid).await, "started").len(), 1);
}

#[tokio::test]
async fn a_sender_who_lost_access_steers_nothing() {
    let h = H::start().await;
    let sid = h.space().await;
    h.sent("bob", &sid, json!({"text": "@agent [tool] go"}))
        .await;
    h.calls_reach(1).await;
    h.sent("bob", &sid, json!({"text": "@agent bob's afterthought"}))
        .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let (s, v) = h
        .req(
            Method::DELETE,
            "alice",
            &format!("/v1/spaces/{sid}/members"),
            Some(json!({"user_id": "bob"})),
        )
        .await;
    assert!(s.is_success(), "{s} {v}");
    h.model.open();
    tokio::time::sleep(Duration::from_millis(800)).await;
    let room = h.room(&sid).await;
    assert!(steered(&room).is_empty(), "{room:?}");
    let all = serde_json::to_string(&h.model.calls()).unwrap();
    assert!(!all.contains("afterthought"), "{all}");
}

// ---- Chat threads: one line and one inbox per conversation ----

impl H {
    async fn thread(&self) -> String {
        let (s, v) = self
            .post("alice", "/v1/chat/threads", json!({"title": "T"}))
            .await;
        assert_eq!(s, StatusCode::CREATED, "{v}");
        v["id"].as_str().unwrap().to_owned()
    }

    async fn say_in(&self, tid: &str, text: &str, delivery: Option<&str>) -> Value {
        let mut body = json!({"text": text});
        if let Some(d) = delivery {
            body["delivery"] = json!(d);
        }
        let (s, v) = self
            .post("alice", &format!("/v1/chat/messages?thread={tid}"), body)
            .await;
        assert_eq!(s, StatusCode::ACCEPTED, "{v}");
        v
    }

    async fn in_thread(&self, tid: &str) -> Vec<Value> {
        let (s, v) = self
            .req(
                Method::GET,
                "alice",
                &format!("/v1/chat/events?thread={tid}"),
                None,
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
        v["events"].as_array().unwrap().clone()
    }
}

#[tokio::test]
async fn a_turn_in_one_conversation_never_blocks_another() {
    let h = H::start().await;
    h.say("[slow] main first", None).await;
    h.calls_reach(1).await;
    let tid = h.thread().await;
    // The thread's turn runs while the main Chat's is still going.
    h.say_in(&tid, "thread question", None).await;
    let t = h
        .until(|| h.in_thread(&tid), |e| !replies(e).is_empty())
        .await;
    assert_eq!(replies(&t), ["reply-2"]);
    // A queued main-Chat message still waits for its own line only.
    h.say("q-main", None).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(h.model.calls().len(), 2, "the main Chat's line is its own");
    // And another thread message doesn't wait for the main Chat either.
    h.say_in(&tid, "thread again", None).await;
    let t = h
        .until(|| h.in_thread(&tid), |e| replies(e).len() == 2)
        .await;
    assert_eq!(replies(&t).len(), 2);
    h.model.open();
    let main = h.until(|| h.chat(), |e| replies(e).len() == 2).await;
    assert_eq!(phase(&main, "started").len(), 2);
    assert_eq!(h.pending(), 0);
    // Each conversation saw only its own messages.
    for call in h.model.calls() {
        let text = call.to_string();
        assert!(
            !(text.contains("main first") && text.contains("thread question")),
            "{text}"
        );
    }
}

#[tokio::test]
async fn a_steer_goes_only_into_the_turn_running_in_the_same_thread() {
    let h = H::start().await;
    h.say("[tool] main lookup", None).await;
    h.calls_reach(1).await;
    let tid = h.thread().await;
    // Nothing runs in the thread: the steer starts its own turn there.
    let steer = h.say_in(&tid, "steer for the thread", Some("steer")).await;
    let t = h
        .until(|| h.in_thread(&tid), |e| !replies(e).is_empty())
        .await;
    assert!(steered(&t).is_empty() && phase(&t, "started").len() == 1);
    assert!(steered(&h.chat().await).is_empty());
    // A steer in the main Chat goes into the main Chat's running turn.
    let main_steer = h.say("steer for main", Some("steer")).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    h.model.open();
    let main = h.until(|| h.chat(), |e| !replies(e).is_empty()).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let main = {
        let _ = main;
        h.chat().await
    };
    assert_eq!(steered(&main), [id(&main_steer)]);
    assert_eq!(phase(&main, "started").len(), 1);
    for call in h.model.calls() {
        let text = call.to_string();
        if text.contains("main lookup") {
            assert!(!text.contains("steer for the thread"), "{text}");
        }
    }
    assert!(!h.chat().await.iter().any(|e| id(e) == id(&steer)));
    assert_eq!(h.pending(), 0);
}

#[tokio::test]
async fn deleting_a_thread_stops_its_running_reply_and_keeps_nothing() {
    // Errors the server logs meanwhile (tasks run on this test's thread).
    let logged = Arc::new(Mutex::new(Vec::<u8>::new()));
    let sink = logged.clone();
    let _logs = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::ERROR)
            .with_ansi(false)
            .with_writer(move || Sink(sink.clone()))
            .finish(),
    );
    let h = H::start().await;
    let tid = h.thread().await;
    h.say_in(&tid, "[slow] long answer", None).await;
    h.calls_reach(1).await;
    // Queued behind it in the same thread.
    h.say_in(&tid, "queued in thread", None).await;
    let (s, v) = h
        .req(
            Method::DELETE,
            "alice",
            &format!("/v1/chat/threads/{tid}"),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    // The running turn stops; let the model answer after the fact.
    h.model.open();
    for _ in 0..200 {
        if h.state.chat_runs.turns_of("alice").is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(h.state.chat_runs.turns_of("alice").is_empty());
    tokio::time::sleep(Duration::from_millis(300)).await;
    // Nothing of it was written after the delete, here or in the main Chat,
    // and the queued message reached no model.
    assert!(
        h.state
            .db
            .chat_after("alice", Some(&tid), 0, 100)
            .unwrap()
            .is_empty()
    );
    assert!(h.chat().await.is_empty());
    assert_eq!(h.model.calls().len(), 1);
    assert_eq!(h.pending(), 0);
    // The main Chat still answers.
    h.say("still here", None).await;
    h.until(|| h.chat(), |e| !replies(e).is_empty()).await;
    let logged = String::from_utf8(logged.lock().unwrap().clone()).unwrap();
    assert!(logged.is_empty(), "the server logged errors: {logged}");
}

struct Sink(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
