//! Reply threads, replies and auto-replies to agents, over HTTP against a
//! scripted OpenAI-compatible model and a scripted System One.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Json;
use axum::routing::post;
use metro_model_protocol::Capability;
use metro_server::auth::{AuthFailure, AuthFuture, Human, HumanAuth, UserIdentity};
use metro_server::db::{Db, Via};
use metro_server::mentions::Mention;
use metro_server::messages::{Outgoing, post_room_message};
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

/// A System One that alice may use and that would pick the first agent
/// for anything, so any call it gets is visible.
#[derive(Default)]
struct Backend {
    calls: Mutex<usize>,
}

impl RoutingBackend for Backend {
    fn system_one<'a>(
        &'a self,
        _state: &'a AppState,
        user: &'a str,
        _space: Option<&'a str>,
        id: &'a str,
        _state_payload: Value,
        questions: Value,
    ) -> BoxFuture<'a, anyhow::Result<Value>> {
        Box::pin(async move {
            *self.calls.lock().unwrap() += 1;
            if user != "alice" || id != CLASSIFIER {
                return Err(ProviderError::Use(UseError::NotFound).into());
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

/// An OpenAI-compatible endpoint that always answers "ok"; counts calls.
async fn model(calls: Arc<Mutex<usize>>) -> String {
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        post(move |Json(body): Json<Value>| {
            let calls = calls.clone();
            async move {
                *calls.lock().unwrap() += 1;
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
    state: AppState,
    backend: Arc<Backend>,
    model_calls: Arc<Mutex<usize>>,
}

impl H {
    async fn start() -> Self {
        Self::start_with(Limits::default()).await
    }

    async fn start_with(limits: Limits) -> Self {
        let model_calls = Arc::new(Mutex::new(0));
        let config = Config {
            bind: "127.0.0.1:0".parse().unwrap(),
            db_path: ":memory:".into(),
            model: Some(ModelConfig {
                base_url: model(model_calls.clone()).await,
                model: "env-model".into(),
                api_key: String::new(),
            }),
            limits: Limits {
                agent_timeout: Duration::from_secs(20),
                ..limits
            },
            org_admins: Vec::new(),
        };
        let db = Db::open_in_memory().unwrap();
        let mut state = AppState::new(config, db, Arc::new(FakeAuth)).await.unwrap();
        let backend = Arc::new(Backend::default());
        state.routing = Routing::new(backend.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let served = state.clone();
        tokio::spawn(async move { axum::serve(listener, router(served)).await.unwrap() });
        Self {
            base,
            http: reqwest::Client::new(),
            state,
            backend,
            model_calls,
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

    async fn say(&self, who: &str, sid: &str, rid: &str, body: Value) -> (StatusCode, Value) {
        self.post(who, &format!("/v1/spaces/{sid}/rooms/{rid}/messages"), body)
            .await
    }

    async fn sent(&self, who: &str, sid: &str, rid: &str, body: Value) -> Value {
        let (s, v) = self.say(who, sid, rid, body).await;
        assert_eq!(s, StatusCode::ACCEPTED, "{v}");
        v
    }

    async fn thread(&self, who: &str, sid: &str, rid: &str, eid: i64) -> (StatusCode, Value) {
        self.post(
            who,
            &format!("/v1/spaces/{sid}/rooms/{rid}/events/{eid}/thread"),
            json!({}),
        )
        .await
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

    /// The agent's reply after `after` in room `rid`.
    async fn reply_after(&self, sid: &str, rid: &str, after: i64) -> Value {
        self.wait_for(sid, rid, after, |e| e["kind"] == "agent")
            .await
    }

    /// The `started` activity after `after` in room `rid`.
    async fn started_after(&self, sid: &str, rid: &str, after: i64) -> Value {
        self.wait_for(sid, rid, after, |e| {
            e["kind"] == "activity" && e["activity"]["phase"] == "started"
        })
        .await["activity"]
            .clone()
    }

    /// No event at all after `after` in room `rid` for a moment.
    async fn quiet_after(&self, sid: &str, rid: &str, after: i64) {
        tokio::time::sleep(Duration::from_millis(300)).await;
        let later: Vec<Value> = self
            .events("alice", sid, rid)
            .await
            .into_iter()
            .filter(|e| e["id"].as_i64().unwrap() > after)
            .collect();
        assert!(later.is_empty(), "unexpected events: {later:?}");
    }

    fn classifier_calls(&self) -> usize {
        *self.backend.calls.lock().unwrap()
    }

    /// Alice asks agent `handle` in the lobby; its reply's id.
    async fn agent_reply(&self, sid: &str, handle: &str) -> i64 {
        let e = self
            .sent(
                "alice",
                sid,
                "lobby",
                json!({"text": format!("@{handle} hello")}),
            )
            .await;
        self.reply_after(sid, "lobby", e["id"].as_i64().unwrap())
            .await["id"]
            .as_i64()
            .unwrap()
    }

    /// The reply thread of `eid` in the lobby, created by alice; its id.
    async fn thread_of(&self, sid: &str, eid: i64) -> String {
        let (s, v) = self.thread("alice", sid, "lobby", eid).await;
        assert!(s == StatusCode::CREATED || s == StatusCode::OK, "{v}");
        v["id"].as_str().unwrap().to_owned()
    }

    /// Turn on alice's classification in `sid`.
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
}

fn id(v: &Value) -> i64 {
    v["id"].as_i64().unwrap()
}

#[tokio::test]
async fn reply_threads_are_get_or_create_for_members_only() {
    let h = H::start().await;
    let sid = h.space().await;
    let m = h
        .sent("bob", &sid, "lobby", json!({"text": "hi all"}))
        .await;
    let (s, created) = h.thread("bob", &sid, "lobby", id(&m)).await;
    assert_eq!(s, StatusCode::CREATED, "{created}");
    assert_eq!(created["parent_room_id"], "lobby");
    assert_eq!(created["anchor_event_id"], id(&m));
    assert_eq!(created["name"], format!("reply-{}", id(&m)));
    // Idempotent, whoever asks.
    let (s, again) = h.thread("alice", &sid, "lobby", id(&m)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(again, created);
    // Listed with its anchor; top-level rooms carry none.
    let (_, rooms) = h
        .req(
            Method::GET,
            "alice",
            &format!("/v1/spaces/{sid}/rooms"),
            None,
        )
        .await;
    let rooms = rooms["rooms"].as_array().unwrap();
    assert!(rooms[0].get("anchor_event_id").is_none());
    assert!(rooms.contains(&created));

    // Outsiders and unknown ids alike: 404.
    let tid = created["id"].as_str().unwrap();
    for (who, rid, eid) in [
        ("carol", "lobby", id(&m)),
        ("alice", "lobby", 999_999),
        ("alice", "r-nope", id(&m)),
    ] {
        let (s, v) = h.thread(who, &sid, rid, eid).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{who} {rid} {eid}: {v}");
    }
    let (s, _) = h.thread("alice", "s-nope", "lobby", id(&m)).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    // No threads in threads.
    let inner = h.sent("bob", &sid, tid, json!({"text": "inside"})).await;
    let (s, v) = h.thread("alice", &sid, tid, id(&inner)).await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    // An event of the thread is not in the lobby.
    let (s, _) = h.thread("alice", &sid, "lobby", id(&inner)).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    // Only messages, agent replies and thread events take threads.
    let r = h.agent_reply(&sid, "agent").await;
    let started = h
        .wait_for(&sid, "lobby", 0, |e| e["kind"] == "activity")
        .await;
    let (s, _) = h.thread("alice", &sid, "lobby", id(&started)).await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, _) = h.thread("alice", &sid, "lobby", r).await;
    assert_eq!(s, StatusCode::CREATED);
    // Names of that shape are taken for rooms.
    let (s, v) = h
        .post(
            "alice",
            &format!("/v1/spaces/{sid}/rooms"),
            json!({"name": "Reply-12"}),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");

    // Losing membership loses the thread.
    let (s, _) = h
        .req(
            Method::DELETE,
            "alice",
            &format!("/v1/spaces/{sid}/members"),
            Some(json!({"user_id": "bob"})),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = h.thread("bob", &sid, "lobby", id(&m)).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = h
        .say("bob", &sid, tid, json!({"text": "still here?"}))
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = h
        .req(
            Method::GET,
            "bob",
            &format!("/v1/spaces/{sid}/rooms/{tid}/events"),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn reply_threads_have_their_own_limit_and_go_with_the_space() {
    let h = H::start_with(Limits {
        max_rooms: 2,
        max_threads_per_room: 1,
        max_reply_threads: 3,
        ..Limits::default()
    })
    .await;
    let sid = h.space().await;
    let (s, _) = h
        .post(
            "alice",
            &format!("/v1/spaces/{sid}/rooms"),
            json!({"name": "dev"}),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED);
    // The Space is full of rooms, yet takes reply threads up to their own limit.
    let mut threads = Vec::new();
    for n in 0..4 {
        let m = h
            .sent("alice", &sid, "lobby", json!({"text": format!("m{n}")}))
            .await;
        let (s, v) = h.thread("alice", &sid, "lobby", id(&m)).await;
        if n < 3 {
            assert_eq!(s, StatusCode::CREATED, "{v}");
            threads.push(v);
        } else {
            assert_eq!(s, StatusCode::CONFLICT, "{v}");
            assert!(v["error"].as_str().unwrap().contains("reply threads"));
        }
    }
    // And they don't use up the room limit either.
    let (s, v) = h
        .post(
            "alice",
            &format!("/v1/spaces/{sid}/rooms"),
            json!({"name": "ops"}),
        )
        .await;
    assert_eq!(s, StatusCode::CONFLICT, "only the two rooms count: {v}");

    let (s, v) = h
        .req(
            Method::DELETE,
            "alice",
            &format!("/v1/spaces/{sid}"),
            Some(json!({"confirm_name": "S"})),
        )
        .await;
    assert!(s.is_success(), "{s} {v}");
    let left = h
        .state
        .db
        .call(move |db| db.list_rooms(&sid))
        .await
        .unwrap();
    assert!(left.is_empty(), "{left:?}");
}

#[tokio::test]
async fn replies_stay_in_their_thread_and_are_counted_on_the_anchor() {
    let h = H::start().await;
    let sid = h.space().await;
    let a = h
        .sent("alice", &sid, "lobby", json!({"text": "anchor"}))
        .await;
    let other = h
        .sent("alice", &sid, "lobby", json!({"text": "other"}))
        .await;
    let tid = h.thread_of(&sid, id(&a)).await;
    let other_tid = h.thread_of(&sid, id(&other)).await;

    // Top-level rooms take no replies.
    let (s, v) = h
        .say(
            "bob",
            &sid,
            "lobby",
            json!({"text": "re", "reply_to": id(&a)}),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    assert_eq!(v["error"], "reply in a thread");
    // The anchor, or an event of the same thread, only.
    let first = h
        .sent(
            "bob",
            &sid,
            &tid,
            json!({"text": "re anchor", "reply_to": id(&a)}),
        )
        .await;
    assert_eq!(first["reply_to"], id(&a));
    let elsewhere = h.sent("bob", &sid, &other_tid, json!({"text": "x"})).await;
    for bad in [id(&other), id(&elsewhere), 999_999] {
        let (s, v) = h
            .say("bob", &sid, &tid, json!({"text": "re", "reply_to": bad}))
            .await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{bad}: {v}");
    }
    let second = h
        .sent(
            "alice",
            &sid,
            &tid,
            json!({"text": "re bob", "reply_to": id(&first)}),
        )
        .await;
    // Legacy clients: no reply, no keys.
    let plain = h.sent("alice", &sid, &tid, json!({"text": "plain"})).await;
    for key in ["reply_to", "via", "forwarded", "replies"] {
        assert!(plain.get(key).is_none(), "{key}: {plain}");
    }
    // Unknown fields are still refused.
    let (s, _) = h
        .say("alice", &sid, &tid, json!({"text": "x", "via": "metro"}))
        .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);

    // Stored: the thread's page shows the ids, the anchor its summary.
    let page = h.events("bob", &sid, &tid).await;
    let got: Vec<(Value, Value)> = page
        .iter()
        .map(|e| (e["text"].clone(), e["reply_to"].clone()))
        .collect();
    assert_eq!(
        got,
        [
            (json!("re anchor"), json!(id(&a))),
            (json!("re bob"), json!(id(&first))),
            (json!("plain"), Value::Null),
        ]
    );
    let lobby = h.events("bob", &sid, "lobby").await;
    let anchor = lobby.iter().find(|e| e["id"] == a["id"]).unwrap();
    assert_eq!(anchor["replies"]["room_id"], tid.as_str());
    assert_eq!(anchor["replies"]["count"], 3);
    assert_eq!(
        anchor["replies"]["participant_ids"],
        json!(["alice", "bob"])
    );
    assert_eq!(anchor["replies"]["last_at"], plain["created_at"]);
    let other = lobby.iter().find(|e| e["id"] == other["id"]).unwrap();
    assert_eq!(other["replies"]["count"], 1);
    assert_eq!(other["replies"]["participant_ids"], json!(["bob"]));
    // Events without a thread have no summary.
    assert!(lobby.iter().filter(|e| e.get("replies").is_some()).count() == 2);
    let _ = second;
}

#[tokio::test]
async fn a_reply_to_an_agent_asks_it_without_classification() {
    let h = H::start().await;
    let sid = h.space().await;
    let r = h.agent_reply(&sid, "agent").await;
    let tid = h.thread_of(&sid, r).await;
    for classify in [false, true] {
        if classify {
            h.classify(&sid).await;
        }
        let calls = h.classifier_calls();
        let e = h
            .sent("alice", &sid, &tid, json!({"text": "why?", "reply_to": r}))
            .await;
        assert_eq!(e["audience"], "agent");
        let agent = h.state.db.space_agents(&sid).unwrap().remove(0);
        assert_eq!(
            e["mentions"],
            json!([{"kind": "agent", "agent_id": agent.id, "addressed_by": "reply"}])
        );
        let started = h.started_after(&sid, &tid, id(&e)).await;
        assert_eq!(started["pick"], "Reply to @agent");
        assert_eq!(started["agent"]["id"], agent.id.as_str());
        h.reply_after(&sid, &tid, id(&e)).await;
        assert_eq!(h.classifier_calls(), calls, "classify={classify}");
    }
}

#[tokio::test]
async fn opting_out_or_replying_to_people_follows_the_normal_rules() {
    let h = H::start().await;
    let sid = h.space().await;
    let r = h.agent_reply(&sid, "agent").await;
    let tid = h.thread_of(&sid, r).await;
    let m = h.sent("bob", &sid, &tid, json!({"text": "me too"})).await;
    let calls = *h.model_calls.lock().unwrap();
    for body in [
        json!({"text": "no thanks", "reply_to": r, "reply_to_agent": false}),
        json!({"text": "to bob", "reply_to": id(&m)}),
    ] {
        let e = h.sent("alice", &sid, &tid, body.clone()).await;
        assert_eq!(e["mentions"], json!([]), "{body}");
        assert_eq!(e["reply_to"], body["reply_to"]);
        h.quiet_after(&sid, &tid, id(&e)).await;
    }
    assert_eq!(*h.model_calls.lock().unwrap(), calls);
    // With classification on, an opted-out reply is unaddressed text.
    h.classify(&sid).await;
    let before = h.classifier_calls();
    let e = h
        .sent(
            "alice",
            &sid,
            &tid,
            json!({"text": "hm", "reply_to": r, "reply_to_agent": false}),
        )
        .await;
    let started = h.started_after(&sid, &tid, id(&e)).await;
    assert!(
        started["pick"]
            .as_str()
            .unwrap()
            .starts_with("Picked by classifier")
    );
    assert_eq!(h.classifier_calls(), before + 1);
}

#[tokio::test]
async fn mentions_combine_with_the_replied_agent() {
    let h = H::start().await;
    let sid = h.space().await;
    let (s, v) = h
        .post(
            "alice",
            &format!("/v1/spaces/{sid}/agents"),
            json!({"name": "Coder", "handle": "coder", "description": "Writes code."}),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let coder = v["id"].as_str().unwrap().to_owned();
    let r = h.agent_reply(&sid, "coder").await;
    let tid = h.thread_of(&sid, r).await;

    // @bob plus a reply: mixed, and the agent runs.
    let e = h
        .sent(
            "alice",
            &sid,
            &tid,
            json!({"text": "@bob look", "reply_to": r, "infer_text_mentions": false,
                   "mentions": [{"kind": "user", "id": "bob"}],
                   "spans": [{"start": 0, "end": 4, "index": 0}]}),
        )
        .await;
    assert_eq!(e["audience"], "mixed");
    assert_eq!(
        e["mentions"],
        json!([{"kind": "user", "id": "bob"},
               {"kind": "agent", "agent_id": coder, "addressed_by": "reply"}])
    );
    assert_eq!(e["spans"], json!([{"start": 0, "end": 4, "index": 0}]));
    let started = h.started_after(&sid, &tid, id(&e)).await;
    assert_eq!(started["pick"], "Reply to @coder");
    h.reply_after(&sid, &tid, id(&e)).await;

    // Naming the replied agent too: one run, stored once, as named.
    let e = h
        .sent(
            "alice",
            &sid,
            &tid,
            json!({"text": "@coder again", "reply_to": r}),
        )
        .await;
    assert_eq!(e["mentions"], json!([{"kind": "agent", "agent_id": coder}]));
    let reply = h.reply_after(&sid, &tid, id(&e)).await;
    let starts = h
        .events("alice", &sid, &tid)
        .await
        .into_iter()
        .filter(|x| {
            x["id"].as_i64().unwrap() > id(&e)
                && x["kind"] == "activity"
                && x["activity"]["phase"] == "started"
        })
        .collect::<Vec<_>>();
    assert_eq!(starts.len(), 1, "{starts:?}");
    assert_eq!(starts[0]["activity"]["pick"], "Addressed: @coder");
    let _ = reply;

    // The replied agent was deleted: nothing runs, and only the sender is told.
    let (s, v) = h
        .req(
            Method::DELETE,
            "alice",
            &format!("/v1/spaces/{sid}/agents/{coder}"),
            None,
        )
        .await;
    assert!(s.is_success(), "{s} {v}");
    let e = h
        .sent(
            "bob",
            &sid,
            &tid,
            json!({"text": "still there?", "reply_to": r}),
        )
        .await;
    assert_eq!(
        e["addressing_note"]["kind"], "reply_agent_unavailable",
        "{e}"
    );
    assert_eq!(e["mentions"], json!([]));
    h.quiet_after(&sid, &tid, id(&e)).await;
    assert!(
        h.events("alice", &sid, &tid)
            .await
            .iter()
            .all(|e| e.get("addressing_note").is_none())
    );
}

#[tokio::test]
async fn older_agent_replies_without_an_agent_ask_the_default_agent() {
    let h = H::start().await;
    let sid = h.space().await;
    let (sid2, old) = (sid.clone(), h.state.db.clone());
    let r = old
        .call(move |db| {
            db.append_event(
                &sid2,
                "lobby",
                "agent",
                &metro_server::db::Author::server("agent"),
                "an old answer",
                &metro_server::db::Addressed::room(),
            )
        })
        .await
        .unwrap();
    let tid = h.thread_of(&sid, r.id).await;
    let e = h
        .sent(
            "alice",
            &sid,
            &tid,
            json!({"text": "and?", "reply_to": r.id}),
        )
        .await;
    let started = h.started_after(&sid, &tid, id(&e)).await;
    assert_eq!(started["pick"], "Reply to @agent");
}

#[tokio::test]
async fn metro_posts_through_the_same_path_and_say_so() {
    let h = H::start().await;
    let sid = h.space().await;
    let agent = h.state.db.space_agents(&sid).unwrap().remove(0);
    let alice = Human {
        id: "alice".into(),
        name: "Alice".into(),
    };
    let posted = post_room_message(
        &h.state,
        &alice,
        &sid,
        "lobby",
        Outgoing {
            mentions: vec![Mention::AgentId {
                agent_id: agent.id.clone(),
            }],
            infer_text_mentions: false,
            via: Some(Via::Metro),
            ..Outgoing::new("@bob please summarise")
        },
    )
    .await
    .unwrap();
    assert_eq!(posted.runs.len(), 1);
    let stored = h
        .events("bob", &sid, "lobby")
        .await
        .into_iter()
        .find(|e| e["id"] == posted.event.id)
        .unwrap();
    assert_eq!(stored["via"], "metro");
    assert_eq!(stored["actor_id"], "alice");
    // `@bob` in text addresses nobody.
    assert_eq!(
        stored["mentions"],
        json!([{"kind": "agent", "agent_id": agent.id, "addressed_by": "metro"}])
    );
    assert_eq!(stored["spans"], json!([]));
    let started = h.started_after(&sid, "lobby", posted.event.id).await;
    assert_eq!(started["id"], posted.runs[0].id.as_str());

    // The same refusals: an outsider and a lost member get 404.
    let carol = Human {
        id: "carol".into(),
        name: "Carol".into(),
    };
    let err = post_room_message(&h.state, &carol, &sid, "lobby", Outgoing::new("hi"))
        .await
        .unwrap_err();
    assert_eq!(err.status(), StatusCode::NOT_FOUND);
}
