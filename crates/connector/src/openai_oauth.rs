//! OpenAI device connection: **Sign in with ChatGPT** (ChatGPT plan usage for
//! open-source, locally hosted apps), driven from the user's own machine.
//!
//! # Which flow this is
//!
//! This is the *direct* OpenAI flow, documented at
//! <https://developers.openai.com/siwc/token-sharing-open-source>. It uses no
//! Codex app-server and no local Codex sign-in. It never reads `~/.codex` or
//! any other app's credentials, and it never runs a model CLI.
//!
//! 1. **Host ID.** The profile directory gets a stable, opaque
//!    `ext_agent_host_id` (`urn:uuid:<v4>`) before its first sign-in. Sign-out
//!    keeps it.
//! 2. **Dynamic registration.** On first sign-in the browser opens
//!    `authorization_endpoint` from OpenAI's OIDC discovery document, with
//!    `client_id=dynamic_agent_client`, `agent_name_hint=Metro`, the host ID,
//!    PKCE S256 and a fresh `state` and `nonce`. The redirect URI is a loopback
//!    callback on `http://127.0.0.1:<port>/auth/callback`. The callback returns
//!    the *issued* `client_id`, which the profile keeps. Later sign-ins reuse
//!    it, with `id_token_hint` and `login_hint`, and never send
//!    `dynamic_agent_client` again.
//! 3. **Code exchange.** This is a public client: there's no client secret
//!    and no partner key. The ID token is checked against the issuer's JWKS
//!    (signature, `iss`, `aud` = issued client ID, `exp`, `nonce`, `sub`).
//!    Inference only goes ahead when the granted scopes include
//!    `chatgpt.tokens.use.direct`.
//! 4. **Inference.** Requests go to `POST {api}/responses` with
//!    `store:false, stream:true`. The SSE stream is read within fixed limits
//!    and only `response.completed` counts as success. Models come from
//!    `GET {api}/models` (`visibility == "list"`).
//! 5. **Refresh and sign-out.** Refreshes are serialized across processes by
//!    a lock file in the profile, and the rotating refresh token is replaced
//!    together with the access token. Sign-out revokes the refresh token at
//!    the discovered `revocation_endpoint`, then clears the local tokens. It
//!    keeps the client and host mapping.
//!
//! # Eligibility
//!
//! OpenAI documents this flow for **open-source and locally hosted apps**. It
//! needs no client-ID request. It does **not** cover paid or remotely hosted
//! apps; those go through OpenAI's interest form. Metro doesn't claim any
//! approval of that kind. The connector runs on the user's device, and the
//! loopback callback only works when the browser runs on the same machine as
//! this process. [`eligibility`] reports this so a UI can show it.
//!
//! # Storage
//!
//! The `profile` argument is one dedicated directory per user connection. It
//! must be mode `0700`. If it's missing, it's created with that mode. It holds
//! `host.json`, `account.json` and `tokens.json`, each written atomically with
//! mode `0600`, plus a `.lock` file. Tokens never appear in argv, URLs (except
//! `id_token_hint`, which only goes to the authorization endpoint, through a
//! local one-time redirect), logs or error messages.
//!
//! # Request translation
//!
//! [`complete`] takes an OpenAI Chat Completions request body and returns a
//! Chat Completions response body. Function and custom tools keep their exact
//! names. The Responses preview requires tools to be grouped in a namespace,
//! so they're sent inside one namespace called [`TOOL_NAMESPACE`], and the
//! namespace is stripped from calls on the way back. Tool call IDs pass
//! through unchanged across rounds. Unsupported fields such as `temperature`,
//! `max_tokens`, `n`, `stop`, audio or `stream:true` fail with an error rather
//! than being dropped. Nothing ever falls back to another billing path.
//!
//! # Errors reported to Metro
//!
//! Errors from [`models`] and [`complete`] reach Metro only as one of five
//! fixed texts: sign in again, plan usage not available, usage limit,
//! unsupported request, or OpenAI unavailable. They're chosen from the HTTP
//! status, the documented error code, translation, or the transport, never
//! from response text. Everything else is the connector's generic message.
//! OpenAI's messages, request IDs and request contents stay in the local
//! redacted log.

mod flow;
mod http;
mod loopback;
mod oidc;
mod sse;
mod store;
mod translate;

use std::path::Path;
use std::time::Duration;

use serde::Serialize;

pub use flow::LogoutOutcome;

/// `agent_name_hint` sent on first registration. It's also the display name in
/// ChatGPT settings, and the user can edit it there.
pub const AGENT_NAME: &str = "Metro";
/// The scope that allows ChatGPT plan usage.
pub const PLAN_SCOPE: &str = "chatgpt.tokens.use.direct";
/// The full scope set that is requested.
pub const SCOPES: &str =
    "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
/// Where users review and limit this app's ChatGPT plan usage.
pub const USAGE_URL: &str = "https://chatgpt.com/settings/usage";
/// The Responses tool namespace that wraps Metro's function/custom tools.
pub const TOOL_NAMESPACE: &str = "metro";
/// First-registration client ID. It is never saved and never used for token
/// exchange.
pub const DYNAMIC_CLIENT_ID: &str = "dynamic_agent_client";
/// Callback port tried first. If it's busy, an ephemeral port is used; only
/// the port of the loopback redirect may vary.
pub const DEFAULT_CALLBACK_PORT: u16 = 1455;

