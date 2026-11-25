//! Managed local user accounts over real HTTP, with the real dev-token
//! authenticator: org admins create users, issue, reissue and revoke their
//! tokens; managed users sign in, see only Spaces they are added to, and
//! never gain admin rights. OIDC servers have no managed accounts. Also the
//! static admin shell and its security headers.

use std::collections::HashMap;
use std::sync::Arc;

use metro_server::auth::{AuthFailure, AuthFuture, HumanAuth, UserIdentity};
use metro_server::db::Db;
use metro_server::{AppState, Config, Limits, router};
use reqwest::StatusCode;
use serde_json::{Value, json};

// Test-only placeholders, not real credentials.
const JM: &str = "jm-dev-token-0000000000000001";
const BOB: &str = "bob-dev-token-000000000000002";

#[tokio::test]
async fn organization_settings_are_bearer_admin_only_and_persist() {
    use reqwest::Method;

    const SETTINGS: &str = "/v1/admin/settings";
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.sqlite");
    let s = Server::dev(Db::open(&path).unwrap()).await;
    let default = json!({"pages_per_space": 50, "versions_per_page": 200});
    assert_eq!(s.get(JM, SETTINGS).await, (StatusCode::OK, default.clone()));

    for method in [Method::GET, Method::PATCH] {
        let (status, headers, _) = s.send(method.clone(), None, SETTINGS, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(headers["www-authenticate"], "Bearer");
        let (status, _, _) = s.send(method, Some(BOB), SETTINGS, None).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
    for body in [
        json!({"pages_per_space": 0}),
        json!({"pages_per_space": 501}),
        json!({"versions_per_page": 1}),
        json!({"versions_per_page": 1001, "pages_per_space": 500}),
    ] {
        let (status, _) = s.call(Method::PATCH, JM, SETTINGS, Some(body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
    for body in [
        json!({"pages_per_space": null}),
        json!({"pages_per_space": 2.5}),
        json!({"versions_per_page": "200"}),
        json!({"unrecognized": 100}),
    ] {
        let (status, _) = s.call(Method::PATCH, JM, SETTINGS, Some(body)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    }
    assert_eq!(s.get(JM, SETTINGS).await, (StatusCode::OK, default.clone()));
    assert_eq!(
        s.call(
            Method::PATCH,
            JM,
            SETTINGS,
            Some(json!({"pages_per_space": 500})),
        )
        .await,
        (
            StatusCode::OK,
            json!({"pages_per_space": 500, "versions_per_page": 200})
        )
    );
    let changed = json!({"pages_per_space": 500, "versions_per_page": 2});
    assert_eq!(
        s.call(
            Method::PATCH,
            JM,
            SETTINGS,
            Some(json!({"versions_per_page": 2}))
        )
        .await,
        (StatusCode::OK, changed.clone())
    );
    // An independently reopened DB must see the same values, with no
    // on-disk defaults replacing them on initialization.
    let reopened = Server::dev(Db::open(&path).unwrap()).await;
    assert_eq!(reopened.get(JM, SETTINGS).await, (StatusCode::OK, changed));

    // Settings are also available to an OIDC subject listed in org admins;
    // they do not depend on the managed-user (dev only) admin API.
    let oidc = Server::start_with(
        config(&["jm"]),
        Db::open_in_memory().unwrap(),
        Arc::new(OidcAuth),
    )
    .await;
    assert_eq!(oidc.get(JM, SETTINGS).await, (StatusCode::OK, default));
}

fn config(admins: &[&str]) -> Config {
    Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        db_path: ":memory:".into(),
        model: None,
        limits: Limits::default(),
        org_admins: admins.iter().map(|s| (*s).to_owned()).collect(),
    }
}

/// The real dev authenticator, as `METRO_DEV_USERS` would configure it.
async fn dev_auth() -> Arc<dyn HumanAuth> {
    let users = format!(r#"{{"jm":"{JM}","bob":"{BOB}"}}"#);
    let auth = metro_auth::Authenticator::from_lookup(move |name| {
        Ok((name == metro_auth::env::DEV_USERS).then(|| users.clone()))
    })
    .await
    .unwrap();
    Arc::new(auth)
}

/// An OIDC stand-in: one admin subject.
struct OidcAuth;

impl HumanAuth for OidcAuth {
    fn identity<'a>(&'a self, bearer: &'a str) -> AuthFuture<'a> {
        Box::pin(async move {
            let users = HashMap::from([(JM, "jm")]);
            users
                .get(bearer)
                .map(|id| UserIdentity {
                    id: (*id).to_owned(),
                    actor: "JM".to_owned(),
                })
                .ok_or_else(|| AuthFailure::Rejected("unknown token".to_owned()))
        })
    }
    fn namespace(&self) -> &str {
        "oidc:https://idp.example"
    }
}

struct Server {
    base: String,
    http: reqwest::Client,
}

impl Server {
    async fn start_with(config: Config, db: Db, auth: Arc<dyn HumanAuth>) -> Self {
        let state = AppState::new(config, db, auth)
            .await
            .unwrap()
            .with_env_user_ids(vec!["jm".to_owned(), "bob".to_owned()]);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, router(state)).await.unwrap() });
        Self {
            base,
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap(),
        }
    }

    async fn dev(db: Db) -> Self {
        Self::start_with(config(&["jm"]), db, dev_auth().await).await
    }

    async fn send(
        &self,
        method: reqwest::Method,
        token: Option<&str>,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, reqwest::header::HeaderMap, String) {
        let mut req = self.http.request(method, format!("{}{path}", self.base));
        if let Some(token) = token {
            req = req.bearer_auth(token);
        }
        if let Some(body) = body {
            req = req.json(&body);
        }
        let r = req.send().await.unwrap();
        let status = r.status();
        let headers = r.headers().clone();
        (status, headers, r.text().await.unwrap())
    }

    async fn call(
        &self,
        method: reqwest::Method,
        token: &str,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let (status, _, text) = self.send(method, Some(token), path, body).await;
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    async fn get(&self, token: &str, path: &str) -> (StatusCode, Value) {
        self.call(reqwest::Method::GET, token, path, None).await
    }

    async fn post(&self, token: &str, path: &str, body: Value) -> (StatusCode, Value) {
        self.call(reqwest::Method::POST, token, path, Some(body))
            .await
    }

    /// Create a managed user as jm; returns `(id, token)`.
    async fn create(&self, name: &str) -> (String, String) {
        let (status, headers, text) = self
            .send(
                reqwest::Method::POST,
                Some(JM),
                "/v1/admin/users",
                Some(json!({"name": name})),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{text}");
        assert_eq!(headers["cache-control"], "no-store");
        let body: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(body["name"], name);
        let id = body["id"].as_str().unwrap().to_owned();
        let token = body["token"].as_str().unwrap().to_owned();
        assert!(id.starts_with("mu:") && id.len() == 35, "{id}");
        assert!(token.starts_with("metro_ut_") && token.len() == 9 + 64);
        (id, token)
    }
}

fn mem() -> Db {
    Db::open_in_memory().unwrap()
}

#[tokio::test]
async fn admin_creates_a_user_who_can_sign_in() {
    let s = Server::dev(mem()).await;
    let (id, token) = s.create("Ana Reyes").await;

    let (status, me) = s.get(&token, "/v1/me").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        me,
        json!({"id": id, "actor": "Ana Reyes", "is_org_admin": false})
    );

    // The env bootstrap admin is untouched.
    let (status, me) = s.get(JM, "/v1/me").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me["id"], "jm");
    assert_eq!(me["is_org_admin"], true);

    // Listing shows the account, never its token or hash.
    let (status, headers, text) = s
        .send(reqwest::Method::GET, Some(JM), "/v1/admin/users", None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["cache-control"], "no-store");
    assert!(!text.contains(&token) && !text.contains("metro_ut_") && !text.contains("hash"));
    let list: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        list,
        json!({"users": [{"id": id, "name": "Ana Reyes", "has_token": true}]})
    );

    // Ids are distinct and server-generated.
    let (id2, token2) = s.create("Ben").await;
    assert_ne!(id, id2);
    assert_ne!(token, token2);
}

#[tokio::test]
async fn reissue_replaces_and_revoke_ends_the_token_at_once() {
    let s = Server::dev(mem()).await;
    let (id, old) = s.create("Ana").await;

    let (status, headers, text) = s
        .send(
            reqwest::Method::POST,
            Some(JM),
            &format!("/v1/admin/users/{id}/token"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(headers["cache-control"], "no-store");
    let body: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(body.as_object().unwrap().len(), 1);
    let new = body["token"].as_str().unwrap().to_owned();
    assert!(new.starts_with("metro_ut_"));
    assert_ne!(new, old);

    assert_eq!(s.get(&old, "/v1/me").await.0, StatusCode::UNAUTHORIZED);
    let (status, me) = s.get(&new, "/v1/me").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me["id"], id);

    let (status, _, text) = s
        .send(
            reqwest::Method::DELETE,
            Some(JM),
            &format!("/v1/admin/users/{id}/token"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(text.is_empty());
    assert_eq!(s.get(&new, "/v1/me").await.0, StatusCode::UNAUTHORIZED);
    let (_, list) = s.get(JM, "/v1/admin/users").await;
    assert_eq!(list["users"][0]["has_token"], false);
    // Revoking again is still 204: the account exists and has no token.
    let (status, _) = s
        .call(
            reqwest::Method::DELETE,
            JM,
            &format!("/v1/admin/users/{id}/token"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // A fresh token after revoke works; the id stays the same.
    let (status, body) = s
        .post(JM, &format!("/v1/admin/users/{id}/token"), json!(null))
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, me) = s.get(body["token"].as_str().unwrap(), "/v1/me").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me["id"], id);

    // Unknown and malformed ids are 404.
    let ghost = format!("mu:{}", "0".repeat(32));
    for path in [
        format!("/v1/admin/users/{ghost}/token"),
        "/v1/admin/users/jm/token".to_owned(),
        "/v1/admin/users/bob/token".to_owned(),
    ] {
        let (status, _, text) = s.send(reqwest::Method::POST, Some(JM), &path, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
        assert!(!text.contains("metro_ut_"));
        let (status, _, _) = s.send(reqwest::Method::DELETE, Some(JM), &path, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
    }
}

#[tokio::test]
async fn only_env_org_admins_manage_users() {
    let s = Server::dev(mem()).await;
    let (id, token) = s.create("Ana").await;
    let token_path = format!("/v1/admin/users/{id}/token");
    let routes: Vec<(reqwest::Method, String, Option<Value>)> = vec![
        (reqwest::Method::GET, "/v1/admin/users".into(), None),
        (
            reqwest::Method::POST,
            "/v1/admin/users".into(),
            Some(json!({"name": "Eve"})),
        ),
        (
            reqwest::Method::PATCH,
            format!("/v1/admin/users/{id}"),
            Some(json!({"name": "Eve"})),
        ),
        (reqwest::Method::POST, token_path.clone(), None),
        (reqwest::Method::DELETE, token_path.clone(), None),
    ];
    for (method, path, body) in &routes {
        // No bearer, garbage, a runner-shaped token: 401.
        let (status, headers, _) = s.send(method.clone(), None, path, body.clone()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {path}");
        assert_eq!(headers["www-authenticate"], "Bearer");
        for bad in ["nope", "metro_ut_0000", "metro_rt_v2_abc", "metro_pc_abc"] {
            let (status, _, _) = s.send(method.clone(), Some(bad), path, body.clone()).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {path} {bad}");
        }
        // An env user who isn't an admin, and a managed user: 403.
        for who in [BOB, token.as_str()] {
            let (status, _, text) = s.send(method.clone(), Some(who), path, body.clone()).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{method} {path}");
            assert!(!text.contains(&token));
        }
    }
    // Nothing changed: still one user, still signed in.
    let (_, list) = s.get(JM, "/v1/admin/users").await;
    assert_eq!(list["users"].as_array().unwrap().len(), 1);
    assert_eq!(list["users"][0]["name"], "Ana");
    assert_eq!(s.get(&token, "/v1/me").await.0, StatusCode::OK);
}

#[tokio::test]
async fn a_managed_id_in_org_admins_refuses_to_start() {
    let id = format!("mu:{}", "a".repeat(32));
    let err = AppState::new(config(&["jm", &id]), mem(), dev_auth().await)
        .await
        .err()
        .expect("must refuse");
    assert!(format!("{err:#}").contains("managed"));
}

#[tokio::test]
async fn oidc_servers_have_no_managed_users() {
    let db = mem();
    // Even a row smuggled into the database is never consulted.
    let s = Server::start_with(config(&["jm"]), db.clone(), Arc::new(OidcAuth)).await;
    let managed = metro_server::admin_users::Managed::new("dev", Vec::new());
    let (_, token) = match db.create_managed_user(&managed, &[], "Mallory", "jm") {
        Ok(v) => v,
        Err(_) => panic!("seeding failed"),
    };
    assert_eq!(s.get(&token, "/v1/me").await.0, StatusCode::UNAUTHORIZED);

    let (status, me) = s.get(JM, "/v1/me").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me["is_org_admin"], true);
    let (status, body) = s.get(JM, "/v1/admin/users").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    let (status, _) = s.post(JM, "/v1/admin/users", json!({"name": "Ana"})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // Non-admins still get 403 first, and unauthenticated 401.
    let (status, _, _) = s
        .send(reqwest::Method::GET, None, "/v1/admin/users", None)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // An OIDC subject shaped like a managed id is an ordinary member id.
    let (_, space) = s.post(JM, "/v1/spaces", json!({"name": "Ops"})).await;
    let sid = space["id"].as_str().unwrap();
    let (status, _) = s
        .post(
            JM,
            &format!("/v1/spaces/{sid}/members"),
            json!({"user_id": format!("mu:{}", "b".repeat(32))}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
}

#[tokio::test]
async fn names_cannot_impersonate_anyone() {
    // History from before managed accounts: a dormant member and author.
    let db = mem();
    let space = db.create_space("olduser", "Old", 10).unwrap();
    db.add_member(&space.id, "ghost", 10).unwrap();
    let s = Server::dev(db).await;
    let (ana, _) = s.create("Ana").await;

    for name in [
        "jm", "JM", "Bob", "agent", "System", "runner", "admin", "olduser", "GHOST", "ana", "mu:x",
        "user:jm",
    ] {
        let (status, body) = s.post(JM, "/v1/admin/users", json!({"name": name})).await;
        assert_eq!(status, StatusCode::CONFLICT, "{name}: {body}");
    }
    for bad in ["", "   ", "a\nb", "a\u{202E}b", &"x".repeat(65)] {
        let (status, _) = s.post(JM, "/v1/admin/users", json!({"name": bad})).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad:?}");
    }
    let (status, _) = s
        .post(JM, "/v1/admin/users", json!({"name": "Eve", "id": "jm"}))
        .await;
    assert!(status.is_client_error());
    let (_, list) = s.get(JM, "/v1/admin/users").await;
    assert_eq!(list["users"].as_array().unwrap().len(), 1);

    // Rename: display name changes, id does not; same rules apply.
    let (ben, ben_token) = s.create("Ben").await;
    let rename = |name: &str| {
        let path = format!("/v1/admin/users/{ben}");
        let body = json!({"name": name});
        let s = &s;
        async move { s.call(reqwest::Method::PATCH, JM, &path, Some(body)).await }
    };
    for name in ["Ana", "jm", "ghost", "agent"] {
        assert_eq!(rename(name).await.0, StatusCode::CONFLICT, "{name}");
    }
    let (status, body) = rename("Benjamin").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({"id": ben, "name": "Benjamin", "has_token": true})
    );
    // Renaming to the same name (or back to one only they used) is fine.
    assert_eq!(rename("Benjamin").await.0, StatusCode::OK);
    assert_eq!(rename("Ben").await.0, StatusCode::OK);
    let (_, me) = s.get(&ben_token, "/v1/me").await;
    assert_eq!(
        me,
        json!({"id": ben, "actor": "Ben", "is_org_admin": false})
    );
    let (status, _) = s
        .call(
            reqwest::Method::PATCH,
            JM,
            &format!("/v1/admin/users/mu:{}", "0".repeat(32)),
            Some(json!({"name": "Zed"})),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_ne!(ana, ben);
}

#[tokio::test]
async fn managed_users_get_no_implicit_membership() {
    let s = Server::dev(mem()).await;
    let (_, space) = s.post(JM, "/v1/spaces", json!({"name": "Ops"})).await;
    let sid = space["id"].as_str().unwrap().to_owned();

    // No Space can be prepared for an account that doesn't exist yet.
    for unknown in [format!("mu:{}", "c".repeat(32)), "mu:short".to_owned()] {
        let (status, _) = s
            .post(
                JM,
                &format!("/v1/spaces/{sid}/members"),
                json!({"user_id": unknown}),
            )
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{unknown}");
    }

    let (id, token) = s.create("Ana").await;
    let (status, spaces) = s.get(&token, "/v1/spaces").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(spaces, json!({"spaces": []}));
    assert_eq!(
        s.get(&token, &format!("/v1/spaces/{sid}/rooms")).await.0,
        StatusCode::NOT_FOUND
    );
    // A managed user can't manage org runners either.
    let (status, _) = s
        .post(
            &token,
            "/v1/runners",
            json!({"name": "r", "visibility": "organization"}),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // The owner adds them explicitly; then they are an ordinary member.
    let (status, _) = s
        .post(
            JM,
            &format!("/v1/spaces/{sid}/members"),
            json!({"user_id": id}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let (_, spaces) = s.get(&token, "/v1/spaces").await;
    assert_eq!(spaces["spaces"][0]["id"], sid);
    assert_eq!(spaces["spaces"][0]["role"], "member");
    let (status, _) = s
        .post(
            &token,
            &format!("/v1/spaces/{sid}/members"),
            json!({"user_id": "bob"}),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Their messages carry their id and display name.
    let (status, ev) = s
        .post(
            &token,
            &format!("/v1/spaces/{sid}/rooms/lobby/messages"),
            json!({"text": "hello"}),
        )
        .await;
    assert!(status.is_success(), "{ev}");
    assert_eq!(ev["actor_id"], id);
    let (_, events) = s
        .get(JM, &format!("/v1/spaces/{sid}/rooms/lobby/events"))
        .await;
    let last = events["events"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(last["actor"], "Ana");
    assert_eq!(last["actor_id"], id);

    // Revoking the token ends access at once; membership is kept.
    s.call(
        reqwest::Method::DELETE,
        JM,
        &format!("/v1/admin/users/{id}/token"),
        None,
    )
    .await;
    assert_eq!(
        s.get(&token, "/v1/spaces").await.0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn admin_shell_is_static_and_locked_down() {
    let s = Server::dev(mem()).await;
    let (status, headers, _) = s.send(reqwest::Method::GET, None, "/", None).await;
    assert!(status.is_redirection());
    assert_eq!(headers["location"], "/admin");

    for (path, ty) in [
        ("/admin", "text/html"),
        ("/admin/assets/admin.css", "text/css"),
        ("/admin/assets/admin.js", "text/javascript"),
    ] {
        let (status, h, body) = s.send(reqwest::Method::GET, None, path, None).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert!(
            h["content-type"].to_str().unwrap().starts_with(ty),
            "{path}"
        );
        let csp = h["content-security-policy"].to_str().unwrap();
        assert!(csp.contains("script-src 'self'") && csp.contains("style-src 'self'"));
        assert!(csp.contains("frame-ancestors 'none'"));
        assert!(!csp.contains("unsafe-inline") && !csp.contains("unsafe-eval"));
        assert_eq!(h["x-content-type-options"], "nosniff");
        assert_eq!(h["referrer-policy"], "no-referrer");
        assert_eq!(h["cache-control"], "no-store");
        assert_eq!(h["x-frame-options"], "DENY");
        assert!(h.get("access-control-allow-origin").is_none());
        assert!(!body.contains("metro_ut_0") && !body.contains(JM));
    }
    let (_, h, body) = s.send(reqwest::Method::GET, None, "/admin", None).await;
    assert!(body.contains("/admin/assets/admin.js"));
    assert!(h.get("set-cookie").is_none());

    // No CORS anywhere: a cross-origin preflight is not granted.
    let r = s
        .http
        .request(
            reqwest::Method::OPTIONS,
            format!("{}/v1/admin/users", s.base),
        )
        .header("origin", "https://evil.example")
        .header("access-control-request-method", "POST")
        .send()
        .await
        .unwrap();
    assert!(r.headers().get("access-control-allow-origin").is_none());
    let r = s
        .http
        .get(format!("{}/v1/admin/users", s.base))
        .header("origin", "https://evil.example")
        .bearer_auth(JM)
        .send()
        .await
        .unwrap();
    assert!(r.headers().get("access-control-allow-origin").is_none());
}
