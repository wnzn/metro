//! The production seams: the library owner's files, the caller's own
//! connections through the provider layer, and the settings handlers.
//!
//! This is the only file that names `crate::library`, the provider
//! functions and `model_routing`, so a change to their signatures is fixed
//! here alone. Used as they are:
//!
//! - `library::visible(&Db, user, id) -> anyhow::Result<Option<Asset>>` and
//!   `library::bytes_for(&Db, user, id) -> anyhow::Result<Option<(Asset,
//!   Vec<u8>)>>`: sync, each rechecks access in SQL at call time (personal:
//!   uploader only; Space: current members only). Run in short `Db::call`s.
//! - `providers::sharing::authorize(&Connection, caller, space, id)`: the
//!   same grant check every typed call makes; with
//!   `providers::catalog::find(provider).media_inputs` it tells whether a
//!   connection is a media embeddings connection the caller may use there.
//! - `providers::media_embed(&AppState, caller, space, id, input)`: one
//!   `POST /v1/media/embeddings` with the connection's saved model; it
//!   refuses text-only connections (`UseError::MediaUnsupported`) before
//!   sending anything, validates the answer
//!   (`metro_model_protocol::check_media_response`) and rechecks a
//!   borrowed grant before returning it.
//! - `providers::model_fingerprint` and `providers::decide`, as message
//!   search uses them.
//! - The sync `providers::sharing::authorize` again inside the store
//!   transaction ([`GrantCheck`]), so a share revoked or a connection
//!   reconfigured while the model ran stores nothing.

use axum::Json;
use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use serde_json::Value;

use super::*;
use crate::AppState;
use crate::api::ApiError;
use crate::auth::Human;

/// The caller's media embedding connection and System One connection.
/// The media connection is the library setting only, never the message
/// search connection.
pub async fn routing(
    state: &AppState,
    who: &Human,
    expected: Option<i64>,
) -> Result<(Routing, i64), LibrarySearchError> {
    let user = who.id.clone();
    let (embedding, revision) = state
        .db
        .call(move |db| Ok(super::store::selection(&db.lock(), &user)?))
        .await?;
    if expected.is_some_and(|expected| revision != expected) {
        return Err(LibrarySearchError::SelectionChanged);
    }
    let prefs = crate::model_routing::preferences(state, &who.id)
        .await
        .map_err(|e| LibrarySearchError::Internal(format!("reading model routing: {e:#}")))?;
    if embedding.is_none() {
        return Err(LibrarySearchError::NoMediaModel);
    }
    Ok((
        Routing {
            embedding,
            classifier: prefs.classifier_id,
        },
        revision,
    ))
}

pub fn seams<'a>(state: &'a AppState, who: &'a Human) -> (DbLibrary, ProviderModels<'a>) {
    (
        DbLibrary {
            db: state.db.clone(),
        },
        ProviderModels {
            state,
            caller: &who.id,
        },
    )
}

/// The library owner's files, read through its own access checks.
pub struct DbLibrary {
    pub db: crate::db::Db,
}

fn info(a: crate::library::Asset) -> AssetInfo {
    AssetInfo {
        id: a.id,
        owner_id: a.uploader_id,
        space_id: a.space_id,
        name: a.name,
        mime_type: a.mime_type,
        size: a.size_bytes,
        sha256: a.sha256,
    }
}

impl Library for DbLibrary {
    fn visible<'a>(&'a self, user: &'a str, asset_id: &'a str) -> LibFuture<'a, Option<AssetInfo>> {
        let (user, id) = (user.to_owned(), asset_id.to_owned());
        Box::pin(async move {
            Ok(self
                .db
                .call(move |db| crate::library::visible(db, &user, &id))
                .await?
                .map(info))
        })
    }

    fn bytes<'a>(
        &'a self,
        user: &'a str,
        asset_id: &'a str,
    ) -> LibFuture<'a, Option<(AssetInfo, Vec<u8>)>> {
        let (user, id) = (user.to_owned(), asset_id.to_owned());
        Box::pin(async move {
            Ok(self
                .db
                .call(move |db| crate::library::bytes_for(db, &user, &id))
                .await?
                .map(|(a, b)| (info(a), b)))
        })
    }
}

pub struct ProviderModels<'a> {
    state: &'a AppState,
    caller: &'a str,
}

/// A provider failure as a [`MediaError`]: definitive (`gone`) when the
/// connection itself can no longer serve this caller here (deleted, share
/// revoked, wrong capability, not a media adapter, no model, a device
/// connection revoked or removed); transient otherwise (busy, offline,
/// timeouts, endpoint errors, key or configuration trouble). Metro faults
/// remain internal errors rather than being reported as upstream failures.
fn provider_error(e: crate::providers::ProviderError) -> MediaError {
    use crate::device_relay::DeviceError as D;
    use crate::providers::{ProviderError as P, UseError as U};
    if matches!(
        &e,
        P::Internal | P::Use(U::Internal) | P::Device(D::Internal)
    ) {
        MediaError::internal(e.to_string())
    } else if matches!(
        &e,
        P::Use(
            U::NotFound
                | U::Revoked
                | U::WrongCapability { .. }
                | U::MediaUnsupported
                | U::Unsupported
                | U::NoModel
        ) | P::Device(D::AccessRevoked | D::NotFound | D::CredentialChanged)
    ) {
        MediaError::gone(e.to_string())
    } else {
        MediaError::transient(e.to_string())
    }
}

fn use_error(e: crate::providers::UseError) -> MediaError {
    provider_error(crate::providers::ProviderError::Use(e))
}

