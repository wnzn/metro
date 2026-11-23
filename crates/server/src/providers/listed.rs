//! Short-lived owner-only model suggestions from explicit server listings.
//! Composer reads never ask the provider for models.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use super::catalog::{self, Adapter, Capability, Entry};
use super::store::{self, Row};
use super::{MAX_LISTED_MODELS, ProviderService};
use crate::db::Db;

const TTL: Duration = Duration::from_secs(10 * 60);
const MAX_PROFILES: usize = 32;

#[derive(Default)]
pub(super) struct Cache {
    by_id: HashMap<String, Listing>,
}

struct Listing {
    config: [u8; 32],
    started_at: Instant,
    expires_at: Instant,
    ids: Vec<String>,
}

pub(super) fn server_chat(entry: &Entry, row: &Row) -> bool {
    row.capability == Capability::Chat
        && entry.capability == Capability::Chat
        && matches!(entry.adapter, Adapter::Registered(_) | Adapter::Custom)
}

fn fingerprint(row: &Row) -> [u8; 32] {
    let mut hash = Sha256::new();
    // A credential round trip can restore the same ciphertext field (None
    // before and after), but never the same configuration revision. Pinning
    // a different model also starts a new revision.
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
fn known_non_chat(id: &str) -> bool {
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

impl Cache {
    fn get(&self, row: &Row, now: Instant) -> &[String] {
        self.by_id
            .get(&row.id)
            .filter(|entry| now < entry.expires_at && entry.config == fingerprint(row))
            .map_or(&[], |entry| entry.ids.as_slice())
    }

    fn insert(&mut self, row: &Row, ids: Vec<String>, started_at: Instant, now: Instant) {
        let config = fingerprint(row);
        self.by_id.retain(|_, entry| now < entry.expires_at);
        if self
            .by_id
            .get(&row.id)
            .is_some_and(|previous| previous.config == config && previous.started_at > started_at)
        {
            return;
        }
        if !self.by_id.contains_key(&row.id)
            && self.by_id.len() >= MAX_PROFILES
            && let Some(oldest) = self
                .by_id
                .iter()
                .min_by_key(|(_, entry)| entry.expires_at)
                .map(|(id, _)| id.clone())
        {
            self.by_id.remove(&oldest);
        }
        self.by_id.insert(
            row.id.clone(),
            Listing {
                config,
                started_at,
                expires_at: now + TTL,
                ids,
            },
        );
    }
}

impl ProviderService {
    /// The pinned model and suggestions for a server-stored chat profile.
    /// Call only with a currently authorized, owner-scoped row (`Access::Own`).
    /// Borrowers and presets must not use or see the cached suggestions.
    pub(crate) fn listed_chat_models(&self, row: &Row) -> Vec<String> {
        if !catalog::find(&row.provider_id).is_some_and(|entry| server_chat(entry, row)) {
            return vec![];
        }
        let cache = self
            .inner
            .listed_chat
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let listed = cache.get(row, Instant::now());
        // Keep the explicitly pinned choice, even if absent from the list.
        // Never let the discovery list crowd it out.
        let pinned = (!row.model.is_empty()).then_some(&row.model);
        pinned
            .into_iter()
            .chain(listed.iter().filter(|id| Some(*id) != pinned))
            .take(MAX_LISTED_MODELS)
            .cloned()
            .collect()
    }

    /// Publish only if the owner still has the same listing configuration.
    /// Hold the DB lock across recheck and insertion, not the upstream call.
    /// Readers also check the fingerprint against their own authorized row.
    pub(super) async fn remember_chat_listing(
        &self,
        db: &Db,
        row: Row,
        started_at: Instant,
        cleaned_ids: &[String],
    ) -> anyhow::Result<bool> {
        let ids = cleaned_ids
            .iter()
            .filter(|id| !known_non_chat(id))
            .take(MAX_LISTED_MODELS)
            .cloned()
            .collect();
        let service = self.clone();
        db.call(move |db| {
            let conn = db.lock();
            let Some(current) = store::get(&conn, &row.user_id, &row.id)? else {
                return Ok(false);
            };
            if fingerprint(&row) != fingerprint(&current) {
                return Ok(false);
            }
            service
                .inner
                .listed_chat
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(&current, ids, started_at, Instant::now());
            Ok(true)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

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
    use crate::providers::api;
    use crate::providers::secret::MasterKey;
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

    #[tokio::test]
    async fn explicit_owner_fetch_populates_local_chat_suggestions() {
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
                            {"id": "chat-b"},
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

        let read = || state.providers.listed_chat_models(&row);
        assert_eq!(read(), ["pinned"]);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
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
        // The owner-facing Settings endpoint preserves its existing list.
        assert!(
            body["models"]
                .as_array()
                .unwrap()
                .contains(&json!("whisper-1"))
        );
        assert_eq!(
            read(),
            ["pinned", "chat-a", "chat-b", "org/chat-with-image-input"]
        );
        assert_eq!(read().len(), 4);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let mut other = row.clone();
        other.id = "pc_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into();
        assert_eq!(state.providers.listed_chat_models(&other), ["pinned"]);
        other = row.clone();
        other.capability = Capability::Embeddings;
        assert!(state.providers.listed_chat_models(&other).is_empty());
        upstream.abort();
    }

    #[tokio::test]
    async fn adding_then_removing_a_key_invalidates_cached_and_in_flight_listings() {
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
                            0 => "old-cached",
                            1 => {
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
        let write = |body| {
            api::update(
                State(state.clone()),
                human("alice"),
                Path(row.id.clone()),
                Ok(Json(serde_json::from_value(body).unwrap())),
            )
        };
        let read = || {
            store::get(&state.db.lock(), "alice", &row.id)
                .unwrap()
                .unwrap()
        };
        let _ = fetch().await.unwrap();
        assert_eq!(
            state.providers.listed_chat_models(&read()),
            ["pinned", "old-cached"]
        );
        let delayed = tokio::spawn(fetch());
        tokio::time::timeout(Duration::from_secs(5), entered.notified())
            .await
            .expect("upstream received the delayed listing");
        assert!(
            write(json!({"api_key": "fixture-key"}))
                .await
                .unwrap()
                .0
                .has_key
        );
        assert!(!write(json!({"api_key": null})).await.unwrap().0.has_key);
        let current = read();
        assert_eq!(current.config_rev, row.config_rev + 2);
        assert!(current.sealed_key.is_none());
        assert_eq!(state.providers.listed_chat_models(&current), ["pinned"]);

        release.notify_one();
        let rejected = delayed.await.unwrap().unwrap_err();
        assert_eq!(rejected.into_response().status(), StatusCode::CONFLICT);
        assert_eq!(state.providers.listed_chat_models(&read()), ["pinned"]);

        let _ = fetch().await.unwrap();
        assert_eq!(
            state.providers.listed_chat_models(&read()),
            ["pinned", "fresh"]
        );
        let _ = write(json!({"name": "New name"})).await.unwrap();
        assert_eq!(
            state.providers.listed_chat_models(&read()),
            ["pinned", "fresh"]
        );
        let _ = write(json!({"model": "new-pinned"})).await.unwrap();
        assert_eq!(state.providers.listed_chat_models(&read()), ["new-pinned"]);
        upstream.abort();
    }

    #[tokio::test]
    async fn delayed_fetch_cannot_publish_after_credential_change_or_delete() {
        let db = Db::open_in_memory().unwrap();
        let providers = ProviderService::new(None, vec![]).unwrap();
        let mut old = profile();
        old.sealed_key = Some(vec![1]);
        assert!(store::insert(&mut db.lock(), &old, 20).unwrap());
        let started = Instant::now();
        assert!(
            providers
                .remember_chat_listing(&db, old.clone(), started, &["old-chat".into()])
                .await
                .unwrap()
        );
        let mut current = old.clone();
        current.sealed_key = Some(vec![2]);
        current.config_rev = store::update(&mut db.lock(), &current).unwrap().unwrap();
        assert_eq!(providers.listed_chat_models(&current), ["pinned"]);
        assert!(
            providers
                .remember_chat_listing(&db, current.clone(), started, &["new-chat".into()])
                .await
                .unwrap()
        );
        assert!(
            !providers
                .remember_chat_listing(&db, old, started, &["stale".into()])
                .await
                .unwrap()
        );
        assert_eq!(
            providers.listed_chat_models(&current),
            ["pinned", "new-chat"]
        );
        store::delete(&db.lock(), &current.user_id, &current.id).unwrap();
        assert!(
            !providers
                .remember_chat_listing(&db, current, started, &["deleted".into()])
                .await
                .unwrap()
        );
    }

    #[test]
    fn cache_bounds_expiry_identity_and_concurrent_refreshes() {
        let mut cache = Cache::default();
        let row = profile();
        let start = Instant::now();
        let later = start + Duration::from_secs(1);
        cache.insert(&row, vec!["newer".into()], later, later);
        cache.insert(&row, vec!["older".into()], start, later);
        assert_eq!(cache.get(&row, later), ["newer"]);
        for changed in [
            Row {
                user_id: "bob".into(),
                ..row.clone()
            },
            Row {
                provider_id: "openai".into(),
                ..row.clone()
            },
            Row {
                base_url: Some("http://127.0.0.1:9877/v1".into()),
                ..row.clone()
            },
            Row {
                sealed_key: Some(vec![1]),
                ..row.clone()
            },
        ] {
            assert!(cache.get(&changed, later).is_empty());
        }
        assert!(cache.get(&row, later + TTL).is_empty());

        let after_expiry = later + TTL;
        cache.insert(&row, vec!["oldest".into()], after_expiry, after_expiry);
        let next = after_expiry + Duration::from_secs(1);
        for n in 0..MAX_PROFILES {
            let other = Row {
                id: format!("pc_{n:032x}"),
                ..row.clone()
            };
            cache.insert(&other, vec!["available".into()], next, next);
        }
        assert_eq!(cache.by_id.len(), MAX_PROFILES);
        assert!(cache.get(&row, next).is_empty());
    }
}
