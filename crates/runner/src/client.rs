//! Outbound JSON HTTP client for the Metro server.
//!
//! - `Authorization: Bearer $METRO_RUNNER_TOKEN` on every request.
//! - Redirects are never followed (a redirect could carry the bearer token to
//!   another origin); any 3xx is an error.
//! - Every request has a global timeout and response bodies are size-limited.
//! - Every request carries `X-Metro-Runner-Protocol: 2`. With a reusable
//!   (`metro_rt_v2_`) credential a claim must name a valid `space_id`;
//!   otherwise the job is refused and reported as failed. Dedicated (legacy)
//!   credentials ignore `space_id` and keep the old single-workspace meaning.

use std::time::Duration;

use serde::Serialize;
use serde_json::Value;

use crate::config::{Config, Credential};

pub const PROTOCOL_HEADER: &str = "X-Metro-Runner-Protocol";
pub const PROTOCOL_VERSION: &str = "2";
/// Maximum length of a Space id the runner accepts.
pub const MAX_SPACE_ID_LEN: usize = 64;

/// The server's request to self-update (a version, nothing else).
pub const UPDATE_HEADER: &str = "x-metro-runner-update";

fn update_header(headers: &ureq::http::HeaderMap) -> Option<String> {
    let mut values = headers.get_all(UPDATE_HEADER).iter();
    let value = values.next()?.to_str().ok()?;
    (values.next().is_none() && value.len() <= 32 && metro_release::Version::parse(value).is_some())
        .then(|| value.to_owned())
}

pub const MAX_CLAIM_BODY: u64 = 8 * 1024 * 1024;
const MAX_ERROR_BODY: u64 = 4 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub struct Job {
    pub id: String,
    pub tool: String,
    pub arguments: Value,
    /// The Space this job belongs to. Always `Some(valid id)` for a reusable
    /// credential (otherwise the claim is [`Claim::Invalid`]); for a dedicated
    /// credential it is informational only and never selects a directory.
    pub space_id: Option<String>,
}

#[derive(Debug)]
pub enum Claim {
    Idle,
    Job(Job),
    /// The server sent a job we can't run as-is; we still know its id and
    /// report the problem as a failed result.
    Invalid {
        id: String,
        reason: String,
    },
}

#[derive(Debug, Clone)]
pub struct ClientError {
    pub retryable: bool,
    pub message: String,
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ClientError {}

fn retryable(msg: impl Into<String>) -> ClientError {
    ClientError {
        retryable: true,
        message: msg.into(),
    }
}

fn fatal(msg: impl Into<String>) -> ClientError {
    ClientError {
        retryable: false,
        message: msg.into(),
    }
}

#[derive(Debug, Serialize)]
pub struct ResultBody<'a> {
    pub ok: bool,
    pub output: &'a str,
}

pub struct ServerClient {
    agent: ureq::Agent,
    base: String,
    auth: String,
    credential: Credential,
}

impl ServerClient {
    pub fn new(cfg: &Config) -> Self {
        Self::with(&cfg.server_url, cfg.token.expose(), cfg.http_timeout)
    }