/// Issuer, API base and resource indicator. Production uses
/// [`Endpoints::openai`]. Other values are only accepted on loopback HTTP, for
/// inert test fixtures. A signed-in profile is bound to the endpoints it signed
/// in with, and refuses others.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct Endpoints {
    /// Exact OIDC issuer. Discovery is `{issuer}/.well-known/openid-configuration`.
    pub issuer: String,
    /// Responses / models base, without a trailing slash.
    pub api_base: String,
    /// `resource` sent on authorize, code exchange and refresh. It is also the
    /// expected access-token audience.
    pub resource: String,
}

impl Endpoints {
    pub fn openai() -> Self {
        Self {
            issuer: "https://auth.openai.com".into(),
            api_base: "https://api.openai.com/v1".into(),
            resource: "https://api.openai.com/v1".into(),
        }
    }
}

/// Receives the local start URL and shows it to the user, normally by
/// opening the system browser.
/// Where a sign-in stopped. Carried in the error chain next to a
/// [`crate::SafeError`] that says, in words a person can act on, what failed.
/// The steps before `Waiting` happen before any browser is opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SignInStage {
    /// The private credential store on this device.
    Storage,
    /// Reaching the issuer's sign-in service (OIDC discovery).
    Discovery,
    /// Listening on 127.0.0.1 for the browser to come back.
    Callback,
    /// Waiting for the person to finish in the browser.
    Waiting,
    /// The browser came back with an error or without a code.
    Response,
    /// Exchanging the code and checking the tokens.
    Exchange,
}

impl SignInStage {
    pub fn as_str(self) -> &'static str {
        match self {
            SignInStage::Storage => "storage",
            SignInStage::Discovery => "discovery",
            SignInStage::Callback => "callback",
            SignInStage::Waiting => "waiting",
            SignInStage::Response => "response",
            SignInStage::Exchange => "exchange",
        }
    }

    /// True when the failure came before the browser was needed.
    pub fn before_browser(self) -> bool {
        matches!(
            self,
            SignInStage::Storage | SignInStage::Discovery | SignInStage::Callback
        )
    }
}

impl std::fmt::Display for SignInStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Sign in with ChatGPT ({})", self.as_str())
    }
}

/// The step a failed sign-in stopped at, if it was one of ours.
pub fn sign_in_stage(e: &anyhow::Error) -> Option<SignInStage> {
    // Contexts are found by `downcast_ref` on the error, through every layer.
    e.downcast_ref::<SignInStage>().copied()
}

pub type BrowserOpener = Box<dyn FnOnce(&str) -> anyhow::Result<()> + Send>;

/// How [`login_with`] runs the browser half of the sign-in.
pub struct LoginOptions {
    pub endpoints: Endpoints,
    /// Receives the local one-time start URL
    /// (`http://127.0.0.1:<port>/auth/start?k=…`). That URL redirects to
    /// OpenAI. It contains no tokens. The default prints it to stderr and
    /// opens the system browser.
    pub open_browser: BrowserOpener,
    pub preferred_port: u16,
    /// How long to wait for the browser callback.
    pub timeout: Duration,
}

impl Default for LoginOptions {
    fn default() -> Self {
        Self {
            endpoints: Endpoints::openai(),
            open_browser: Box::new(flow::open_system_browser),
            preferred_port: DEFAULT_CALLBACK_PORT,
            timeout: Duration::from_secs(10 * 60),
        }
    }
}

/// What a settings UI may show. It never includes token material.
#[derive(Clone, Debug, Serialize)]
pub struct Status {
    /// Tokens are present locally. They may still be refreshable after expiry.
    pub signed_in: bool,
    /// A client ID was issued for this profile, so later sign-ins reuse it.
    pub registered: bool,
    /// The granted scopes include [`PLAN_SCOPE`]. Without it, nothing is sent
    /// to inference.
    pub plan_usage_enabled: bool,
    pub email: Option<String>,
    /// The validated ID-token `sub`, the stable account identity.
    pub subject: Option<String>,
    /// Unix seconds.
    pub access_expires_at: Option<u64>,
    pub ext_agent_host_id: Option<String>,
    pub manage_usage_url: &'static str,
}

/// What this connector is documented for, so a UI can show it as it is.
#[derive(Clone, Debug, Serialize)]
pub struct Eligibility {
    pub flow: &'static str,
    pub eligible: &'static str,
    pub not_covered: &'static str,
    pub requires: &'static str,
    pub docs: &'static [&'static str],
}

