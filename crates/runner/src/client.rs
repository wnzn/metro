//! Outbound JSON HTTP client for the Metro server.
//!
//! - `Authorization: Bearer $METRO_RUNNER_TOKEN` on every request.
//! - Redirects are never followed (a redirect could carry the bearer token to
//!   another origin); any 3xx is an error.
//! - Every request has a global timeout and response bodies are size-limited.

use std::time::Duration;

use serde::Serialize;
use serde_json::Value;

use crate::config::Config;

pub const MAX_CLAIM_BODY: u64 = 8 * 1024 * 1024;
const MAX_ERROR_BODY: u64 = 4 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub struct Job {
    pub id: String,
    pub tool: String,
    pub arguments: Value,
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
            .send(body)
            // Transport errors (connect, timeout, reset) are transient.
            .map_err(|e| retryable(format!("POST {path}: {e}")))
    }

    /// POST /v1/runner/claim {} -> 204 (idle) or 200 {id, tool, arguments}.
    pub fn claim(&self) -> Result<Claim, ClientError> {
        let mut resp = self.post("/v1/runner/claim", "{}".into())?;
        let status = resp.status().as_u16();
        match status {
            204 => Ok(Claim::Idle),
            200 => {
                let body = resp
                    .body_mut()
                    .with_config()
                    .limit(MAX_CLAIM_BODY)
                    .read_to_string()
                    .map_err(|e| retryable(format!("claim: reading body: {e}")))?;
                parse_claim(&body)
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

pub fn parse_claim(body: &str) -> Result<Claim, ClientError> {
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
    Ok(Claim::Job(Job {
        id,
        tool,
        arguments,
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
}
