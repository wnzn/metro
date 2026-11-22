//! HTTP to the desktop's own hosted Metro server: the owner's calls (with the
//! person's session) and the runner calls (with the Space's dedicated token).
//!
//! No redirects, no proxy (loopback only), bounded bodies, and tokens only in
//! the `Authorization` header, never in a URL or a log line.

use std::time::Duration;

use serde_json::{Value, json};

use crate::Secret;
use crate::http::read_capped;

pub const PROTOCOL_HEADER: &str = "X-Metro-Runner-Protocol";
const MAX_BODY: usize = 256 * 1024;
pub const MAX_CLAIM_BODY: usize = 1024 * 1024;

/// A failure, with short text safe to show the person.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallError {
    /// Network trouble or 5xx/429: try again later.
    Unreachable(String),
    /// 401 on a session call: the session expired; on a runner call: the
    /// token was revoked or replaced.
    Unauthorized,
    Forbidden(String),
    NotFound(String),
    /// 412 on a guarded call: the Space's runner is not what we expected.
    Precondition(String),
    Other(String),
}

impl CallError {
    pub fn message(&self) -> String {
        match self {
            CallError::Unreachable(m)
            | CallError::Forbidden(m)
            | CallError::NotFound(m)
            | CallError::Precondition(m)
            | CallError::Other(m) => m.clone(),
            CallError::Unauthorized => "the local Metro server refused the sign-in".into(),
        }
    }
}

/// The Space's runner, as `GET /v1/spaces/{sid}/runner` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpaceRunner {
    None,
    Dedicated,
    Registered(String),
}

#[derive(Debug)]
pub struct Job {
    pub id: String,
    pub tool: String,
    pub arguments: Value,
    pub space_id: Option<String>,
}

#[derive(Debug)]
pub enum Claim {
    Idle,
    Job(Job),
    /// A job we know the id of but can't run as sent.
    Invalid {
        id: String,
        reason: String,
    },
}

pub fn valid_job_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id != "."
        && id != ".."
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
}

#[derive(Clone)]
pub struct Hosted {
    http: reqwest::Client,
    base: String,
}

fn transport(e: reqwest::Error) -> CallError {
    let _ = e; // never echo: the error can carry the URL
    CallError::Unreachable("the local Metro server didn't answer".into())
}

/// The server's own `{"error": "..."}` text, single line and short.
fn server_message(v: &Value, fallback: &str) -> String {
    let m = v
        .get("error")
        .and_then(|e| {
            e.as_str()
                .or_else(|| e.get("message").and_then(Value::as_str))
        })
        .unwrap_or(fallback);
    crate::secret::clean_line(m, 200)
}

