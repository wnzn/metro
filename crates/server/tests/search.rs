//! Semantic search over real HTTP, through the real provider layer, against
//! a loopback fixture that speaks the OpenAI embeddings wire
//! (`POST /v1/embeddings`) and the System One wire (`POST /v1/systemone`).
//! No vendor is contacted. The fixture records every request body, so the
//! tests check what reached "upstream" as well as what came back.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Json;
use axum::http::StatusCode as AxumStatus;
use axum::response::IntoResponse;
use axum::routing::post;
use metro_server::auth::{AuthFailure, AuthFuture, HumanAuth, UserIdentity};
use metro_server::db::Db;
use metro_server::providers::ProviderService;
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

// ---- Fixture ----

const WORDS: [&str; 4] = ["deploy", "lunch", "invoice", "holiday"];

/// Keyword counts scaled by length, so ranking needs normalization.
fn vector(text: &str) -> Vec<f64> {
    let lower = text.to_lowercase();
    let scale = text.len() as f64;
    let mut v: Vec<f64> = WORDS
        .iter()
        .map(|w| lower.matches(w).count() as f64 * scale)
        .collect();
    v.push(0.01 * scale);
    v
}

#[derive(Default)]
struct Seen {
    /// Every embeddings input, with the model asked for.
    embedded: Vec<(String, String)>,
    /// Every System One `state`.
    decided: Vec<Value>,
}

#[derive(Default)]
struct Script {
    nouls: HashMap<String, f64>,
    decide_status: Option<u16>,
}

struct Fixture {
    base_url: String,
    seen: Arc<Mutex<Seen>>,
    script: Arc<Mutex<Script>>,
}

impl Fixture {
    async fn start() -> Self {
        let seen = Arc::new(Mutex::new(Seen::default()));
        let script = Arc::new(Mutex::new(Script::default()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (s1, s2, sc) = (seen.clone(), seen.clone(), script.clone());
        let app = axum::Router::new()
            .route(
                "/v1/embeddings",
                post(move |Json(body): Json<Value>| {
                    let seen = s1.clone();
                    async move {
                        let model = body["model"].as_str().unwrap_or_default().to_owned();
                        let inputs: Vec<String> = body["input"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|v| v.as_str().unwrap().to_owned())
                            .collect();
                        let mut seen = seen.lock().unwrap();
                        let data: Vec<Value> = inputs
                            .iter()
                            .enumerate()
                            .map(|(i, t)| {
                                seen.embedded.push((model.clone(), t.clone()));
                                json!({"object": "embedding", "index": i, "embedding": vector(t)})
                            })
                            .collect();
                        Json(json!({"object": "list", "data": data, "model": model}))
                    }
                }),
            )
            .route(
                "/v1/systemone",
                post(move |Json(body): Json<Value>| {
                    let (seen, script) = (s2.clone(), sc.clone());
                    async move {
                        seen.lock().unwrap().decided.push(body["state"].clone());
                        let script = script.lock().unwrap();
                        if let Some(status) = script.decide_status {
                            return (
                                AxumStatus::from_u16(status).unwrap(),
                                Json(json!({"detail": "overloaded"})),
                            )
                                .into_response();
                        }
                        let text = body["state"]["candidate_message"]
                            .as_str()
                            .unwrap_or_default();
                        let answers: serde_json::Map<String, Value> = body["questions"]
                            .as_object()
                            .unwrap()
                            .keys()
                            .map(|name| {
                                let n = script.nouls.get(text).copied().unwrap_or(0.0);
                                (name.clone(), json!({"type": "noul", "noul": n}))
                            })
                            .collect();
                        Json(json!({
                            "model": "jev-latest",
                            "answers": answers,
                            "usage": {"input_tokens": 10, "output_tokens": 1}
                        }))
                        .into_response()
                    }
                }),
            );
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            base_url: format!("http://{addr}/v1"),
            seen,
            script,
        }
    }

    fn embedded(&self) -> Vec<(String, String)> {
        self.seen.lock().unwrap().embedded.clone()
    }

    fn decided(&self) -> Vec<Value> {
        self.seen.lock().unwrap().decided.clone()
    }

    fn noul(&self, text: &str, n: f64) {
        self.script.lock().unwrap().nouls.insert(text.to_owned(), n);
    }

    /// Every text that reached the fixture, embedded or reranked.
    fn all_texts(&self) -> String {
        let mut out: Vec<String> = self.embedded().into_iter().map(|(_, t)| t).collect();
        out.extend(self.decided().into_iter().map(|s| s.to_string()));
        out.join("\n")
    }
}

// ---- Server ----

struct Server {
    base: String,
    http: reqwest::Client,
}

impl Server {
    async fn start(allowed: Vec<String>) -> Self {
        let config = Config {
            bind: "127.0.0.1:0".parse().unwrap(),
            db_path: ":memory:".into(),
            model: None,
            limits: Limits {
                agent_timeout: Duration::from_secs(20),
                ..Limits::default()
            },
            org_admins: Vec::new(),
        };
        let db = Db::open_in_memory().unwrap();
        let mut state = AppState::new(config, db, auth()).await.unwrap();
        state.providers = ProviderService::new(None, allowed).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = router(state);
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            base,
            http: reqwest::Client::new(),
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
        let r = req.send().await.unwrap();
        let status = r.status();
        (status, r.json().await.unwrap_or(Value::Null))
    }

