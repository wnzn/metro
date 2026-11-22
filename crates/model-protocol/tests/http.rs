//! The clients against a local fixture server: exact paths and bodies, the
//! bearer header only with a key, no redirects, bounded bodies, fixed
//! error text.

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use metro_model_protocol::{decide, embed, list_models};
use serde_json::{Value, json};

/// (route, Authorization header, body)
type Call = (String, Option<String>, Value);

#[derive(Clone, Default)]
struct Seen {
    calls: Arc<Mutex<Vec<Call>>>,
    mode: Arc<Mutex<&'static str>>,
}

fn auth(h: &HeaderMap) -> Option<String> {
    h.get(header::AUTHORIZATION)
        .map(|v| v.to_str().unwrap().to_owned())
}

async fn systemone(State(s): State<Seen>, h: HeaderMap, body: Bytes) -> Response {
    let v: Value = serde_json::from_slice(&body).unwrap();
    s.calls
        .lock()
        .unwrap()
        .push(("systemone".into(), auth(&h), v.clone()));
    match *s.mode.lock().unwrap() {
        "redirect" => (
            StatusCode::TEMPORARY_REDIRECT,
            [(header::LOCATION, "http://127.0.0.1:1/steal")],
        )
            .into_response(),
        "unauthorized" => (StatusCode::UNAUTHORIZED, "bad key sk-test-secret").into_response(),
        "huge" => (
            [(header::CONTENT_TYPE, "application/json")],
            format!("{{\"pad\":\"{}\"}}", "x".repeat(2 * 1024 * 1024)),
        )
            .into_response(),
        "malformed" => {
            axum::Json(json!({"answers": {"billing": {"type": "noul", "noul": 7}}})).into_response()
        }
        _ => {
            let mut answers = serde_json::Map::new();
            for (id, q) in v["questions"].as_object().unwrap() {
                let a = match q["type"].as_str().unwrap() {
                    "noul" => json!({"type": "noul", "noul": 0.9}),
                    "choice" => {
                        let keys: Vec<&String> =
                            q["criteria"].as_object().unwrap().keys().collect();
                        let n = keys.len() as f64;
                        let probs: serde_json::Map<String, Value> = keys
                            .iter()
                            .map(|k| ((*k).clone(), json!(1.0 / n)))
                            .collect();
                        json!({"type": "choice", "choice": keys[0], "confidence": 0.0, "probabilities": probs})
                    }
                    _ => {
                        let n = q["criteria"].as_array().unwrap().len();
                        let probs: serde_json::Map<String, Value> = (0..n)
                            .map(|i| (i.to_string(), json!(1.0 / n as f64)))
                            .collect();
                        json!({"type": "score", "score": 0.5, "confidence": 0.1, "probabilities": probs})
                    }
                };
                answers.insert(id.clone(), a);
            }
            axum::Json(json!({"model": "jev-1", "answers": answers, "usage": {"input_tokens": 3, "output_tokens": 0}}))
                .into_response()
        }
    }
}

async fn embeddings(State(s): State<Seen>, h: HeaderMap, body: Bytes) -> Response {
    let v: Value = serde_json::from_slice(&body).unwrap();
    s.calls
        .lock()
        .unwrap()
        .push(("embeddings".into(), auth(&h), v.clone()));
    let n = v["input"].as_array().unwrap().len();
    // Reverse order on purpose; the client sorts.
    let data: Vec<Value> = (0..n)
        .rev()
        .map(|i| json!({"object": "embedding", "index": i, "embedding": [1.0, i as f64]}))
        .collect();
    axum::Json(json!({"object": "list", "data": data, "model": "nomic"})).into_response()
}

async fn models(State(s): State<Seen>, h: HeaderMap) -> Response {
    s.calls
        .lock()
        .unwrap()
        .push(("models".into(), auth(&h), Value::Null));
    axum::Json(json!({
        "models": [{"name": "jev-latest", "description": "d", "release_date": "2026-09-15"}],
        "data": [{"id": "text-embedding-3-small"}]
    }))
    .into_response()
}

