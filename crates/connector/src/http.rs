//! Shared outbound HTTP helpers: no redirects, timeouts, bounded bodies.

use std::time::Duration;

use anyhow::{Result, bail};
use serde_json::Value;

use crate::SafeError;

/// Loopback hosts never go through a proxy: a proxy from `HTTP_PROXY` and
/// friends must not see bearers meant for a server on this machine.
pub const LOOPBACK_NO_PROXY: [&str; 3] = ["127.0.0.1", "localhost", "::1"];

/// `NO_PROXY` (or `no_proxy`) with the loopback hosts added.
pub fn no_proxy_with_loopback() -> String {
    let current = std::env::var("NO_PROXY")
        .or_else(|_| std::env::var("no_proxy"))
        .unwrap_or_default();
    let mut parts: Vec<String> = current
        .split(',')
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect();
    if !parts.iter().any(|p| p == "*") {
        for h in LOOPBACK_NO_PROXY {
            if !parts.iter().any(|p| p.eq_ignore_ascii_case(h)) {
                parts.push(h.to_string());
            }
        }
    }
    parts.join(",")
}

/// Make every HTTP client this process builds skip proxies for loopback.
///
/// # Safety
///
/// Changes the process environment: call it first thing in `main`, before
/// any other thread exists.
pub unsafe fn exempt_loopback_from_proxy() {
    let v = no_proxy_with_loopback();
    // SAFETY: the caller guarantees no other thread runs yet.
    unsafe {
        std::env::set_var("NO_PROXY", &v);
        std::env::set_var("no_proxy", &v);
    }
}

/// Build a client that never follows redirects (a redirect could carry a
/// bearer token or key to another origin) and gives up after `timeout`.
pub fn client(timeout: Duration) -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10).min(timeout))
        .timeout(timeout)
        .user_agent(crate::USER_AGENT)
        .build()?)
}

/// Read at most `cap` bytes of a response body. A longer body is an error,
/// and nothing past the cap is buffered.
pub async fn read_capped(mut resp: reqwest::Response, cap: usize) -> Result<Vec<u8>> {
    if let Some(len) = resp.content_length()
        && len > cap as u64
    {
        bail!(SafeError::new(format!(
            "response is larger than {cap} bytes"
        )));
    }
    let mut buf = Vec::new();
    while let Some(chunk) = resp.chunk().await? {
        if buf.len() + chunk.len() > cap {
            bail!(SafeError::new(format!(
                "response is larger than {cap} bytes"
            )));
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

/// Read a capped JSON body.
pub async fn read_json(resp: reqwest::Response, cap: usize) -> Result<Value> {
    let bytes = read_capped(resp, cap).await?;
    match serde_json::from_slice(&bytes) {
        Ok(v) => Ok(v),
        Err(_) => bail!(SafeError::new("response is not valid JSON")),
    }
}