/// The production [`GrantCheck`]: `providers::sharing::authorize` for
/// `caller`, keyed by every fact a stored vector depends on (chosen and
/// running connection, provider, capability, model, config revision,
/// endpoint, and how the caller reaches it).
pub(super) fn grant_check(caller: String) -> GrantCheck {
    std::sync::Arc::new(move |conn, space, id| {
        let a =
            crate::providers::sharing::authorize(conn, &caller, space, id).map_err(use_error)?;
        let media = a.row.capability == crate::providers::Capability::Embeddings
            && crate::providers::catalog::find(&a.row.provider_id).is_some_and(|e| {
                e.media_inputs && e.capability == crate::providers::Capability::Embeddings
            });
        if !media {
            return Err(use_error(crate::providers::UseError::MediaUnsupported));
        }
        if a.row.model.is_empty() {
            return Err(use_error(crate::providers::UseError::NoModel));
        }
        Ok(format!(
            "{:?}",
            (
                &a.chosen,
                &a.row.id,
                &a.row.provider_id,
                a.row.capability.as_str(),
                &a.row.model,
                a.row.config_rev,
                &a.row.base_url,
                &a.access,
            )
        ))
    })
}

impl MediaModels for ProviderModels<'_> {
    fn media_capable<'a>(
        &'a self,
        dest: Option<&'a str>,
        connection: &'a str,
    ) -> ModelFuture<'a, bool> {
        let (caller, space, id) = (
            self.caller.to_owned(),
            dest.map(str::to_owned),
            connection.to_owned(),
        );
        Box::pin(async move {
            let grant = self
                .state
                .db
                .call(move |db| {
                    Ok(crate::providers::sharing::authorize(
                        &db.lock(),
                        &caller,
                        space.as_deref(),
                        &id,
                    ))
                })
                .await
                .map_err(|e| {
                    tracing::error!(%e, "reading the media embedding connection");
                    MediaError::internal("internal error reading the connection")
                })?;
            let a = grant.map_err(use_error)?;
            Ok(a.row.capability == crate::providers::Capability::Embeddings
                && crate::providers::catalog::find(&a.row.provider_id).is_some_and(|e| {
                    e.media_inputs && e.capability == crate::providers::Capability::Embeddings
                }))
        })
    }

    fn fingerprint<'a>(
        &'a self,
        dest: Option<&'a str>,
        connection: &'a str,
    ) -> ModelFuture<'a, String> {
        Box::pin(async move {
            crate::providers::model_fingerprint(self.state, self.caller, dest, connection)
                .await
                .map_err(provider_error)
        })
    }

    fn embed<'a>(
        &'a self,
        dest: Option<&'a str>,
        connection: &'a str,
        input: Value,
        binding: Option<(i64, &'a str)>,
    ) -> ModelFuture<'a, Value> {
        Box::pin(async move {
            crate::providers::media_embed_bound(
                self.state,
                self.caller,
                dest,
                connection,
                input,
                binding,
            )
            .await
            .map_err(provider_error)
        })
    }

    fn decide<'a>(
        &'a self,
        dest: Option<&'a str>,
        connection: &'a str,
        state: Value,
        questions: Value,
        selection: Option<(&'a str, i64)>,
    ) -> ModelFuture<'a, Value> {
        Box::pin(async move {
            crate::providers::library_decide(
                self.state,
                self.caller,
                dest,
                connection,
                (state, questions),
                selection,
            )
            .await
            .map_err(provider_error)
        })
    }

    fn grant(&self) -> GrantCheck {
        grant_check(self.caller.to_owned())
    }
}

/// `GET /v1/library/settings`.
pub async fn get_settings(
    State(state): State<AppState>,
    who: Human,
) -> Result<Json<SettingsView>, ApiError> {
    let prior = crate::model_routing::preferences(&state, &who.id)
        .await
        .map_err(ApiError::internal)?
        .embedding_id;
    let (_, models) = seams(&state, &who);
    Ok(Json(
        engine::settings_view(&state.db, &models, &who.id, prior).await?,
    ))
}

/// `PUT /v1/library/settings`.
pub async fn put_settings(
    State(state): State<AppState>,
    who: Human,
    body: Result<Json<Settings>, JsonRejection>,
) -> Result<Json<SettingsView>, ApiError> {
    let Json(body) = body?;
    let (_, models) = seams(&state, &who);
    Ok(Json(
        engine::save_settings(&state.db, &models, &who.id, body.embedding_id, None).await?,
    ))
}

/// `PUT /v1/library/settings/selection`: guarded write, never routed to the legacy PUT.
pub async fn put_selection(
    State(state): State<AppState>,
    who: Human,
    body: Result<Json<CheckedSettings>, JsonRejection>,
) -> Result<Json<SettingsView>, ApiError> {
    let Json(body) = body?;
    let (_, models) = seams(&state, &who);
    Ok(Json(
        engine::save_settings(
            &state.db,
            &models,
            &who.id,
            body.embedding_id,
            Some((
                body.expected_embedding_id,
                request_revision(body.expected_selection_revision.as_deref(), false)?,
            )),
        )
        .await
        .map_err(selection_save_error)?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device_relay::DeviceError as D;
    use crate::providers::{ProviderError as P, UseError as U};

    #[test]
    fn library_keeps_provider_and_relay_internal_failures_distinct() {
        for e in [P::Internal, P::Use(U::Internal), P::Device(D::Internal)] {
            assert_eq!(provider_error(e).kind, MediaFailure::Internal);
        }
        assert_eq!(
            provider_error(P::Device(D::AccessRevoked)).kind,
            MediaFailure::Gone
        );
        assert_eq!(
            provider_error(P::Endpoint("unavailable".into())).kind,
            MediaFailure::Upstream
        );
    }
}
