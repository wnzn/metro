//! HTTP plumbing: endpoint policy, bounded bodies and safe error categories.

use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use serde_json::Value;
use url::Url;

use super::Endpoints;
use super::diagnostic::{Detail, Phase, mark};

pub(crate) const MAX_JSON: usize = 4 << 20;

/// The only reasons that can reach Metro. Each one is fixed text written
/// here, never anything from a server response, request or `anyhow` chain.
/// Attached as context where the cause is known, then turned into a
/// `SafeError` at the public boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Category {
    SignIn,
    Registration,
    Route,
    Storage,
    Refresh,
    Consent,
    PlanNotAvailable,
    UsageLimit,
    Unsupported,
    Model,
    ReasoningEffort,
    ContextLength,
    ContentFilter,
    Incomplete,
    Interrupted,
    UnexpectedResponse,
    Rejected,
    Unavailable,
    /// Inference answered 503 with no documented code: the service is down,
    /// or direct plan routing isn't enabled. The status alone can't tell.
    ServiceOrRouting,
    /// Not a category of its own: the failure carried its own fixed
    /// `SafeError` text. Used only for the local diagnostic token.
    Other,
}

impl Category {
    pub(crate) const ALL: [Category; 20] = [
        Category::SignIn,
        Category::Registration,
        Category::Route,
        Category::Storage,
        Category::Refresh,
        Category::Consent,
        Category::PlanNotAvailable,
        Category::UsageLimit,
        Category::Unsupported,
        Category::Model,
        Category::ReasoningEffort,
        Category::ContextLength,
        Category::ContentFilter,
        Category::Incomplete,
        Category::Interrupted,
        Category::UnexpectedResponse,
        Category::Rejected,
        Category::Unavailable,
        Category::ServiceOrRouting,
        Category::Other,
    ];

    /// At most 200 characters (`MAX_SAFE_ERROR`).
    pub(crate) fn user_text(self) -> &'static str {
        match self {
            Category::SignIn => {
                "The ChatGPT sign-in for this connection ended or was not accepted. Sign in again \
                 on the device."
            }
            Category::Registration => {
                "OpenAI didn't accept this connection's ChatGPT registration. Add a new ChatGPT connection and sign in on this device."
            }
            Category::Route => {
                "OpenAI didn't accept Metro's ChatGPT plan usage route. Update Metro or use another model connection."
            }
            Category::Storage => {
                "Metro couldn't read or update this device's ChatGPT sign-in. Retry; if it keeps failing, reconnect ChatGPT on this device."
            }
            Category::Refresh => {
                "Metro couldn't renew the ChatGPT session. Retry; if it keeps failing, sign in again on this device."
            }
            Category::Consent => {
                "ChatGPT plan usage isn't enabled for this connection. Sign in again on this device and approve plan usage."
            }
            Category::PlanNotAvailable => {
                "ChatGPT plan usage isn't available for this connection (not approved, or not \
                 allowed for the account, workspace or region). Check your ChatGPT account and workspace settings."
            }
            Category::UsageLimit => {
                "ChatGPT plan usage limit reached. Review it in ChatGPT Settings under Usage. \
                 Metro doesn't switch to another billing path."
            }
            Category::Unsupported => {
                "The request uses an option, input, tool or model that ChatGPT plan usage doesn't \
                 support. Remove unsupported options or tools, or select another ChatGPT model."
            }
            Category::Model => {
                "This model isn't available through the signed-in ChatGPT account. Refresh the model list and select an available model."
            }
            Category::ReasoningEffort => {
                "The ChatGPT model rejected the reasoning effort setting. Choose a supported effort for this model or use the model default."
            }
            Category::ContextLength => {
                "This request exceeds the ChatGPT model's context limit. Start a shorter chat or choose a model with more context."
            }
            Category::ContentFilter => {
                "OpenAI's content filter stopped the response. Revise the request before trying again."
            }
            Category::Incomplete => {
                "OpenAI stopped before completing the response. Try a shorter request or ask for a shorter answer."
            }
            Category::Interrupted => {
                "The OpenAI response stream ended early or was interrupted. Check this device's connection and try again."
            }
            Category::UnexpectedResponse => {
                "OpenAI returned an unexpected response. Try again; if it keeps failing, update Metro or select another ChatGPT model."
            }
            Category::Rejected => {
                "OpenAI rejected the ChatGPT request. Try a new chat with default model settings; if it keeps failing, update Metro."
            }
            Category::Unavailable => {
                "OpenAI couldn't be reached from the device or is temporarily unavailable. Try \
                 again later."
            }
            Category::ServiceOrRouting => {
                "OpenAI's ChatGPT plan service is unavailable, or direct plan routing isn't enabled \
                 for this connection. Try again later; if it continues, check ChatGPT plan usage."
            }
            Category::Other => {
                "The ChatGPT request failed on this device. Try again; if it keeps failing, update Metro."
            }
        }
    }

    /// A stable token for device-local diagnostics.
    pub(crate) fn code(self) -> &'static str {
        match self {
            Category::SignIn => "chatgpt_sign_in",
            Category::Registration => "chatgpt_registration",
            Category::Route => "chatgpt_route",
            Category::Storage => "chatgpt_storage",
            Category::Refresh => "chatgpt_refresh",
            Category::Consent => "chatgpt_consent",
            Category::PlanNotAvailable => "chatgpt_plan_not_available",
            Category::UsageLimit => "chatgpt_usage_limit",
            Category::Unsupported => "chatgpt_unsupported",
            Category::Model => "chatgpt_model",
            Category::ReasoningEffort => "chatgpt_reasoning_effort",
            Category::ContextLength => "chatgpt_context_length",
            Category::ContentFilter => "chatgpt_content_filter",
            Category::Incomplete => "chatgpt_incomplete",
            Category::Interrupted => "chatgpt_interrupted",
            Category::UnexpectedResponse => "chatgpt_unexpected_response",
            Category::Rejected => "chatgpt_rejected",
            Category::Unavailable => "chatgpt_unavailable",
            Category::ServiceOrRouting => "chatgpt_service_or_routing_unavailable",
            Category::Other => "chatgpt_failed",
        }
    }
}