    pub fn with(base: &str, token: &str, timeout: Duration) -> Self {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(timeout))
            .max_redirects(0)
            .http_status_as_error(false)
            .user_agent(concat!("metro-runner/", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        ServerClient {
            agent,
            base: base.trim_end_matches('/').to_string(),
            auth: format!("Bearer {token}"),
            credential: Credential::from_token(token),
        }
    }

    fn post(
        &self,
        path: &str,
        body: String,
    ) -> Result<ureq::http::Response<ureq::Body>, ClientError> {
        let url = format!("{}{}", self.base, path);
        self.agent
            .post(&url)
            .header("Authorization", &self.auth)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .header(PROTOCOL_HEADER, PROTOCOL_VERSION)
            .send(body)
            // Transport errors (connect, timeout, reset) are transient.
            .map_err(|e| retryable(format!("POST {path}: {e}")))
    }

    /// POST /v1/runner/claim {} -> 204 (idle) or 200 {id, tool, arguments}.
    pub fn claim(&self) -> Result<Claim, ClientError> {
        self.claim_with("{}").map(|(claim, _)| claim)
    }

    /// POST /v1/runner/claim with a presence body (see [`crate::update`]).
    /// Also returns the server's `X-Metro-Runner-Update` version, if any:
    /// the only thing a server ever says about updates.
    pub fn claim_with(&self, body: &str) -> Result<(Claim, Option<String>), ClientError> {
        let mut resp = self.post("/v1/runner/claim", body.to_owned())?;
        let status = resp.status().as_u16();
        let update = update_header(resp.headers());
        match status {
            204 => Ok((Claim::Idle, update)),
            200 => {
                let body = resp
                    .body_mut()
                    .with_config()
                    .limit(MAX_CLAIM_BODY)
                    .read_to_string()
                    .map_err(|e| retryable(format!("claim: reading body: {e}")))?;
                Ok((parse_claim_for(&body, self.credential)?, update))
            }
            _ => Err(status_error("claim", status, &mut resp)),
        }
    }

    /// POST /v1/runner/jobs/{id}/result {ok, output} -> 2xx.
    pub fn post_result(&self, id: &str, ok: bool, output: &str) -> Result<(), ClientError> {
        if !valid_job_id(id) {
            return Err(fatal(format!(
                "refusing to post result for invalid job id {id:?}"
            )));
        }
        let body =
            serde_json::to_string(&ResultBody { ok, output }).map_err(|e| fatal(e.to_string()))?;
        let mut resp = self.post(&format!("/v1/runner/jobs/{id}/result"), body)?;
        let status = resp.status().as_u16();
        if (200..300).contains(&status) {
            // Drain a little so the connection can be reused; ignore content.
            let _ = resp
                .body_mut()
                .with_config()
                .limit(MAX_ERROR_BODY)
                .read_to_vec();
            Ok(())
        } else {
            Err(status_error("result", status, &mut resp))
        }
    }
}

fn status_error(
    what: &str,
    status: u16,
    resp: &mut ureq::http::Response<ureq::Body>,
) -> ClientError {
    let snippet = resp
        .body_mut()
        .with_config()
        .limit(MAX_ERROR_BODY)
        .read_to_string()
        .unwrap_or_default();
    let snippet: String = snippet
        .chars()
        .filter(|c| !c.is_control())
        .take(200)
        .collect();
    let msg = format!("{what}: HTTP {status} {snippet}");
    match status {
        300..=399 => fatal(format!("{msg} (redirects are not followed)")),
        408 | 425 | 429 | 500..=599 => retryable(msg),
        _ => fatal(msg),
    }
}

/// Job ids are interpolated into a URL path: restrict to a safe charset.
pub fn valid_job_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id != "."
        && id != ".."
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
}

/// Space ids name a directory (`spaces/<id>/`), so only a conservative ASCII
/// slug is accepted: 1..=64 bytes of `[a-z0-9_-]`, starting with `[a-z0-9]`.
/// Lowercase only, so case-insensitive filesystems can't alias two Spaces.
/// Server ids look like `s-0123456789abcdef`.
pub fn valid_space_id(id: &str) -> bool {
    let b = id.as_bytes();
    !b.is_empty()
        && b.len() <= MAX_SPACE_ID_LEN
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'-' | b'_'))
}

/// Parse a claim for a dedicated (legacy) credential.
pub fn parse_claim(body: &str) -> Result<Claim, ClientError> {
    parse_claim_for(body, Credential::Dedicated)
}

