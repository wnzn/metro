//! Bring-your-own-key adapter for any OpenAI-compatible endpoint
//! (`device-openai-compatible`).
//!
//! The base URL and the optional key live only in the local profile. Requests
//! go straight from this machine to that URL; the key is sent only there,
//! as `Authorization: Bearer`, and never to Metro. With no key saved, no
//! `Authorization` header is sent at all (for local servers such as
//! `http://127.0.0.1:11434/v1`). Redirects are not followed and every body is
//! size-limited.

use std::time::Duration;

use anyhow::{Result, bail};
use serde_json::Value;

use crate::http::{client, read_json};
use crate::{SafeError, Secret};

/// Largest model list we read.
pub const MAX_MODELS_BODY: usize = 2 * 1024 * 1024;
/// Largest completion response we read.
pub const MAX_COMPLETION_BODY: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Endpoint {
    /// Validated base URL without trailing slash, e.g. `https://api.example.com/v1`.
    pub base_url: String,
    pub key: Option<Secret>,
    pub timeout: Duration,
}

impl Endpoint {
    fn request(
        &self,
        http: &reqwest::Client,
        method: reqwest::Method,
        path: &str,
    ) -> reqwest::RequestBuilder {
        let mut rb = http
            .request(method, format!("{}{}", self.base_url, path))
            .header("Accept", "application/json");
        if let Some(k) = self.key.as_ref().filter(|k| !k.is_empty()) {
            rb = rb.bearer_auth(k.expose());
        }
        rb
    }
}

fn status_error(status: reqwest::StatusCode) -> anyhow::Error {
    let hint = match status.as_u16() {
        401 | 403 => " (check the API key saved on the device)",
        404 => " (check the base URL saved on the device)",
        429 => " (rate limited by the provider)",
        300..=399 => " (redirects are not followed; check the base URL)",
        _ => "",
    };
    SafeError::new(format!("provider returned HTTP {}{hint}", status.as_u16())).into()
}

fn send_error(e: reqwest::Error) -> anyhow::Error {
    let what = if e.is_timeout() {
        "provider request timed out"
    } else if e.is_connect() {
        "cannot reach the provider from the device"
    } else {
        "provider request failed"
    };
    anyhow::Error::new(SafeError::new(what)).context(e.without_url())
}

/// `GET {base}/models` → model ids.
pub async fn models(ep: &Endpoint) -> Result<Vec<String>> {
    let http = client(ep.timeout.min(Duration::from_secs(60)))?;
    let resp = ep
        .request(&http, reqwest::Method::GET, "/models")
        .send()
        .await
        .map_err(send_error)?;
    if !resp.status().is_success() {
        return Err(status_error(resp.status()));
    }
    let v = read_json(resp, MAX_MODELS_BODY).await?;
    let data = v
        .get("data")
        .or_else(|| v.get("models"))
        .and_then(Value::as_array)
        .ok_or_else(|| SafeError::new("provider model list has no 'data' array"))?;
    let mut out = Vec::new();
    for m in data.iter().take(4096) {
        let id = m
            .get("id")
            .or_else(|| m.get("name"))
            .and_then(Value::as_str)
            .or_else(|| m.as_str());
        if let Some(id) = id
            && crate::profile::validate_model_name(id).is_ok()
            && !out.iter().any(|x: &String| x == id)
        {
            out.push(id.to_string());
        }
    }
    Ok(out)
}

/// `POST {base}/chat/completions` with an already sanitized request.
pub async fn complete(ep: &Endpoint, request: Value) -> Result<Value> {
    if !request.is_object() {
        bail!(SafeError::new("invalid request"));
    }
    let mut request = request;
    request["stream"] = Value::Bool(false);
    let http = client(ep.timeout)?;
    let resp = ep
        .request(&http, reqwest::Method::POST, "/chat/completions")
        .header("Content-Type", "application/json")
        .body(serde_json::to_vec(&request)?)
        .send()
        .await
        .map_err(send_error)?;
    if !resp.status().is_success() {
        return Err(status_error(resp.status()));
    }
    read_json(resp, MAX_COMPLETION_BODY).await
}
