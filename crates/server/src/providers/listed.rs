//! Merging an explicit model listing into a chat connection's model list.
//! Composer reads never ask the provider for models: they read the stored
//! list ([`super::models`]).

use sha2::{Digest, Sha256};

use super::catalog::{Adapter, Capability, Entry};
use super::models::{self, Synced};
use super::store::{self, Row};
use crate::db::Db;

pub(super) fn server_chat(entry: &Entry, row: &Row) -> bool {
    row.capability == Capability::Chat
        && entry.capability == Capability::Chat
        && matches!(entry.adapter, Adapter::Registered(_) | Adapter::Custom)
}

/// What a listing was made with: a listing made with an older key, endpoint
/// or revision is never merged.
fn fingerprint(row: &Row) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(row.config_rev.to_be_bytes());
    for part in [
        Some(row.id.as_bytes()),
        Some(row.user_id.as_bytes()),
        Some(row.provider_id.as_bytes()),
        Some(row.capability.as_str().as_bytes()),
        row.base_url.as_deref().map(str::as_bytes),
        row.sealed_key.as_deref(),
    ] {
        match part {
            Some(bytes) => {
                hash.update([1]);
                hash.update((bytes.len() as u64).to_be_bytes());
                hash.update(bytes);
            }
            None => hash.update([0]),
        }
    }
    hash.finalize().into()
}

// Listing APIs often mix inference products in the same response. Remove
// known non-chat IDs/families, not arbitrary IDs based on words in the name.
pub(super) fn known_non_chat(id: &str) -> bool {
    let name = id.rsplit('/').next().unwrap_or(id);
    let name = name.split_once(':').map_or(name, |(name, _)| name);
    matches!(
        name,
        "whisper-1"
            | "tts-1"
            | "tts-1-hd"
            | "dall-e-2"
            | "dall-e-3"
            | "babbage-002"
            | "davinci-002"
            | "gpt-3.5-turbo-instruct"
            | "gpt-3.5-turbo-instruct-0914"
            | "embedding-001"
            | "aqa"
            | "embeddinggemma"
            | "embeddinggemma-2"
            | "nomic-embed-text"
            | "bge-m3"
            | "all-minilm"
            | "mxbai-embed-large"
    ) || [
        "text-embedding-",
        "gemini-embedding-",
        "text-moderation-",
        "omni-moderation-",
        "gpt-image-",
        "gpt-realtime",
        "gpt-4o-realtime-",
        "gpt-4o-mini-realtime-",
        "gpt-4o-transcribe",
        "gpt-4o-mini-transcribe",
        "gpt-4o-mini-tts",
        "sora-2",
        "imagen-",
        "veo-",
    ]
    .iter()
    .any(|prefix| name.starts_with(prefix))
}

