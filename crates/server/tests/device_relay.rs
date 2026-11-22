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
        for _ in 0..200 {
            let (status, body) = self.claim(device).await;
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
    let so = s.connection(ALICE, "device-systemone").await;
    let emb = s.connection(ALICE, "device-embeddings").await;
    let chat = s.connection(ALICE, "device-openai-compatible").await;
    let so_device = s.device_token(ALICE, &so).await;
    let emb_device = s.device_token(ALICE, &emb).await;
    let chat_device = s.device_token(ALICE, &chat).await;
    for d in [&so_device, &emb_device, &chat_device] {
        assert_eq!(s.claim(d).await.0, StatusCode::NO_CONTENT);
    }
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
