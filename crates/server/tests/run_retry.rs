//! `POST /v1/runs/{id}/retry`: initiator only, failed or cancelled runs
//! only, a new run on a fresh route answering the same message.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::Json;
use axum::routing::post;
use metro_server::auth::{AuthFailure, AuthFuture, HumanAuth, UserIdentity};
use metro_server::db::Db;
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

struct H {
    base: String,
    http: reqwest::Client,
    hang: Arc<AtomicBool>,
    db_path: std::path::PathBuf,
    _dir: tempfile::TempDir,
}

impl H {
    async fn start() -> Self {
        let hang = Arc::new(AtomicBool::new(false));
        let h = hang.clone();
        let app = axum::Router::new().route(
            "/v1/chat/completions",
            post(move |Json(_): Json<Value>| {
                let h = h.clone();
                async move {
                    if h.load(Ordering::SeqCst) {
                        tokio::time::sleep(Duration::from_secs(60)).await;
                    }
                    Json(
                        json!({"id": "c", "object": "chat.completion", "created": 0, "model": "m",
                        "choices": [{"index": 0, "finish_reason": "stop",
                                     "message": {"role": "assistant", "content": "ok"}}],
                        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}}),
                    )
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let model_url = format!("http://{}/v1", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("metro.db");
        let config = Config {
            bind: "127.0.0.1:0".parse().unwrap(),
            db_path: db_path.clone(),
            model: Some(ModelConfig {
                base_url: model_url,
                model: "env-model".into(),
                api_key: String::new(),
            }),
            limits: Limits::default(),
            org_admins: Vec::new(),
        };
        let state = AppState::new(config, Db::open(&db_path).unwrap(), Arc::new(FakeAuth))
            .await
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, router(state)).await.unwrap() });
        Self {
            base,
            http: reqwest::Client::new(),
            hang,
            db_path,
            _dir: dir,
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

    async fn retry(&self, who: &str, run: &str) -> (StatusCode, Value) {
        self.post(who, &format!("/v1/runs/{run}/retry"), json!({}))
            .await
    }

    /// A Space of alice's with bob in it.
    async fn space(&self) -> String {
        let (_, v) = self.post("alice", "/v1/spaces", json!({"name": "S"})).await;
        let sid = v["id"].as_str().unwrap().to_owned();
        let (s, _) = self
            .post(
                "alice",
                &format!("/v1/spaces/{sid}/members"),
                json!({"user_id": "bob"}),
            )
            .await;
        assert_eq!(s, StatusCode::CREATED);
        sid
    }

    /// `who` asks `agent` (default when `None`) in the lobby; the run.
    async fn ask(&self, who: &str, sid: &str, agent: Option<&str>) -> Value {
        let mut body = json!({"text": "do it", "invoke_agent": true});
        if let Some(a) = agent {
            body["agent_id"] = json!(a);
        }
        let (s, v) = self
            .post(who, &format!("/v1/spaces/{sid}/rooms/lobby/messages"), body)
            .await;
        assert_eq!(s, StatusCode::ACCEPTED, "{v}");
        let eid = v["id"].as_i64().unwrap();
        for _ in 0..400 {
            let (_, runs) = self.req(Method::GET, who, "/v1/runs", None).await;
            if let Some(r) = runs["runs"]
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["trigger_event_id"] == eid)
            {
                return r.clone();
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("no run for {eid}");
    }

    async fn status(&self, who: &str, run: &str) -> String {
        let (_, v) = self
            .req(Method::GET, who, &format!("/v1/runs/{run}"), None)
            .await;
        v["status"].as_str().unwrap_or_default().to_owned()
    }

    async fn settle(&self, who: &str, run: &str) -> String {
        for _ in 0..400 {
            let s = self.status(who, run).await;
            if s != "running" {
                return s;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("{run} still running");
    }
}

#[tokio::test]
async fn only_the_initiator_retries_a_failed_or_cancelled_run() {
    let h = H::start().await;
    let sid = h.space().await;
    h.hang.store(true, Ordering::SeqCst);
    let run = h.ask("bob", &sid, None).await;
    let id = run["id"].as_str().unwrap();
    // Running: not retryable.
    assert_eq!(h.retry("bob", id).await.0, StatusCode::CONFLICT);
    let (s, _) = h
        .post("bob", &format!("/v1/runs/{id}/stop"), json!({}))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(h.settle("bob", id).await, "cancelled");
    // Not the initiator (even the Space owner), not a member, unknown.
    assert_eq!(h.retry("alice", id).await.0, StatusCode::FORBIDDEN);
    assert_eq!(h.retry("carol", id).await.0, StatusCode::NOT_FOUND);
    assert_eq!(h.retry("bob", "run_nope").await.0, StatusCode::NOT_FOUND);

    h.hang.store(false, Ordering::SeqCst);
    let (s, new) = h.retry("bob", id).await;
    assert_eq!(s, StatusCode::CREATED, "{new}");
    let new_id = new["id"].as_str().unwrap();
    assert_ne!(new_id, id);
    assert_eq!(new["trigger_event_id"], run["trigger_event_id"]);
    assert_eq!(new["actor"], "bob");
    assert_eq!(h.settle("bob", new_id).await, "completed");
    // The new run says it is a retry, on the route resolved now.
    let (_, room) = h
        .req(
            Method::GET,
            "bob",
            &format!("/v1/spaces/{sid}/rooms/lobby/events"),
            None,
        )
        .await;
    let started = room["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["activity"]["id"] == new_id && e["activity"]["phase"] == "started")
        .unwrap()
        .clone();
    assert_eq!(started["activity"]["pick"], format!("Retry of {id}"));
    assert!(started["activity"]["route"].is_object());
    // A completed run isn't retryable; retrying twice gives two runs.
    assert_eq!(h.retry("bob", new_id).await.0, StatusCode::CONFLICT);
    let (s, again) = h.retry("bob", id).await;
    assert_eq!(s, StatusCode::CREATED);
    assert_ne!(again["id"], new["id"]);
}

#[tokio::test]
async fn a_retry_needs_its_message_and_its_agent() {
    let h = H::start().await;
    let sid = h.space().await;
    let (s, agent) = h
        .post(
            "alice",
            &format!("/v1/spaces/{sid}/agents"),
            json!({"name": "Coder", "handle": "coder", "description": "Writes code."}),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{agent}");
    let aid = agent["id"].as_str().unwrap();
    h.hang.store(true, Ordering::SeqCst);
    let by_agent = h.ask("bob", &sid, Some(aid)).await;
    let by_default = h.ask("bob", &sid, None).await;
    for r in [&by_agent, &by_default] {
        let id = r["id"].as_str().unwrap();
        h.post("bob", &format!("/v1/runs/{id}/stop"), json!({}))
            .await;
        h.settle("bob", id).await;
    }
    h.hang.store(false, Ordering::SeqCst);
    let (s, _) = h
        .req(
            Method::DELETE,
            "alice",
            &format!("/v1/spaces/{sid}/agents/{aid}"),
            None,
        )
        .await;
    assert!(s.is_success(), "{s}");
    let (s, v) = h.retry("bob", by_agent["id"].as_str().unwrap()).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert!(v["error"].as_str().unwrap().contains("agent"), "{v}");

    // The message it answered is gone.
    let conn = rusqlite::Connection::open(&h.db_path).unwrap();
    conn.execute(
        "DELETE FROM space_events WHERE id = ?1",
        [by_default["trigger_event_id"].as_i64().unwrap()],
    )
    .unwrap();
    let (s, v) = h.retry("bob", by_default["id"].as_str().unwrap()).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert!(
        v["error"].as_str().unwrap().contains("no longer exists"),
        "{v}"
    );
}
