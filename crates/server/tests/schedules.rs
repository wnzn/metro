//! Scheduled prompts over real HTTP: shapes, owner-only access, validation.
//! The scheduler loop is not started; firing is covered by the unit tests
//! in `src/schedules/tests.rs` with an injected clock.

use std::collections::HashMap;
use std::sync::Arc;

use metro_server::auth::{AuthFailure, AuthFuture, HumanAuth, UserIdentity};
use metro_server::db::Db;
use metro_server::{AppState, Config, Limits, router};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

struct FakeAuth(HashMap<String, UserIdentity>);

impl HumanAuth for FakeAuth {
    fn identity<'a>(&'a self, bearer: &'a str) -> AuthFuture<'a> {
        Box::pin(async move {
            self.0
                .get(bearer)
                .cloned()
                .ok_or_else(|| AuthFailure::Rejected("unknown".to_owned()))
        })
    }
    fn namespace(&self) -> &str {
        "dev"
    }
}

struct Server {
    base: String,
    http: reqwest::Client,
}

impl Server {
    async fn start() -> Self {
        let user = |id: &str| UserIdentity {
            id: id.to_owned(),
            actor: id.to_owned(),
        };
        let auth = FakeAuth(HashMap::from([
            ("alice-token".to_owned(), user("alice")),
            ("bob-token".to_owned(), user("bob")),
        ]));
        let config = Config {
            bind: "127.0.0.1:0".parse().unwrap(),
            db_path: ":memory:".into(),
            model: None,
            limits: Limits::default(),
            org_admins: Vec::new(),
        };
        let state = AppState::new(config, Db::open_in_memory().unwrap(), Arc::new(auth))
            .await
            .unwrap();
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
        m: Method,
        who: &str,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut req = self.http.request(m, format!("{}{path}", self.base));
        if !who.is_empty() {
            req = req.bearer_auth(format!("{who}-token"));
        }
        if let Some(b) = body {
            req = req.json(&b);
        }
        let r = req.send().await.unwrap();
        let s = r.status();
        (s, r.json().await.unwrap_or(Value::Null))
    }
}

fn daily() -> Value {
    json!({
        "name": "Morning brief",
        "prompt": "Summarize what changed overnight",
        "space_id": null,
        "room_id": null,
        "schedule": {"kind": "cron", "expression": "0 9 * * *", "timezone": "Asia/Manila"},
        "enabled": true
    })
}

#[tokio::test]
async fn crud_shapes_and_owner_only() {
    let s = Server::start().await;
    let (st, _) = s.send(Method::GET, "", "/v1/schedules", None).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);

    let (st, task) = s
        .send(Method::POST, "alice", "/v1/schedules", Some(daily()))
        .await;
    assert_eq!(st, StatusCode::CREATED, "{task}");
    let id = task["id"].as_str().unwrap().to_owned();
    assert!(id.starts_with("sch_"));
    assert_eq!(task["owner_id"], "alice");
    assert_eq!(task["schedule"]["timezone"], "Asia/Manila");
    assert_eq!(task["last_status"], Value::Null);
    let local = task["next_run_local"].as_str().unwrap();
    assert!(local.ends_with("T09:00:00+08:00"), "{local}");
    assert!(
        task["next_run_at"]
            .as_str()
            .unwrap()
            .ends_with("T01:00:00Z")
    );

    let (st, list) = s.send(Method::GET, "alice", "/v1/schedules", None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(list["tasks"].as_array().unwrap().len(), 1);
    let (_, list) = s.send(Method::GET, "bob", "/v1/schedules", None).await;
    assert_eq!(list, json!({"tasks": []}));

    let path = format!("/v1/schedules/{id}");
    let hist = format!("{path}/history");
    for (m, body) in [
        (Method::PATCH, Some(json!({"enabled": false}))),
        (Method::DELETE, None),
        (Method::GET, None),
    ] {
        let p = if m == Method::GET { &hist } else { &path };
        let (st, _) = s.send(m.clone(), "bob", p, body).await;
        assert_eq!(st, StatusCode::NOT_FOUND, "{m} by a non-owner");
    }

    let (st, runs) = s.send(Method::GET, "alice", &hist, None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(runs, json!({"runs": []}));

    let (st, task) = s
        .send(
            Method::PATCH,
            "alice",
            &path,
            Some(json!({"enabled": false})),
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(task["enabled"], false);
    assert_eq!(task["next_run_at"], Value::Null);

    let (st, _) = s
        .send(
            Method::PATCH,
            "alice",
            &path,
            Some(json!({"owner_id": "bob"})),
        )
        .await;
    assert!(st.is_client_error());

    let (st, _) = s.send(Method::DELETE, "alice", &path, None).await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let (st, _) = s.send(Method::DELETE, "alice", &path, None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn validation_and_space_access() {
    let s = Server::start().await;
    let mut b = daily();
    b["schedule"] = json!({"kind": "cron", "expression": "* * * * *", "timezone": "Asia/Manila"});
    let (st, _) = s
        .send(Method::POST, "alice", "/v1/schedules", Some(b))
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let mut b = daily();
    b["schedule"] = json!({"kind": "once", "at": "2020-01-01T00:00:00Z"});
    let (st, _) = s
        .send(Method::POST, "alice", "/v1/schedules", Some(b))
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    let (st, space) = s
        .send(
            Method::POST,
            "alice",
            "/v1/spaces",
            Some(json!({"name": "Team"})),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{space}");
    let sid = space["id"].as_str().unwrap();
    let mut b = daily();
    b["space_id"] = json!(sid);
    b["room_id"] = json!("lobby");
    let (st, _) = s
        .send(Method::POST, "bob", "/v1/schedules", Some(b.clone()))
        .await;
    assert_eq!(
        st,
        StatusCode::NOT_FOUND,
        "non-members cannot target a Space"
    );
    let (st, task) = s
        .send(Method::POST, "alice", "/v1/schedules", Some(b))
        .await;
    assert_eq!(st, StatusCode::CREATED);
    assert_eq!(task["space_id"], json!(sid));
    assert_eq!(task["room_id"], "lobby");
}
