//! HTTP to the bound Metro server (the desktop's own hosted one, or the one
//! the person signed in to): the person's calls (with their session) and
//! the runner calls (with a dedicated or registered runner token).
//!
//! No redirects, the bridge's proxy rules for that server (loopback and
//! Tailscale directly), bounded bodies, and tokens only in the
//! `Authorization` header, never in a URL or a log line.

use std::collections::HashSet;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::Secret;
use crate::http::read_capped;

pub const PROTOCOL_HEADER: &str = "X-Metro-Runner-Protocol";
const MAX_BODY: usize = 256 * 1024;
/// Room for a `workspace.write` job: 1 MiB of content, JSON-escaped.
pub const MAX_CLAIM_BODY: usize = 8 * 1024 * 1024;

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
            CallError::Unauthorized => "the Metro server refused the sign-in".into(),
        }
    }
}

/// A failed `POST /v1/runners`. `definite`: the server refused and created
/// nothing. Otherwise the answer was lost or unusable, and the runner may
/// exist.
#[derive(Debug, Clone)]
pub struct CreateError {
    pub definite: bool,
    pub error: CallError,
}

/// The Space's runner, as `GET /v1/spaces/{sid}/runner` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpaceRunner {
    None,
    Dedicated,
    Registered(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SpaceRole {
    Owner,
    Member,
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
    CallError::Unreachable("the Metro server didn't answer".into())
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

/// The ETag stored by older launchers too; the decimal part is not a secret.
pub(super) fn pairing_revision(etag: &str) -> Option<&str> {
    let raw = etag.strip_prefix("\"rtg-")?.strip_suffix('"')?;
    let value: u64 = raw.parse().ok()?;
    (value <= i64::MAX as u64 && value.to_string() == raw).then_some(raw)
}

impl Hosted {
    pub fn new(base: &str) -> anyhow::Result<Self> {
        let http = crate::http::client_for_server(Duration::from_secs(30), base)?;
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
        let (status, headers, bytes) = self
            .send_raw(method, path, bearer, headers, body, cap)
            .await?;
        Ok((
            status,
            headers,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        ))
    }

    async fn send_raw(
        &self,
        method: reqwest::Method,
        path: &str,
        bearer: &Secret,
        headers: &[(&str, &str)],
        body: Option<&Value>,
        cap: usize,
    ) -> Result<(u16, reqwest::header::HeaderMap, Vec<u8>), CallError> {
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
            .map_err(|_| CallError::Other("the Metro server sent too much data".into()))?;
        Ok((status, resp_headers, bytes))
    }

    fn classify(status: u16, v: &Value) -> CallError {
        match status {
            401 => CallError::Unauthorized,
            403 => CallError::Forbidden(server_message(v, "not allowed")),
            404 => CallError::NotFound(server_message(v, "not found")),
            412 => CallError::Precondition(server_message(v, "the Space's runner changed")),
            300..=399 => CallError::Other("the Metro server answered with a redirect".into()),
            408 | 425 | 429 | 500..=599 => {
                CallError::Unreachable(format!("the Metro server answered HTTP {status}"))
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
            .ok_or_else(|| CallError::Other("the Metro server sent no account id".into()))
    }

    /// `None` is proof of absence only in a fully valid roster.
    pub async fn role(&self, session: &Secret, sid: &str) -> Result<Option<SpaceRole>, CallError> {
        let (s, _, bytes) = self
            .send_raw(
                reqwest::Method::GET,
                "/v1/spaces",
                session,
                &[],
                None,
                4 * MAX_BODY,
            )
            .await?;
        if s != 200 {
            return Err(Self::classify(
                s,
                &serde_json::from_slice(&bytes).unwrap_or(Value::Null),
            ));
        }
        // Decode raw JSON so duplicate required fields aren't silently
        // collapsed by Value. Extra Space metadata is not used for access.
        #[derive(Deserialize)]
        struct Roster {
            spaces: Vec<Entry>,
        }
        #[derive(Deserialize)]
        struct Entry {
            id: String,
            role: SpaceRole,
        }
        let invalid = || CallError::Other("the Metro server sent an invalid Space roster".into());
        // Serde structs also accept positional arrays. Only JSON objects
        // count as roster entries, regardless of their positional values.
        let shape: Value = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
        if !shape
            .get("spaces")
            .and_then(Value::as_array)
            .is_some_and(|rows| rows.iter().all(Value::is_object))
        {
            return Err(invalid());
        }
        let roster: Roster = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
        let mut seen = HashSet::with_capacity(roster.spaces.len());
        let mut role = None;
        for sp in &roster.spaces {
            if !super::store::valid_space_id(&sp.id) || !seen.insert(&sp.id) {
                return Err(invalid());
            }
            if sp.id == sid {
                role = Some(sp.role);
            }
        }
        Ok(role)
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

    /// The stable id of the registered runner attached to `sid`, if one is
    /// (`None` for no runner or a dedicated token). Display names are never
    /// used to identify a runner.
    pub async fn space_runner_id(
        &self,
        session: &Secret,
        sid: &str,
    ) -> Result<Option<String>, CallError> {
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
        if r.get("kind").and_then(Value::as_str) != Some("registered") {
            return Ok(None);
        }
        Ok(r.get("id")
            .and_then(Value::as_str)
            .filter(|i| valid_job_id(i))
            .map(str::to_owned))
    }

    /// Use a separate route so an older server cannot ignore the guard.
    pub async fn issue_guarded(
        &self,
        session: &Secret,
        sid: &str,
    ) -> Result<(Secret, String), CallError> {
        let (s, _, mut v) = self
            .send(
                reqwest::Method::POST,
                &format!("/v1/spaces/{sid}/runner-token/issue-if-empty"),
                session,
                &[],
                Some(&json!({})),
                MAX_BODY,
            )
            .await?;
        if s == 409 {
            return Err(CallError::Precondition("the Space's runner changed".into()));
        }
        if s != 200 {
            return Err(Self::classify(s, &v));
        }
        let etag = v
            .get("assignment_revision")
            .and_then(Value::as_str)
            .map(|revision| format!("\"rtg-{revision}\""));
        let token = match v.get_mut("token") {
            Some(Value::String(t)) => {
                let out = Secret::new(t.as_str());
                zeroize::Zeroize::zeroize(t);
                out
            }
            _ => {
                return Err(CallError::Other("the Metro server sent no token".into()));
            }
        };
        // A dedicated token only: never a reusable (`metro_rt_v2_`) one.
        let t = token.expose();
        if !t.starts_with("metro_rt_") || t.starts_with("metro_rt_v2_") || t.len() > 256 {
            return Err(CallError::Other(
                "the Metro server sent an unexpected token".into(),
            ));
        }
        let Some(etag) = etag.filter(|e| pairing_revision(e).is_some()) else {
            return Err(CallError::Other(
                "the Metro server doesn't support guarded pairing".into(),
            ));
        };
        Ok((token, etag))
    }

    /// Revoke only a dedicated revision; never fall back to a legacy route.
    pub async fn revoke_guarded(
        &self,
        session: &Secret,
        sid: &str,
        etag: &str,
    ) -> Result<(), CallError> {
        let revision = pairing_revision(etag).ok_or_else(|| {
            CallError::Other("This local pairing has no supported revision.".into())
        })?;
        let (s, _, v) = self
            .send(
                reqwest::Method::POST,
                &format!("/v1/spaces/{sid}/runner-token/revoke-if-current"),
                session,
                &[],
                Some(&json!({"assignment_revision": revision})),
                MAX_BODY,
            )
            .await?;
        if s == 204 {
            return Ok(());
        }
        if s == 409 {
            return Err(CallError::Precondition("the Space's runner changed".into()));
        }
        Err(Self::classify(s, &v))
    }

    /// `GET /v1/runners`: the caller's own personal runners that they can
    /// manage (organization runners are left out).
    pub async fn personal_runners(&self, session: &Secret) -> Result<Vec<RunnerInfo>, CallError> {
        let (s, _, v) = self
            .send(
                reqwest::Method::GET,
                "/v1/runners",
                session,
                &[],
                None,
                4 * MAX_BODY,
            )
            .await?;
        if s != 200 {
            return Err(Self::classify(s, &v));
        }
        let Some(list) = v.get("runners").and_then(Value::as_array) else {
            return Err(CallError::Other(
                "the Metro server sent no runner list".into(),
            ));
        };
        Ok(list.iter().filter_map(RunnerInfo::personal).collect())
    }

    /// `POST /v1/runners` for a personal runner: its id and its token,
    /// which the server shows only this once.
    pub async fn create_personal_runner(
        &self,
        session: &Secret,
        name: &str,
    ) -> Result<(RunnerInfo, Secret), CreateError> {
        let lost = |error| CreateError {
            definite: false,
            error,
        };
        let mut body = json!({"name": name, "visibility": "personal", "built_in": true});
        let (s, _, mut v) = self
            .with_built_in(reqwest::Method::POST, "/v1/runners", session, &mut body)
            .await
            .map_err(lost)?;
        if s != 201 && s != 200 {
            let error = Self::classify(s, &v);
            // A 4xx refusal (other than timeouts and rate limits) created
            // nothing; anything else may have.
            let definite = (400..500).contains(&s) && !matches!(s, 408 | 425 | 429);
            return Err(CreateError { definite, error });
        }
        let token = take_registered_token(&mut v).map_err(lost)?;
        let info = v
            .get("runner")
            .and_then(RunnerInfo::personal)
            .ok_or_else(|| {
                lost(CallError::Other(
                    "the Metro server sent an unexpected runner".into(),
                ))
            })?;
        Ok((info, token))
    }

    /// `POST /v1/runners/{id}/token`: the same runner, a new token. The old
    /// one stops working at once.
    pub async fn rotate_runner(&self, session: &Secret, id: &str) -> Result<Secret, CallError> {
        if !valid_job_id(id) {
            return Err(CallError::NotFound("no such runner".into()));
        }
        let (s, _, mut v) = self
            .with_built_in(
                reqwest::Method::POST,
                &format!("/v1/runners/{id}/token"),
                session,
                &mut json!({"built_in": true}),
            )
            .await?;
        if s != 200 {
            return Err(Self::classify(s, &v));
        }
        take_registered_token(&mut v)
    }

    /// Send `body`, which marks the runner as this app's built-in one
    /// (`"built_in": true`). A server from before that field refuses it with
    /// 400 or 422 and changes nothing; then the same request goes once
    /// without it.
    async fn with_built_in(
        &self,
        method: reqwest::Method,
        path: &str,
        session: &Secret,
        body: &mut Value,
    ) -> Result<(u16, reqwest::header::HeaderMap, Value), CallError> {
        let r = self
            .send(method.clone(), path, session, &[], Some(body), MAX_BODY)
            .await?;
        if !matches!(r.0, 400 | 422) {
            return Ok(r);
        }
        if let Some(o) = body.as_object_mut() {
            o.remove("built_in");
        }
        self.send(method, path, session, &[], Some(body), MAX_BODY)
            .await
    }

    /// `POST /v1/runner/jobs/{id}/heartbeat` while a job runs: `false` when
    /// the server says the job is over (the run stopped, the lease ran out,
    /// the Space's runner changed, or it isn't this runner's job). A
    /// transport or other error is not an answer: `true`.
    pub async fn heartbeat(&self, token: &Secret, id: &str) -> bool {
        if !valid_job_id(id) {
            return false;
        }
        match self
            .send(
                reqwest::Method::POST,
                &format!("/v1/runner/jobs/{id}/heartbeat"),
                token,
                &[(PROTOCOL_HEADER, "2")],
                Some(&json!({})),
                MAX_BODY,
            )
            .await
        {
            Ok((s, _, _)) => !matches!(s, 401 | 404 | 409 | 410),
            Err(_) => true,
        }
    }

    /// `DELETE /v1/runners/{id}`.
    pub async fn delete_runner(&self, session: &Secret, id: &str) -> Result<(), CallError> {
        if !valid_job_id(id) {
            return Err(CallError::NotFound("no such runner".into()));
        }
        let (s, _, v) = self
            .send(
                reqwest::Method::DELETE,
                &format!("/v1/runners/{id}"),
                session,
                &[],
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
                // Presence for Settings → Runners: this runner updates with the app.
                Some(&json!({"runner": {
                    "version": env!("CARGO_PKG_VERSION"),
                    "channel": metro_release::Channel::current().as_str(),
                    "install": "desktop",
                    "self_update": false,
                }})),
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

/// A personal registered runner the caller manages, as `/v1/runners` shows
/// it (never with a token).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerInfo {
    pub id: String,
    pub name: String,
}

impl RunnerInfo {
    fn personal(v: &Value) -> Option<Self> {
        if v.get("visibility").and_then(Value::as_str) != Some("personal")
            || v.get("can_manage").and_then(Value::as_bool) != Some(true)
        {
            return None;
        }
        let id = v
            .get("id")
            .and_then(Value::as_str)
            .filter(|i| valid_job_id(i))?;
        let name = v.get("name").and_then(Value::as_str)?;
        Some(RunnerInfo {
            id: id.to_owned(),
            name: name.to_owned(),
        })
    }
}

/// The `token` of a registered-runner answer, wiped from the JSON. Only a
/// registered (`metro_rt_v2_`) token is accepted.
fn take_registered_token(v: &mut Value) -> Result<Secret, CallError> {
    let token = match v.get_mut("token") {
        Some(Value::String(t)) => {
            let out = Secret::new(t.as_str());
            zeroize::Zeroize::zeroize(t);
            out
        }
        _ => {
            return Err(CallError::Other("the Metro server sent no token".into()));
        }
    };
    let t = token.expose();
    if !t.starts_with("metro_rt_v2_") || t.len() > 256 || t.chars().any(|c| !c.is_ascii_graphic()) {
        return Err(CallError::Other(
            "the Metro server sent an unexpected token".into(),
        ));
    }
    Ok(token)
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::post;
    use std::sync::{Arc, Mutex};

    /// `old`: a server from before `built_in`, refusing unknown fields.
    async fn fake(old: bool) -> (Hosted, Arc<Mutex<Vec<Value>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let answer = move |seen: Arc<Mutex<Vec<Value>>>, b: Value, ok: Value| {
            seen.lock().unwrap().push(b.clone());
            if old && b.get("built_in").is_some() {
                // As axum's JSON rejection (create) or the `{}`-only check
                // (rotate) answer.
                let s = if b.get("name").is_some() { 422 } else { 400 };
                let s = reqwest::StatusCode::from_u16(s).unwrap();
                return (s, axum::Json(json!({"error": "x"})));
            }
            (reqwest::StatusCode::OK, axum::Json(ok))
        };
        let tok = "metro_rt_v2_abc";
        let (s1, s2) = (seen.clone(), seen.clone());
        let app = axum::Router::new()
            .route(
                "/v1/runners",
                post(move |axum::Json(b): axum::Json<Value>| async move {
                    answer(
                        s1,
                        b,
                        json!({"token": tok, "runner": {"id": "rn-1", "name": "n",
                        "visibility": "personal", "can_manage": true}}),
                    )
                }),
            )
            .route(
                "/v1/runners/{id}/token",
                post(move |axum::Json(b): axum::Json<Value>| async move {
                    answer(s2, b, json!({"token": tok}))
                }),
            )
            .route(
                "/v1/runner/jobs/{id}/heartbeat",
                post(
                    |axum::extract::Path(id): axum::extract::Path<String>| async move {
                        match id.as_str() {
                            "live" => reqwest::StatusCode::OK,
                            "gone" => reqwest::StatusCode::GONE,
                            "other" => reqwest::StatusCode::NOT_FOUND,
                            _ => reqwest::StatusCode::INTERNAL_SERVER_ERROR,
                        }
                    },
                ),
            );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        (Hosted::new(&format!("http://{addr}")).unwrap(), seen)
    }

    #[tokio::test]
    async fn marks_the_runner_built_in_and_still_works_with_older_servers() {
        let s = Secret::new("sess");
        for old in [false, true] {
            let (h, seen) = fake(old).await;
            let (info, _) = h.create_personal_runner(&s, "n").await.unwrap();
            assert_eq!(info.id, "rn-1");
            h.rotate_runner(&s, "rn-1").await.unwrap();
            let seen = seen.lock().unwrap().clone();
            let want: Vec<Value> = if old {
                vec![
                    json!({"name": "n", "visibility": "personal", "built_in": true}),
                    json!({"name": "n", "visibility": "personal"}),
                    json!({"built_in": true}),
                    json!({}),
                ]
            } else {
                vec![
                    json!({"name": "n", "visibility": "personal", "built_in": true}),
                    json!({"built_in": true}),
                ]
            };
            assert_eq!(seen, want, "old server: {old}");
        }
    }

    #[tokio::test]
    async fn heartbeat_ends_the_job_only_when_the_server_says_so() {
        let (h, _) = fake(false).await;
        let t = Secret::new("metro_rt_v2_abc");
        assert!(h.heartbeat(&t, "live").await);
        assert!(!h.heartbeat(&t, "gone").await);
        assert!(!h.heartbeat(&t, "other").await);
        // A server error or no answer is not a cancellation.
        assert!(h.heartbeat(&t, "boom").await);
        let unreachable = Hosted::new("http://127.0.0.1:9").unwrap();
        assert!(unreachable.heartbeat(&t, "live").await);
    }
}
