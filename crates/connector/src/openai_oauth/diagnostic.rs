//! Device-local diagnostics for failed ChatGPT catalog and inference calls.
//!
//! A [`LocalFailure`] is built only from closed vocabularies: the operation,
//! the phase, a validated numeric HTTP status, and a provider error code and
//! parameter that are looked up in fixed allowlists of documented values.
//! Anything else becomes `unrecognized`. Response bodies, provider messages,
//! request IDs and headers are never captured, so nothing here can quote a
//! prompt, an account or a credential.
//!
//! It travels under the fixed [`crate::SafeError`] in the returned error
//! chain. `metro_error_text` and the relay result read only the `SafeError`,
//! so the diagnostic never reaches Metro. Read it on the device with
//! [`local_failure`].

use serde::{Deserialize, Serialize};

use super::http::Category;

/// What the connector was doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    /// `GET {api}/models`, the account's model catalog.
    Catalog,
    /// `POST {api}/responses`, one chat turn.
    Inference,
}

/// Where it stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// On this device, before the request: the local sign-in, the credential
    /// store, plan permission, token renewal or client setup.
    Session,
    /// OpenAI answered with an HTTP error, or a success that wasn't the
    /// documented shape.
    Http,
    /// The response stream opened, then failed, stopped or broke.
    Stream,
    /// No HTTP answer: connection, TLS or timeout.
    Transport,
    /// Translating the request to Responses, or the completed response back.
    Translation,
}

/// The value used for a code or parameter outside the allowlists.
pub const UNRECOGNIZED: &str = "unrecognized";

/// Provider error codes (and `error.type` values) that are documented for this
/// flow, the OpenAI API or OAuth refresh. Nothing else is ever kept.
const PROVIDER_CODES: &[&str] = &[
    // https://developers.openai.com/siwc/token-sharing-open-source/errors-and-recovery
    "subscription_sharing_user_not_eligible",
    "subscription_sharing_usage_limit_exceeded",
    "subscription_sharing_usage_unavailable",
    "subscription_sharing_unsupported_capability",
    "subscription_sharing_route_not_supported",
    "subscription_sharing_invalid_user",
    "chatpass_v2_scope_not_authorized",
    "chatpass_v2_invalid_authorization_context",
    "subscription_sharing_user_unavailable",
    // Refresh errors on the same page.
    "invalid_grant",
    "invalid_refresh_token",
    "token_expired",
    "refresh_token_expired",
    "refresh_token_invalidated",
    "refresh_token_reused",
    "invalid_client",
    // OpenAI API error codes and types.
    "invalid_api_key",
    "model_not_found",
    "model_not_available",
    "context_length_exceeded",
    "content_filter",
    "insufficient_quota",
    "rate_limit_exceeded",
    "server_error",
    "temporarily_unavailable",
    "overloaded",
    "invalid_request_error",
    "invalid_value",
    "unsupported_value",
    "unsupported_parameter",
];

/// Top-level Responses request fields. A provider `param` is reduced to its
/// top-level field (`tools[0].strict` → `tools`), except `reasoning.effort`.
const PARAMS: &[&str] = &[
    "reasoning.effort",
    "model",
    "input",
    "instructions",
    "tools",
    "tool_choice",
    "parallel_tool_calls",
    "reasoning",
    "text",
    "store",
    "stream",
    "include",
    "service_tier",
    "max_output_tokens",
    "temperature",
    "top_p",
    "metadata",
    "user",
    "prompt",
    "previous_response_id",
    "truncation",
    "background",
    "conversation",
];

pub(crate) fn provider_code(raw: Option<&str>) -> Option<&'static str> {
    raw.map(|c| {
        PROVIDER_CODES
            .iter()
            .copied()
            .find(|k| *k == c)
            .unwrap_or(UNRECOGNIZED)
    })
}

pub(crate) fn param(raw: Option<&str>) -> Option<&'static str> {
    raw.map(|p| {
        if let Some(exact) = PARAMS.iter().copied().find(|k| *k == p) {
            return exact;
        }
        let root = p.split(['.', '[']).next().unwrap_or("");
        PARAMS
            .iter()
            .copied()
            .find(|k| *k == root && !k.contains('.'))
            .unwrap_or(UNRECOGNIZED)
    })
}

pub(crate) fn status(raw: u16) -> Option<u16> {
    (100..=599).contains(&raw).then_some(raw)
}

/// Facts recorded where a failure happens. Already reduced to allowlisted
/// values; attached as error context and read by [`finish`].
#[derive(Clone, Copy, Debug)]
pub(crate) struct Detail {
    pub phase: Phase,
    pub status: Option<u16>,
    pub code: Option<&'static str>,
    pub param: Option<&'static str>,
}

impl Detail {
    pub fn phase(phase: Phase) -> Self {
        Self {
            phase,
            status: None,
            code: None,
            param: None,
        }
    }

    pub fn http(phase: Phase, raw_status: u16, code: Option<&str>, param_: Option<&str>) -> Self {
        Self {
            phase,
            status: status(raw_status),
            code: provider_code(code),
            param: param(param_),
        }
    }
}

impl std::fmt::Display for Detail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ChatGPT failure detail")
    }
}

/// Adds `detail` unless the error already carries one.
pub(crate) fn mark(e: anyhow::Error, detail: Detail) -> anyhow::Error {
    if e.downcast_ref::<Detail>().is_some() {
        e
    } else {
        e.context(detail)
    }
}

