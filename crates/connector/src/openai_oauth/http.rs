//! HTTP plumbing: endpoint policy, bounded bodies, and error messages that
//! keep status, code, param and request ID without leaking credentials.

use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use serde_json::Value;
use url::Url;

use super::Endpoints;

pub(crate) const MAX_JSON: usize = 4 << 20;

/// The only reasons that can reach Metro. Each one is fixed text written
/// here, never anything from a server response, request or `anyhow` chain.
/// It's attached as context where the cause is known (HTTP status and
/// error code, stream event code, translation, transport) and turned into
/// a `SafeError` at the public boundary. Errors without a category reach
/// Metro only as the connector's generic message, and their details stay in
/// the local log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Category {
    SignIn,
    PlanNotAvailable,
    UsageLimit,
    Unsupported,
    Unavailable,
}

impl Category {
    /// At most 200 characters (`MAX_SAFE_ERROR`).
    pub(crate) fn user_text(self) -> &'static str {
        match self {
            Category::SignIn => {
                "The ChatGPT sign-in for this connection ended or was not accepted. Sign in again \
                 on the device (metro-connector login)."
            }
            Category::PlanNotAvailable => {
                "ChatGPT plan usage isn't available for this connection (not approved, or not \
                 allowed for the account, workspace or region). Check on the device."
            }
            Category::UsageLimit => {
                "ChatGPT plan usage limit reached. Review it at https://chatgpt.com/settings/usage. \
                 Metro doesn't switch to another billing path."
            }
            Category::Unsupported => {
                "The request uses an option, input, tool or model that ChatGPT plan usage doesn't \
                 support, so it wasn't completed. See the connector log on the device."
            }
            Category::Unavailable => {
                "OpenAI couldn't be reached from the device or is temporarily unavailable. Try \
                 again later."
            }
        }
    }
}