async fn serve() -> (String, Seen) {
    let seen = Seen::default();
    *seen.mode.lock().unwrap() = "ok";
    let app = Router::new()
        .route("/v1/systemone", post(systemone))
        .route("/v1/embeddings", post(embeddings))
        .route("/v1/models", get(models))
        .with_state(seen.clone());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (format!("http://{addr}/v1"), seen)
}

fn decide_request() -> Value {
    json!({
        "model": "jev-latest",
        "state": "Charged twice.",
        "questions": {"billing": {"type": "noul", "instructions": "Is this about billing?"}}
    })
}

#[tokio::test]
async fn decide_posts_the_documented_body() {
    let (base, seen) = serve().await;
    let out = decide(&base, Some("sk-test-secret"), decide_request())
        .await
        .unwrap();
    assert_eq!(
        out["answers"]["billing"],
        json!({"type": "noul", "noul": 0.9})
    );
    assert_eq!(out["model"], "jev-1");
    let calls = seen.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "systemone");
    assert_eq!(calls[0].1.as_deref(), Some("Bearer sk-test-secret"));
    assert_eq!(calls[0].2, decide_request());

    // Choice, then score, each its own request; no key: no Authorization
    // header at all.
    let choice = json!({"model": "openjev", "state": {"msg": "hi"}, "questions": {
        "route": {"type": "choice", "instructions": "Team?", "criteria": {"billing": null, "tech": "bugs"}}
    }});
    let out = decide(&base, None, choice).await.unwrap();
    assert_eq!(out["answers"]["route"]["choice"], "billing");
    let score = json!({"model": "openjev", "state": {"msg": "hi"}, "questions": {
        "urgency": {"type": "score", "instructions": "Urgent?", "criteria": ["no", "yes"]}
    }});
    let out = decide(&base, None, score).await.unwrap();
    assert_eq!(out["answers"]["urgency"]["score"], 0.5);
    assert_eq!(seen.calls.lock().unwrap()[2].1, None);
}

#[tokio::test]
async fn failures_have_fixed_text() {
    let (base, seen) = serve().await;
    for (mode, expect) in [
        ("redirect", "HTTP 307"),
        ("unauthorized", "HTTP 401"),
        ("huge", "too large"),
        ("malformed", "invalid System One response"),
    ] {
        *seen.mode.lock().unwrap() = mode;
        let e = decide(&base, Some("sk-test-secret"), decide_request())
            .await
            .unwrap_err()
            .to_string();
        assert!(e.contains(expect), "{mode}: {e}");
        assert!(
            !e.contains("sk-test") && !e.contains("127.0.0.1") && !e.contains("Charged"),
            "{e}"
        );
    }
    // An invalid request never leaves the machine.
    let before = seen.calls.lock().unwrap().len();
    let mut r = decide_request();
    r["questions"]["billing"]["type"] = json!("box");
    assert!(decide(&base, None, r).await.is_err());
    assert_eq!(seen.calls.lock().unwrap().len(), before);
    let e = decide("http://127.0.0.1:9/v1", None, decide_request())
        .await
        .unwrap_err()
        .to_string();
    assert_eq!(e, "cannot reach the model endpoint");
}

#[tokio::test]
async fn embed_and_models() {
    let (base, seen) = serve().await;
    let out = embed(
        &base,
        None,
        json!({"model": "nomic", "input": ["a", "b", "c"]}),
    )
    .await
    .unwrap();
    let idx: Vec<u64> = out["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["index"].as_u64().unwrap())
        .collect();
    assert_eq!(idx, vec![0, 1, 2]);
    assert_eq!(out["data"][2]["embedding"], json!([1.0, 2.0]));
    let calls = seen.calls.lock().unwrap().clone();
    assert_eq!(
        calls[0].2,
        json!({"model": "nomic", "input": ["a", "b", "c"]})
    );
    assert_eq!(calls[0].1, None);

    assert_eq!(
        list_models(&base, Some("k"), "system_one").await.unwrap(),
        vec!["jev-latest"]
    );
    assert_eq!(
        list_models(&base, None, "embeddings").await.unwrap(),
        vec!["text-embedding-3-small"]
    );
    assert!(list_models(&base, None, "chat").await.is_err());
}
