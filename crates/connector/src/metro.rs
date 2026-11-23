//! Outbound client for Metro's device endpoints.
//!
//! - `POST /v1/device/claim` with `Authorization: Bearer metro_pc_...`
//!   and `{"approved_models":[...]}` for chat, or `{}` otherwise
//!   → `204` (nothing to do) or `200 {id, operation, request}`.
//! - `POST /v1/device/jobs/{id}/result` with `{ok:true, response}` or
//!   `{ok:false, error}`.
//!
//! Redirects are never followed, bodies are size-limited, and the token is
//! only ever sent to the configured server URL.

use std::time::Duration;

use serde_json::{Value, json};

use crate::Secret;
use crate::http::{client, read_capped};
use crate::profile::validate_models;

/// Metro hands a device at most 4 MiB of request (`max_request_bytes`).
pub const MAX_CLAIM_BODY: usize = 4 * 1024 * 1024;
/// A `media_embed` job may be larger (`max_media_request_bytes`: one 8 MiB
/// asset as base64). Only a media profile reads claims this large, and only
/// a `media_embed` job may use the extra room.
pub const MAX_MEDIA_CLAIM_BODY: usize = metro_model_protocol::MEDIA_MAX_REQUEST_BYTES + 64 * 1024;
/// Metro accepts at most 2 MiB of `response` (`max_response_bytes`) plus a
/// small envelope; a larger result is reported as an error instead.
pub const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_RESULT_BODY: usize = MAX_RESPONSE_BYTES + 4096;
const MAX_ACK_BODY: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub struct Job {
    pub id: String,
    pub operation: String,
    pub request: Value,
}

#[derive(Debug, PartialEq)]
pub enum Claim {
    Idle,
    Job(Job),
    /// We know the job id but the job is malformed; report it as failed.
    Invalid {
        id: String,
        reason: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// Network trouble, 5xx, 429: try again later.
    Retryable,
    /// 401/403: the device token was revoked or replaced.
    Unauthorized,
    /// 404/409/410 on a result: the job is gone (expired or already done).
    Gone,
    /// Anything else: a bug or a protocol mismatch.
    Fatal,
}

#[derive(Debug, Clone)]
pub struct MetroError {
    pub kind: ErrorKind,
    pub message: String,
}

impl std::fmt::Display for MetroError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for MetroError {}

fn e(kind: ErrorKind, message: impl Into<String>) -> MetroError {
    MetroError {
        kind,
        message: message.into(),
    }
}

/// Job ids go into a URL path: a conservative charset only.
pub fn valid_job_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id != "."
        && id != ".."
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.' | b':'))
}

pub struct MetroClient {
    http: reqwest::Client,
    base: String,
    token: Secret,
    claim_cap: usize,
    claim_mode: tokio::sync::Mutex<ClaimMode>,
}

#[derive(Clone, Copy)]
enum ClaimMode {
    Probe,
    Extended,
    Legacy,
}

impl MetroClient {
    /// `base` must already be validated (`netpolicy::validate_base_url`).
    pub fn new(base: &str, token: Secret, timeout: Duration) -> anyhow::Result<Self> {
        Ok(MetroClient {
            http: client(timeout)?,
            base: base.trim_end_matches('/').to_string(),
            token,
            claim_cap: MAX_CLAIM_BODY,
            claim_mode: tokio::sync::Mutex::new(ClaimMode::Probe),
        })
    }

    /// Accept `media_embed` jobs up to [`MAX_MEDIA_CLAIM_BODY`]; for the
    /// media profile kind only.
    pub fn with_media_claims(mut self) -> Self {
        self.claim_cap = MAX_MEDIA_CLAIM_BODY;
        self
    }

    async fn post(
        &self,
        path: &str,
        body: Vec<u8>,
        timeout: Option<Duration>,
    ) -> Result<reqwest::Response, MetroError> {
        let mut rb = self.http.post(format!("{}{}", self.base, path));
        if let Some(t) = timeout {
            rb = rb.timeout(t);
        }
        rb.bearer_auth(self.token.expose())
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .body(body)
            .send()
            .await
            .map_err(|err| {
                let what = if err.is_timeout() {
                    "timed out"
                } else if err.is_connect() {
                    "cannot connect"
                } else {
                    "request failed"
                };
                e(ErrorKind::Retryable, format!("POST {path}: {what}"))
            })
    }