    async fn post(&self, token: &str, path: &str, body: Value) -> (StatusCode, Value) {
        self.send(reqwest::Method::POST, token, path, Some(body))
            .await
    }

    async fn ok(&self, method: reqwest::Method, token: &str, path: &str, body: Value) -> Value {
        let (status, v) = self.send(method, token, path, Some(body)).await;
        assert!(status.is_success(), "{path}: {status} {v}");
        v
    }

    async fn connect(&self, token: &str, provider: &str, base_url: &str, model: &str) -> String {
        let v = self
            .ok(
                reqwest::Method::POST,
                token,
                "/v1/providers",
                json!({"name": provider, "provider_id": provider, "base_url": base_url, "model": model}),
            )
            .await;
        v["id"].as_str().unwrap().to_owned()
    }

    async fn route(&self, token: &str, embedding: Option<&str>, classifier: Option<&str>) {
        self.ok(
            reqwest::Method::PUT,
            token,
            "/v1/model-routing",
            json!({"embedding_id": embedding, "classifier_id": classifier}),
        )
        .await;
    }

    async fn space(&self, owner: &str, members: &[&str]) -> String {
        let v = self
            .ok(
                reqwest::Method::POST,
                owner,
                "/v1/spaces",
                json!({"name": "space"}),
            )
            .await;
        let sid = v["id"].as_str().unwrap().to_owned();
        for m in members {
            self.ok(
                reqwest::Method::POST,
                owner,
                &format!("/v1/spaces/{sid}/members"),
                json!({"user_id": m}),
            )
            .await;
        }
        sid
    }

    async fn say(&self, token: &str, sid: &str, text: &str) {
        self.ok(
            reqwest::Method::POST,
            token,
            &format!("/v1/spaces/{sid}/rooms/lobby/messages"),
            json!({"text": text}),
        )
        .await;
    }

    async fn chat(&self, token: &str, text: &str) {
        self.ok(
            reqwest::Method::POST,
            token,
            "/v1/chat/messages",
            json!({"text": text}),
        )
        .await;
    }

