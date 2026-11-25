//! Pages over real HTTP: access (outsiders 404, non-editors 403, revoked
//! membership, private personal pages, a Space page reading only its own
//! Space), optimistic saves, actions as the clicker with per-version
//! confirmation, the page-only personal Chat lane, run now, and generation
//! against a scripted OpenAI-compatible model.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Json;
use axum::routing::post;
use metro_server::auth::{AuthFailure, AuthFuture, HumanAuth, UserIdentity};
use metro_server::db::Db;
use metro_server::{AppState, Config, Limits, ModelConfig, router};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

const ALICE: &str = "alice-token";
const BOB: &str = "bob-token";
const CAROL: &str = "carol-token";
const DAVE: &str = "dave-token";

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

// ---- Scripted model ----

/// A script reply that never arrives (within the test).
const HANG: &str = "__hang__";

type Script = Arc<dyn Fn(&Value, usize) -> String + Send + Sync>;

struct Model {
    config: ModelConfig,
    requests: Arc<Mutex<Vec<Value>>>,
}

/// An OpenAI-compatible endpoint answering each request with
/// `script(request, n)` (n counts from 0).
async fn model(script: impl Fn(&Value, usize) -> String + Send + Sync + 'static) -> Model {
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let seen = requests.clone();
    let script: Script = Arc::new(script);
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        post(move |Json(body): Json<Value>| {
            let (seen, script) = (seen.clone(), script.clone());
            async move {
                let n = {
                    let mut s = seen.lock().unwrap();
                    s.push(body.clone());
                    s.len() - 1
                };
                let text = script(&body, n);
                if text == HANG {
                    tokio::time::sleep(Duration::from_secs(60)).await;
                }
                Json(json!({
                    "id": "chatcmpl-1", "object": "chat.completion", "created": 0, "model": "mock",
                    "choices": [{"index": 0, "message": {"role": "assistant", "content": text},
                                 "finish_reason": "stop"}],
                    "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                }))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Model {
        config: ModelConfig {
            base_url,
            model: "mock".into(),
            api_key: "test-key".into(),
        },
        requests,
    }
}

fn is_builder(request: &Value) -> bool {
    request.to_string().contains("You are Metro's page builder")
}

fn last_user_text(request: &Value) -> String {
    request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|m| m["role"] == "user")
        .map(|m| m["content"].to_string())
        .unwrap_or_default()
}

// ---- Server ----

struct Server {
    base: String,
    http: reqwest::Client,
}

impl Server {
    async fn start(model: Option<ModelConfig>) -> Self {
        Self::start_with(
            model,
            Limits {
                agent_timeout: Duration::from_secs(30),
                ..Limits::default()
            },
        )
        .await
    }

    async fn start_with(model: Option<ModelConfig>, limits: Limits) -> Self {
        let user = |id: &str, name: &str| UserIdentity {
            id: id.to_owned(),
            actor: name.to_owned(),
        };
        let auth = FakeAuth(HashMap::from([
            (ALICE.to_owned(), user("alice", "Alice")),
            (BOB.to_owned(), user("bob", "Bob")),
            (CAROL.to_owned(), user("carol", "Carol")),
            (DAVE.to_owned(), user("dave", "Dave")),
        ]));
        let config = Config {
            bind: "127.0.0.1:0".parse().unwrap(),
            db_path: ":memory:".into(),
            model,
            limits,
            // An org admin gets no view of anyone's pages.
            org_admins: vec!["carol".to_owned()],
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
        method: Method,
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
    async fn get(&self, t: &str, p: &str) -> (StatusCode, Value) {
        self.send(Method::GET, t, p, None).await
    }
    async fn post(&self, t: &str, p: &str, b: Value) -> (StatusCode, Value) {
        self.send(Method::POST, t, p, Some(b)).await
    }
    async fn patch(&self, t: &str, p: &str, b: Value) -> (StatusCode, Value) {
        self.send(Method::PATCH, t, p, Some(b)).await
    }
    async fn delete(&self, t: &str, p: &str) -> (StatusCode, Value) {
        self.send(Method::DELETE, t, p, None).await
    }

    async fn space(&self, token: &str, name: &str, members: &[&str]) -> String {
        let (s, v) = self.post(token, "/v1/spaces", json!({"name": name})).await;
        assert_eq!(s, StatusCode::CREATED, "{v}");
        let sid = v["id"].as_str().unwrap().to_owned();
        for m in members {
            let (s, v) = self
                .post(
                    token,
                    &format!("/v1/spaces/{sid}/members"),
                    json!({"user_id": m}),
                )
                .await;
            assert_eq!(s, StatusCode::CREATED, "{v}");
        }
        sid
    }

    async fn create_page(&self, token: &str, scope: Value, source: &str) -> Value {
        let mut body = scope;
        body["source"] = json!(source);
        let (s, v) = self.post(token, "/v1/pages", body).await;
        assert_eq!(s, StatusCode::CREATED, "{v}");
        v
    }

    async fn draft_done(&self, token: &str, id: &str) -> Value {
        for _ in 0..400 {
            let (s, v) = self.get(token, &format!("/v1/pages/drafts/{id}")).await;
            assert_eq!(s, StatusCode::OK, "{v}");
            if v["status"] != "generating" {
                return v;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("draft {id} never finished");
    }

    async fn data_until(&self, token: &str, pid: &str, pred: impl Fn(&Value) -> bool) -> Value {
        for _ in 0..400 {
            let (s, v) = self
                .post(token, &format!("/v1/pages/{pid}/data"), json!({}))
                .await;
            assert_eq!(s, StatusCode::OK, "{v}");
            if pred(&v) {
                return v;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("page data never matched");
    }
}

fn space_scope(sid: &str) -> Value {
    json!({"scope": "space", "space_id": sid})
}

fn personal() -> Value {
    json!({"scope": "personal"})
}

const SIMPLE: &str = r#"export default (
  <Page title="Simple">
    <Data name="agents" source="agents" />
    <Section key="s" title="Agents">
      <List key="l" items={agents.items} as="a"><Text key="t">{a.name}</Text></List>
    </Section>
  </Page>
);"#;

fn ask_page(title: &str) -> String {
    format!(
        r#"export default (
  <Page title="{title}">
    <Data name="answer" source="agentAnswer" room="lobby" agent="agent" block="ask" />
    <Data name="msgs" source="messages" room="lobby" />
    <AgentAnswer key="a" from={{answer}} />
    <Button key="ask" label="Ask" action="runAgent" agent="agent" room="lobby" prompt="Status please" />
  </Page>
);"#
    )
}

// ---- Access ----

#[tokio::test]
async fn personal_pages_are_private_even_from_org_admins() {
    let s = Server::start(None).await;
    let p = s
        .create_page(
            ALICE,
            personal(),
            "export default <Page title=\"Mine\"><Divider/></Page>",
        )
        .await;
    let pid = p["id"].as_str().unwrap();
    assert_eq!(p["scope"], "personal");
    assert_eq!(p["head_version"], 1);
    // The missing key was added and stored.
    assert!(
        p["source"]
            .as_str()
            .unwrap()
            .contains("<Divider key=\"divider-1\"/>"),
        "{p}"
    );
    for token in [BOB, CAROL] {
        for (method, path, body) in [
            (Method::GET, format!("/v1/pages/{pid}"), None),
            (Method::GET, format!("/v1/pages/{pid}/versions"), None),
            (Method::GET, format!("/v1/pages/{pid}/versions/1"), None),
            (
                Method::POST,
                format!("/v1/pages/{pid}/data"),
                Some(json!({})),
            ),
            (
                Method::POST,
                format!("/v1/pages/{pid}/versions"),
                Some(json!({"base_version": 1, "source": SIMPLE})),
            ),
            (
                Method::POST,
                format!("/v1/pages/{pid}/actions"),
                Some(json!({"version": 1, "block_key": "x"})),
            ),
            (
                Method::PATCH,
                format!("/v1/pages/{pid}"),
                Some(json!({"icon": "x"})),
            ),
            (Method::DELETE, format!("/v1/pages/{pid}"), None),
            (
                Method::POST,
                "/v1/pages/drafts".to_owned(),
                Some(json!({"page_id": pid, "prompt": "x"})),
            ),
        ] {
            let (st, v) = s.send(method.clone(), token, &path, body).await;
            assert_eq!(st, StatusCode::NOT_FOUND, "{method} {path}: {v}");
        }
        let (_, list) = s.get(token, "/v1/pages").await;
        assert_eq!(list["pages"], json!([]));
    }
    let (_, list) = s.get(ALICE, "/v1/pages").await;
    assert_eq!(list["pages"].as_array().unwrap().len(), 1);
    // Malformed ids look like missing ones.
    let (st, _) = s.get(ALICE, "/v1/pages/nope").await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn space_pages_follow_membership_and_edit_rights() {
    let s = Server::start(None).await;
    let sid = s.space(ALICE, "Team", &["bob", "dave"]).await;
    let p = s.create_page(BOB, space_scope(&sid), SIMPLE).await;
    let pid = p["id"].as_str().unwrap().to_owned();
    assert_eq!(p["can_edit"], true);
    assert_eq!(p["creator_id"], "bob");

    // Carol is not a member: everything is 404.
    let (st, _) = s.get(CAROL, &format!("/v1/pages/{pid}")).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = s
        .post(
            CAROL,
            "/v1/pages",
            json!({"scope": "space", "space_id": sid, "source": SIMPLE}),
        )
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = s
        .post(
            CAROL,
            "/v1/pages/validate",
            json!({"scope": "space", "space_id": sid, "source": SIMPLE}),
        )
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    // Dave is a member without edit rights: reads work, writes are 403.
    let (st, v) = s.get(DAVE, &format!("/v1/pages/{pid}")).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["can_edit"], false);
    assert_eq!(v["can_delete"], false);
    let save = json!({"base_version": 1, "source": SIMPLE.replace("Simple", "Dave's")});
    let (st, _) = s
        .post(DAVE, &format!("/v1/pages/{pid}/versions"), save.clone())
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = s.delete(DAVE, &format!("/v1/pages/{pid}")).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = s
        .patch(
            DAVE,
            &format!("/v1/pages/{pid}"),
            json!({"members_edit": true}),
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = s
        .patch(
            DAVE,
            &format!("/v1/pages/{pid}"),
            json!({"title": "Mine now"}),
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = s
        .post(
            DAVE,
            "/v1/pages/drafts",
            json!({"page_id": pid, "prompt": "x"}),
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, v) = s
        .post(DAVE, &format!("/v1/pages/{pid}/data"), json!({}))
        .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["data"]["agents"]["ok"], true);
    assert_eq!(v["data"]["agents"]["value"]["items"][0]["handle"], "agent");

    // The creator turns on "Members can edit": Dave can save.
    let (st, v) = s
        .patch(
            BOB,
            &format!("/v1/pages/{pid}"),
            json!({"members_edit": true}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["members_edit"], true);
    let (st, v) = s
        .post(DAVE, &format!("/v1/pages/{pid}/versions"), save)
        .await;
    assert_eq!(st, StatusCode::CREATED, "{v}");
    assert_eq!(v["head_version"], 2);
    assert_eq!(v["title"], "Dave's");
    assert_eq!(v["version"]["author_id"], "dave");
    // Still not delete or manage rights.
    let (st, _) = s.delete(DAVE, &format!("/v1/pages/{pid}")).await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    // The Space owner manages and deletes any page of the Space.
    let (st, _) = s
        .patch(
            ALICE,
            &format!("/v1/pages/{pid}"),
            json!({"members_edit": false}),
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    let (st, v) = s
        .patch(
            ALICE,
            &format!("/v1/pages/{pid}"),
            json!({"title": "Renamed"}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["title"], "Renamed");
    assert_eq!(v["head_version"], 3);
    assert!(v["source"].as_str().unwrap().contains("title=\"Renamed\""));
    let (st, _) = s.delete(ALICE, &format!("/v1/pages/{pid}")).await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    // Deleted: gone for members, restorable by those who may delete it.
    let (st, _) = s.get(DAVE, &format!("/v1/pages/{pid}")).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (_, v) = s.get(DAVE, "/v1/pages?deleted=true").await;
    assert_eq!(v["pages"], json!([]));
    let (_, v) = s.get(BOB, "/v1/pages?deleted=true").await;
    assert_eq!(v["pages"][0]["id"], pid);
    let (st, _) = s
        .post(DAVE, &format!("/v1/pages/{pid}/undelete"), json!({}))
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, v) = s
        .post(BOB, &format!("/v1/pages/{pid}/undelete"), json!({}))
        .await;
    assert_eq!(st, StatusCode::OK, "{v}");

    // Removing Dave revokes everything at once.
    let (st, _) = s
        .send(
            Method::DELETE,
            ALICE,
            &format!("/v1/spaces/{sid}/members"),
            Some(json!({"user_id": "dave"})),
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    for (m, path) in [
        (Method::GET, format!("/v1/pages/{pid}")),
        (Method::POST, format!("/v1/pages/{pid}/data")),
        (Method::GET, format!("/v1/pages/{pid}/versions")),
    ] {
        let (st, _) = s.send(m, DAVE, &path, Some(json!({}))).await;
        assert_eq!(st, StatusCode::NOT_FOUND, "{path}");
    }
    let (_, v) = s.get(DAVE, "/v1/pages").await;
    assert_eq!(v["pages"], json!([]));

    // Deleting the Space removes its pages.
    let (st, _) = s
        .send(
            Method::DELETE,
            ALICE,
            &format!("/v1/spaces/{sid}"),
            Some(json!({"confirm_name": "Team"})),
        )
        .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let (st, _) = s.get(BOB, &format!("/v1/pages/{pid}")).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_space_page_reads_only_its_own_space() {
    let s = Server::start(None).await;
    let team = s.space(ALICE, "Team", &["bob"]).await;
    let other = s.space(BOB, "Other", &["alice"]).await;
    // Naming another Space, or personal Chat, is refused when saving.
    for source in [
        format!(
            r#"export default <Page title="x"><Data name="m" source="messages" space="{other}" room="lobby"/></Page>"#
        ),
        r#"export default <Page title="x"><Data name="m" source="messages" room="chat"/></Page>"#
            .to_owned(),
    ] {
        let (st, v) = s
            .post(
                ALICE,
                "/v1/pages",
                json!({"scope": "space", "space_id": team, "source": source}),
            )
            .await;
        assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{v}");
        assert!(!v["diagnostics"].as_array().unwrap().is_empty());
    }
    // A personal page may name a Space its owner belongs to, and loses it
    // when they leave.
    let src = format!(
        r#"export default <Page title="x">
          <Data name="m" source="messages" space="{other}" room="lobby"/>
          <Data name="mine" source="schedules"/>
        </Page>"#
    );
    let p = s.create_page(ALICE, personal(), &src).await;
    let pid = p["id"].as_str().unwrap();
    let (st, _) = s
        .post(
            BOB,
            &format!("/v1/spaces/{other}/rooms/lobby/messages"),
            json!({"text": "secret plans"}),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED);
    let (_, v) = s
        .post(ALICE, &format!("/v1/pages/{pid}/data"), json!({}))
        .await;
    assert_eq!(v["data"]["m"]["ok"], true, "{v}");
    assert_eq!(v["data"]["m"]["value"]["items"][0]["text"], "secret plans");
    let (st, _) = s
        .post(ALICE, &format!("/v1/spaces/{other}/leave"), json!({}))
        .await;
    assert_eq!(st, StatusCode::OK);
    let (_, v) = s
        .post(ALICE, &format!("/v1/pages/{pid}/data"), json!({}))
        .await;
    assert_eq!(v["data"]["m"]["ok"], false);
    assert_eq!(v["data"]["m"]["error"], "no_access");
    assert!(!v.to_string().contains("secret plans"));
    // Other sources still resolve.
    assert_eq!(v["data"]["mine"]["ok"], true);
    // Saving it again is refused now.
    let (st, _) = s
        .post(
            ALICE,
            &format!("/v1/pages/{pid}/versions"),
            json!({"base_version": 1, "source": src}),
        )
        .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn saves_are_optimistic_and_restores_add_versions() {
    let s = Server::start(None).await;
    let sid = s.space(ALICE, "Team", &["bob"]).await;
    let p = s.create_page(ALICE, space_scope(&sid), SIMPLE).await;
    let pid = p["id"].as_str().unwrap();
    let v2 = SIMPLE.replace("Simple", "Two");
    let (st, _) = s
        .post(
            ALICE,
            &format!("/v1/pages/{pid}/versions"),
            json!({"base_version": 1, "source": v2}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);
    // A second save on the same base is a conflict, never an overwrite.
    let (st, v) = s
        .post(
            ALICE,
            &format!("/v1/pages/{pid}/versions"),
            json!({"base_version": 1, "source": SIMPLE.replace("Simple", "Lost")}),
        )
        .await;
    assert_eq!(st, StatusCode::CONFLICT, "{v}");
    assert_eq!(v["head_version"], 2);
    // Invalid source: diagnostics, nothing saved.
    let (st, v) = s
        .post(
            ALICE,
            &format!("/v1/pages/{pid}/versions"),
            json!({"base_version": 2, "source": "export default <Page title=\"x\"><div/></Page>"}),
        )
        .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        v["diagnostics"][0]["message"]
            .as_str()
            .unwrap()
            .contains("HTML elements")
    );
    // Restore copies version 1 into version 3.
    let (st, v) = s
        .post(
            ALICE,
            &format!("/v1/pages/{pid}/versions/1/restore"),
            json!({"base_version": 2}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{v}");
    assert_eq!(v["head_version"], 3);
    assert_eq!(v["title"], "Simple");
    assert_eq!(v["version"]["kind"], "restore");
    assert_eq!(v["version"]["restored_from"], 1);
    let (_, v) = s.get(BOB, &format!("/v1/pages/{pid}/versions")).await;
    let kinds: Vec<&str> = v["versions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["restore", "manual", "create"]);
    let (st, v) = s
        .get(BOB, &format!("/v1/pages/{pid}/diff?from=1&to=2"))
        .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["changes"]["page"], true);
    // Validation is pure.
    let (st, v) = s.post(BOB, "/v1/pages/validate", json!({"scope": "space", "space_id": sid, "source": "export default <Page title=\"x\"><Stat label=\"a\" value={1}/></Page>"})).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["ok"], true);
    assert!(v["source"].as_str().unwrap().contains("key=\"stat-1\""));
    // Templates.
    let (st, v) = s
        .post(
            BOB,
            "/v1/pages",
            json!({"scope": "space", "space_id": sid, "template": "release-watch"}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{v}");
    assert_eq!(v["version"]["kind"], "template");
    let (st, _) = s
        .post(
            BOB,
            "/v1/pages",
            json!({"scope": "personal", "template": "space-overview"}),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, v) = s
        .post(
            BOB,
            "/v1/pages",
            json!({"scope": "personal", "template": "daily-brief"}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{v}");
}

// ---- Actions ----

#[tokio::test]
async fn actions_run_as_the_clicker_and_confirm_once_per_version() {
    let m = model(|req, _| {
        if is_builder(req) {
            "no".to_owned()
        } else {
            "All green.".to_owned()
        }
    })
    .await;
    let s = Server::start(Some(m.config.clone())).await;
    let sid = s.space(ALICE, "Team", &["bob", "dave"]).await;
    let p = s
        .create_page(BOB, space_scope(&sid), &ask_page("Ask"))
        .await;
    let pid = p["id"].as_str().unwrap().to_owned();
    let act = |confirm: bool, version: i64| json!({"version": version, "block_key": "ask", "confirm": confirm});

    // The author runs their own version without confirming.
    let (st, v) = s
        .post(BOB, &format!("/v1/pages/{pid}/actions"), act(false, 1))
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{v}");
    assert_eq!(v["started"], true, "{v}");
    let data = s
        .data_until(BOB, &pid, |d| {
            d["data"]["answer"]["value"]["status"] == "done"
        })
        .await;
    assert_eq!(data["data"]["answer"]["value"]["text"], "All green.");
    // The trigger is Bob's own message to the named agent.
    let msgs = &data["data"]["msgs"]["value"]["items"];
    assert_eq!(msgs[0]["author_id"], "bob");
    assert_eq!(msgs[0]["text"], "Status please");

    // Someone else's version: Dave must confirm once.
    let (_, d) = s
        .post(DAVE, &format!("/v1/pages/{pid}/data"), json!({}))
        .await;
    assert_eq!(d["actions"]["ask"]["enabled"], true);
    assert_eq!(d["actions"]["ask"]["confirm"], true);
    let (st, v) = s
        .post(DAVE, &format!("/v1/pages/{pid}/actions"), act(false, 1))
        .await;
    assert_eq!(st, StatusCode::PRECONDITION_REQUIRED, "{v}");
    let c = &v["confirmation"];
    assert_eq!(c["action"]["prompt"], "Status please");
    assert_eq!(c["action"]["agent"], "agent");
    assert_eq!(c["action"]["room"], "lobby");
    assert_eq!(c["edited_by"], "bob");
    assert_eq!(c["runs_as"], "dave");
    let (st, v) = s
        .post(DAVE, &format!("/v1/pages/{pid}/actions"), act(true, 1))
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{v}");
    let (st, _) = s
        .post(DAVE, &format!("/v1/pages/{pid}/actions"), act(false, 1))
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "asked once per version");
    let (_, d) = s
        .post(DAVE, &format!("/v1/pages/{pid}/data"), json!({}))
        .await;
    assert_eq!(d["actions"]["ask"]["confirm"], false);
    // Dave's run is his: the trigger message is his.
    let events = s
        .get(DAVE, &format!("/v1/spaces/{sid}/rooms/lobby/events"))
        .await
        .1;
    assert!(
        events["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"] == "message" && e["actor_id"] == "dave")
    );

    // A new version asks again; a stale version is a conflict.
    let (st, _) = s
        .post(
            BOB,
            &format!("/v1/pages/{pid}/versions"),
            json!({"base_version": 1, "source": ask_page("Ask v2")}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);
    let (st, v) = s
        .post(DAVE, &format!("/v1/pages/{pid}/actions"), act(false, 1))
        .await;
    assert_eq!(st, StatusCode::CONFLICT);
    assert_eq!(v["head_version"], 2);
    let (st, _) = s
        .post(DAVE, &format!("/v1/pages/{pid}/actions"), act(false, 2))
        .await;
    assert_eq!(st, StatusCode::PRECONDITION_REQUIRED);

    // Outsiders can't; unknown and navigation buttons aren't run here.
    let (st, _) = s
        .post(CAROL, &format!("/v1/pages/{pid}/actions"), act(true, 2))
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = s
        .post(
            BOB,
            &format!("/v1/pages/{pid}/actions"),
            json!({"version": 2, "block_key": "nope"}),
        )
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    // A removed member's confirmation is gone with their membership.
    let (st, _) = s
        .send(
            Method::DELETE,
            ALICE,
            &format!("/v1/spaces/{sid}/members"),
            Some(json!({"user_id": "dave"})),
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = s
        .post(DAVE, &format!("/v1/pages/{pid}/actions"), act(true, 2))
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn run_schedule_is_for_the_schedule_owner_only() {
    let m = model(|_, _| "ok".to_owned()).await;
    let s = Server::start(Some(m.config.clone())).await;
    let sid = s.space(ALICE, "Team", &["bob"]).await;
    let (st, task) = s
        .post(
            ALICE,
            "/v1/schedules",
            json!({"name": "Nightly", "prompt": "Nightly report", "space_id": sid, "room_id": "lobby",
                   "schedule": {"kind": "cron", "expression": "0 3 * * *", "timezone": "UTC"}}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{task}");
    let tid = task["id"].as_str().unwrap();
    // Run now: owner only.
    let (st, _) = s
        .post(BOB, &format!("/v1/schedules/{tid}/run"), json!({}))
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, v) = s
        .post(ALICE, &format!("/v1/schedules/{tid}/run"), json!({}))
        .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["run"]["status"], "dispatched", "{v}");
    let (_, h) = s.get(ALICE, &format!("/v1/schedules/{tid}/history")).await;
    assert_eq!(h["runs"].as_array().unwrap().len(), 1);
    let (_, t) = s.get(ALICE, "/v1/schedules").await;
    assert_eq!(
        t["tasks"][0]["next_run_at"], task["next_run_at"],
        "the schedule doesn't move"
    );

    // Bob's page with Alice's schedule button.
    let src = format!(
        r#"export default <Page title="Ops"><Button key="nightly" label="Run nightly" action="runSchedule" schedule="{tid}"/></Page>"#
    );
    let p = s.create_page(BOB, space_scope(&sid), &src).await;
    let pid = p["id"].as_str().unwrap();
    let (_, d) = s
        .post(BOB, &format!("/v1/pages/{pid}/data"), json!({}))
        .await;
    assert_eq!(d["actions"]["nightly"]["enabled"], false);
    let (st, _) = s
        .post(
            BOB,
            &format!("/v1/pages/{pid}/actions"),
            json!({"version": 1, "block_key": "nightly"}),
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (_, d) = s
        .post(ALICE, &format!("/v1/pages/{pid}/data"), json!({}))
        .await;
    assert_eq!(d["actions"]["nightly"]["enabled"], true);
    let (st, v) = s
        .post(
            ALICE,
            &format!("/v1/pages/{pid}/actions"),
            json!({"version": 1, "block_key": "nightly"}),
        )
        .await;
    assert_eq!(st, StatusCode::PRECONDITION_REQUIRED, "{v}");
    assert_eq!(v["confirmation"]["action"]["prompt"], "Nightly report");
    let (st, v) = s
        .post(
            ALICE,
            &format!("/v1/pages/{pid}/actions"),
            json!({"version": 1, "block_key": "nightly", "confirm": true}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["run"]["status"], "dispatched");
}

#[tokio::test]
async fn personal_page_runs_show_only_on_the_page() {
    let m = model(|req, _| {
        if is_builder(req) {
            "no".to_owned()
        } else {
            "Here is your brief.".to_owned()
        }
    })
    .await;
    let s = Server::start(Some(m.config.clone())).await;
    let (st, p) = s
        .post(
            ALICE,
            "/v1/pages",
            json!({"scope": "personal", "template": "daily-brief"}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{p}");
    let pid = p["id"].as_str().unwrap().to_owned();
    let (st, v) = s
        .post(
            ALICE,
            &format!("/v1/pages/{pid}/actions"),
            json!({"version": 1, "block_key": "ask"}),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{v}");
    assert_eq!(v["chat"], true);
    let d = s
        .data_until(ALICE, &pid, |d| {
            d["data"]["brief"]["value"]["status"] == "done"
        })
        .await;
    assert_eq!(d["data"]["brief"]["value"]["text"], "Here is your brief.");
    // Nothing in Alice's personal Chat.
    let (_, chat) = s.get(ALICE, "/v1/chat/events").await;
    assert_eq!(chat["events"], json!([]), "{chat}");
    // The personal agent got no tools.
    let req = m.requests.lock().unwrap().last().cloned().unwrap();
    assert!(
        req.get("tools")
            .is_none_or(|t| t.as_array().is_none_or(Vec::is_empty)),
        "{req}"
    );
}

// ---- Generation ----

const GOOD: &str = "```tsx\nexport default (\n  <Page title=\"Generated\">\n    <Data name=\"agents\" source=\"agents\" />\n    <Section key=\"top\" title=\"Agents\">\n      <List key=\"l\" items={agents.items} as=\"a\"><Text key=\"t\">{a.name}</Text></List>\n    </Section>\n    <Section key=\"other\" title=\"Other\"><Text key=\"o\">Keep me</Text></Section>\n  </Page>\n);\n```";

#[tokio::test]
async fn generation_retries_with_diagnostics_and_applies() {
    let m = model(|req, _| {
        if !is_builder(req) {
            return "agent".to_owned();
        }
        let last = last_user_text(req);
        if last.contains("Metro couldn't use that page") {
            GOOD.to_owned()
        } else if last.contains("Change only the block") {
            // Changes the selected block and, against instructions, another one.
            GOOD.replace("title=\\\"Agents\\\"", "title=\\\"Team agents\\\"")
                .replace("title=\"Agents\"", "title=\"Team agents\"")
                .replace("Keep me", "Changed")
        } else {
            "```tsx\nexport default <Page title=\"Bad\"><div onClick={() => fetch('x')}/></Page>\n```".to_owned()
        }
    })
    .await;
    let s = Server::start(Some(m.config.clone())).await;
    let sid = s.space(ALICE, "Team", &["bob"]).await;
    let (st, d) = s
        .post(
            BOB,
            "/v1/pages/drafts",
            json!({"scope": "space", "space_id": sid, "prompt": "Show the agents"}),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{d}");
    let id = d["id"].as_str().unwrap().to_owned();
    // Only the author sees the draft.
    let (st, _) = s.get(ALICE, &format!("/v1/pages/drafts/{id}")).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let d = s.draft_done(BOB, &id).await;
    assert_eq!(d["status"], "ready", "{d}");
    assert_eq!(d["attempts"], 2);
    assert_eq!(d["changes"]["added"].as_array().unwrap().len(), 5);
    let reqs = m.requests.lock().unwrap().clone();
    let retry = last_user_text(&reqs[1]);
    assert!(retry.contains("HTML elements like <div>"), "{retry}");
    // The context lists names the author can use, and no message content.
    assert!(reqs[0].to_string().contains("agent @agent"));
    // Preview data as the author.
    let (st, pd) = s
        .post(BOB, &format!("/v1/pages/drafts/{id}/data"), json!({}))
        .await;
    assert_eq!(st, StatusCode::OK, "{pd}");
    assert_eq!(pd["data"]["agents"]["ok"], true);
    let (st, page) = s
        .post(BOB, &format!("/v1/pages/drafts/{id}/apply"), json!({}))
        .await;
    assert_eq!(st, StatusCode::CREATED, "{page}");
    assert_eq!(page["version"]["kind"], "create");
    assert_eq!(page["version"]["prompt"], "Show the agents");
    assert_eq!(page["version"]["model"], "server/mock");
    let pid = page["id"].as_str().unwrap().to_owned();
    let (st, _) = s.get(BOB, &format!("/v1/pages/drafts/{id}")).await;
    assert_eq!(st, StatusCode::NOT_FOUND, "an applied draft is gone");

    // Select a block and prompt: changes outside it need one more click.
    let (st, d) = s
        .post(
            BOB,
            "/v1/pages/drafts",
            json!({"page_id": pid, "prompt": "Rename this", "block_key": "top"}),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{d}");
    let id = d["id"].as_str().unwrap().to_owned();
    let d = s.draft_done(BOB, &id).await;
    assert_eq!(d["status"], "ready", "{d}");
    assert_eq!(d["outside_selection"], json!(["o"]), "{d}");
    assert!(d["base_source"].as_str().unwrap().contains("Keep me"));
    let (st, v) = s
        .post(BOB, &format!("/v1/pages/drafts/{id}/apply"), json!({}))
        .await;
    assert_eq!(st, StatusCode::CONFLICT, "{v}");
    assert_eq!(v["outside_selection"], json!(["o"]));
    let (st, v) = s
        .post(
            BOB,
            &format!("/v1/pages/drafts/{id}/apply"),
            json!({"confirm_outside": true}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{v}");
    assert_eq!(v["head_version"], 2);
    assert_eq!(v["version"]["kind"], "prompt");

    // A draft based on an old head can't be applied over a newer one.
    let (_, d) = s
        .post(
            BOB,
            "/v1/pages/drafts",
            json!({"page_id": pid, "prompt": "Anything"}),
        )
        .await;
    let id = d["id"].as_str().unwrap().to_owned();
    s.draft_done(BOB, &id).await;
    let (st, _) = s
        .post(
            BOB,
            &format!("/v1/pages/{pid}/versions"),
            json!({"base_version": 2, "source": SIMPLE}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);
    let (st, v) = s
        .post(BOB, &format!("/v1/pages/drafts/{id}/apply"), json!({}))
        .await;
    // The draft was made from version 2; version 3 is never overwritten.
    assert_eq!(st, StatusCode::CONFLICT, "{v}");
    assert_eq!(v["head_version"], 3);
    let (_, page) = s.get(BOB, &format!("/v1/pages/{pid}")).await;
    assert_eq!(page["head_version"], 3);
}

#[tokio::test]
async fn generation_gives_up_after_two_retries() {
    let m = model(|_, _| "export default <Page title=\"x\"><script/></Page>".to_owned()).await;
    let s = Server::start(Some(m.config.clone())).await;
    let (_, d) = s
        .post(
            ALICE,
            "/v1/pages/drafts",
            json!({"scope": "personal", "prompt": "anything"}),
        )
        .await;
    let d = s.draft_done(ALICE, d["id"].as_str().unwrap()).await;
    assert_eq!(d["status"], "failed");
    assert_eq!(d["attempts"], 3);
    assert_eq!(m.requests.lock().unwrap().len(), 3);
    assert!(
        d["diagnostics"][0]["message"]
            .as_str()
            .unwrap()
            .contains("<script>")
    );
    let (st, _) = s
        .post(
            ALICE,
            &format!("/v1/pages/drafts/{}/apply", d["id"].as_str().unwrap()),
            json!({}),
        )
        .await;
    assert_eq!(st, StatusCode::CONFLICT);
}

#[tokio::test]
async fn generation_never_falls_back() {
    // No server model and no connection: the draft fails visibly.
    let s = Server::start(None).await;
    let (st, d) = s
        .post(
            ALICE,
            "/v1/pages/drafts",
            json!({"scope": "personal", "prompt": "anything"}),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED);
    let d = s.draft_done(ALICE, d["id"].as_str().unwrap()).await;
    assert_eq!(d["status"], "failed");
    assert!(
        d["error"]
            .as_str()
            .unwrap()
            .contains("no default model connection"),
        "{d}"
    );
    // A connection that isn't theirs is refused before anything starts.
    let (st, _) = s
        .post(
            ALICE,
            "/v1/pages/drafts",
            json!({"scope": "personal", "prompt": "x", "provider_connection_id": "pc_00000000000000000000000000000000"}),
        )
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn generations_per_person_are_bounded() {
    // A model that never answers keeps drafts generating.
    let m = model(|_, _| HANG.to_owned()).await;
    let s = Server::start(Some(m.config.clone())).await;
    for _ in 0..2 {
        let (st, _) = s
            .post(
                ALICE,
                "/v1/pages/drafts",
                json!({"scope": "personal", "prompt": "x"}),
            )
            .await;
        assert_eq!(st, StatusCode::ACCEPTED);
    }
    let (st, _) = s
        .post(
            ALICE,
            "/v1/pages/drafts",
            json!({"scope": "personal", "prompt": "x"}),
        )
        .await;
    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS);
    // Someone else isn't affected.
    let (st, _) = s
        .post(
            BOB,
            "/v1/pages/drafts",
            json!({"scope": "personal", "prompt": "x"}),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED);
}

// ---- Review fixes ----

#[tokio::test]
async fn validate_survives_a_15k_coalesce_chain() {
    let s = Server::start(None).await;
    let chain = vec!["r"; 15_000].join("??");
    let src = format!(
        "export default <Page title=\"x\"><Data name=\"r\" source=\"runs\"/><Show key=\"s\" when={{{chain}}}><Divider/></Show></Page>"
    );
    let (st, v) = s
        .post(
            ALICE,
            "/v1/pages/validate",
            json!({"scope": "personal", "source": src}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["ok"], false);
    assert!(
        v["diagnostics"][0]["message"]
            .as_str()
            .unwrap()
            .contains("at most 8 values")
    );
    // The server is still up.
    let (st, _) = s.get(ALICE, "/v1/pages").await;
    assert_eq!(st, StatusCode::OK);
}

#[tokio::test]
async fn renames_and_restores_dont_count_as_reviewing_buttons() {
    let m = model(|_, _| "ok".to_owned()).await;
    let s = Server::start(Some(m.config.clone())).await;
    let sid = s.space(ALICE, "Team", &["bob"]).await;
    let p = s
        .create_page(BOB, space_scope(&sid), &ask_page("Ask"))
        .await;
    let pid = p["id"].as_str().unwrap().to_owned();
    let act = |v: i64| json!({"version": v, "block_key": "ask"});
    // Bob changes the prompt (v2); Alice, the Space owner, renames (v3).
    let (st, _) = s
        .post(BOB, &format!("/v1/pages/{pid}/versions"),
              json!({"base_version": 1, "source": ask_page("Ask").replace("Status please", "Delete the backups")}))
        .await;
    assert_eq!(st, StatusCode::CREATED);
    let (st, v) = s
        .patch(
            ALICE,
            &format!("/v1/pages/{pid}"),
            json!({"title": "Renamed"}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["version"]["kind"], "rename");
    assert_eq!(v["version"]["author_id"], "alice");
    // Alice is the head's author but never reviewed Bob's button.
    let (_, d) = s
        .post(ALICE, &format!("/v1/pages/{pid}/data"), json!({}))
        .await;
    assert_eq!(d["actions"]["ask"]["confirm"], true);
    let (st, v) = s
        .post(ALICE, &format!("/v1/pages/{pid}/actions"), act(3))
        .await;
    assert_eq!(st, StatusCode::PRECONDITION_REQUIRED, "{v}");
    assert_eq!(v["confirmation"]["action"]["prompt"], "Delete the backups");
    // A restore of Bob's version by Alice doesn't count either.
    let (st, v) = s
        .post(
            ALICE,
            &format!("/v1/pages/{pid}/versions/2/restore"),
            json!({"base_version": 3}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{v}");
    let (st, _) = s
        .post(ALICE, &format!("/v1/pages/{pid}/actions"), act(4))
        .await;
    assert_eq!(st, StatusCode::PRECONDITION_REQUIRED);
    // Saving the source herself with that exact button does.
    let (_, head) = s.get(ALICE, &format!("/v1/pages/{pid}")).await;
    let (st, _) = s
        .post(ALICE, &format!("/v1/pages/{pid}/versions"),
              json!({"base_version": 4, "source": head["source"].as_str().unwrap().replace("Renamed", "Mine")}))
        .await;
    assert_eq!(st, StatusCode::CREATED);
    let (st, v) = s
        .post(ALICE, &format!("/v1/pages/{pid}/actions"), act(5))
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{v}");
    // Bob authored that action in v2, so he isn't asked on Alice's v5.
    let (st, v) = s
        .post(BOB, &format!("/v1/pages/{pid}/actions"), act(5))
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{v}");
}

#[tokio::test]
async fn deleting_a_generating_draft_keeps_its_slot() {
    let m = model(|_, _| HANG.to_owned()).await;
    let s = Server::start(Some(m.config.clone())).await;
    let mut ids = Vec::new();
    for _ in 0..2 {
        let (st, d) = s
            .post(
                ALICE,
                "/v1/pages/drafts",
                json!({"scope": "personal", "prompt": "x"}),
            )
            .await;
        assert_eq!(st, StatusCode::ACCEPTED);
        ids.push(d["id"].as_str().unwrap().to_owned());
    }
    let (st, _) = s
        .delete(ALICE, &format!("/v1/pages/drafts/{}", ids[0]))
        .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let (st, _) = s.get(ALICE, &format!("/v1/pages/drafts/{}", ids[0])).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    // Its job still runs, so the slot is still taken.
    let (st, _) = s
        .post(
            ALICE,
            "/v1/pages/drafts",
            json!({"scope": "personal", "prompt": "x"}),
        )
        .await;
    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn page_generation_never_takes_agent_run_slots() {
    let m = model(|req, _| {
        if is_builder(req) {
            HANG.to_owned()
        } else {
            "Agent here.".to_owned()
        }
    })
    .await;
    let s = Server::start_with(
        Some(m.config.clone()),
        Limits {
            agent_timeout: Duration::from_secs(30),
            max_agent_runs: 1,
            ..Limits::default()
        },
    )
    .await;
    let sid = s.space(ALICE, "Team", &["bob", "carol", "dave"]).await;
    for token in [ALICE, BOB, CAROL, DAVE] {
        for _ in 0..2 {
            let (st, _) = s
                .post(
                    token,
                    "/v1/pages/drafts",
                    json!({"scope": "personal", "prompt": "x"}),
                )
                .await;
            assert_eq!(st, StatusCode::ACCEPTED);
        }
    }
    // Let the builder turns start and hold whatever they hold.
    for _ in 0..200 {
        if m.requests.lock().unwrap().len() >= 4 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let (st, v) = s
        .post(
            ALICE,
            &format!("/v1/spaces/{sid}/rooms/lobby/messages"),
            json!({"text": "@agent hi"}),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{v}");
    for _ in 0..400 {
        let (_, e) = s
            .get(ALICE, &format!("/v1/spaces/{sid}/rooms/lobby/events"))
            .await;
        let events = e["events"].as_array().unwrap().clone();
        if events
            .iter()
            .any(|e| e["kind"] == "agent" && e["text"] == "Agent here.")
        {
            return;
        }
        assert!(
            !events.iter().any(|e| e["kind"] == "error"),
            "the agent run was refused: {events:#?}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("the agent never answered");
}

#[tokio::test]
async fn a_personal_page_title_never_reaches_a_space_room() {
    let m = model(|_, _| "done".to_owned()).await;
    let s = Server::start(Some(m.config.clone())).await;
    let sid = s.space(ALICE, "Team", &["bob"]).await;
    let src = format!(
        r#"export default <Page title="Alice private plans"><Button key="ask" label="Ask" action="runAgent" space="{sid}" room="lobby" agent="agent" prompt="Hello"/></Page>"#
    );
    let p = s.create_page(ALICE, personal(), &src).await;
    let pid = p["id"].as_str().unwrap();
    let (st, v) = s
        .post(
            ALICE,
            &format!("/v1/pages/{pid}/actions"),
            json!({"version": 1, "block_key": "ask"}),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{v}");
    let mut seen = false;
    for _ in 0..400 {
        let (_, e) = s
            .get(BOB, &format!("/v1/spaces/{sid}/rooms/lobby/events"))
            .await;
        let text = e.to_string();
        assert!(!text.contains("Alice private plans"), "{text}");
        if e["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"] == "agent")
        {
            assert!(text.contains("(from a page)"), "{text}");
            seen = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(seen);
}