impl Hosted {
    pub fn new(base: &str) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(30))
            .user_agent(crate::USER_AGENT)
            .build()?;
        Ok(Hosted {
            http,
            base: base.trim_end_matches('/').to_owned(),
        })
    }

    async fn send(
        &self,
        method: reqwest::Method,
        path: &str,
        bearer: &Secret,
        headers: &[(&str, &str)],
        body: Option<&Value>,
        cap: usize,
    ) -> Result<(u16, reqwest::header::HeaderMap, Value), CallError> {
        let mut req = self
            .http
            .request(method, format!("{}{}", self.base, path))
            .bearer_auth(bearer.expose())
            .header("Accept", "application/json");
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        if let Some(b) = body {
            req = req
                .header("Content-Type", "application/json")
                .body(serde_json::to_vec(b).unwrap_or_default());
        }
        let resp = req.send().await.map_err(transport)?;
        let status = resp.status().as_u16();
        let resp_headers = resp.headers().clone();
        let bytes = read_capped(resp, cap)
            .await
            .map_err(|_| CallError::Other("the local Metro server sent too much data".into()))?;
        let v = if bytes.iter().all(u8::is_ascii_whitespace) {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        Ok((status, resp_headers, v))
    }

    fn classify(status: u16, v: &Value) -> CallError {
        match status {
            401 => CallError::Unauthorized,
            403 => CallError::Forbidden(server_message(v, "not allowed")),
            404 => CallError::NotFound(server_message(v, "not found")),
            412 => CallError::Precondition(server_message(v, "the Space's runner changed")),
            300..=399 => CallError::Other("the local Metro server answered with a redirect".into()),
            408 | 425 | 429 | 500..=599 => {
                CallError::Unreachable(format!("the local Metro server answered HTTP {status}"))
            }
            _ => CallError::Other(server_message(v, &format!("HTTP {status}"))),
        }
    }

    /// `GET /v1/me` → the account id.
    pub async fn me(&self, session: &Secret) -> Result<String, CallError> {
        let (s, _, v) = self
            .send(reqwest::Method::GET, "/v1/me", session, &[], None, MAX_BODY)
            .await?;
        if s != 200 {
            return Err(Self::classify(s, &v));
        }
        v.get("id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| CallError::Other("the local Metro server sent no account id".into()))
    }

    /// The caller's role in `sid` (`owner`/`member`), or `None` when the
    /// Space isn't visible to them.
    pub async fn role(&self, session: &Secret, sid: &str) -> Result<Option<String>, CallError> {
        let (s, _, v) = self
            .send(
                reqwest::Method::GET,
                "/v1/spaces",
                session,
                &[],
                None,
                4 * MAX_BODY,
            )
            .await?;
        if s != 200 {
            return Err(Self::classify(s, &v));
        }
        let spaces = v
            .get("spaces")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(spaces
            .iter()
            .find(|sp| sp.get("id").and_then(Value::as_str) == Some(sid))
            .and_then(|sp| sp.get("role").and_then(Value::as_str))
            .map(str::to_owned))
    }

    pub async fn space_runner(
        &self,
        session: &Secret,
        sid: &str,
    ) -> Result<SpaceRunner, CallError> {
        let (s, _, v) = self
            .send(
                reqwest::Method::GET,
                &format!("/v1/spaces/{sid}/runner"),
                session,
                &[],
                None,
                MAX_BODY,
            )
            .await?;
        if s != 200 {
            return Err(Self::classify(s, &v));
        }
        let r = v.get("runner").cloned().unwrap_or(Value::Null);
        Ok(match r.get("kind").and_then(Value::as_str) {
            None => SpaceRunner::None,
            Some("dedicated") => SpaceRunner::Dedicated,
            Some(_) => SpaceRunner::Registered(
                r.get("name")
                    .and_then(Value::as_str)
                    .map(|n| crate::secret::clean_line(n, 80))
                    .unwrap_or_else(|| "a registered runner".into()),
            ),
        })
    }

    /// `POST /v1/spaces/{sid}/runner-token` with `If-None-Match: *`: a new
    /// dedicated token and its ETag, only if the Space has no runner.
    pub async fn issue_guarded(
        &self,
        session: &Secret,
        sid: &str,
    ) -> Result<(Secret, String), CallError> {
        let (s, h, mut v) = self
            .send(
                reqwest::Method::POST,
                &format!("/v1/spaces/{sid}/runner-token"),
                session,
                &[("If-None-Match", "*")],
                Some(&json!({})),
                MAX_BODY,
            )
            .await?;
        if s != 200 {
            return Err(Self::classify(s, &v));
        }
        let etag = h
            .get(reqwest::header::ETAG)
            .and_then(|e| e.to_str().ok())
            .map(str::to_owned);
        let token = match v.get_mut("token") {
            Some(Value::String(t)) => {
                let out = Secret::new(t.as_str());
                zeroize::Zeroize::zeroize(t);
                out
            }
            _ => {
                return Err(CallError::Other(
                    "the local Metro server sent no token".into(),
                ));
            }
        };
        // A dedicated token only: never a reusable (`metro_rt_v2_`) one.
        let t = token.expose();
        if !t.starts_with("metro_rt_") || t.starts_with("metro_rt_v2_") || t.len() > 256 {
            return Err(CallError::Other(
                "the local Metro server sent an unexpected token".into(),
            ));
        }
        let Some(etag) = etag.filter(|e| e.len() <= 64 && e.starts_with('"') && e.ends_with('"'))
        else {
            return Err(CallError::Other(
                "the local Metro server doesn't support guarded pairing".into(),
            ));
        };
        Ok((token, etag))
    }

    /// `DELETE /v1/spaces/{sid}/runner-token` with `If-Match: <etag>`.
    pub async fn revoke_guarded(
        &self,
        session: &Secret,
        sid: &str,
        etag: &str,
    ) -> Result<(), CallError> {
        let (s, _, v) = self
            .send(
                reqwest::Method::DELETE,
                &format!("/v1/spaces/{sid}/runner-token"),
                session,
                &[("If-Match", etag)],
                None,
                MAX_BODY,
            )
            .await?;
        if s == 204 || s == 200 {
            return Ok(());
        }
        Err(Self::classify(s, &v))
    }

    /// `POST /v1/runner/claim` with the Space's dedicated token.
    pub async fn claim(&self, token: &Secret) -> Result<Claim, CallError> {
        let (s, _, v) = self
            .send(
                reqwest::Method::POST,
                "/v1/runner/claim",
                token,
                &[(PROTOCOL_HEADER, "2")],
                Some(&json!({})),
                MAX_CLAIM_BODY,
            )
            .await?;
        match s {
            204 => Ok(Claim::Idle),
            200 => Ok(parse_claim(&v)),
            _ => Err(Self::classify(s, &v)),
        }
    }

    /// `POST /v1/runner/jobs/{id}/result`.
    pub async fn post_result(
        &self,
        token: &Secret,
        id: &str,
        ok: bool,
        output: &str,
    ) -> Result<(), CallError> {
        if !valid_job_id(id) {
            return Err(CallError::Other("invalid job id".into()));
        }
        let (s, _, v) = self
            .send(
                reqwest::Method::POST,
                &format!("/v1/runner/jobs/{id}/result"),
                token,
                &[(PROTOCOL_HEADER, "2")],
                Some(&json!({"ok": ok, "output": output})),
                MAX_BODY,
            )
            .await?;
        match s {
            200..=299 => Ok(()),
            // Expired or already done: nothing more to deliver.
            404 | 409 | 410 => Ok(()),
            _ => Err(Self::classify(s, &v)),
        }
    }
}

pub fn parse_claim(v: &Value) -> Claim {
    let Some(id) = v
        .get("id")
        .and_then(Value::as_str)
        .filter(|i| valid_job_id(i))
    else {
        // No usable id: nothing to report to; treat as idle.
        return Claim::Idle;
    };
    let id = id.to_owned();
    let Some(tool) = v.get("tool").and_then(Value::as_str) else {
        return Claim::Invalid {
            id,
            reason: "job has no string 'tool'".into(),
        };
    };
    let arguments = match v.get("arguments") {
        Some(a @ Value::Object(_)) => a.clone(),
        None | Some(Value::Null) => Value::Object(Default::default()),
        Some(_) => {
            return Claim::Invalid {
                id,
                reason: "job 'arguments' must be an object".into(),
            };
        }
    };
    let space_id = match v.get("space_id") {
        Some(Value::String(s)) => Some(s.clone()),
        None | Some(Value::Null) => None,
        Some(_) => {
            return Claim::Invalid {
                id,
                reason: "job 'space_id' must be a string".into(),
            };
        }
    };
    Claim::Job(Job {
        id,
        tool: tool.to_owned(),
        arguments,
        space_id,
    })
}