impl std::fmt::Display for Category {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Category::SignIn => "sign-in required",
            Category::PlanNotAvailable => "plan usage not available",
            Category::UsageLimit => "usage limit",
            Category::Unsupported => "unsupported request",
            Category::Unavailable => "unavailable",
        };
        write!(f, "[{s}]")
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
pub(crate) fn categorize(status: Option<u16>, code: Option<&str>) -> Option<Category> {
    use Category::*;
    let by_code = match code {
        Some("subscription_sharing_usage_limit_exceeded") => Some(UsageLimit),
        Some("subscription_sharing_user_not_eligible")
        | Some("chatpass_v2_scope_not_authorized")
        | Some("chatpass_v2_invalid_authorization_context") => Some(PlanNotAvailable),
        Some("subscription_sharing_usage_unavailable")
        | Some("subscription_sharing_user_unavailable") => Some(Unavailable),
        Some("subscription_sharing_unsupported_capability")
        | Some("subscription_sharing_route_not_supported") => Some(Unsupported),
        Some("subscription_sharing_invalid_user") => Some(SignIn),
        _ => None,
    };
    by_code.or(match status {
        Some(401) => Some(SignIn),
        Some(403) => Some(PlanNotAvailable),
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
    tagged(
        anyhow!("network error: {kind}"),
        Some(Category::Unavailable),
    )
}

pub(crate) fn request_id(h: &reqwest::header::HeaderMap) -> Option<String> {
    ["x-request-id", "openai-request-id"]
        .iter()
        .find_map(|k| h.get(*k))
        .and_then(|v| v.to_str().ok())
        .map(|s| clean(s, 128))
}

/// Parsed failure body. It may be the Responses `{error:{code,param,message}}`,
/// OAuth `{error, error_description}`, or direct-admission `{detail}`.
#[derive(Debug, Default, Clone)]
pub(crate) struct Failure {
    pub status: u16,
    pub code: Option<String>,
    pub param: Option<String>,
    pub message: Option<String>,
    pub request_id: Option<String>,
}

impl Failure {
    pub fn parse(status: u16, request_id: Option<String>, body: &[u8]) -> Self {
        let mut f = Failure {
            status,
            request_id,
            ..Default::default()
        };
        let Ok(v) = serde_json::from_slice::<Value>(body) else {
            return f;
        };
        match v.get("error") {
            Some(Value::Object(e)) => {
                f.code = e.get("code").and_then(Value::as_str).map(|s| clean(s, 96));
                f.param = e
                    .get("param")
                    .and_then(Value::as_str)
                    .map(|s| clean(s, 128));
                f.message = e
                    .get("message")
                    .and_then(Value::as_str)
                    .map(|s| clean(s, 300));
                if f.code.is_none() {
                    f.code = e.get("type").and_then(Value::as_str).map(|s| clean(s, 96));
                }
            }
            Some(Value::String(code)) => {
                f.code = Some(clean(code, 96));
                f.message = v
                    .get("error_description")
                    .and_then(Value::as_str)
                    .map(|s| clean(s, 300));
            }
            _ => {}
        }
        if f.message.is_none() {
            f.message = v
                .get("detail")
                .and_then(Value::as_str)
                .map(|s| clean(s, 300));
        }
        f
    }

    /// The detailed local error, tagged with its category if it has one.
    pub fn error(&self, what: &str) -> anyhow::Error {
        tagged(anyhow!(self.describe(what)), self.category())
    }

    pub fn category(&self) -> Option<Category> {
        let status = (self.status != 200).then_some(self.status);
        categorize(status, self.code.as_deref())
    }

    pub fn describe(&self, what: &str) -> String {
        let mut s = format!("{what} failed: HTTP {}", self.status);
        if let Some(c) = &self.code {
            s += &format!(", code {c}");
        }
        if let Some(p) = &self.param {
            s += &format!(", param {p}");
        }
        if let Some(m) = &self.message {
            s += &format!(": {m}");
        }
        if let Some(id) = &self.request_id {
            s += &format!(" (request id {id})");
        }
        if let Some(hint) = hint(self.code.as_deref(), self.status) {
            s += &format!(". {hint}");
        }
        s
    }
}

/// Recovery hints from the documented error table.
fn hint(code: Option<&str>, status: u16) -> Option<&'static str> {
    Some(match code {
        Some("subscription_sharing_usage_limit_exceeded") => {
            "ChatGPT plan usage limit reached for this app or plan; review it at \
             https://chatgpt.com/settings/usage. Metro does not switch to another billing path"
        }
        Some("subscription_sharing_user_not_eligible") => {
            "ChatGPT plan usage is not available for this account, workspace or policy"
        }
        Some("subscription_sharing_usage_unavailable")
        | Some("subscription_sharing_user_unavailable") => {
            "Temporarily unavailable; try again later"
        }
        Some("subscription_sharing_unsupported_capability") => {
            "Remove the unsupported input, tool, model or option named by param"
        }
        Some("subscription_sharing_route_not_supported") => {
            "This route isn't supported for ChatGPT plan usage"
        }
        Some("subscription_sharing_invalid_user") => {
            "The ChatGPT session could not be validated; sign in again"
        }
        Some("chatpass_v2_scope_not_authorized")
        | Some("chatpass_v2_invalid_authorization_context") => {
            "The granted permission does not cover this request; sign in again and approve ChatGPT plan usage"
        }
        _ => match status {
            401 => {
                "The signed-in identity or ChatGPT plan permission was not accepted; check the account and sign in again"
            }
            403 => "A policy or permission check, such as the serving region, refused the request",
            503 => "Direct routing is unavailable right now; try again later",
            _ => return None,
        },
    })
}

/// Bounds the length of server-supplied text and strips control characters
/// before it goes into an error message.
pub(crate) fn clean(s: &str, max: usize) -> String {
    let mut out: String = s.chars().filter(|c| !c.is_control()).take(max).collect();
    if s.chars().count() > max {
        out.push('…');
    }
    out
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
            PlanNotAvailable,
            UsageLimit,
            Unsupported,
            Unavailable,
        ] {
            let t = c.user_text();
            assert!(t.chars().count() <= 200, "{c}: {}", t.chars().count());
            assert!(!t.contains('\n'));
        }
    }

    #[test]
    fn categories_come_from_codes_and_statuses_only() {
        assert_eq!(
            categorize(Some(429), Some("subscription_sharing_usage_limit_exceeded")),
            Some(Category::UsageLimit)
        );
        assert_eq!(
            categorize(None, Some("subscription_sharing_invalid_user")),
            Some(Category::SignIn)
        );
        assert_eq!(categorize(Some(401), None), Some(Category::SignIn));
        assert_eq!(
            categorize(Some(502), Some("whatever")),
            Some(Category::Unavailable)
        );
        assert_eq!(categorize(Some(400), Some("whatever")), None);
        assert_eq!(categorize(None, None), None);
    }
}