/// Merge `listed` (cleaned ids with catalog names) into the chat connection
/// `row` was when the listing started, then settle its model and default.
/// `None` when the connection changed or was deleted meanwhile (nothing is
/// merged). The DB lock is held across the recheck and the merge, never
/// across the upstream call.
pub(super) async fn merge_chat_listing(
    db: &Db,
    row: Row,
    listed: Vec<(String, Option<String>)>,
) -> anyhow::Result<Option<(Row, Synced)>> {
    let listed: Vec<(String, Option<String>)> = listed
        .into_iter()
        .filter(|(id, _)| !known_non_chat(id))
        .take(models::MAX_MODELS_PER_CONNECTION)
        .collect();
    db.call(move |db| {
        let mut conn = db.lock();
        let Some(current) = store::get(&conn, &row.user_id, &row.id)? else {
            return Ok(None);
        };
        if fingerprint(&row) != fingerprint(&current) {
            return Ok(None);
        }
        let tx = conn.transaction()?;
        let synced = models::sync_listed(&tx, &row.id, &listed)?;
        tx.commit()?;
        let Some(settled) = models::settle(&mut conn, &row.user_id, &row.id)? else {
            return Ok(None);
        };
        Ok(Some((settled, synced)))
    })
    .await
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use axum::body::Bytes;
    use axum::extract::{Path, State};
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use axum::{Json, Router, routing::get};
    use base64::Engine;
    use serde_json::json;
    use tokio::net::TcpListener;
    use tokio::sync::Notify;

    use super::*;
    use crate::auth::{AuthFailure, AuthFuture, Human, HumanAuth};
    use crate::providers::secret::MasterKey;
    use crate::providers::{ProviderService, api, catalog};
    use crate::{AppState, Config, Limits};

    fn profile() -> Row {
        Row {
            id: "pc_0123456789abcdef0123456789abcdef".into(),
            user_id: "alice".into(),
            name: "Local gateway".into(),
            provider_id: catalog::CUSTOM.into(),
            base_url: Some("http://127.0.0.1:9876/v1".into()),
            model: "pinned".into(),
            is_default: false,
            sealed_key: None,
            capability: Capability::Chat,
            config_rev: 0,
        }
    }

    struct NoAuth;

    impl HumanAuth for NoAuth {
        fn identity<'a>(&'a self, _bearer: &'a str) -> AuthFuture<'a> {
            Box::pin(async { Err(AuthFailure::Rejected("test".into())) })
        }

        fn namespace(&self) -> &str {
            "dev"
        }
    }

    async fn with_upstream(upstream: Router) -> (AppState, Row, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        let upstream = tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        let config = Config {
            bind: "127.0.0.1:0".parse().unwrap(),
            db_path: ":memory:".into(),
            model: None,
            limits: Limits::default(),
            org_admins: Vec::new(),
        };
        let db = Db::open_in_memory().unwrap();
        let mut state = AppState::new(config, db, Arc::new(NoAuth)).await.unwrap();
        let encoded = base64::engine::general_purpose::STANDARD.encode([42; 32]);
        let master = MasterKey::parse(&encoded).unwrap();
        state.providers = ProviderService::new(Some(master), vec![base.clone()]).unwrap();
        let mut row = profile();
        row.base_url = Some(base);
        assert!(store::insert(&mut state.db.lock(), &row, 20).unwrap());
        (state, row, upstream)
    }

    fn human(id: &str) -> Human {
        Human {
            id: id.into(),
            name: id.into(),
        }
    }

    fn enabled(state: &AppState, row: &Row) -> Vec<String> {
        models::enabled(&state.db.lock(), &row.id)
            .unwrap()
            .into_iter()
            .map(|m| m.model_id)
            .collect()
    }

    #[tokio::test]
    async fn explicit_owner_fetch_adds_chat_models_enabled() {
        let calls = Arc::new(AtomicUsize::new(0));
        let upstream = Router::new().route(
            "/v1/models",
            get({
                let calls = calls.clone();
                move || {
                    let calls = calls.clone();
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        Json(json!({"data": [
                            {"id": "chat-b", "name": "Chat B"},
                            {"id": "chat-a"},
                            {"id": "chat-a"},
                            {"id": "text-embedding-3-small"},
                            {"id": "models/gemini-embedding-001"},
                            {"id": "whisper-1"},
                            {"id": "google/embeddinggemma-2"},
                            {"id": "chat ambiguous"},
                            {"id": "org/chat-with-image-input"}
                        ]}))
                    }
                }
            }),
        );
        let (state, row, upstream) = with_upstream(upstream).await;
        assert_eq!(enabled(&state, &row), ["pinned"]);
        let fetch = |id: &str| {
            api::models(
                State(state.clone()),
                human(id),
                Path(row.id.clone()),
                Bytes::from_static(b"{}"),
            )
        };
        assert!(fetch("bob").await.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let Json(body) = fetch("alice").await.unwrap();
        // The owner-facing Settings answer keeps the raw list.
        assert!(
            body["models"]
                .as_array()
                .unwrap()
                .contains(&json!("whisper-1"))
        );
        assert_eq!(body["added"], 3);
        assert_eq!(
            enabled(&state, &row),
            ["pinned", "chat-b", "chat-a", "org/chat-with-image-input"]
        );
        let b = models::get(&state.db.lock(), &row.id, "chat-b")
            .unwrap()
            .unwrap();
        assert_eq!(b.name(), "Chat B");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        upstream.abort();
    }

    #[tokio::test]
    async fn a_listing_made_before_a_credential_change_is_not_merged() {
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let upstream = Router::new().route(
            "/v1/models",
            get({
                let entered = entered.clone();
                let release = release.clone();
                let calls = Arc::new(AtomicUsize::new(0));
                move || {
                    let (entered, release, calls) =
                        (entered.clone(), release.clone(), calls.clone());
                    async move {
                        let id = match calls.fetch_add(1, Ordering::SeqCst) {
                            0 => {
                                entered.notify_one();
                                release.notified().await;
                                "old-in-flight"
                            }
                            _ => "fresh",
                        };
                        Json(json!({"data": [{"id": id}]}))
                    }
                }
            }),
        );
        let (state, row, upstream) = with_upstream(upstream).await;
        let fetch = || {
            api::models(
                State(state.clone()),
                human("alice"),
                Path(row.id.clone()),
                Bytes::from_static(b"{}"),
            )
        };
        let delayed = tokio::spawn(fetch());
        tokio::time::timeout(Duration::from_secs(5), entered.notified())
            .await
            .expect("upstream received the delayed listing");
        let write = api::update(
            State(state.clone()),
            human("alice"),
            Path(row.id.clone()),
            Ok(Json(
                serde_json::from_value(json!({"api_key": "fixture-key"})).unwrap(),
            )),
        )
        .await
        .unwrap();
        assert!(write.0.has_key);
        release.notify_one();
        let rejected = delayed.await.unwrap().unwrap_err();
        assert_eq!(rejected.into_response().status(), StatusCode::CONFLICT);
        assert_eq!(enabled(&state, &row), ["pinned"]);
        let _ = fetch().await.unwrap();
        assert_eq!(enabled(&state, &row), ["pinned", "fresh"]);
        upstream.abort();
    }

    #[tokio::test]
    async fn merge_after_delete_is_dropped() {
        let db = Db::open_in_memory().unwrap();
        let row = profile();
        assert!(store::insert(&mut db.lock(), &row, 20).unwrap());
        store::delete(&db.lock(), &row.user_id, &row.id).unwrap();
        assert!(
            merge_chat_listing(&db, row, vec![("x".into(), None)])
                .await
                .unwrap()
                .is_none()
        );
    }
}
