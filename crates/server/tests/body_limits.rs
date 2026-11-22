//! Body limits of the assembled router: human routes keep 64 KiB, the device
//! result route alone raises it (to its 2 MiB response bound plus envelope),
//! and the outer human layer does not override that route's own limit.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use metro_server::auth::{AuthFailure, AuthFuture, HumanAuth, UserIdentity};
use metro_server::db::Db;
use metro_server::{AppState, Config, Limits, router};
use reqwest::StatusCode;
use serde_json::{Value, json};

const ALICE: &str = "alice-token";

struct FakeAuth;

impl HumanAuth for FakeAuth {
    fn identity<'a>(&'a self, bearer: &'a str) -> AuthFuture<'a> {
        Box::pin(async move {
            let users = HashMap::from([(ALICE, "alice")]);
            users
                .get(bearer)
                .map(|id| UserIdentity {
                    id: (*id).to_owned(),
                    actor: (*id).to_owned(),
                })
                .ok_or_else(|| AuthFailure::Rejected("unknown token".to_owned()))
        })
    }
    fn namespace(&self) -> &str {
        "dev"
    }
}

async fn post(
    http: &reqwest::Client,
    base: &str,
    token: &str,
    path: &str,
    body: String,
) -> (StatusCode, Value) {
    let r = http
        .post(format!("{base}{path}"))
        .bearer_auth(token)
        .header("content-type", "application/json")
        .body(body)
        .send()
        .await
        .unwrap();
    let status = r.status();
    (status, r.json().await.unwrap_or(Value::Null))
}

#[tokio::test]
async fn device_result_route_alone_accepts_large_bodies() {
    let config = Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        db_path: ":memory:".into(),
        model: None,
        limits: Limits::default(),
        org_admins: Vec::new(),
    };
    let state = AppState::new(config, Db::open_in_memory().unwrap(), Arc::new(FakeAuth))
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = router(state.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let http = reqwest::Client::new();

    // A human route refuses 100 KiB.
    let big_name = json!({"name": "x".repeat(100 * 1024)}).to_string();
    let (status, _) = post(&http, &base, ALICE, "/v1/spaces", big_name).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);

    // A device connection with a claimed job.
    let (status, conn) = post(
        &http,
        &base,
        ALICE,
        "/v1/providers",
        json!({"name": "laptop", "provider_id": "device-openai-compatible"}).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{conn}");
    let id = conn["id"].as_str().unwrap().to_owned();
    let (status, body) = post(
        &http,
        &base,
        ALICE,
        &format!("/v1/providers/{id}/device-token"),
        "{}".to_owned(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let device = body["token"].as_str().unwrap().to_owned();
    assert_eq!(
        post(&http, &base, &device, "/v1/device/claim", "{}".into())
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    let (relay, db, conn_id) = (state.device.clone(), state.db.clone(), id.clone());
    let waiter = tokio::spawn(async move {
        relay
            .complete(
                &db,
                "alice",
                &conn_id,
                json!({"model": "m", "messages": []}),
            )
            .await
    });
    let job = loop {
        let (status, job) = post(&http, &base, &device, "/v1/device/claim", "{}".into()).await;
        if status == StatusCode::OK {
            break job;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    let path = format!("/v1/device/jobs/{}/result", job["id"].as_str().unwrap());

    // Over the route's own bound: refused by the limit.
    let huge =
        json!({"ok": true, "response": {"content": "y".repeat(3 * 1024 * 1024)}}).to_string();
    let (status, _) = post(&http, &base, &device, &path, huge).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);

    // 300 KiB, far above the human limit: accepted.
    let response = json!({
        "id": "c1",
        "object": "chat.completion",
        "created": 0,
        "model": "m",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": "z".repeat(300 * 1024)},
            "finish_reason": "stop"
        }]
    });
    let (status, body) = post(
        &http,
        &base,
        &device,
        &path,
        json!({"ok": true, "response": response}).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let reply = waiter.await.unwrap().unwrap();
    assert_eq!(
        reply["choices"][0]["message"]["content"]
            .as_str()
            .unwrap()
            .len(),
        300 * 1024
    );
}
