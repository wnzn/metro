use std::collections::BTreeSet;
use std::sync::Arc;

use metro_server::auth::{AuthFailure, AuthFuture, HumanAuth, UserIdentity};
use metro_server::db::Db;
use metro_server::{AppState, Config, Limits, router};
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};

struct People;

impl HumanAuth for People {
    fn identity<'a>(&'a self, bearer: &'a str) -> AuthFuture<'a> {
        Box::pin(async move {
            let actor = match bearer {
                "lookup" | "another" => "Reader",
                "literal" => "Anne 100%_ Off",
                "other" => "Jo ANNable",
                "unseen" => "Never Signed In",
                "substring" => "Joanne",
                b if b.starts_with("person-") => "Same name",
                _ => return Err(AuthFailure::Rejected("unknown test user".into())),
            };
            Ok(UserIdentity {
                id: bearer.into(),
                actor: actor.into(),
            })
        })
    }

    fn namespace(&self) -> &str {
        "directory-test"
    }
}

struct Server {
    client: Client,
    base: String,
}

impl Server {
    async fn start() -> Self {
        let config = Config {
            bind: "127.0.0.1:0".parse().unwrap(),
            db_path: ":memory:".into(),
            model: None,
            limits: Limits::default(),
            org_admins: vec![],
        };
        let state = AppState::new(config, Db::open_in_memory().unwrap(), Arc::new(People))
            .await
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, router(state)).await.unwrap() });
        Self {
            client: Client::new(),
            base,
        }
    }

    async fn sign_in(&self, user: &str) {
        assert_eq!(
            self.client
                .get(format!("{}/v1/me", self.base))
                .bearer_auth(user)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
    }

    async fn page(&self, user: &str, query: &[(&str, &str)]) -> (StatusCode, Value) {
        let mut url = url::Url::parse(&format!("{}/v1/directory", self.base)).unwrap();
        url.query_pairs_mut().extend_pairs(query.iter().copied());
        let response = self.client.get(url).bearer_auth(user).send().await.unwrap();
        if response.status().is_success() {
            assert_eq!(response.headers()["cache-control"], "no-store");
        }
        (response.status(), response.json().await.unwrap())
    }
}

fn ids(page: &Value) -> Vec<&str> {
    page["users"]
        .as_array()
        .unwrap()
        .iter()
        .map(|user| user["id"].as_str().unwrap())
        .collect()
}

#[tokio::test]
async fn lists_only_cards_and_searches_literal_word_prefixes_or_exact_ids() {
    let s = Server::start().await;
    for name in ["literal", "other", "substring"] {
        s.sign_in(name).await;
    }
    let (status, page) = s.page("lookup", &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ids(&page), ["literal", "other", "substring"]);
    assert_eq!(
        page["users"][0],
        json!({"id": "literal", "display_name": "Anne 100%_ Off", "avatar_version": null})
    );
    assert!(page.get("next_cursor").is_none());
    let cases: [(&str, &[&str]); 10] = [
        ("aNn", &["literal", "other"]),
        ("nne", &[]),
        ("100%_", &["literal"]),
        ("100__", &[]),
        ("%", &[]),
        ("_", &[]),
        ("literal", &["literal"]),
        ("lit", &[]),
        ("LITERAL", &[]),
        ("unseen", &[]),
    ];
    for (query, expected) in cases {
        assert_eq!(ids(&s.page("lookup", &[("q", query)]).await.1), expected);
    }
    assert_eq!(s.page("bogus", &[]).await.0, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn limits_and_cursor_are_bounded_and_ties_paginate_by_id() {
    let s = Server::start().await;
    for i in (0..30).rev() {
        s.sign_in(&format!("person-{i:02}")).await;
    }
    let (_, default) = s.page("lookup", &[]).await;
    assert_eq!(ids(&default).len(), 20);
    let (_, first) = s.page("lookup", &[("limit", "999")]).await;
    assert_eq!(ids(&first).len(), 25);
    assert_eq!(ids(&first).first().copied(), Some("person-00"));
    let cursor = first["next_cursor"].as_str().unwrap();
    let (_, second) = s.page("lookup", &[("cursor", cursor)]).await;
    assert_eq!(
        ids(&second),
        [
            "person-25",
            "person-26",
            "person-27",
            "person-28",
            "person-29"
        ]
    );
    assert!(second.get("next_cursor").is_none());
    assert_eq!(
        ids(&first)
            .into_iter()
            .chain(ids(&second))
            .collect::<BTreeSet<_>>()
            .len(),
        30
    );
    for query in [
        ("q", "é".repeat(65)),
        ("cursor", "!".into()),
        ("cursor", "a".repeat(2049)),
    ] {
        assert_eq!(
            s.page("lookup", &[(query.0, &query.1)]).await.0,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(
        s.page("lookup", &[("q", &"é".repeat(64))]).await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn directory_rate_limit_is_per_person() {
    let s = Server::start().await;
    for _ in 0..30 {
        assert_eq!(s.page("lookup", &[]).await.0, StatusCode::OK);
    }
    assert_eq!(s.page("lookup", &[]).await.0, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(s.page("another", &[]).await.0, StatusCode::OK);
    assert_eq!(
        s.page("lookup", &[("q", "anything")]).await.0,
        StatusCode::TOO_MANY_REQUESTS
    );
}
