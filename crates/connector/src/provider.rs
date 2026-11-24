//! Dispatch to the adapter a profile is pinned to.
//!
//! The adapter is chosen by the local profile's `kind`, never by anything in
//! a job. Vendor adapters share one interface, each taking the profile's
//! private `vendor/` directory:
//!
//! ```ignore
//! pub async fn login(profile: &Path) -> anyhow::Result<()>;
//! pub async fn logout(profile: &Path) -> anyhow::Result<()>;
//! pub async fn models(profile: &Path) -> anyhow::Result<Vec<String>>;
//! pub async fn complete(profile: &Path, request: serde_json::Value) -> anyhow::Result<serde_json::Value>;
//! ```
//!
//! `complete` receives a request already checked by `policy::sanitize_complete`
//! (approved model, text messages, function tools only) and returns an OpenAI
//! Chat Completions response, which `policy::sanitize_response` rebuilds
//! before it leaves the machine.
//!
//! The typed kinds (`device-systemone`, `device-embeddings`,
//! `device-embeddinggemma-2`) use the shared
//! clients of `metro-model-protocol` on the profile's own base URL and key:
//! `decide` and `embed` validate the request before anything is sent and
//! rebuild the response from its documented fields.

use std::time::Duration;

use anyhow::Result;
use serde_json::Value;

use crate::openai_compat::{self, Endpoint};
use crate::profile::{Kind, Profile, ProfileConfig};
use crate::{SafeError, claude_code, openai_oauth};

/// The shared client's errors carry fixed, safe text; pass it on to Metro.
fn typed_error(e: anyhow::Error) -> anyhow::Error {
    match e.downcast_ref::<metro_model_protocol::ClientError>() {
        Some(c) => SafeError::new(c.as_str()).into(),
        None => SafeError::new("the model request failed").into(),
    }
}

fn not_for_kind(what: &str) -> anyhow::Error {
    SafeError::new(format!(
        "unsupported operation: this connection does not serve {what}"
    ))
    .into()
}

pub struct Provider<'a> {
    pub profile: &'a Profile,
    pub config: &'a ProfileConfig,
}

