//! Deleting messages, and System One decisions made after a message is
//! stored, over HTTP against a scripted OpenAI-compatible model and a
//! scripted, optionally slow System One.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Json;
use axum::routing::post;
use metro_model_protocol::Capability;
use metro_server::auth::{AuthFailure, AuthFuture, HumanAuth, UserIdentity};
use metro_server::db::Db;
use metro_server::model_routing::{BoxFuture, Routing, RoutingBackend};
use metro_server::providers::{ProviderError, SelectedModel, UseError};
use metro_server::{AppState, Config, Limits, ModelConfig, router};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

struct FakeAuth;

impl HumanAuth for FakeAuth {
    fn identity<'a>(&'a self, bearer: &'a str) -> AuthFuture<'a> {
        Box::pin(async move {
            match bearer {
                "alice" | "bob" | "carol" => Ok(UserIdentity {
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

const CLASSIFIER: &str = "pc_11111111111111111111111111111111";

/// Alice's System One: picks the first agent, after `delay_ms`; fails for
/// a message containing `[fail]`.
#[derive(Default)]
struct Backend {
    calls: Mutex<usize>,
    delay_ms: AtomicU64,
}

impl RoutingBackend for Backend {
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
            *self.calls.lock().unwrap() += 1;
            let delay = self.delay_ms.load(Ordering::SeqCst);
            if delay > 0 {
                tokio::time::sleep(Duration::from_millis(delay)).await;
            }
            if user != "alice" || id != CLASSIFIER {
                return Err(ProviderError::Use(UseError::NotFound).into());
            }
            if payload["message"]
                .as_str()
                .is_some_and(|m| m.contains("[fail]"))
            {
                anyhow::bail!("System One is down");
            }
            let has_none = questions["agent"]["criteria"].get("no_agent").is_some();
            let mut probabilities = json!({"agent_1": 0.95});
            if has_none {
                probabilities["no_agent"] = json!(0.05);
            }
            Ok(json!({"model": "jev", "usage": {}, "answers": {"agent": {
                "type": "choice", "choice": "agent_1", "confidence": 0.9,
                "probabilities": probabilities}}}))
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

/// An OpenAI-compatible endpoint that answers "ok" and records requests.
async fn model(calls: Arc<Mutex<Vec<Value>>>) -> String {
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        post(move |Json(body): Json<Value>| {
            let calls = calls.clone();
            async move {
                calls.lock().unwrap().push(body.clone());
                Json(json!({
                    "id": "c", "object": "chat.completion", "created": 0, "model": body["model"],
                    "choices": [{"index": 0, "finish_reason": "stop",
                                 "message": {"role": "assistant", "content": "ok"}}],
                    "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                }))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}/v1")
}

struct H {
    base: String,
    http: reqwest::Client,
    backend: Arc<Backend>,
    model_calls: Arc<Mutex<Vec<Value>>>,
    state: AppState,
}

impl H {
    async fn start() -> Self {
        Self::start_with(None, 0).await
    }

    /// A server on `file` (a fresh in-memory database without one) whose
    /// System One answers after `delay_ms`.
    async fn start_with(file: Option<&std::path::Path>, delay_ms: u64) -> Self {
        let model_calls = Arc::new(Mutex::new(Vec::new()));
        let config = Config {
            bind: "127.0.0.1:0".parse().unwrap(),
            db_path: file.map_or(":memory:".into(), |f| f.to_path_buf()),
            model: Some(ModelConfig {
                base_url: model(model_calls.clone()).await,
                model: "env-model".into(),
                api_key: String::new(),
            }),
            limits: Limits {
                agent_timeout: Duration::from_secs(20),
                ..Limits::default()
            },
            org_admins: vec!["carol".into()],
        };
        let db = match file {
            Some(f) => Db::open(f).unwrap(),
            None => Db::open_in_memory().unwrap(),
        };
        let mut state = AppState::new(config, db, Arc::new(FakeAuth)).await.unwrap();
        let backend = Arc::new(Backend::default());
        backend.delay_ms.store(delay_ms, Ordering::SeqCst);
        state.routing = Routing::new(backend.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let served = state.clone();
        tokio::spawn(async move { axum::serve(listener, router(served)).await.unwrap() });
        Self {
            base,
            http: reqwest::Client::new(),
            backend,
            model_calls,
            state,
        }
    }

    fn pending(&self) -> i64 {
        metro_server::decisions::count(&self.state.db).unwrap()
    }

    async fn chat(&self, who: &str) -> Vec<Value> {
        let (s, v) = self.req(Method::GET, who, "/v1/chat/events", None).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        v["events"].as_array().unwrap().clone()
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

    async fn delete(&self, who: &str, path: &str) -> (StatusCode, Value) {
        self.req(Method::DELETE, who, path, None).await
    }

    /// A Space owned by alice with bob as a member; its id.
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

    async fn sent(&self, who: &str, sid: &str, rid: &str, body: Value) -> Value {
        let (s, v) = self
            .post(who, &format!("/v1/spaces/{sid}/rooms/{rid}/messages"), body)
            .await;
        assert_eq!(s, StatusCode::ACCEPTED, "{v}");
        v
    }

    async fn events(&self, who: &str, sid: &str, rid: &str) -> Vec<Value> {
        let (s, v) = self
            .req(
                Method::GET,
                who,
                &format!("/v1/spaces/{sid}/rooms/{rid}/events"),
                None,
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
        v["events"].as_array().unwrap().clone()
    }

    async fn wait_for(
        &self,
        sid: &str,
        rid: &str,
        after: i64,
        pred: impl Fn(&Value) -> bool,
    ) -> Value {
        for _ in 0..400 {
            if let Some(e) = self
                .events("alice", sid, rid)
                .await
                .into_iter()
                .find(|e| e["id"].as_i64().unwrap() > after && pred(e))
            {
                return e;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("nothing matched after {after}");
    }

    async fn reply_after(&self, sid: &str, rid: &str, after: i64) -> Value {
        self.wait_for(sid, rid, after, |e| e["kind"] == "agent")
            .await
    }

    async fn classify(&self, sid: &str) {
        let (s, v) = self
            .req(
                Method::PUT,
                "alice",
                "/v1/model-routing",
                Some(json!({"enabled": false, "infer_addressing": false,
                            "classifier_id": CLASSIFIER, "cases": []})),
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
        let (s, v) = self
            .req(
                Method::PUT,
                "alice",
                &format!("/v1/spaces/{sid}/prefs"),
                Some(json!({"classify_unaddressed": true})),
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
    }

    fn classifier_calls(&self) -> usize {
        *self.backend.calls.lock().unwrap()
    }

    async fn decisions(&self, who: &str, sid: &str, eid: i64) -> (StatusCode, Value) {
        self.post(
            who,
            &format!("/v1/spaces/{sid}/rooms/lobby/messages/{eid}/decisions"),
            json!({}),
        )
        .await
    }

    /// Every text part the model was sent, in every request so far.
    fn model_saw(&self) -> String {
        serde_json::to_string(&*self.model_calls.lock().unwrap()).unwrap()
    }
}

fn id(v: &Value) -> i64 {
    v["id"].as_i64().unwrap()
}

fn ids(events: &[Value]) -> Vec<i64> {
    events.iter().map(id).collect()
}

fn payload(marker: &Value) -> Value {
    assert_eq!(marker["kind"], "message_deleted", "{marker}");
    serde_json::from_str(marker["text"].as_str().unwrap()).unwrap()
}

// ---- Decisions after the message is stored ----

#[tokio::test]
async fn a_deferred_classification_shows_the_message_before_system_one_answers() {
    let h = H::start().await;
    let sid = h.space().await;
    h.classify(&sid).await;
    h.backend.delay_ms.store(700, Ordering::SeqCst);

    // Before: the POST (and so the message's appearance) waits on System One.
    let t = Instant::now();
    let sync = h
        .sent("alice", &sid, "lobby", json!({"text": "sync question"}))
        .await;
    let sync_ms = t.elapsed();
    assert!(sync.get("decisions_pending").is_none(), "{sync}");
    assert!(sync_ms >= Duration::from_millis(700), "{sync_ms:?}");
    h.reply_after(&sid, "lobby", id(&sync)).await;

    // After: stored and visible at once; the decision follows.
    let calls = h.classifier_calls();
    let t = Instant::now();
    let posted = h
        .sent(
            "alice",
            &sid,
            "lobby",
            json!({"text": "deferred question", "defer_decisions": true}),
        )
        .await;
    let deferred_ms = t.elapsed();
    let visible = h.events("bob", &sid, "lobby").await;
    let shown_ms = t.elapsed();
    assert_eq!(posted["decisions_pending"], true, "{posted}");
    assert!(posted["addressing_note"].is_null() && posted["routing_note"].is_null());
    assert!(ids(&visible).contains(&id(&posted)), "not visible yet");
    assert!(deferred_ms < Duration::from_millis(400), "{deferred_ms:?}");
    // The classifier's pick is run activity; it never edits the message.
    assert_eq!(posted["mentions"], json!([]));
    println!(
        "latency to the message: synchronous {sync_ms:?}, deferred POST {deferred_ms:?}, \
         visible to another member after {shown_ms:?}"
    );

    // Only the sender collects the notes, and only once.
    assert_eq!(
        h.decisions("bob", &sid, id(&posted)).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        h.decisions("carol", &sid, id(&posted)).await.0,
        StatusCode::NOT_FOUND
    );
    let (s, decided) = h.decisions("alice", &sid, id(&posted)).await;
    assert_eq!(s, StatusCode::OK, "{decided}");
    assert_eq!(decided["id"], posted["id"]);
    assert_eq!(decided["classified"], true);
    assert!(decided["addressing_note"].is_null(), "{decided}");
    assert_eq!(h.classifier_calls(), calls + 1, "exactly one decision");
    let started = h
        .wait_for(&sid, "lobby", id(&posted), |e| {
            e["kind"] == "activity" && e["activity"]["phase"] == "started"
        })
        .await;
    // The pick is the run's agent, with no "Picked by classifier" label.
    assert!(started["activity"].get("pick").is_none(), "{started}");
    assert!(started["activity"]["agent"]["id"].is_string(), "{started}");
    h.reply_after(&sid, "lobby", id(&posted)).await;
    assert_eq!(
        h.decisions("alice", &sid, id(&posted)).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(h.classifier_calls(), calls + 1, "never classified twice");
    // The polled row never carries a sender note.
    let row = h
        .events("alice", &sid, "lobby")
        .await
        .into_iter()
        .find(|e| id(e) == id(&posted))
        .unwrap();
    assert!(row.get("addressing_note").is_none() && row.get("classified").is_none());
}

#[tokio::test]
async fn a_deferred_failure_note_is_sender_only_post_metadata() {
    let h = H::start().await;
    let sid = h.space().await;
    h.classify(&sid).await;
    let posted = h
        .sent(
            "alice",
            &sid,
            "lobby",
            json!({"text": "[fail] is this for anyone", "defer_decisions": true}),
        )
        .await;
    assert_eq!(posted["decisions_pending"], true);
    let (s, decided) = h.decisions("alice", &sid, id(&posted)).await;
    assert_eq!(s, StatusCode::OK, "{decided}");
    assert_eq!(decided["addressing_note"]["kind"], "classification_failed");
    assert_eq!(decided["classified"], false);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let later: Vec<Value> = h
        .events("bob", &sid, "lobby")
        .await
        .into_iter()
        .filter(|e| id(e) > id(&posted))
        .collect();
    assert!(
        later.is_empty(),
        "no agent work and no room note: {later:?}"
    );
    // Repeat collection and other rooms or ids find nothing.
    assert_eq!(
        h.decisions("alice", &sid, id(&posted)).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        h.decisions("alice", &sid, id(&posted) + 1000).await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn people_only_and_named_agents_are_never_deferred_or_classified() {
    let h = H::start().await;
    let sid = h.space().await;
    h.classify(&sid).await;
    let (_, agents) = h
        .req(
            Method::GET,
            "alice",
            &format!("/v1/spaces/{sid}/agents"),
            None,
        )
        .await;
    let agent = agents["agents"][0]["id"].as_str().unwrap().to_owned();
    let calls = h.classifier_calls();
    let people = h
        .sent(
            "alice",
            &sid,
            "lobby",
            json!({"text": "hi bob", "mentions": [{"kind": "user", "id": "bob"}],
                   "infer_text_mentions": false, "defer_decisions": true}),
        )
        .await;
    assert!(people.get("decisions_pending").is_none(), "{people}");
    let named = h
        .sent(
            "alice",
            &sid,
            "lobby",
            json!({"text": "please answer", "mentions": [{"kind": "agent", "agent_id": agent}],
                   "infer_text_mentions": false, "defer_decisions": true}),
        )
        .await;
    assert!(named.get("decisions_pending").is_none(), "{named}");
    h.reply_after(&sid, "lobby", id(&named)).await;
    assert_eq!(h.classifier_calls(), calls);
    assert_eq!(
        h.decisions("alice", &sid, id(&named)).await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn personal_chat_returns_before_its_turn_starts() {
    let h = H::start().await;
    let (s, v) = h
        .post("alice", "/v1/chat/messages", json!({"text": "hello Chat"}))
        .await;
    assert_eq!(s, StatusCode::ACCEPTED, "{v}");
    for _ in 0..400 {
        let (_, chat) = h.req(Method::GET, "alice", "/v1/chat/events", None).await;
        if chat["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"] == "agent")
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("the Chat turn never answered");
}

// ---- Deleting messages ----

#[tokio::test]
async fn space_message_deletion_follows_ownership_and_membership() {
    let h = H::start().await;
    let sid = h.space().await;
    let room = format!("/v1/spaces/{sid}/rooms/lobby");
    let bobs = h
        .sent("bob", &sid, "lobby", json!({"text": "bob says hi"}))
        .await;
    let alices = h
        .sent("alice", &sid, "lobby", json!({"text": "alice says hi"}))
        .await;
    let (s, _) = h
        .req(
            Method::PUT,
            "alice",
            &format!("{room}/events/{}/reactions", id(&bobs)),
            Some(json!({"emoji": "👍"})),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let ask = h
        .sent(
            "alice",
            &sid,
            "lobby",
            json!({"text": "@agent hello", "defer_decisions": true}),
        )
        .await;
    let reply = h.reply_after(&sid, "lobby", id(&ask)).await;
    let activity = h
        .events("alice", &sid, "lobby")
        .await
        .into_iter()
        .find(|e| e["kind"] == "activity")
        .unwrap();

    // Outsiders (an org admin too) and unknown ids: 404. Members: 403 for
    // what isn't theirs. Non-messages: 409.
    for (who, path, want) in [
        (
            "carol",
            format!("{room}/events/{}", id(&bobs)),
            StatusCode::NOT_FOUND,
        ),
        (
            "bob",
            format!("/v1/spaces/nope/rooms/lobby/events/{}", id(&bobs)),
            StatusCode::NOT_FOUND,
        ),
        (
            "bob",
            format!("/v1/spaces/{sid}/rooms/nope/events/{}", id(&bobs)),
            StatusCode::NOT_FOUND,
        ),
        (
            "bob",
            format!("{room}/events/999999"),
            StatusCode::NOT_FOUND,
        ),
        (
            "bob",
            format!("{room}/events/{}", id(&alices)),
            StatusCode::FORBIDDEN,
        ),
        (
            "bob",
            format!("{room}/events/{}", id(&reply)),
            StatusCode::FORBIDDEN,
        ),
        (
            "alice",
            format!("{room}/events/{}", id(&activity)),
            StatusCode::CONFLICT,
        ),
    ] {
        let (s, v) = h.delete(who, &path).await;
        assert_eq!(s, want, "{who} {path}: {v}");
    }
    let before = h.events("alice", &sid, "lobby").await;
    let cursor = id(before.last().unwrap());
    let runs_before = before.iter().filter(|e| e["kind"] == "activity").count();

    // Bob deletes his own message; its reaction (and reaction event) go too.
    let (s, deleted) = h
        .delete("bob", &format!("{room}/events/{}", id(&bobs)))
        .await;
    assert_eq!(s, StatusCode::OK, "{deleted}");
    assert_eq!(
        payload(&deleted["events"][0]),
        json!({"target_event_id": id(&bobs), "tombstone": false})
    );
    assert_eq!(
        h.delete("bob", &format!("{room}/events/{}", id(&bobs)))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    // The owner deletes another member's message and an agent reply.
    let (s, _) = h
        .delete("alice", &format!("{room}/events/{}", id(&reply)))
        .await;
    assert_eq!(s, StatusCode::OK);
    let after = h.events("bob", &sid, "lobby").await;
    for gone in [id(&bobs), id(&reply)] {
        assert!(!ids(&after).contains(&gone), "{gone} still listed");
    }
    assert!(
        !after.iter().any(|e| e["kind"] == "reaction"),
        "reaction events of a deleted message remain"
    );
    let (s, _) = h
        .delete("alice", &format!("{room}/events/{}", id(&alices)))
        .await;
    assert_eq!(s, StatusCode::OK);
    // Others pick it up from their cursor; nothing started.
    let (_, polled) = h
        .req(
            Method::GET,
            "bob",
            &format!("{room}/events?after={cursor}"),
            None,
        )
        .await;
    let markers: Vec<Value> = polled["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(payload)
        .collect();
    assert_eq!(markers.len(), 3, "{polled}");
    tokio::time::sleep(Duration::from_millis(200)).await;
    let now = h.events("alice", &sid, "lobby").await;
    assert_eq!(
        now.iter().filter(|e| e["kind"] == "activity").count(),
        runs_before,
        "deleting started agent work"
    );
}

#[tokio::test]
async fn a_deleted_message_is_no_longer_model_history() {
    let h = H::start().await;
    let sid = h.space().await;
    let room = format!("/v1/spaces/{sid}/rooms/lobby");
    let secret = h
        .sent(
            "bob",
            &sid,
            "lobby",
            json!({"text": "the password is swordfish"}),
        )
        .await;
    let (s, _) = h
        .delete("bob", &format!("{room}/events/{}", id(&secret)))
        .await;
    assert_eq!(s, StatusCode::OK);
    let ask = h
        .sent(
            "alice",
            &sid,
            "lobby",
            json!({"text": "@agent what did bob say?"}),
        )
        .await;
    h.reply_after(&sid, "lobby", id(&ask)).await;
    let saw = h.model_saw();
    assert!(
        saw.contains("what did bob say"),
        "the turn reached the model"
    );
    assert!(
        !saw.contains("swordfish"),
        "a deleted message reached the model"
    );
}

#[tokio::test]
async fn a_thread_anchor_with_replies_leaves_a_tombstone() {
    let h = H::start().await;
    let sid = h.space().await;
    let room = format!("/v1/spaces/{sid}/rooms/lobby");
    let anchored = h
        .sent("bob", &sid, "lobby", json!({"text": "start a thread"}))
        .await;
    let (s, thread) = h
        .post(
            "alice",
            &format!("{room}/events/{}/thread", id(&anchored)),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{thread}");
    let tid = thread["id"].as_str().unwrap().to_owned();
    h.sent(
        "alice",
        &sid,
        &tid,
        json!({"text": "a reply", "reply_to": id(&anchored)}),
    )
    .await;
    let (s, deleted) = h
        .delete("bob", &format!("{room}/events/{}", id(&anchored)))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(payload(&deleted["events"][0])["tombstone"], true);
    let listed = h.events("alice", &sid, "lobby").await;
    let stone = listed.iter().find(|e| id(e) == id(&anchored)).unwrap();
    assert_eq!(stone["kind"], "deleted");
    assert_eq!(stone["text"], "");
    assert_eq!(stone["mentions"], json!([]));
    assert_eq!(stone["replies"]["count"], 1, "{stone}");
    // The thread still opens; a tombstone gets no new thread, reply or delete.
    let (s, again) = h
        .post(
            "bob",
            &format!("{room}/events/{}/thread", id(&anchored)),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{again}");
    assert_eq!(again["id"], tid);
    assert_eq!(
        h.delete("alice", &format!("{room}/events/{}", id(&anchored)))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let (s, _) = h
        .post(
            "alice",
            &format!("/v1/spaces/{sid}/rooms/{tid}/messages"),
            json!({"text": "to the stone", "reply_to": id(&anchored)}),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    // An empty thread stays too, under a tombstone with no replies.
    let lonely = h
        .sent("bob", &sid, "lobby", json!({"text": "no replies"}))
        .await;
    let (s, empty) = h
        .post(
            "bob",
            &format!("{room}/events/{}/thread", id(&lonely)),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, deleted) = h
        .delete("bob", &format!("{room}/events/{}", id(&lonely)))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(payload(&deleted["events"][0])["tombstone"], true);
    let stone = h
        .events("alice", &sid, "lobby")
        .await
        .into_iter()
        .find(|e| id(e) == id(&lonely))
        .unwrap();
    assert_eq!(stone["kind"], "deleted");
    assert_eq!(stone["replies"]["count"], 0, "{stone}");
    let (_, rooms) = h
        .req(Method::GET, "bob", &format!("/v1/spaces/{sid}/rooms"), None)
        .await;
    assert!(
        rooms["rooms"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"] == empty["id"]),
        "{rooms}"
    );
    let (s, again) = h
        .post(
            "bob",
            &format!("{room}/events/{}/thread", id(&lonely)),
            json!({}),
        )
        .await;
    assert_eq!((s, &again["id"]), (StatusCode::OK, &empty["id"]));
}

#[tokio::test]
async fn personal_chat_deletion_is_the_owners_alone() {
    let h = H::start().await;
    let (s, note) = h
        .post(
            "alice",
            "/v1/chat/messages",
            json!({"text": "a note", "save_note": true}),
        )
        .await;
    assert_eq!(s, StatusCode::ACCEPTED);
    let (s, asked) = h
        .post("alice", "/v1/chat/messages", json!({"text": "hello"}))
        .await;
    assert_eq!(s, StatusCode::ACCEPTED);
    let mut reply = Value::Null;
    for _ in 0..400 {
        let (_, chat) = h.req(Method::GET, "alice", "/v1/chat/events", None).await;
        if let Some(r) = chat["events"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["kind"] == "agent" && id(e) > id(&asked))
        {
            reply = r.clone();
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(!reply.is_null(), "no reply");
    // Another person's Chat ids are unknown to them; activity can't go.
    assert_eq!(
        h.delete("bob", &format!("/v1/chat/events/{}", id(&note)))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let (_, chat) = h.req(Method::GET, "alice", "/v1/chat/events", None).await;
    let activity = chat["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "activity")
        .unwrap()
        .clone();
    assert_eq!(
        h.delete("alice", &format!("/v1/chat/events/{}", id(&activity)))
            .await
            .0,
        StatusCode::CONFLICT
    );
    for target in [&note, &reply] {
        let (s, v) = h
            .delete("alice", &format!("/v1/chat/events/{}", id(target)))
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
        assert_eq!(payload(&v["events"][0])["target_event_id"], target["id"]);
    }
    let (_, chat) = h.req(Method::GET, "alice", "/v1/chat/events", None).await;
    let left = ids(chat["events"].as_array().unwrap());
    assert!(!left.contains(&id(&note)) && !left.contains(&id(&reply)));
    assert!(left.contains(&id(&asked)));
}

// ---- Durable, rechecked decisions ----

fn started_for(events: &[Value], after: i64) -> usize {
    events
        .iter()
        .filter(|e| id(e) > after && e["kind"] == "activity" && e["activity"]["phase"] == "started")
        .count()
}

#[tokio::test]
async fn a_restart_makes_a_pending_decision_once() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("metro.sqlite3");
    // The first server's System One never answers in time: it "crashes"
    // with the decision pending.
    let before = H::start_with(Some(&file), 60_000).await;
    let sid = before.space().await;
    before.classify(&sid).await;
    let posted = before
        .sent(
            "alice",
            &sid,
            "lobby",
            json!({"text": "decide after a restart", "defer_decisions": true}),
        )
        .await;
    assert_eq!(posted["decisions_pending"], true);
    assert_eq!(
        before.pending(),
        1,
        "the pending decision is stored with its message"
    );
    // A Chat turn left pending by a crash between storing and starting.
    let note = {
        let author = metro_server::db::Author {
            id: Some("bob".into()),
            name: "bob".into(),
        };
        let addressed = metro_server::db::Addressed {
            mentions: json!([{"kind": "agent"}]),
            audience: "agent".into(),
            spans: None,
        };
        let eid = before
            .state
            .db
            .append_personal(
                "bob",
                "message",
                &author,
                "chat after a restart",
                &addressed,
            )
            .unwrap()
            .id;
        metro_server::decisions::insert(
            &before.state.db,
            &metro_server::decisions::NewPending {
                scope: metro_server::decisions::Scope::Personal,
                user_id: "bob".into(),
                place: None,
                payload: json!({"choice": null}),
            },
            eid,
        )
        .unwrap();
        eid
    };
    assert_eq!(before.pending(), 2);

    let after = H::start_with(Some(&file), 0).await;
    assert_eq!(
        metro_server::decisions::resume(after.state.clone())
            .await
            .unwrap(),
        2
    );
    after.reply_after(&sid, "lobby", id(&posted)).await;
    let mut answered = false;
    for _ in 0..400 {
        if after
            .chat("bob")
            .await
            .iter()
            .any(|e| e["kind"] == "agent" && id(e) > note)
        {
            answered = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        answered,
        "the pending Chat turn didn't start after the restart"
    );
    assert_eq!(after.classifier_calls(), 1);
    assert_eq!(after.pending(), 0, "a decision that ran is settled");
    // Another restart finds nothing to do: no double dispatch.
    assert_eq!(
        metro_server::decisions::resume(after.state.clone())
            .await
            .unwrap(),
        0
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    let room = after.events("alice", &sid, "lobby").await;
    assert_eq!(started_for(&room, id(&posted)), 1);
    assert_eq!(started_for(&after.chat("bob").await, note), 1);
    assert_eq!(after.classifier_calls(), 1);
}

#[tokio::test]
async fn a_message_deleted_while_system_one_decides_starts_nothing() {
    let h = H::start_with(None, 700).await;
    let sid = h.space().await;
    h.classify(&sid).await;
    let room = format!("/v1/spaces/{sid}/rooms/lobby");
    let calls = h.model_calls.lock().unwrap().len();
    let posted = h
        .sent(
            "alice",
            &sid,
            "lobby",
            json!({"text": "never mind this", "defer_decisions": true}),
        )
        .await;
    assert_eq!(posted["decisions_pending"], true);
    let (s, _) = h
        .delete("alice", &format!("{room}/events/{}", id(&posted)))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(h.pending(), 0, "deleting drops the pending decision");
    assert_eq!(
        h.decisions("alice", &sid, id(&posted)).await.0,
        StatusCode::NOT_FOUND
    );
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let events = h.events("alice", &sid, "lobby").await;
    assert_eq!(started_for(&events, id(&posted)), 0, "{events:?}");
    assert!(!events.iter().any(|e| e["kind"] == "error"), "{events:?}");
    assert_eq!(
        h.model_calls.lock().unwrap().len(),
        calls,
        "the model was called"
    );
}

#[tokio::test]
async fn a_chat_message_deleted_before_its_turn_starts_nothing() {
    let h = H::start().await;
    // Hold Alice's line: her next turn waits behind this ticket.
    let held = h.state.decisions.chat_ticket("alice");
    let (s, sent) = h
        .post("alice", "/v1/chat/messages", json!({"text": "forget this"}))
        .await;
    assert_eq!(s, StatusCode::ACCEPTED, "{sent}");
    let (s, _) = h
        .delete("alice", &format!("/v1/chat/events/{}", id(&sent)))
        .await;
    assert_eq!(s, StatusCode::OK);
    drop(held);
    tokio::time::sleep(Duration::from_millis(400)).await;
    let chat = h.chat("alice").await;
    assert_eq!(started_for(&chat, 0), 0, "{chat:?}");
    assert!(h.model_calls.lock().unwrap().is_empty());
    assert_eq!(h.pending(), 0);
    assert!(h.state.chat_runs.turns_of("alice").is_empty());
}

#[tokio::test]
async fn a_chat_turn_can_be_stopped_before_it_starts_and_turns_keep_send_order() {
    let h = H::start().await;
    let held = h.state.decisions.chat_ticket("alice");
    let mut sent = Vec::new();
    for text in ["first", "second", "third"] {
        let (s, v) = h
            .post("alice", "/v1/chat/messages", json!({"text": text}))
            .await;
        assert_eq!(s, StatusCode::ACCEPTED, "{v}");
        sent.push(id(&v));
    }
    // All three are registered while they wait, so Stop finds them.
    let mut turns = Vec::new();
    for _ in 0..100 {
        turns = h.state.chat_runs.turns_of("alice");
        if turns.len() == 3 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(turns.len(), 3);
    // Compaction waits while a turn is registered.
    let (s, _) = h.post("alice", "/v1/chat/compact", json!({})).await;
    assert_eq!(s, StatusCode::CONFLICT);
    // Stop one of them (which one doesn't matter: each answers its own message).
    let (s, v) = h
        .post(
            "alice",
            &format!("/v1/chat/turns/{}/stop", turns[0]),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    drop(held);
    for _ in 0..400 {
        let chat = h.chat("alice").await;
        if chat.iter().filter(|e| e["kind"] == "agent").count() >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    let chat = h.chat("alice").await;
    assert_eq!(
        chat.iter().filter(|e| e["kind"] == "agent").count(),
        2,
        "{chat:?}"
    );
    assert_eq!(started_for(&chat, 0), 2, "the stopped turn never started");
    // Started markers follow send order: each before the next message's.
    let firsts: Vec<i64> = chat
        .iter()
        .filter(|e| e["kind"] == "activity" && e["activity"]["phase"] == "started")
        .map(id)
        .collect();
    assert!(firsts.windows(2).all(|w| w[0] < w[1]));
    assert_eq!(h.pending(), 0);
    let _ = sent;
}

#[tokio::test]
async fn a_tombstone_has_no_decisions_and_isnt_in_reply_thread_history() {
    let h = H::start_with(None, 300).await;
    let sid = h.space().await;
    h.classify(&sid).await;
    let room = format!("/v1/spaces/{sid}/rooms/lobby");
    let anchor = h
        .sent(
            "alice",
            &sid,
            "lobby",
            json!({"text": "the anchor says pineapple", "defer_decisions": true}),
        )
        .await;
    let (s, thread) = h
        .post(
            "bob",
            &format!("{room}/events/{}/thread", id(&anchor)),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, _) = h
        .delete("alice", &format!("{room}/events/{}", id(&anchor)))
        .await;
    assert_eq!(s, StatusCode::OK);
    // A tombstone is not the sender's message any more.
    assert_eq!(
        h.decisions("alice", &sid, id(&anchor)).await.0,
        StatusCode::NOT_FOUND
    );
    let tid = thread["id"].as_str().unwrap();
    let ask = h
        .sent(
            "bob",
            &sid,
            tid,
            json!({"text": "@agent what was it about?"}),
        )
        .await;
    h.reply_after(&sid, tid, id(&ask)).await;
    let saw = h.model_saw();
    assert!(saw.contains("what was it about"));
    assert!(
        !saw.contains("pineapple"),
        "a deleted anchor reached the model"
    );
    assert!(
        !saw.contains("replying_to"),
        "a deleted anchor is still in the preamble"
    );
}