pub fn eligibility() -> Eligibility {
    Eligibility {
        flow: "Sign in with ChatGPT: ChatGPT plan usage for open-source apps (direct OAuth with \
               dynamic client registration, no client secret, Responses API with store:false and \
               stream:true). OpenAI labels this a preview.",
        eligible: "An open-source app running locally on the signed-in user's own device. Each \
                   user registers their own client through dynamic registration.",
        not_covered: "Paid or remotely hosted apps, and any shared hosted approval. These need \
                      OpenAI's interest form and are not claimed here.",
        requires: "A browser on the same machine as the connector (loopback callback on \
                   127.0.0.1), a ChatGPT account whose plan and workspace policy allow app usage, \
                   and consent to the chatgpt.tokens.use.direct scope.",
        docs: &[
            "https://developers.openai.com/siwc/token-sharing-open-source",
            "https://developers.openai.com/siwc/token-sharing-open-source/sign-in",
            "https://developers.openai.com/siwc/token-sharing-open-source/profiles-and-sessions",
            "https://developers.openai.com/siwc/token-sharing-open-source/models-and-inference",
            "https://developers.openai.com/siwc/token-sharing-open-source/errors-and-recovery",
            "https://developers.openai.com/siwc/token-sharing-open-source/preview-limitations",
        ],
    }
}

/// Signs in with ChatGPT in the system browser. On first use this registers
/// a client; later it reauthorizes the saved one.
///
/// Returns an error when sign-in succeeds without ChatGPT plan permission. The
/// sign-in is kept, and calling `login` again asks for consent.
pub async fn login(profile: &Path) -> anyhow::Result<()> {
    login_with(profile, LoginOptions::default()).await
}

pub async fn login_with(profile: &Path, options: LoginOptions) -> anyhow::Result<()> {
    flow::login(profile, options).await
}

/// Revokes the renewable session, then clears the local tokens. Returns an
/// error if remote revocation couldn't be confirmed. The local tokens are
/// cleared either way, and the error says so. The user can also disconnect
/// the app in ChatGPT settings.
pub async fn logout(profile: &Path) -> anyhow::Result<()> {
    let outcome = logout_outcome(profile, &Endpoints::openai()).await?;
    if outcome.signed_out && !outcome.remote_revoked {
        anyhow::bail!(
            "signed out locally, but OpenAI did not confirm revocation of the session; \
             disconnect Metro in ChatGPT Settings to be sure"
        );
    }
    Ok(())
}

pub async fn logout_outcome(
    profile: &Path,
    endpoints: &Endpoints,
) -> anyhow::Result<LogoutOutcome> {
    flow::logout(profile, endpoints).await
}

/// The model slugs that the signed-in account's catalog marks for display,
/// in server order.
pub async fn models(profile: &Path) -> anyhow::Result<Vec<String>> {
    models_with(profile, &Endpoints::openai()).await
}

pub async fn models_with(profile: &Path, endpoints: &Endpoints) -> anyhow::Result<Vec<String>> {
    Ok(model_catalog(profile, endpoints)
        .await?
        .into_iter()
        .map(|m| m.slug)
        .collect())
}

/// A listed model: `slug` goes in `model`, and `display_name` is for the UI.
#[derive(Clone, Debug, Serialize)]
pub struct ModelInfo {
    pub slug: String,
    pub display_name: Option<String>,
}

pub async fn model_catalog(
    profile: &Path,
    endpoints: &Endpoints,
) -> anyhow::Result<Vec<ModelInfo>> {
    flow::model_catalog(profile, endpoints)
        .await
        .map_err(for_metro)
}

/// Runs one Chat Completions request through the Responses API on the user's
/// ChatGPT plan and returns a `chat.completion` object.
pub async fn complete(
    profile: &Path,
    request: serde_json::Value,
) -> anyhow::Result<serde_json::Value> {
    complete_with(profile, &Endpoints::openai(), request).await
}

pub async fn complete_with(
    profile: &Path,
    endpoints: &Endpoints,
    request: serde_json::Value,
) -> anyhow::Result<serde_json::Value> {
    flow::complete(profile, endpoints, request)
        .await
        .map_err(for_metro)
}

/// Errors returned from [`models`] and [`complete`], which the connector
/// reports to Metro. If the error has a category from the fixed list in
/// `http::Category`, Metro gets only that category's fixed text, as a
/// [`crate::SafeError`]. Everything else stays without a `SafeError`, so
/// Metro shows the connector's generic message. The detailed chain is kept
/// as context for the device's local, redacted log. Its text may include
/// OpenAI's error message or request ID, which never leave the device.
fn for_metro(e: anyhow::Error) -> anyhow::Error {
    match e.downcast_ref::<http::Category>().copied() {
        Some(c) => {
            anyhow::Error::new(crate::SafeError::new(c.user_text())).context(format!("{e:#}"))
        }
        None => e,
    }
}

/// The local sign-in state. It reads no tokens out and makes no network calls.
pub async fn status(profile: &Path) -> anyhow::Result<Status> {
    flow::status(profile).await
}

/// Translates a Chat Completions body to a Responses body without sending
/// it. Use it for previews and tests.
pub fn chat_to_responses(request: &serde_json::Value) -> anyhow::Result<serde_json::Value> {
    translate::to_responses(request)
}

/// Normalizes a completed Responses `response` object to a `chat.completion`.
pub fn responses_to_chat(response: &serde_json::Value) -> anyhow::Result<serde_json::Value> {
    translate::to_chat_completion(response)
}