/// Parse a claim. With [`Credential::Reusable`] a missing, non-string or
/// unsafe `space_id` turns the job into [`Claim::Invalid`]: it is reported as
/// failed and never runs (there is no fallback to the base directory).
pub fn parse_claim_for(body: &str, credential: Credential) -> Result<Claim, ClientError> {
    let v: Value =
        serde_json::from_str(body).map_err(|e| fatal(format!("claim: invalid JSON: {e}")))?;
    let id = match v.get("id").and_then(Value::as_str) {
        Some(id) if valid_job_id(id) => id.to_string(),
        Some(id) => return Err(fatal(format!("claim: invalid job id {id:?}"))),
        None => return Err(fatal("claim: response has no string 'id'")),
    };
    let tool = match v.get("tool").and_then(Value::as_str) {
        Some(t) => t.to_string(),
        None => {
            return Ok(Claim::Invalid {
                id,
                reason: "job has no string 'tool'".into(),
            });
        }
    };
    let arguments = match v.get("arguments") {
        Some(a @ Value::Object(_)) => a.clone(),
        None | Some(Value::Null) => Value::Object(Default::default()),
        Some(_) => {
            return Ok(Claim::Invalid {
                id,
                reason: "job 'arguments' must be an object".into(),
            });
        }
    };
    let space_id = match (credential, v.get("space_id")) {
        (Credential::Dedicated, Some(Value::String(s))) => Some(s.clone()),
        (Credential::Dedicated, _) => None,
        (Credential::Reusable, Some(Value::String(s))) if valid_space_id(s) => Some(s.clone()),
        (Credential::Reusable, Some(Value::String(_))) => {
            return Ok(Claim::Invalid {
                id,
                reason: "refusing job: 'space_id' is not a valid Space id \
                         (expected 1-64 chars of [a-z0-9_-], starting with [a-z0-9])"
                    .into(),
            });
        }
        (Credential::Reusable, _) => {
            return Ok(Claim::Invalid {
                id,
                reason: "refusing job: a reusable runner credential requires a string \
                         'space_id' in every claim (protocol 2)"
                    .into(),
            });
        }
    };
    Ok(Claim::Job(Job {
        id,
        tool,
        arguments,
        space_id,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_ids() {
        assert!(valid_job_id("job_01HX-abc.1"));
        for bad in ["", ".", "..", "a/b", "a?b", "a b", "a%2f", &"x".repeat(129)] {
            assert!(!valid_job_id(bad), "{bad}");
        }
    }

    #[test]
    fn claim_parsing() {
        match parse_claim(r#"{"id":"j1","tool":"workspace.list","arguments":{"path":"."}}"#)
            .unwrap()
        {
            Claim::Job(j) => assert_eq!(j.tool, "workspace.list"),
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            parse_claim(r#"{"id":"j1","tool":"x","arguments":[1]}"#).unwrap(),
            Claim::Invalid { .. }
        ));
        assert!(matches!(
            parse_claim(r#"{"id":"j1"}"#).unwrap(),
            Claim::Invalid { .. }
        ));
        assert!(parse_claim(r#"{"id":"../x","tool":"t"}"#).is_err());
        assert!(parse_claim("nope").is_err());
    }

    #[test]
    fn space_ids() {
        for ok in ["s-0123456789abcdef", "a", "0", "team_1-x", &"a".repeat(64)] {
            assert!(valid_space_id(ok), "{ok}");
        }
        for bad in [
            "",
            ".",
            "..",
            "-a",
            "_a",
            "S-ABC",
            "a/b",
            "a.b",
            "a b",
            "a\0",
            "é",
            "../s-b",
            "a\\b",
            &"a".repeat(65),
        ] {
            assert!(!valid_space_id(bad), "{bad:?}");
        }
    }

    #[test]
    fn reusable_claims_require_space_id() {
        let r = Credential::Reusable;
        match parse_claim_for(
            r#"{"id":"j1","tool":"workspace.list","arguments":{},"space_id":"s-a"}"#,
            r,
        )
        .unwrap()
        {
            Claim::Job(j) => assert_eq!(j.space_id.as_deref(), Some("s-a")),
            other => panic!("{other:?}"),
        }
        for body in [
            r#"{"id":"j1","tool":"workspace.list","arguments":{}}"#,
            r#"{"id":"j1","tool":"workspace.list","space_id":null}"#,
            r#"{"id":"j1","tool":"workspace.list","space_id":7}"#,
            r#"{"id":"j1","tool":"workspace.list","space_id":""}"#,
            r#"{"id":"j1","tool":"workspace.list","space_id":"../s-b"}"#,
            r#"{"id":"j1","tool":"workspace.list","space_id":"S-A"}"#,
        ] {
            assert!(
                matches!(parse_claim_for(body, r).unwrap(), Claim::Invalid { .. }),
                "{body}"
            );
        }
        // Dedicated credentials keep the legacy meaning.
        assert!(matches!(
            parse_claim(r#"{"id":"j1","tool":"workspace.list"}"#).unwrap(),
            Claim::Job(Job { space_id: None, .. })
        ));
    }
}