/// A sanitized description of one failed catalog or inference call, for the
/// device's own log. Every field comes from a closed set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct LocalFailure {
    operation: Operation,
    phase: Phase,
    http_status: Option<u16>,
    provider_code: Option<&'static str>,
    provider_param: Option<&'static str>,
    code: &'static str,
}

impl LocalFailure {
    pub(crate) fn new(operation: Operation, detail: Option<Detail>, category: Category) -> Self {
        let d = detail.unwrap_or(Detail::phase(Phase::Session));
        Self {
            operation,
            phase: d.phase,
            http_status: d.status,
            provider_code: d.code,
            provider_param: d.param,
            code: category.code(),
        }
    }

    pub fn operation(&self) -> Operation {
        self.operation
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// The HTTP status OpenAI answered with, when there was one.
    pub fn http_status(&self) -> Option<u16> {
        self.http_status
    }

    /// A documented provider error code, `unrecognized`, or `None` when the
    /// answer carried no code.
    pub fn provider_code(&self) -> Option<&'static str> {
        self.provider_code
    }

    /// A documented top-level request field, `unrecognized`, or `None`.
    pub fn provider_param(&self) -> Option<&'static str> {
        self.provider_param
    }

    /// The outcome Metro was told, as a stable token such as
    /// `chatgpt_rejected` or `chatgpt_service_or_routing_unavailable`.
    pub fn code(&self) -> &'static str {
        self.code
    }

    /// One line of allowlisted tokens and the numeric status only.
    pub fn summary(&self) -> String {
        let op = match self.operation {
            Operation::Catalog => "catalog",
            Operation::Inference => "inference",
        };
        let phase = match self.phase {
            Phase::Session => "session",
            Phase::Http => "http",
            Phase::Stream => "stream",
            Phase::Transport => "transport",
            Phase::Translation => "translation",
        };
        let status = self
            .http_status
            .map(|s| s.to_string())
            .unwrap_or_else(|| "none".into());
        format!(
            "{} operation={op} phase={phase} http_status={status} provider_code={} provider_param={}",
            self.code,
            self.provider_code.unwrap_or("none"),
            self.provider_param.unwrap_or("none"),
        )
    }
}

/// Reading a stored record re-applies the allowlists, so a file edited on
/// disk can't turn a diagnostic into free text.
impl<'de> Deserialize<'de> for LocalFailure {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            operation: Operation,
            phase: Phase,
            http_status: Option<u16>,
            provider_code: Option<String>,
            provider_param: Option<String>,
            code: String,
        }
        let r = Raw::deserialize(d)?;
        Ok(Self {
            operation: r.operation,
            phase: r.phase,
            http_status: r.http_status.and_then(status),
            provider_code: provider_code(r.provider_code.as_deref()),
            provider_param: param(r.provider_param.as_deref()),
            code: Category::ALL
                .iter()
                .map(|c| c.code())
                .find(|c| *c == r.code)
                .unwrap_or(UNRECOGNIZED),
        })
    }
}

impl std::fmt::Display for LocalFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.summary())
    }
}

impl std::error::Error for LocalFailure {}

/// The diagnostic of a failed ChatGPT catalog or inference call, from the
/// error that call returned (also after `?` or added context).
pub fn local_failure(err: &anyhow::Error) -> Option<LocalFailure> {
    err.downcast_ref::<LocalFailure>().copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_allowlisted_values_survive() {
        assert_eq!(
            provider_code(Some("subscription_sharing_usage_limit_exceeded")),
            Some("subscription_sharing_usage_limit_exceeded")
        );
        assert_eq!(provider_code(Some("weird_code")), Some(UNRECOGNIZED));
        assert_eq!(provider_code(Some("user@example.test")), Some(UNRECOGNIZED));
        assert_eq!(provider_code(None), None);
        assert_eq!(param(Some("reasoning.effort")), Some("reasoning.effort"));
        assert_eq!(param(Some("tools[0].tools[2].strict")), Some("tools"));
        assert_eq!(param(Some("input[3].content")), Some("input"));
        assert_eq!(param(Some("reasoning.summary")), Some("reasoning"));
        assert_eq!(param(Some("SENSITIVE-PARAM")), Some(UNRECOGNIZED));
        assert_eq!(param(Some("effort")), Some(UNRECOGNIZED));
        assert_eq!(status(503), Some(503));
        assert_eq!(status(42), None);
        assert_eq!(status(999), None);
    }

    #[test]
    fn stored_records_are_revalidated() {
        let f = LocalFailure::new(
            Operation::Inference,
            Some(Detail::http(Phase::Http, 503, None, None)),
            Category::ServiceOrRouting,
        );
        let back: LocalFailure = serde_json::from_value(serde_json::to_value(f).unwrap()).unwrap();
        assert_eq!(back, f);
        let forged: LocalFailure = serde_json::from_value(serde_json::json!({
            "operation": "catalog", "phase": "http", "http_status": 7,
            "provider_code": "SENSITIVE", "provider_param": "input[0].SENSITIVE",
            "code": "SENSITIVE"
        }))
        .unwrap();
        assert_eq!(forged.http_status(), None);
        assert_eq!(forged.provider_code(), Some(UNRECOGNIZED));
        assert_eq!(forged.provider_param(), Some("input"));
        assert_eq!(forged.code(), UNRECOGNIZED);
        assert!(!forged.summary().contains("SENSITIVE"));
        assert!(
            serde_json::from_value::<LocalFailure>(serde_json::json!({
                "operation": "catalog", "phase": "http", "http_status": null,
                "provider_code": null, "provider_param": null, "code": "chatgpt_rejected",
                "message": "x"
            }))
            .is_err()
        );
    }
}