impl Provider<'_> {
    fn endpoint(&self) -> Result<Endpoint> {
        let base_url = self
            .config
            .base_url
            .clone()
            .ok_or_else(|| anyhow::anyhow!("profile has no base_url"))?;
        Ok(Endpoint {
            // Only a key bound to exactly this base URL is ever used.
            key: self.profile.provider_key_for(&base_url)?,
            base_url,
            timeout: Duration::from_secs(self.config.request_timeout_secs),
        })
    }

    /// Interactive sign-in for vendor kinds. The OpenAI-compatible key is
    /// handled by the CLI (it is a prompt, not a vendor flow).
    pub async fn login(&self) -> Result<()> {
        let dir = self.profile.vendor_dir();
        match self.config.kind {
            Kind::OpenAiOAuth => openai_oauth::login(&dir).await,
            Kind::ClaudeCode => claude_code::login(&dir).await,
            Kind::OpenAiCompatible | Kind::SystemOne | Kind::Embeddings | Kind::MediaEmbeddings => {
                Ok(())
            }
        }
    }

    pub async fn logout(&self) -> Result<()> {
        let dir = self.profile.vendor_dir();
        match self.config.kind {
            Kind::OpenAiOAuth => openai_oauth::logout(&dir).await,
            Kind::ClaudeCode => claude_code::logout(&dir).await,
            Kind::OpenAiCompatible | Kind::SystemOne | Kind::Embeddings | Kind::MediaEmbeddings => {
                self.profile.forget_provider_key().map(|_| ())
            }
        }
    }

    /// Every model the provider offers (not filtered by the allowlist).
    pub async fn available_models(&self) -> Result<Vec<String>> {
        let dir = self.profile.vendor_dir();
        match self.config.kind {
            Kind::OpenAiOAuth => openai_oauth::models(&dir).await,
            Kind::ClaudeCode => claude_code::models(&dir).await,
            Kind::OpenAiCompatible => openai_compat::models(&self.endpoint()?).await,
            Kind::SystemOne => self.typed_models("system_one").await,
            Kind::Embeddings | Kind::MediaEmbeddings => self.typed_models("embeddings").await,
        }
    }

    /// As [`Provider::available_models`], with the catalog display name of
    /// each model when the provider reports one (ChatGPT plan usage only).
    pub async fn available_models_named(&self) -> Result<Vec<(String, Option<String>)>> {
        match self.config.kind {
            Kind::OpenAiOAuth => Ok(openai_oauth::model_catalog(
                &self.profile.vendor_dir(),
                &openai_oauth::Endpoints::openai(),
            )
            .await?
            .into_iter()
            .map(|m| (m.slug, m.display_name))
            .collect()),
            _ => Ok(self
                .available_models()
                .await?
                .into_iter()
                .map(|m| (m, None))
                .collect()),
        }
    }

    async fn typed_models(&self, kind: &str) -> Result<Vec<String>> {
        let ep = self.endpoint()?;
        let key = ep.key.as_ref().map(|k| k.expose());
        let fut = metro_model_protocol::list_models(&ep.base_url, key, kind);
        match tokio::time::timeout(ep.timeout.min(Duration::from_secs(60)), fut).await {
            Ok(r) => r.map_err(typed_error),
            Err(_) => Err(SafeError::new("the model endpoint did not answer in time").into()),
        }
    }

    /// One System One call. `request` must already have passed
    /// `policy::check_decide` (approved model); the shared client checks it
    /// again in full and validates the answers.
    pub async fn decide(&self, request: Value) -> Result<Value> {
        if self.config.kind != Kind::SystemOne {
            return Err(not_for_kind("System One decisions"));
        }
        let ep = self.endpoint()?;
        let key = ep.key.as_ref().map(|k| k.expose());
        metro_model_protocol::decide(&ep.base_url, key, request)
            .await
            .map_err(typed_error)
    }

    /// One embeddings call, like [`Provider::decide`].
    pub async fn embed(&self, request: Value) -> Result<Value> {
        if self.config.kind.capability() != crate::profile::Capability::Embeddings {
            return Err(not_for_kind("embeddings"));
        }
        let ep = self.endpoint()?;
        let key = ep.key.as_ref().map(|k| k.expose());
        metro_model_protocol::embed(&ep.base_url, key, request)
            .await
            .map_err(typed_error)
    }

    /// One media embeddings call (`media_embed`), only on the media kind.
    /// `request` must already name an approved model; the shared client
    /// checks it in full (bytes only, bounded) and validates the answer.
    pub async fn media_embed(&self, request: Value) -> Result<Value> {
        if !self.config.kind.embeds_media() {
            return Err(not_for_kind("documents or media"));
        }
        let ep = self.endpoint()?;
        let key = ep.key.as_ref().map(|k| k.expose());
        metro_model_protocol::media_embed(&ep.base_url, key, request)
            .await
            .map_err(typed_error)
    }

    /// Run a sanitized request. Callers must sanitize first (see `run.rs`).
    pub async fn complete(&self, sanitized: Value) -> Result<Value> {
        let dir = self.profile.vendor_dir();
        match self.config.kind {
            Kind::OpenAiOAuth => openai_oauth::complete(&dir, sanitized).await,
            Kind::ClaudeCode => claude_code::complete(&dir, sanitized).await,
            Kind::OpenAiCompatible => openai_compat::complete(&self.endpoint()?, sanitized).await,
            Kind::SystemOne | Kind::Embeddings | Kind::MediaEmbeddings => {
                Err(not_for_kind("chat completions"))
            }
        }
    }

    /// Secrets this profile knows about, for log redaction.
    pub fn known_secrets(&self) -> Vec<crate::Secret> {
        self.profile.secrets_for_redaction()
    }
}