    async fn search(&self, token: &str, body: Value) -> Value {
        let (status, v) = self.post(token, "/v1/search", body).await;
        assert_eq!(status, StatusCode::OK, "{v}");
        v
    }
}

fn texts(v: &Value) -> Vec<String> {
    v["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["text"].as_str().unwrap().to_owned())
        .collect()
}

async fn setup() -> (Fixture, Server, String, String) {
    let fixture = Fixture::start().await;
    let server = Server::start(vec![fixture.base_url.clone()]).await;
    let emb = server
        .connect(ALICE, "custom-embeddings", &fixture.base_url, "emb-1")
        .await;
    let s1 = server
        .connect(ALICE, "custom-systemone", &fixture.base_url, "jev-latest")
        .await;
    (fixture, server, emb, s1)
}

#[tokio::test]
async fn search_ranks_by_embeddings_then_system_one_and_sends_only_readable_text() {
    let (fixture, server, emb, s1) = setup().await;
    let shared = server.space(BOB, &["alice"]).await;
    let private = server.space(BOB, &[]).await;
    server.chat(ALICE, "deploy deploy").await;
    server.chat(ALICE, "deploy lunch").await;
    server.say(BOB, &shared, "deploy in the shared space").await;
    server
        .say(BOB, &private, "deploy secret in bob's space")
        .await;
    server.chat(BOB, "deploy bob private chat").await;

    // Embeddings only: retrieval order, with a visible notice.
    server.route(ALICE, Some(&emb), None).await;
    let r = server
        .search(ALICE, json!({"query": "deploy", "scope": "all"}))
        .await;
    assert_eq!(r["ranking"], "embeddings");
    assert!(
        r["notice"]
            .as_str()
            .unwrap()
            .contains("No System One connection")
    );
    assert_eq!(texts(&r).last().unwrap(), "deploy lunch");
    assert_eq!(texts(&r).len(), 3);
    let in_space = r["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["text"] == "deploy in the shared space")
        .unwrap();
    assert_eq!(in_space["space_id"], shared.as_str());
    assert_eq!(in_space["room_id"], "lobby");
    assert!(in_space["event_id"].as_i64().unwrap() > 0);
    assert!(fixture.decided().is_empty());

    // With System One: the shortlist is reordered by noul.
    server.route(ALICE, Some(&emb), Some(&s1)).await;
    fixture.noul("deploy lunch", 0.9);
    fixture.noul("deploy in the shared space", 0.6);
    fixture.noul("deploy deploy", 0.1);
    let r = server
        .search(ALICE, json!({"query": "deploy", "scope": "all"}))
        .await;
    assert_eq!(r["ranking"], "classifier", "{r}");
    assert_eq!(
        texts(&r),
        [
            "deploy lunch",
            "deploy in the shared space",
            "deploy deploy"
        ]
    );
    assert_eq!(r["results"][0]["score"], 0.9);
    assert_eq!(r["partial"], false);
    // Pointwise: one request per candidate, the query and one message.
    for state in fixture.decided() {
        assert_eq!(state["search_query"], "deploy");
        assert_eq!(state.as_object().unwrap().len(), 2);
    }
    assert_eq!(fixture.decided().len(), 3);

    let sent = fixture.all_texts();
    assert!(!sent.contains("bob's space"), "{sent}");
    assert!(!sent.contains("bob private chat"), "{sent}");
    assert!(fixture.embedded().iter().all(|(m, _)| m == "emb-1"));

    // Bob removes alice: the Space disappears from her results, and
    // nothing new from it is sent upstream.
    let before = fixture.embedded().len();
    server
        .ok(
            reqwest::Method::DELETE,
            BOB,
            &format!("/v1/spaces/{shared}/members"),
            json!({"user_id": "alice"}),
        )
        .await;
    server.say(BOB, &shared, "deploy after alice left").await;
    let r = server
        .search(ALICE, json!({"query": "deploy", "scope": "all"}))
        .await;
    assert_eq!(texts(&r), ["deploy lunch", "deploy deploy"]);
    assert!(
        fixture.embedded()[before..]
            .iter()
            .all(|(_, t)| !t.contains("after alice left"))
    );
    let (status, _) = server
        .post(
            ALICE,
            "/v1/search",
            json!({"query": "deploy", "scope": "space", "space_id": shared}),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn system_one_failure_falls_back_to_embeddings_visibly() {
    let (fixture, server, emb, s1) = setup().await;
    server.route(ALICE, Some(&emb), Some(&s1)).await;
    server.chat(ALICE, "deploy deploy").await;
    server.chat(ALICE, "deploy lunch").await;
    fixture.script.lock().unwrap().decide_status = Some(529);
    let r = server
        .search(ALICE, json!({"query": "deploy", "scope": "chat"}))
        .await;
    assert_eq!(r["ranking"], "embeddings");
    assert_eq!(texts(&r), ["deploy deploy", "deploy lunch"]);
    assert!(
        r["notice"].as_str().unwrap().contains("could not rerank"),
        "{r}"
    );
    assert!(
        !fixture.decided().is_empty(),
        "System One was really called"
    );
}

#[tokio::test]
async fn reconfiguring_the_embedding_connection_reindexes() {
    let (fixture, server, emb, _) = setup().await;
    server.route(ALICE, Some(&emb), None).await;
    server.chat(ALICE, "deploy notes").await;
    server
        .search(ALICE, json!({"query": "deploy", "scope": "chat"}))
        .await;
    server
        .search(ALICE, json!({"query": "deploy", "scope": "chat"}))
        .await;
    let notes = |f: &Fixture| {
        f.embedded()
            .iter()
            .filter(|(_, t)| t == "deploy notes")
            .count()
    };
    assert_eq!(notes(&fixture), 1, "indexed once, not on every search");

    server
        .ok(
            reqwest::Method::PATCH,
            ALICE,
            &format!("/v1/providers/{emb}"),
            json!({"model": "emb-2"}),
        )
        .await;
    let r = server
        .search(ALICE, json!({"query": "deploy", "scope": "chat"}))
        .await;
    assert_eq!(texts(&r), ["deploy notes"]);
    assert_eq!(notes(&fixture), 2, "a new model reindexes");
    assert_eq!(fixture.embedded().last().unwrap().0, "emb-2");
}

#[tokio::test]
async fn requests_are_validated_and_indexing_reports_progress() {
    let (_fixture, server, emb, _) = setup().await;
    let (status, v) = server
        .post(
            ALICE,
            "/v1/search",
            json!({"query": "deploy", "scope": "chat"}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{v}");

    server.route(ALICE, Some(&emb), None).await;
    for (body, want) in [
        (
            json!({"query": "  ", "scope": "chat"}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"query": "x", "scope": "space"}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"query": "x", "scope": "chat", "space_id": "s-1"}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"query": "x", "scope": "chat", "limit": 0}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"query": "x", "scope": "space", "space_id": "s-nope"}),
            StatusCode::NOT_FOUND,
        ),
        (
            json!({"query": "x", "scope": "everything"}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
    ] {
        let (status, v) = server.post(ALICE, "/v1/search", body.clone()).await;
        assert_eq!(status, want, "{body} -> {v}");
    }
    let (status, _) = server
        .send(
            reqwest::Method::POST,
            "nobody",
            "/v1/search",
            Some(json!({"query": "x", "scope": "chat"})),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    for i in 0..40 {
        server.chat(ALICE, &format!("lunch {i}")).await;
    }
    let (status, v) = server
        .post(ALICE, "/v1/search/index", json!({"scope": "chat"}))
        .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["indexed"], 40);
    assert_eq!(v["remaining"], 0);
    assert_eq!(v["done"], true);
}
