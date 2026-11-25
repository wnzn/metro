//! Device relay over HTTP: token issue, rotation and revocation by the
//! connection's owner, the claim/result wire with an inert mock device, and
//! the personal-only boundary. No vendor sign-in, CLI or network.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use metro_server::auth::{AuthFailure, AuthFuture, HumanAuth, UserIdentity};
use metro_server::db::Db;
use metro_server::device_relay::Operation;
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

fn auth() -> Arc<FakeAuth> {
    let user = |id: &str| UserIdentity {
        id: id.to_owned(),
        actor: id.to_owned(),
    };
    Arc::new(FakeAuth(HashMap::from([
        (ALICE.to_owned(), user("alice")),
        (BOB.to_owned(), user("bob")),
    ])))
}

struct Server {
    base: String,
    http: reqwest::Client,
    state: AppState,
}

impl Server {
    async fn start() -> Self {
        let config = Config {
            bind: "127.0.0.1:0".parse().unwrap(),
            db_path: ":memory:".into(),
            model: None,
            limits: Limits::default(),
            org_admins: Vec::new(),
        };
        let state = AppState::new(config, Db::open_in_memory().unwrap(), auth())
            .await
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = router(state.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            base,
            http: reqwest::Client::new(),
            state,
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
        let res = req.send().await.unwrap();
        let status = res.status();
        let text = res.text().await.unwrap();
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    async fn post(&self, token: &str, path: &str, body: Value) -> (StatusCode, Value) {
        self.send(reqwest::Method::POST, token, path, Some(body))
            .await
    }

    async fn connection(&self, token: &str, provider: &str) -> String {
        let (status, body) = self
            .post(
                token,
                "/v1/providers",
                json!({"name": "laptop", "provider_id": provider}),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        body["id"].as_str().unwrap().to_owned()
    }

    async fn device_token(&self, token: &str, id: &str) -> String {
        let (status, body) = self
            .post(
                token,
                &format!("/v1/providers/{id}/device-token"),
                json!({}),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        body["token"].as_str().unwrap().to_owned()
    }

    async fn claim(&self, device: &str) -> (StatusCode, Value) {
        self.post(device, "/v1/device/claim", json!({})).await
    }

    async fn claim_job(&self, device: &str) -> Value {
        self.claim_with_job(device, &json!({})).await
    }

    async fn claim_with_job(&self, device: &str, poll: &Value) -> Value {
        for _ in 0..200 {
            let (status, body) = self.post(device, "/v1/device/claim", poll.clone()).await;
            if status == StatusCode::OK {
                return body;
            }
            assert_eq!(status, StatusCode::NO_CONTENT);
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("no job arrived");
    }

    async fn result(&self, device: &str, id: &str, body: Value) -> (StatusCode, Value) {
        self.post(device, &format!("/v1/device/jobs/{id}/result"), body)
            .await
    }
}

#[tokio::test]
async fn composer_inventory_is_current_generation_owner_scoped_and_pinned_for_presets() {
    let s = Server::start().await;
    let (status, connection) = s
        .post(
            ALICE,
            "/v1/providers",
            json!({
                "name": "ChatGPT", "provider_id": "device-openai-oauth", "model": "gpt-5.5"
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{connection}");
    let id = connection["id"].as_str().unwrap();
    let device = s.device_token(ALICE, id).await;
    let available = |token, path| s.send(reqwest::Method::GET, token, path, None);
    let path = "/v1/models/available";
    assert_eq!(
        available(ALICE, path).await.1["models"][0]["chat_models"],
        json!([])
    );
    assert_eq!(s.claim(&device).await.0, StatusCode::NO_CONTENT);
    assert_eq!(
        available(ALICE, path).await.1["models"][0]["chat_models"],
        json!([{"id": "gpt-5.5", "display_name": "gpt-5.5", "reasoning_efforts": [], "is_default": true, "available": true}])
    );

    let poll = json!({"approved_models": ["gpt-6.2-sol", "gpt-5.5", "gpt-4.1"]});
    let bad = json!({"approved_models": [" padded"]});
    assert_eq!(
        s.post(&device, "/v1/device/claim", bad).await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        s.post(&device, "/v1/device/claim", poll.clone()).await.0,
        StatusCode::NO_CONTENT
    );
    let models = available(ALICE, path).await.1;
    assert_eq!(
        models["models"][0]["chat_models"],
        json!([
            {"id": "gpt-4.1", "display_name": "gpt-4.1", "reasoning_efforts": [], "is_default": false, "available": true},
            {"id": "gpt-5.5", "display_name": "gpt-5.5", "reasoning_efforts": ["low", "medium", "high", "xhigh"], "is_default": true, "available": true},
            {"id": "gpt-6.2-sol", "display_name": "gpt-6.2-sol", "reasoning_efforts": ["low", "medium", "high", "xhigh", "max"], "is_default": false, "available": true}
        ])
    );
    assert!(!models.to_string().contains(&device));
    assert_eq!(available(BOB, path).await.1["models"], json!([]));

    let (_, space) = s.post(ALICE, "/v1/spaces", json!({"name": "shared"})).await;
    let sid = space["id"].as_str().unwrap();
    let room = format!("/v1/spaces/{sid}/rooms/lobby");
    let messages = format!("{room}/messages");
    let scoped = format!("{path}?space_id={sid}");
    assert_eq!(available(BOB, &scoped).await.0, StatusCode::NOT_FOUND);
    assert_eq!(
        s.post(
            ALICE,
            &format!("/v1/spaces/{sid}/members"),
            json!({"user_id":"bob"})
        )
        .await
        .0,
        StatusCode::CREATED
    );
    let (status, share) = s
        .send(
            reqwest::Method::PUT,
            ALICE,
            &format!("/v1/providers/{id}/sharing"),
            Some(json!({"visibility":"spaces","space_ids":[sid]})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{share}");
    assert_eq!(available(BOB, path).await.1["models"], json!([]));
    assert_eq!(
        available(BOB, &scoped).await.1["models"][0]["chat_models"],
        json!([])
    );

    let (_, own) = s
        .post(
            BOB,
            "/v1/providers",
            json!({
                "name": "Bob's ChatGPT", "provider_id": "device-openai-oauth", "model": "gpt-5.5"
            }),
        )
        .await;
    let own_device = s.device_token(BOB, own["id"].as_str().unwrap()).await;
    s.post(&own_device, "/v1/device/claim", poll.clone()).await;
    let entries = available(BOB, &scoped).await.1;
    let preset = entries["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["connection_id"] == id)
        .unwrap();
    assert_eq!(
        preset["chat_models"],
        json!([
            {"id": "gpt-5.5", "display_name": "gpt-5.5", "reasoning_efforts": ["low","medium","high","xhigh"], "is_default": true, "available": true}
        ])
    );
    let forged = json!({
        "text":"@agent not this model", "provider_connection_id":id, "model_id":"gpt-6.2-sol",
        "reasoning_effort":"high"
    });
    assert_eq!(
        s.post(BOB, &messages, forged.clone()).await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        s.post(BOB, "/v1/chat/messages", forged).await.0,
        StatusCode::NOT_FOUND
    );
    let events = format!("{room}/events");
    assert_eq!(available(BOB, &events).await.1["events"], json!([]));

    let rotated = s.device_token(ALICE, id).await;
    assert_eq!(
        available(ALICE, path).await.1["models"][0]["chat_models"],
        json!([])
    );
    assert_eq!(
        s.post(&device, "/v1/device/claim", poll).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(s.claim(&rotated).await.0, StatusCode::NO_CONTENT);
    assert_eq!(
        available(ALICE, path).await.1["models"][0]["chat_models"],
        json!([{"id": "gpt-5.5", "display_name": "gpt-5.5", "reasoning_efforts": [], "is_default": true, "available": true}])
    );
}

#[tokio::test]
async fn chosen_device_model_and_effort_reach_each_tool_turn_and_child() {
    let s = Server::start().await;
    let (_, connection) = s
        .post(
            ALICE,
            "/v1/providers",
            json!({
                "name":"ChatGPT", "provider_id":"device-openai-oauth", "model":"gpt-5.5"
            }),
        )
        .await;
    let id = connection["id"].as_str().unwrap();
    let device = s.device_token(ALICE, id).await;
    let poll = json!({"approved_models":["gpt-5.5","gpt-6.2-sol"]});
    assert_eq!(
        s.post(&device, "/v1/device/claim", poll.clone()).await.0,
        StatusCode::NO_CONTENT
    );
    let (_, space) = s.post(ALICE, "/v1/spaces", json!({"name":"threads"})).await;
    let sid = space["id"].as_str().unwrap();
    assert_eq!(
        s.post(
            ALICE,
            &format!("/v1/spaces/{sid}/members"),
            json!({"user_id":"bob"})
        )
        .await
        .0,
        StatusCode::CREATED
    );
    let room = format!("/v1/spaces/{sid}/rooms/lobby");
    let messages = format!("{room}/messages");
    for (text, audience) in [("ordinary", "room"), ("@bob ordinary", "users")] {
        let (status, event) = s.post(ALICE, &messages, json!({"text": text})).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{event}");
        assert_eq!(event["audience"], audience);
    }
    for (bad, expected) in [
        (
            json!({"text":"@agent no pinned connection","model_id":"gpt-6.2-sol"}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"text":"@bob only","provider_connection_id":id,"model_id":"gpt-6.2-sol"}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"text":"unaddressed","provider_connection_id":id,"model_id":"gpt-6.2-sol"}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"text":"@agent forged","provider_connection_id":id,"model_id":"gpt-6.2-astra"}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            json!({"text":"@agent wrong effort","provider_connection_id":id,"model_id":"gpt-6.2-sol","reasoning_effort":"none"}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
    ] {
        assert_eq!(s.post(ALICE, &messages, bad).await.0, expected);
    }
    let (_, events) = s
        .send(reqwest::Method::GET, ALICE, &format!("{room}/events"), None)
        .await;
    assert_eq!(events["events"].as_array().unwrap().len(), 2);
    assert_eq!(
        s.post(&device, "/v1/device/claim", poll.clone()).await.0,
        StatusCode::NO_CONTENT
    );

    let (status, event) = s
        .post(
            ALICE,
            &messages,
            json!({
                "text":"@agent @bob please start a child", "provider_connection_id":id,
                "model_id":"gpt-6.2-sol", "reasoning_effort":"xhigh"
            }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{event}");
    assert_eq!(event["audience"], "mixed");
    let mut saw_child = false;
    let mut saw_tool_result = false;
    for step in 0..3 {
        let job = s.claim_with_job(&device, &poll).await;
        let req = &job["request"];
        assert_eq!(job["operation"], "complete");
        assert_eq!(req["model"], "gpt-6.2-sol");
        assert_eq!(req["reasoning_effort"], "xhigh");
        let parent = req["tools"].as_array().is_some_and(|tools| {
            tools
                .iter()
                .any(|t| t["function"]["name"] == "thread_create")
        });
        let (message, finish) = if step == 0 {
            assert!(parent);
            (
                json!({"role":"assistant", "content":null,"tool_calls":[{
                    "id":"call_1","type":"function","function":{
                        "name":"thread_create",
                        "arguments":json!({"name":"research","task":"do the child task"}).to_string()
                    }
                }]}),
                "tool_calls",
            )
        } else {
            if parent {
                saw_tool_result = req["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|m| m["role"] == "tool");
            } else {
                saw_child = req.to_string().contains("do the child task");
            }
            (json!({"role":"assistant","content":"done"}), "stop")
        };
        let reply = json!({"ok":true,"response":{
            "id":"fixture","object":"chat.completion","created":0,"model":req["model"],
            "choices":[{"index":0,"finish_reason":finish,"message":message}]
        }});
        assert_eq!(
            s.result(&device, job["id"].as_str().unwrap(), reply)
                .await
                .0,
            StatusCode::NO_CONTENT
        );
    }
    assert!(saw_child && saw_tool_result);
}

#[tokio::test]
async fn owner_only_device_tokens() {
    let s = Server::start().await;
    let id = s.connection(ALICE, "device-openai-oauth").await;
    let path = format!("/v1/providers/{id}/device-token");
    // Another user's id and an unknown id look the same.
    let (status, _) = s.post(BOB, &path, json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = s
        .post(ALICE, "/v1/providers/nope/device-token", json!({}))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = s.post(ALICE, &path, json!({"owner": "bob"})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = s.send(reqwest::Method::DELETE, BOB, &path, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let token = s.device_token(ALICE, &id).await;
    assert!(token.starts_with("metro_pc_"));
    let (status, body) = s.send(reqwest::Method::GET, ALICE, &path, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["paired"], true);
    assert_eq!(body["online"], false);
    assert!(!body.to_string().contains(&token), "never shown again");

    // A device token is no person's credential, and a person's token is no
    // device's.
    let (status, _) = s.send(reqwest::Method::GET, &token, "/v1/me", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = s.claim(ALICE).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = s.post(&token, "/v1/runner/claim", json!({})).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    assert_eq!(s.claim(&token).await.0, StatusCode::NO_CONTENT);
    let (_, body) = s.send(reqwest::Method::GET, ALICE, &path, None).await;
    assert_eq!(body["online"], true);

    // Rotation replaces the token.
    let rotated = s.device_token(ALICE, &id).await;
    assert_ne!(rotated, token);
    assert_eq!(s.claim(&token).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(s.claim(&rotated).await.0, StatusCode::NO_CONTENT);
    // Revocation ends it.
    let (status, _) = s.send(reqwest::Method::DELETE, ALICE, &path, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(s.claim(&rotated).await.0, StatusCode::UNAUTHORIZED);
    let (_, body) = s.send(reqwest::Method::GET, ALICE, &path, None).await;
    assert_eq!(body["paired"], false);
}

#[tokio::test]
async fn mock_device_serves_models_and_completions() {
    let s = Server::start().await;
    let id = s.connection(ALICE, "device-openai-compatible").await;
    let device = s.device_token(ALICE, &id).await;
    assert_eq!(s.claim(&device).await.0, StatusCode::NO_CONTENT);

    let relay = s.state.device.clone();
    let db = s.state.db.clone();
    let conn = id.clone();
    let waiter = tokio::spawn(async move { relay.models(&db, "alice", &conn).await });
    let job = s.claim_job(&device).await;
    assert_eq!(job["operation"], "models");
    assert_eq!(job["request"], json!({}));
    let jid = job["id"].as_str().unwrap();
    let (status, _) = s
        .result(
            &device,
            jid,
            json!({"ok": true, "response": {"models": ["m1"]}}),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(waiter.await.unwrap().unwrap(), vec!["m1"]);
    // A retried result is acknowledged and runs nothing again.
    let (status, _) = s
        .result(
            &device,
            jid,
            json!({"ok": true, "response": {"models": ["m1"]}}),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(s.claim(&device).await.0, StatusCode::NO_CONTENT);

    let relay = s.state.device.clone();
    let db = s.state.db.clone();
    let conn = id.clone();
    let waiter = tokio::spawn(async move {
        relay
            .dispatch(
                &db,
                "alice",
                &conn,
                Operation::Complete,
                json!({"model": "m1", "messages": [{"role": "user", "content": "hi"}]}),
            )
            .await
    });
    let job = s.claim_job(&device).await;
    assert_eq!(job["operation"], "complete");
    assert_eq!(job["request"]["messages"][0]["content"], "hi");
    let reply = json!({"id": "x", "object": "chat.completion", "created": 1, "model": "m1",
        "choices": [{"index": 0, "finish_reason": "tool_calls", "message": {"role": "assistant",
          "content": null, "tool_calls": [{"id": "call_1", "type": "function",
          "function": {"name": "workspace_list", "arguments": "{}"}}]}}]});
    let (status, _) = s
        .result(
            &device,
            job["id"].as_str().unwrap(),
            json!({"ok": true, "response": reply}),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(waiter.await.unwrap().unwrap(), reply);

    // Device failures and malformed answers reach the waiter clearly.
    let relay = s.state.device.clone();
    let db = s.state.db.clone();
    let conn = id.clone();
    let waiter = tokio::spawn(async move { relay.models(&db, "alice", &conn).await });
    let job = s.claim_job(&device).await;
    let (status, _) = s
        .result(
            &device,
            job["id"].as_str().unwrap(),
            json!({"ok": true, "response": {"models": "m1"}}),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let err = waiter.await.unwrap().unwrap_err();
    assert!(err.to_string().contains("invalid response"), "{err}");

    let relay = s.state.device.clone();
    let db = s.state.db.clone();
    let conn = id.clone();
    let waiter = tokio::spawn(async move { relay.models(&db, "alice", &conn).await });
    let job = s.claim_job(&device).await;
    let (status, _) = s
        .result(
            &device,
            job["id"].as_str().unwrap(),
            json!({"ok": false, "error": "signed out"}),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let err = waiter.await.unwrap().unwrap_err();
    assert!(err.to_string().contains("signed out"), "{err}");

    // Shapes outside the wire are refused.
    let (status, _) = s
        .result(&device, "whatever", json!({"ok": true, "output": "x"}))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn devices_are_personal() {
    let s = Server::start().await;
    let alice_conn = s.connection(ALICE, "device-openai-oauth").await;
    let bob_conn = s.connection(BOB, "device-claude-code").await;
    let alice = s.device_token(ALICE, &alice_conn).await;
    let bob = s.device_token(BOB, &bob_conn).await;
    assert_eq!(s.claim(&alice).await.0, StatusCode::NO_CONTENT);
    assert_eq!(s.claim(&bob).await.0, StatusCode::NO_CONTENT);

    // Bob can't dispatch to alice's connection; nothing falls back to his.
    let err = s
        .state
        .device
        .models(&s.state.db, "bob", &alice_conn)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("no such device"), "{err}");
    assert_eq!(s.claim(&bob).await.0, StatusCode::NO_CONTENT);

    let relay = s.state.device.clone();
    let db = s.state.db.clone();
    let conn = alice_conn.clone();
    let waiter = tokio::spawn(async move { relay.models(&db, "alice", &conn).await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        s.claim(&bob).await.0,
        StatusCode::NO_CONTENT,
        "bob never sees it"
    );
    let job = s.claim_job(&alice).await;
    let jid = job["id"].as_str().unwrap();
    let (status, _) = s
        .result(
            &bob,
            jid,
            json!({"ok": true, "response": {"models": ["evil"]}}),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    s.result(
        &alice,
        jid,
        json!({"ok": true, "response": {"models": ["m1"]}}),
    )
    .await;
    assert_eq!(waiter.await.unwrap().unwrap(), vec!["m1"]);

    // Deleting the connection ends its device token.
    let (status, _) = s
        .send(
            reqwest::Method::DELETE,
            ALICE,
            &format!("/v1/providers/{alice_conn}"),
            None,
        )
        .await;
    assert!(status.is_success(), "{status}");
    assert_eq!(s.claim(&alice).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(s.claim(&bob).await.0, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn typed_device_connections_serve_decide_and_embed_over_http() {
    use metro_server::device_relay::{Caller, DeviceError};
    let s = Server::start().await;
    let (status, connection) = s
        .post(
            ALICE,
            "/v1/providers",
            json!({
                "name":"Typed fixture", "provider_id":"device-systemone", "model":"jev-latest"
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{connection}");
    let so = connection["id"].as_str().unwrap().to_owned();
    let emb = s.connection(ALICE, "device-embeddings").await;
    let chat = s.connection(ALICE, "device-openai-compatible").await;
    let so_device = s.device_token(ALICE, &so).await;
    let emb_device = s.device_token(ALICE, &emb).await;
    let chat_device = s.device_token(ALICE, &chat).await;
    let ready = |online| {
        let (s, so) = (&s, so.as_str());
        async move {
            let (status, catalog) = s
                .send(reqwest::Method::GET, ALICE, "/v1/models/available", None)
                .await;
            assert_eq!(status, StatusCode::OK);
            let entry = catalog["models"]
                .as_array()
                .unwrap()
                .iter()
                .find(|entry| entry["connection_id"] == so)
                .unwrap();
            assert_eq!(entry["ready"], online);
            assert!(entry.get("chat_models").is_none());
        }
    };
    ready(false).await;
    for d in [&so_device, &emb_device, &chat_device] {
        assert_eq!(s.claim(d).await.0, StatusCode::NO_CONTENT);
    }
    ready(true).await;
    let alice = Caller::personal("alice");
    let request = json!({"model": "jev-latest", "state": "Charged twice.",
        "questions": {"billing": {"type": "noul", "instructions": "Is this about billing?"}}});

    // decide: the job carries the typed request; the answer is checked.
    let (relay, db, who, conn, req) = (
        s.state.device.clone(),
        s.state.db.clone(),
        alice.clone(),
        so.clone(),
        request.clone(),
    );
    let waiter = tokio::spawn(async move { relay.decide(&db, &who, &conn, req).await });
    let job = s.claim_job(&so_device).await;
    assert_eq!(job["operation"], "decide");
    assert_eq!(job["request"], request);
    let jid = job["id"].as_str().unwrap();
    let answer =
        json!({"model": "jev-latest", "answers": {"billing": {"type": "noul", "noul": 0.97}}});
    let (status, _) = s
        .result(&so_device, jid, json!({"ok": true, "response": answer}))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(waiter.await.unwrap().unwrap(), answer);

    // A response without answers is refused on arrival.
    let (relay, db, who, conn, req) = (
        s.state.device.clone(),
        s.state.db.clone(),
        alice.clone(),
        so.clone(),
        request.clone(),
    );
    let waiter = tokio::spawn(async move { relay.decide(&db, &who, &conn, req).await });
    let job = s.claim_job(&so_device).await;
    let (status, _) = s
        .result(
            &so_device,
            job["id"].as_str().unwrap(),
            json!({"ok": true, "response": {"text": "probably yes"}}),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(matches!(
        waiter.await.unwrap(),
        Err(DeviceError::BadResponse(_))
    ));

    // embed on the embeddings connection.
    let (relay, db, who, conn) = (
        s.state.device.clone(),
        s.state.db.clone(),
        alice.clone(),
        emb.clone(),
    );
    let waiter = tokio::spawn(async move {
        relay
            .embed(&db, &who, &conn, json!({"model": "nomic", "input": ["a"]}))
            .await
    });
    let job = s.claim_job(&emb_device).await;
    assert_eq!(job["operation"], "embed");
    let (status, _) = s
        .result(
            &emb_device,
            job["id"].as_str().unwrap(),
            json!({"ok": true, "response": {"data": [{"index": 0, "embedding": [0.6, 0.8]}]}}),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let out = waiter.await.unwrap().unwrap();
    assert_eq!(out["data"][0]["embedding"], json!([0.6, 0.8]));

    // Each connection serves only its own capability, and other people get
    // nothing without a share.
    let relay = &s.state.device;
    assert_eq!(
        relay
            .decide(&s.state.db, &alice, &chat, request.clone())
            .await,
        Err(DeviceError::NotFound)
    );
    assert_eq!(
        relay
            .embed(
                &s.state.db,
                &alice,
                &so,
                json!({"model": "m", "input": ["a"]})
            )
            .await,
        Err(DeviceError::NotFound)
    );
    assert_eq!(
        relay
            .decide(&s.state.db, &Caller::personal("bob"), &so, request.clone())
            .await,
        Err(DeviceError::NotFound)
    );
    for d in [&so_device, &emb_device, &chat_device] {
        assert_eq!(s.claim(d).await.0, StatusCode::NO_CONTENT);
    }
}