impl std::fmt::Display for Category {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.user_text())
    }
}

/// Attaches `category`, if any, as context.
pub(crate) fn tagged(e: anyhow::Error, category: Option<Category>) -> anyhow::Error {
    match category {
        Some(c) => e.context(c),
        None => e,
    }
}

/// Category for a documented code or a status.
pub(crate) fn categorize(
    status: Option<u16>,
    code: Option<&str>,
    param: Option<&str>,
) -> Option<Category> {
    use Category::*;
    let by_code = match code {
        Some("subscription_sharing_usage_limit_exceeded") => Some(UsageLimit),
        Some("subscription_sharing_user_not_eligible")
        | Some("chatpass_v2_scope_not_authorized")
        | Some("chatpass_v2_invalid_authorization_context") => Some(PlanNotAvailable),
        Some("subscription_sharing_usage_unavailable")
        | Some("subscription_sharing_user_unavailable") => Some(Unavailable),
        Some("subscription_sharing_route_not_supported") => Some(Route),
        Some("subscription_sharing_invalid_user" | "invalid_api_key") => Some(SignIn),
        Some("model_not_found" | "model_not_available") => Some(Model),
        Some("context_length_exceeded") => Some(ContextLength),
        Some("content_filter") => Some(ContentFilter),
        Some("insufficient_quota" | "rate_limit_exceeded") => Some(UsageLimit),
        Some("server_error" | "temporarily_unavailable" | "overloaded") => Some(Unavailable),
        Some("invalid_client") => Some(Registration),
        Some(
            "invalid_request_error"
            | "invalid_value"
            | "unsupported_value"
            | "unsupported_parameter"
            | "subscription_sharing_unsupported_capability",
        )
        | None
            if matches!(status, None | Some(400 | 422))
                && matches!(param, Some("reasoning" | "reasoning.effort")) =>
        {
            Some(ReasoningEffort)
        }
        Some(
            "subscription_sharing_unsupported_capability"
            | "unsupported_parameter"
            | "unsupported_value",
        ) => Some(Unsupported),
        Some("invalid_request_error" | "invalid_value") => Some(Rejected),
        _ => None,
    };
    by_code.or(match status {
        Some(401) => Some(SignIn),
        Some(403) => Some(PlanNotAvailable),
        Some(400 | 413 | 422) => Some(Rejected),
        Some(404 | 405) => Some(Route),
        Some(408) => Some(Interrupted),
        Some(429) => Some(UsageLimit),
        Some(500..=599) => Some(Unavailable),
        _ => None,
    })
}

/// Production endpoints must be HTTPS. Plain HTTP is accepted only for
/// literal loopback hosts, which is what the inert test fixtures use.
pub(crate) fn check_url(raw: &str) -> Result<Url> {
    let url = Url::parse(raw).map_err(|_| anyhow!("invalid endpoint URL"))?;
    if url.username() != "" || url.password().is_some() || url.fragment().is_some() {
        bail!("endpoint URL must not carry credentials or a fragment");
    }
    match url.scheme() {
        "https" => {}
        "http" if is_loopback(&url) => {}
        _ => bail!("endpoint {} must use https", url.host_str().unwrap_or("?")),
    }
    Ok(url)
}

pub(crate) fn is_loopback(url: &Url) -> bool {
    matches!(url.host(), Some(url::Host::Ipv4(ip)) if ip.is_loopback())
        || matches!(url.host(), Some(url::Host::Ipv6(ip)) if ip.is_loopback())
}

pub(crate) fn check_endpoints(e: &Endpoints) -> Result<()> {
    check_url(&e.issuer)?;
    check_url(&e.api_base)?;
    if e.api_base.ends_with('/') {
        bail!("api_base must not end with '/'");
    }
    Url::parse(&e.resource).map_err(|_| anyhow!("invalid resource indicator"))?;
    Ok(())
}

