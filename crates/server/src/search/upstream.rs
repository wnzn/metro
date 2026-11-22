//! The production [`Models`]: the caller's own connections through the
//! provider layer.
//!
//! This is the only file that calls `model_routing` and the provider
//! functions, so a change to their signatures is fixed here alone.
//! Assumed (agreed with the provider lane):
//!
//! - `model_routing::preferences(&AppState, user_id: &str) -> anyhow::Result<RoutingSettings>`
//!   with `embedding_id: Option<String>` and `classifier_id: Option<String>`.
//! - `providers::model_fingerprint(&AppState, caller: &str, space: Option<&str>, connection_id: &str)
//!   -> Result<String, ProviderError>`, async; errors when the connection may not be used for
//!   that destination or capability.
//! - `providers::embed(&AppState, caller: &str, space: Option<&str>, connection_id: &str,
//!   inputs: Vec<String>) -> Result<Vec<Vec<f32>>, ProviderError>`, async.
//! - `providers::decide(&AppState, caller: &str, space: Option<&str>, connection_id: &str,
//!   state: Value, questions: Value) -> Result<Value, ProviderError>`, async; one System One
//!   request.
//!
//! `ProviderError: Display` with messages safe to show the caller.

use serde_json::Value;

use super::SearchError;
use super::engine::{ModelError, ModelFuture, Models, Routing};
use crate::AppState;
use crate::auth::Human;

/// The caller's embedding and System One connections.
pub async fn routing(state: &AppState, who: &Human) -> Result<Routing, SearchError> {
    let prefs = crate::model_routing::preferences(state, &who.id)
        .await
        .map_err(|e| SearchError::Internal(format!("reading model routing: {e:#}")))?;
    let routing = Routing {
        embedding: prefs.embedding_id.clone(),
        classifier: prefs.classifier_id.clone(),
    };
    if routing.embedding.is_none() {
        return Err(SearchError::NoEmbeddingModel);
    }
    Ok(routing)
}

pub struct ProviderModels<'a> {
    state: &'a AppState,
    caller: &'a str,
}

impl<'a> ProviderModels<'a> {
    pub fn new(state: &'a AppState, who: &'a Human) -> Self {
        Self {
            state,
            caller: &who.id,
        }
    }
}

fn safe(e: impl std::fmt::Display) -> ModelError {
    ModelError(e.to_string())
}

impl Models for ProviderModels<'_> {
    fn fingerprint<'a>(
        &'a self,
        dest: Option<&'a str>,
        connection: &'a str,
    ) -> ModelFuture<'a, String> {
        Box::pin(async move {
            crate::providers::model_fingerprint(self.state, self.caller, dest, connection)
                .await
                .map_err(safe)
        })
    }

    fn embed<'a>(
        &'a self,
        dest: Option<&'a str>,
        connection: &'a str,
        inputs: Vec<String>,
    ) -> ModelFuture<'a, Vec<Vec<f32>>> {
        Box::pin(async move {
            crate::providers::embed(self.state, self.caller, dest, connection, inputs)
                .await
                .map_err(safe)
        })
    }

    fn decide<'a>(
        &'a self,
        dest: Option<&'a str>,
        connection: &'a str,
        state: Value,
        questions: Value,
    ) -> ModelFuture<'a, Value> {
        Box::pin(async move {
            crate::providers::decide(self.state, self.caller, dest, connection, state, questions)
                .await
                .map_err(safe)
        })
    }
}