    /// Claim without advertising a chat inventory.
    pub async fn claim(&self) -> Result<Claim, MetroError> {
        self.claim_with_inventory(None).await
    }

    /// `approved_models` is the local profile's approval list, never a
    /// vendor's list. `None` is a non-chat poll; `Some(&[])` is explicit.
    pub async fn claim_with_inventory(
        &self,
        approved_models: Option<&[String]>,
    ) -> Result<Claim, MetroError> {
        const PATH: &str = "/v1/device/claim";
        let Some(models) = approved_models else {
            return self
                .decode_claim(self.post(PATH, b"{}".to_vec(), None).await?)
                .await;
        };
        // Never truncate, normalize, or include a bad identifier in an error.
        validate_models(models)
            .map_err(|_| e(ErrorKind::Fatal, "claim: invalid approved models"))?;
        // Keep negotiation scoped to this client, even if shared by callers.
        let mut mode = self.claim_mode.lock().await;
        if matches!(*mode, ClaimMode::Legacy) {
            return self
                .decode_claim(self.post(PATH, b"{}".to_vec(), None).await?)
                .await;
        }
        let body = serde_json::to_vec(&json!({"approved_models": models}))
            .map_err(|_| e(ErrorKind::Fatal, "claim: encode"))?;
        let resp = self.post(PATH, body, None).await?;
        match (*mode, resp.status().as_u16()) {
            (ClaimMode::Probe, status @ (400 | 422)) => {
                // Allow a bare 400 or the prior handler's exact empty-object
                // rejection. Don't turn another reported error into legacy mode.
                let legacy = read_capped(resp, MAX_ACK_BODY).await.is_ok_and(|body| {
                    (status == 400 && body.is_empty())
                        || serde_json::from_slice::<Value>(&body)
                            .is_ok_and(|v| v == json!({"error": "the body must be {}"}))
                });
                if !legacy {
                    return Err(status_error("claim", status, false));
                }
            }
            _ => {
                let claim = self.decode_claim(resp).await?;
                *mode = ClaimMode::Extended;
                return Ok(claim);
            }
        }
        // Consume the one compatibility attempt before sending it.
        // Don't enter legacy mode if the retry fails.
        *mode = ClaimMode::Extended;
        let claim = self
            .decode_claim(self.post(PATH, b"{}".to_vec(), None).await?)
            .await?;
        *mode = ClaimMode::Legacy;
        Ok(claim)
    }

    async fn decode_claim(&self, resp: reqwest::Response) -> Result<Claim, MetroError> {
        let status = resp.status().as_u16();
        match status {
            204 => Ok(Claim::Idle),
            200 => {
                let body = read_capped(resp, self.claim_cap)
                    .await
                    .map_err(|_| e(ErrorKind::Retryable, "claim: body too large or unreadable"))?;
                let claim = parse_claim(&body)?;
                Ok(limit_claim(claim, body.len()))
            }
            _ => Err(status_error("claim", status, false)),
        }
    }

    /// Post a result once, giving up after `timeout`. The caller decides
    /// about retries.
    pub async fn post_result(
        &self,
        id: &str,
        body: &Value,
        timeout: Duration,
    ) -> Result<(), MetroError> {
        if !valid_job_id(id) {
            return Err(e(
                ErrorKind::Fatal,
                "refusing to post a result for an invalid job id",
            ));
        }
        let bytes = serde_json::to_vec(body).map_err(|_| e(ErrorKind::Fatal, "result: encode"))?;
        if bytes.len() > MAX_RESULT_BODY {
            return Err(e(ErrorKind::Fatal, "result is too large"));
        }
        let resp = self
            .post(
                &format!("/v1/device/jobs/{id}/result"),
                bytes,
                Some(timeout),
            )
            .await?;
        let status = resp.status().as_u16();
        if (200..300).contains(&status) {
            let _ = read_capped(resp, MAX_ACK_BODY).await;
            Ok(())
        } else {
            Err(status_error("result", status, true))
        }
    }
}