pub(crate) fn client(allow_loopback_http: bool) -> Result<reqwest::Client> {
    reqwest::Client::builder()
        // Credentials are never replayed to a redirect target.
        .redirect(reqwest::redirect::Policy::none())
        .https_only(!allow_loopback_http)
        .connect_timeout(Duration::from_secs(15))
        .user_agent(concat!("metro-connector/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| anyhow!("building HTTP client: {e}"))
}

pub(crate) fn client_for(e: &Endpoints) -> Result<reqwest::Client> {
    let loopback = [&e.issuer, &e.api_base]
        .iter()
        .any(|u| Url::parse(u).is_ok_and(|u| u.scheme() == "http" && is_loopback(&u)));
    client(loopback)
}

/// Reads at most `max` bytes, and fails if the body is larger.
pub(crate) async fn read_bounded(mut resp: reqwest::Response, max: usize) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(net)? {
        if out.len() + chunk.len() > max {
            bail!("response body exceeded {max} bytes");
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

/// Describes a transport error without the URL. Our URLs carry no tokens,
/// but keeping it out is cheap.
pub(crate) fn net(e: reqwest::Error) -> anyhow::Error {
    let kind = if e.is_timeout() {
        "timed out"
    } else if e.is_connect() {
        "connection failed"
    } else if e.is_body() || e.is_decode() {
        "interrupted while reading the body"
    } else {
        "request failed"
    };
    mark(
        tagged(
            anyhow!("network error: {kind}"),
            Some(Category::Unavailable),
        ),
        Detail::phase(Phase::Transport),
    )
}

/// Parsed failure body. It may be the Responses `{error:{code,param,message}}`,
/// OAuth `{error, error_description}`, or direct-admission `{detail}`. Only
/// the exact machine-readable fields are used, never interpolated into errors.
#[derive(Default)]
pub(crate) struct Failure {
    pub status: u16,
    pub code: Option<String>,
    param: Option<String>,
}

impl Failure {
    pub fn parse(status: u16, body: &[u8]) -> Self {
        let mut f = Failure {
            status,
            ..Default::default()
        };
        let Ok(v) = serde_json::from_slice::<Value>(body) else {
            return f;
        };
        match v.get("error") {
            Some(Value::Object(e)) => {
                f.code = error_code(&v["error"]).map(str::to_owned);
                f.param = e.get("param").and_then(Value::as_str).map(str::to_owned);
            }
            Some(Value::String(code)) => f.code = Some(code.clone()),
            _ => {}
        }
        f
    }

    pub fn error(&self, what: &str) -> anyhow::Error {
        mark(
            tagged(anyhow!(self.describe(what)), self.category()),
            self.detail(Phase::Http),
        )
    }

    /// The allowlisted facts of this failure.
    pub fn detail(&self, phase: Phase) -> Detail {
        Detail::http(
            phase,
            self.status,
            self.code.as_deref(),
            self.param.as_deref(),
        )
    }

    /// The category from a documented code alone, ignoring the status.
    pub fn code_category(&self) -> Option<Category> {
        self.code.as_deref()?;
        categorize(None, self.code.as_deref(), self.param.as_deref())
    }

    pub fn category(&self) -> Option<Category> {
        categorize(
            Some(self.status),
            self.code.as_deref(),
            self.param.as_deref(),
        )
    }

    pub fn describe(&self, what: &str) -> String {
        format!("{what} failed: HTTP {}", self.status)
    }
}

pub(crate) fn error_code(error: &Value) -> Option<&str> {
    error.get("code").and_then(Value::as_str).or_else(|| {
        error
            .get("type")
            .and_then(Value::as_str)
            .filter(|t| *t != "error")
    })
}

/// Percent-encodes per RFC 3986 unreserved characters (`%20` for space).
pub(crate) fn pct(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out += &format!("%{b:02X}");
        }
    }
    out
}

pub(crate) fn form(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", pct(k), pct(v)))
        .collect::<Vec<_>>()
        .join("&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn category_texts_fit_a_safe_error() {
        use Category::*;
        for c in [
            SignIn,
            Registration,
            Route,
            Storage,
            Refresh,
            Consent,
            PlanNotAvailable,
            UsageLimit,
            Unsupported,
            Model,
            ReasoningEffort,
            ContextLength,
            ContentFilter,
            Incomplete,
            Interrupted,
            UnexpectedResponse,
            Rejected,
            Unavailable,
            ServiceOrRouting,
            Other,
        ] {
            let t = c.user_text();
            assert!(t.chars().count() <= 200, "{c}: {}", t.chars().count());
            assert!(!t.contains('\n'));
        }
    }

    #[test]
    fn categories_come_from_codes_and_statuses_only() {
        assert_eq!(
            categorize(
                Some(429),
                Some("subscription_sharing_usage_limit_exceeded"),
                None
            ),
            Some(Category::UsageLimit)
        );
        assert_eq!(
            categorize(None, Some("subscription_sharing_invalid_user"), None),
            Some(Category::SignIn)
        );
        assert_eq!(categorize(Some(401), None, None), Some(Category::SignIn));
        assert_eq!(
            categorize(Some(502), Some("whatever"), None),
            Some(Category::Unavailable)
        );
        assert_eq!(
            categorize(Some(400), Some("whatever"), None),
            Some(Category::Rejected)
        );
        assert_eq!(categorize(None, None, None), None);
    }
}
