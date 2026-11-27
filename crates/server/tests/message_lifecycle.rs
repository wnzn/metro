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
                b if ["alice", "bob", "carol"].contains(&b) || b.starts_with("user") => {
                    Ok(UserIdentity {
                        id: bearer.to_owned(),
                        actor: bearer.to_owned(),
                    })
                }
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
    /// Every state payload System One was sent.
    seen: Mutex<Vec<Value>>,
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
            self.seen.lock().unwrap().push(payload.clone());
            let delay = self.delay_ms.load(Ordering::SeqCst);
            if delay > 0 {
                tokio::time::sleep(Duration::from_millis(delay)).await;
            }
            if id != CLASSIFIER {
                return Err(ProviderError::Use(UseError::NotFound).into());
            }
            let _ = user;
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
            Ok((!user.is_empty() && id == CLASSIFIER).then_some(Capability::SystemOne))
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

    /// With room for many runs at once.
    async fn roomy(delay_ms: u64) -> Self {
        Self::start_limits(None, delay_ms, |l| Limits {
            max_agent_runs: 64,
            max_space_runs: 64,
            ..l
        })
        .await
    }

    async fn start_with(file: Option<&std::path::Path>, delay_ms: u64) -> Self {
        Self::start_limits(file, delay_ms, |l| l).await
    }

    /// A server on `file` (a fresh in-memory database without one) whose
    /// System One answers after `delay_ms`.
    async fn start_limits(
        file: Option<&std::path::Path>,
        delay_ms: u64,
        limits: impl FnOnce(Limits) -> Limits,
    ) -> Self {
        let model_calls = Arc::new(Mutex::new(Vec::new()));
        let config = Config {
            bind: "127.0.0.1:0".parse().unwrap(),
            db_path: file.map_or(":memory:".into(), |f| f.to_path_buf()),
            model: Some(ModelConfig {
                base_url: model(model_calls.clone()).await,
                model: "env-model".into(),
                api_key: String::new(),
            }),
            limits: limits(Limits {
                agent_timeout: Duration::from_secs(20),
                ..Limits::default()
            }),
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
        self.classify_as("alice", sid).await;
    }

    async fn classify_as(&self, who: &str, sid: &str) {
        let (s, v) = self
            .req(
                Method::PUT,
                who,
                "/v1/model-routing",
                Some(json!({"enabled": false, "infer_addressing": false,
                            "classifier_id": CLASSIFIER, "cases": []})),
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
        let (s, v) = self
            .req(
                Method::PUT,
                who,
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
                // Stored by the crashed process.
                claimed_by: "epoch_before".into(),
                turn_id: None,
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

// ---- Second review: previews, planned runs, claims, live triggers, Stop ----

#[tokio::test]
async fn a_deleted_trigger_leaves_no_preview_in_the_run_list() {
    let h = H::start().await;
    let sid = h.space().await;
    let room = format!("/v1/spaces/{sid}/rooms/lobby");
    let ask = h
        .sent(
            "alice",
            &sid,
            "lobby",
            json!({"text": "@agent the secret plan is tulips"}),
        )
        .await;
    h.reply_after(&sid, "lobby", id(&ask)).await;
    let (_, runs) = h.req(Method::GET, "bob", "/v1/runs", None).await;
    let run = runs["runs"][0].clone();
    assert!(run["task"].as_str().unwrap().contains("tulips"), "{run}");
    let (s, _) = h
        .delete("alice", &format!("{room}/events/{}", id(&ask)))
        .await;
    assert_eq!(s, StatusCode::OK);
    let (_, runs) = h.req(Method::GET, "bob", "/v1/runs", None).await;
    assert_eq!(runs["runs"][0]["id"], run["id"]);
    assert_eq!(runs["runs"][0]["task"], "", "{runs}");
    assert_eq!(runs["runs"][0]["status"], run["status"], "the status stays");
    let (_, one) = h
        .req(
            Method::GET,
            "bob",
            &format!("/v1/runs/{}", run["id"].as_str().unwrap()),
            None,
        )
        .await;
    assert_eq!(one["task"], "");
    assert!(!serde_json::to_string(&runs).unwrap().contains("tulips"));
}

#[tokio::test]
async fn queued_work_never_sends_a_deleted_message_to_system_one() {
    let h = H::start().await;
    let sid = h.space().await;
    h.classify(&sid).await;
    let room = format!("/v1/spaces/{sid}/rooms/lobby");
    // Alice's decision slots are busy: hers waits in line.
    let a = h.state.decisions.slot("alice").await;
    let b = h.state.decisions.slot("alice").await;
    let posted = h
        .sent(
            "alice",
            &sid,
            "lobby",
            json!({"text": "queued then deleted", "defer_decisions": true}),
        )
        .await;
    let (s, _) = h
        .delete("alice", &format!("{room}/events/{}", id(&posted)))
        .await;
    assert_eq!(s, StatusCode::OK);
    drop((a, b));
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        h.classifier_calls(),
        0,
        "a deleted message reached System One"
    );
    assert_eq!(h.pending(), 0);
    assert!(h.model_calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn deferred_classification_context_is_only_what_came_before() {
    let h = H::start_with(None, 500).await;
    let sid = h.space().await;
    h.sent("bob", &sid, "lobby", json!({"text": "earlier words", "mentions": [{"kind": "user", "id": "alice"}], "infer_text_mentions": false}))
        .await;
    h.classify(&sid).await;
    let posted = h
        .sent(
            "alice",
            &sid,
            "lobby",
            json!({"text": "classify me", "defer_decisions": true}),
        )
        .await;
    // Something written after it, while System One is being asked.
    h.sent("bob", &sid, "lobby", json!({"text": "later words", "mentions": [{"kind": "user", "id": "alice"}], "infer_text_mentions": false}))
        .await;
    let (s, _) = h.decisions("alice", &sid, id(&posted)).await;
    assert_eq!(s, StatusCode::OK);
    let seen = h.backend.seen.lock().unwrap().clone();
    let previous = &seen[0]["previous"];
    let texts: Vec<&str> = previous
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["text"].as_str().unwrap())
        .collect();
    assert_eq!(texts, ["earlier words"], "{seen:?}");
}

#[tokio::test]
async fn planned_runs_survive_a_restart_one_by_one_and_start_once() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("metro.sqlite3");
    let before = H::start_with(Some(&file), 0).await;
    let sid = before.space().await;
    let (s, coder) = before
        .post(
            "alice",
            &format!("/v1/spaces/{sid}/agents"),
            json!({"name": "Coder", "handle": "coder", "description": "Writes code."}),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{coder}");
    let (_, agents) = before
        .req(
            Method::GET,
            "alice",
            &format!("/v1/spaces/{sid}/agents"),
            None,
        )
        .await;
    let ids: Vec<String> = agents["agents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(ids.len(), 2);
    // A message whose decision planned two runs; the process died before
    // starting either (one could equally have started: its row would be gone).
    let msg = before
        .sent(
            "alice",
            &sid,
            "lobby",
            json!({"text": "for both of you", "mentions": [{"kind": "user", "id": "bob"}], "infer_text_mentions": false}),
        )
        .await;
    metro_server::decisions::insert(
        &before.state.db,
        &metro_server::decisions::NewPending {
            scope: metro_server::decisions::Scope::Space,
            user_id: "alice".into(),
            place: Some((sid.clone(), "lobby".into())),
            payload: json!({}),
            claimed_by: "epoch_before".into(),
            turn_id: None,
        },
        id(&msg),
    )
    .unwrap();
    let planned: Vec<_> = ids
        .iter()
        .map(|a| metro_server::decisions::PlannedTarget {
            agent_id: a.clone(),
            pick: Some("Addressed".into()),
            addressed_by: metro_server::model_routing::AddressedBy::Explicit,
        })
        .collect();
    assert!(
        metro_server::decisions::plan_in(
            &before.state.db,
            "epoch_before",
            &sid,
            "lobby",
            "alice",
            id(&msg),
            &planned,
            None,
            &[],
        )
        .unwrap()
    );
    assert_eq!(before.pending(), 2);
    let after = H::start_with(Some(&file), 0).await;
    assert_eq!(
        metro_server::decisions::resume(after.state.clone())
            .await
            .unwrap(),
        1
    );
    // Recovery claimed everything: nothing is left for anyone else.
    assert_eq!(
        metro_server::decisions::resume(after.state.clone())
            .await
            .unwrap(),
        0
    );
    for _ in 0..400 {
        let events = after.events("alice", &sid, "lobby").await;
        if events
            .iter()
            .filter(|e| e["kind"] == "agent" && id(e) > id(&msg))
            .count()
            == 2
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    let events = after.events("alice", &sid, "lobby").await;
    let started: Vec<&Value> = events
        .iter()
        .filter(|e| {
            id(e) > id(&msg) && e["kind"] == "activity" && e["activity"]["phase"] == "started"
        })
        .collect();
    assert_eq!(started.len(), 2, "{events:?}");
    let mut agents_started: Vec<String> = started
        .iter()
        .map(|e| e["activity"]["agent"]["id"].as_str().unwrap().to_owned())
        .collect();
    agents_started.sort();
    let mut want = ids.clone();
    want.sort();
    assert_eq!(agents_started, want);
    assert_eq!(after.pending(), 0);
}

#[tokio::test]
async fn stop_before_start_works_through_the_api_and_across_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("metro.sqlite3");
    let before = H::start_with(Some(&file), 0).await;
    // Alice's line is busy: her turn waits.
    let held = before.state.decisions.chat_ticket("alice");
    let (s, sent) = before
        .post("alice", "/v1/chat/messages", json!({"text": "never mind"}))
        .await;
    assert_eq!(s, StatusCode::ACCEPTED, "{sent}");
    let turn = sent["turn_id"].as_str().unwrap().to_owned();
    assert!(turn.starts_with("chat_"));
    // Only its sender can stop it.
    let (s, _) = before
        .post("bob", &format!("/v1/chat/turns/{turn}/stop"), json!({}))
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, v) = before
        .post("alice", &format!("/v1/chat/turns/{turn}/stop"), json!({}))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    // The process "crashes" with the turn still queued; the next one
    // remembers the Stop.
    let after = H::start_with(Some(&file), 0).await;
    assert_eq!(
        metro_server::decisions::resume(after.state.clone())
            .await
            .unwrap(),
        0
    );
    assert_eq!(after.pending(), 0);
    // And the old process, once its line moves, doesn't start it either.
    drop(held);
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(started_for(&after.chat("alice").await, 0), 0);
    assert!(before.model_calls.lock().unwrap().is_empty());
    assert!(after.model_calls.lock().unwrap().is_empty());
}

// ---- Third review: slots, failure paths, stale work, Space deletion ----

/// Each message's `started` markers after `after`.
fn starts(events: &[Value], after: i64) -> usize {
    started_for(events, after)
}

async fn with_coder(h: &H, sid: &str) {
    let (s, v) = h
        .post(
            "alice",
            &format!("/v1/spaces/{sid}/agents"),
            json!({"name": "Coder", "handle": "coder", "description": "Writes code."}),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
}

async fn all_started(h: &H, sid: &str, after: i64, want: usize) -> usize {
    let mut n = 0;
    for _ in 0..600 {
        n = starts(&h.events("alice", sid, "lobby").await, after);
        if n >= want {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    n
}

#[tokio::test]
async fn more_deferred_messages_than_one_persons_slots_all_start() {
    let h = H::roomy(300).await;
    let sid = h.space().await;
    with_coder(&h, &sid).await;
    h.classify(&sid).await;
    let first = h.events("alice", &sid, "lobby").await.last().map_or(0, id);
    let n = metro_server::decisions::PER_USER + 1;
    for i in 0..n {
        let v = h
            .sent(
                "alice",
                &sid,
                "lobby",
                json!({"text": format!("@agent number {i}"), "defer_decisions": true}),
            )
            .await;
        assert_eq!(v["decisions_pending"], true, "{v}");
    }
    assert_eq!(
        all_started(&h, &sid, first, n).await,
        n,
        "a deferred decision deadlocked"
    );
    assert_eq!(h.classifier_calls(), n);
}

#[tokio::test]
async fn more_deferred_messages_than_the_servers_slots_all_start() {
    let h = H::roomy(300).await;
    let sid = h.space().await;
    with_coder(&h, &sid).await;
    let n = metro_server::decisions::AT_ONCE + 1;
    let people: Vec<String> = (0..n).map(|i| format!("user{i}")).collect();
    for p in &people {
        let (s, v) = h
            .post(
                "alice",
                &format!("/v1/spaces/{sid}/members"),
                json!({"user_id": p}),
            )
            .await;
        assert_eq!(s, StatusCode::CREATED, "{v}");
        h.classify_as(p, &sid).await;
    }
    let first = h.events("alice", &sid, "lobby").await.last().map_or(0, id);
    for p in &people {
        h.sent(
            p,
            &sid,
            "lobby",
            json!({"text": "@agent hello", "defer_decisions": true}),
        )
        .await;
    }
    assert_eq!(
        all_started(&h, &sid, first, n).await,
        n,
        "a deferred decision deadlocked"
    );
}

fn chat_pending(user: &str) -> metro_server::decisions::NewPending {
    metro_server::decisions::NewPending {
        scope: metro_server::decisions::Scope::Personal,
        user_id: user.into(),
        place: None,
        payload: json!({"choice": null}),
        claimed_by: "epoch_before".into(),
        turn_id: Some("chat_0123".into()),
    }
}

fn space_pending(sid: &str, payload: Value) -> metro_server::decisions::NewPending {
    metro_server::decisions::NewPending {
        scope: metro_server::decisions::Scope::Space,
        user_id: "alice".into(),
        place: Some((sid.into(), "lobby".into())),
        payload,
        claimed_by: "epoch_before".into(),
        turn_id: None,
    }
}

fn chat_message(h: &H, user: &str, text: &str) -> i64 {
    let author = metro_server::db::Author {
        id: Some(user.into()),
        name: user.into(),
    };
    let addressed = metro_server::db::Addressed {
        mentions: json!([{"kind": "agent"}]),
        audience: "agent".into(),
        spans: None,
    };
    h.state
        .db
        .append_personal(user, "message", &author, text, &addressed)
        .unwrap()
        .id
}

#[tokio::test]
async fn stale_work_is_noted_at_a_restart_never_run() {
    let h = H::start().await;
    let sid = h.space().await;
    let msg = h
        .sent("alice", &sid, "lobby", json!({"text": "from yesterday", "mentions": [{"kind": "user", "id": "bob"}], "infer_text_mentions": false}))
        .await;
    let chat = chat_message(&h, "bob", "chat from yesterday");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let old = now - 2 * 3_600_000;
    let db = &h.state.db;
    metro_server::decisions::insert_at(
        db,
        &space_pending(&sid, json!({"resolved": [{"kind": "agent"}]})),
        id(&msg),
        old,
    )
    .unwrap();
    metro_server::decisions::insert_at(db, &chat_pending("bob"), chat, old).unwrap();
    assert_eq!(
        metro_server::decisions::resume(h.state.clone())
            .await
            .unwrap(),
        0
    );
    assert_eq!(h.pending(), 0);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let room = h.events("alice", &sid, "lobby").await;
    assert!(
        room.iter().any(|e| id(e) > id(&msg)
            && e["kind"] == "error"
            && e["text"] == metro_server::decisions::STALE_NOTE),
        "{room:?}"
    );
    assert_eq!(starts(&room, id(&msg)), 0);
    let bobs = h.chat("bob").await;
    assert!(
        bobs.iter().any(|e| id(e) > chat
            && e["kind"] == "error"
            && e["text"] == metro_server::decisions::STALE_NOTE),
        "{bobs:?}"
    );
    assert_eq!(starts(&bobs, chat), 0);
    assert!(h.model_calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn failed_pending_work_is_dropped_and_said_never_left_to_replay() {
    let h = H::start().await;
    let sid = h.space().await;
    let msg = h
        .sent("alice", &sid, "lobby", json!({"text": "will fail", "mentions": [{"kind": "user", "id": "bob"}], "infer_text_mentions": false}))
        .await;
    let chat = chat_message(&h, "bob", "will fail too");
    metro_server::decisions::insert(&h.state.db, &space_pending(&sid, json!({})), id(&msg))
        .unwrap();
    metro_server::decisions::insert(&h.state.db, &chat_pending("bob"), chat).unwrap();
    // An error path (or a panic) drops its watch armed.
    drop(metro_server::decisions::Watch::space(
        &h.state,
        &sid,
        "lobby",
        id(&msg),
    ));
    drop(metro_server::decisions::Watch::chat(
        &h.state,
        "bob",
        chat,
        Some("chat_0123"),
    ));
    // A settled one says nothing.
    metro_server::decisions::Watch::space(&h.state, &sid, "lobby", id(&msg)).done();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(h.pending(), 0, "nothing left for a later restart");
    let room = h.events("alice", &sid, "lobby").await;
    let notes: Vec<&Value> = room.iter().filter(|e| e["kind"] == "error").collect();
    assert_eq!(notes.len(), 1, "{room:?}");
    assert_eq!(notes[0]["text"], metro_server::decisions::FAILED_SPACE_NOTE);
    let bobs = h.chat("bob").await;
    assert!(bobs.iter().any(|e| e["kind"] == "error"
        && e["text"] == metro_server::decisions::FAILED_CHAT_NOTE), "{bobs:?}");
}

#[tokio::test]
async fn a_resumed_choice_that_cant_be_used_says_only_that_metro_restarted() {
    let h = H::start().await;
    let sid = h.space().await;
    let msg = h
        .sent("alice", &sid, "lobby", json!({"text": "@agent with my model", "mentions": [{"kind": "user", "id": "bob"}], "infer_text_mentions": false}))
        .await;
    let gone = "pc_99999999999999999999999999999999";
    metro_server::decisions::insert(
        &h.state.db,
        &space_pending(
            &sid,
            json!({"resolved": [{"kind": "agent"}], "choice": {"provider_connection_id": gone}}),
        ),
        id(&msg),
    )
    .unwrap();
    assert_eq!(
        metro_server::decisions::resume(h.state.clone())
            .await
            .unwrap(),
        1
    );
    let mut note = Value::Null;
    for _ in 0..200 {
        if let Some(e) = h
            .events("bob", &sid, "lobby")
            .await
            .into_iter()
            .find(|e| e["kind"] == "error")
        {
            note = e;
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(
        note["text"],
        metro_server::decisions::RESTART_NOTE,
        "{note}"
    );
    assert!(!note.to_string().contains("pc_"));
    assert_eq!(h.pending(), 0);
    assert!(h.model_calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn deleting_a_space_drops_its_pending_work() {
    let h = H::start_with(None, 60_000).await;
    let sid = h.space().await;
    h.classify(&sid).await;
    h.sent(
        "alice",
        &sid,
        "lobby",
        json!({"text": "pending forever", "defer_decisions": true}),
    )
    .await;
    assert_eq!(h.pending(), 1);
    let (s, v) = h
        .req(
            Method::DELETE,
            "alice",
            &format!("/v1/spaces/{sid}"),
            Some(json!({"confirm_name": "S"})),
        )
        .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "{v}");
    assert_eq!(h.pending(), 0);
}

// ---- Final review: quiet drops, shutdown, tagged Chat errors, failed starts ----

#[tokio::test]
async fn dropping_an_unaddressed_message_says_nothing_in_the_room() {
    let h = H::start().await;
    let sid = h.space().await;
    let quiet = h
        .sent("alice", &sid, "lobby", json!({"text": "just chatting", "mentions": [{"kind": "user", "id": "bob"}], "infer_text_mentions": false}))
        .await;
    let loud = h
        .sent("alice", &sid, "lobby", json!({"text": "for the agent", "mentions": [{"kind": "user", "id": "bob"}], "infer_text_mentions": false}))
        .await;
    let db = &h.state.db;
    metro_server::decisions::insert(
        db,
        &space_pending(&sid, json!({"resolved": []})),
        id(&quiet),
    )
    .unwrap();
    metro_server::decisions::insert(
        db,
        &space_pending(&sid, json!({"resolved": [{"kind": "agent"}]})),
        id(&loud),
    )
    .unwrap();
    // Failed: an unaddressed message's work goes without a room note (that
    // would reveal its sender's opt-in); addressed work says so.
    drop(metro_server::decisions::Watch::space(&h.state, &sid, "lobby", id(&quiet)).quiet(true));
    drop(metro_server::decisions::Watch::space(
        &h.state,
        &sid,
        "lobby",
        id(&loud),
    ));
    tokio::time::sleep(Duration::from_millis(300)).await;
    let errors: Vec<Value> = h
        .events("bob", &sid, "lobby")
        .await
        .into_iter()
        .filter(|e| e["kind"] == "error")
        .collect();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(h.pending(), 0);
    // Stale at a restart: the same rule.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let old = now - 2 * 3_600_000;
    metro_server::decisions::insert_at(
        db,
        &space_pending(&sid, json!({"resolved": [], "reply_agent": null})),
        id(&quiet),
        old,
    )
    .unwrap();
    assert_eq!(
        metro_server::decisions::resume(h.state.clone())
            .await
            .unwrap(),
        0
    );
    let after: Vec<Value> = h
        .events("bob", &sid, "lobby")
        .await
        .into_iter()
        .filter(|e| e["kind"] == "error")
        .collect();
    assert_eq!(
        after.len(),
        1,
        "a stale unaddressed message got a room note: {after:?}"
    );
    assert_eq!(h.pending(), 0);
}

#[tokio::test]
async fn during_shutdown_a_guard_leaves_work_for_the_next_start() {
    let h = H::start().await;
    let sid = h.space().await;
    let msg = h
        .sent("alice", &sid, "lobby", json!({"text": "@agent later", "mentions": [{"kind": "user", "id": "bob"}], "infer_text_mentions": false}))
        .await;
    metro_server::decisions::insert(
        &h.state.db,
        &space_pending(&sid, json!({"resolved": [{"kind": "agent"}]})),
        id(&msg),
    )
    .unwrap();
    h.state.decisions.begin_shutdown();
    drop(metro_server::decisions::Watch::space(
        &h.state,
        &sid,
        "lobby",
        id(&msg),
    ));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(h.pending(), 1, "shutdown dropped pending work");
    assert!(
        !h.events("bob", &sid, "lobby")
            .await
            .iter()
            .any(|e| e["kind"] == "error")
    );
}

#[test]
fn work_pending_at_shutdown_survives_and_resumes() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("metro.sqlite3");
    // The first server runs on its own runtime, with System One too slow to
    // answer before it shuts down.
    let (sid, msg) = {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (state, sid, msg) = rt.block_on(async {
            let h = H::start_with(Some(&file), 60_000).await;
            let sid = h.space().await;
            with_coder(&h, &sid).await;
            h.classify(&sid).await;
            let msg = h
                .sent(
                    "alice",
                    &sid,
                    "lobby",
                    json!({"text": "@agent after the restart", "defer_decisions": true}),
                )
                .await;
            // Its decision is waiting on System One.
            for _ in 0..100 {
                if h.classifier_calls() == 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            assert_eq!(h.classifier_calls(), 1);
            (h.state.clone(), sid, id(&msg))
        });
        state.decisions.begin_shutdown();
        // The runtime goes, and every task with it.
        rt.shutdown_timeout(Duration::from_secs(2));
        (sid, msg)
    };
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let after = H::start_with(Some(&file), 0).await;
        assert_eq!(
            after.pending(),
            1,
            "the pending decision survived the shutdown"
        );
        assert!(
            !after
                .events("alice", &sid, "lobby")
                .await
                .iter()
                .any(|e| e["kind"] == "error")
        );
        assert_eq!(
            metro_server::decisions::resume(after.state.clone())
                .await
                .unwrap(),
            1
        );
        assert_eq!(all_started(&after, &sid, msg, 1).await, 1);
        assert_eq!(after.pending(), 0);
    });
}

#[tokio::test]
async fn a_dropped_chat_turn_names_its_turn_and_a_failed_start_is_said() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("metro.sqlite3");
    let h = H::start_with(Some(&file), 0).await;
    // The `started` marker can't be written: the start fails after the
    // turn was handed to its worker.
    rusqlite::Connection::open(&file)
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER refuse_start BEFORE INSERT ON personal_events WHEN new.kind = 'activity'
             BEGIN SELECT RAISE(ABORT, 'no starts'); END;",
        )
        .unwrap();
    let (s, sent) = h
        .post(
            "alice",
            "/v1/chat/messages",
            json!({"text": "will not start"}),
        )
        .await;
    assert_eq!(s, StatusCode::ACCEPTED, "{sent}");
    let turn = sent["turn_id"].as_str().unwrap().to_owned();
    let mut note = Value::Null;
    for _ in 0..200 {
        if let Some(e) = h
            .chat("alice")
            .await
            .into_iter()
            .find(|e| e["kind"] == "error")
        {
            note = e;
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(
        note["text"],
        metro_server::decisions::FAILED_CHAT_NOTE,
        "{note}"
    );
    assert_eq!(note["turn_id"], turn.as_str(), "the note names its turn");
    assert_eq!(h.pending(), 0);
    assert!(h.model_calls.lock().unwrap().is_empty());
}