fn status_error(what: &str, status: u16, is_result: bool) -> MetroError {
    let msg = format!("{what}: HTTP {status}");
    match status {
        300..=399 => e(
            ErrorKind::Fatal,
            format!("{msg} (redirects are not followed; check the server URL)"),
        ),
        401 | 403 => e(ErrorKind::Unauthorized, msg),
        404 | 409 | 410 if is_result => e(ErrorKind::Gone, msg),
        404 => e(
            ErrorKind::Fatal,
            format!("{msg} (this server has no device endpoint; check the server URL)"),
        ),
        408 | 425 | 429 | 500..=599 => e(ErrorKind::Retryable, msg),
        _ => e(ErrorKind::Fatal, msg),
    }
}

/// Only a `media_embed` job may be larger than [`MAX_CLAIM_BODY`].
pub fn limit_claim(claim: Claim, body_len: usize) -> Claim {
    match claim {
        Claim::Job(job) if body_len > MAX_CLAIM_BODY && job.operation != "media_embed" => {
            Claim::Invalid {
                id: job.id,
                reason: "the job is too large for its operation".into(),
            }
        }
        other => other,
    }
}

pub fn parse_claim(body: &[u8]) -> Result<Claim, MetroError> {
    let v: Value = serde_json::from_slice(body)
        .map_err(|_| e(ErrorKind::Fatal, "claim: response is not JSON"))?;
    let id = match v.get("id").and_then(Value::as_str) {
        Some(id) if valid_job_id(id) => id.to_string(),
        Some(_) => return Err(e(ErrorKind::Fatal, "claim: invalid job id")),
        None => return Err(e(ErrorKind::Fatal, "claim: response has no job id")),
    };
    let operation = match v.get("operation").and_then(Value::as_str) {
        Some(op) => op.to_string(),
        None => {
            return Ok(Claim::Invalid {
                id,
                reason: "job has no operation".into(),
            });
        }
    };
    let request = match v.get("request") {
        Some(r @ Value::Object(_)) => r.clone(),
        None | Some(Value::Null) => json!({}),
        Some(_) => {
            return Ok(Claim::Invalid {
                id,
                reason: "job 'request' must be an object".into(),
            });
        }
    };
    Ok(Claim::Job(Job {
        id,
        operation,
        request,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claims() {
        assert_eq!(
            parse_claim(br#"{"id":"j1","operation":"models","request":{}}"#).unwrap(),
            Claim::Job(Job {
                id: "j1".into(),
                operation: "models".into(),
                request: json!({})
            })
        );
        assert!(matches!(
            parse_claim(br#"{"id":"j1","request":{}}"#).unwrap(),
            Claim::Invalid { .. }
        ));
        assert!(matches!(
            parse_claim(br#"{"id":"j1","operation":"complete","request":"x"}"#).unwrap(),
            Claim::Invalid { .. }
        ));
        for bad in [
            &b"nope"[..],
            br#"{"id":"../x","operation":"models"}"#,
            br#"{}"#,
        ] {
            assert!(parse_claim(bad).is_err());
        }
    }

    #[test]
    fn only_media_jobs_may_be_large() {
        let job = |op: &str| {
            Claim::Job(Job {
                id: "j1".into(),
                operation: op.into(),
                request: json!({}),
            })
        };
        assert!(matches!(
            limit_claim(job("media_embed"), MAX_CLAIM_BODY + 1),
            Claim::Job(_)
        ));
        for op in ["embed", "complete", "decide", "models"] {
            assert!(matches!(
                limit_claim(job(op), MAX_CLAIM_BODY + 1),
                Claim::Invalid { .. }
            ));
            assert!(matches!(
                limit_claim(job(op), MAX_CLAIM_BODY),
                Claim::Job(_)
            ));
        }
    }

    #[test]
    fn job_ids() {
        assert!(valid_job_id("dj_01HX-abc.1"));
        for bad in ["", ".", "..", "a/b", "a?b", "a b", "a%2f"] {
            assert!(!valid_job_id(bad), "{bad}");
        }
    }
}
