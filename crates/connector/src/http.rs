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
    Ok(client_builder(timeout).build()?)
}

/// Build a client for a single URL's origin. For loopback, don't depend on
/// the caller's NO_PROXY or on the connector CLI having initialized it.
pub(crate) fn client_for_url(timeout: Duration, url: &url::Url) -> Result<reqwest::Client> {
    let mut builder = client_builder(timeout);
    if match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        Some(url::Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
        None => false,
    } {
        builder = builder.no_proxy();
    }
    Ok(builder.build()?)
}

/// Build a client for the Metro server at `base` (already validated by
/// `netpolicy::validate_server_url`). Loopback and Tailscale hosts are
/// reached directly, never through a proxy from `HTTP_PROXY` and friends:
/// plain HTTP to a Tailscale host is only treated as encrypted while it
/// travels over the tailnet itself (see `metro-url-policy`).
pub fn client_for_server(timeout: Duration, base: &str) -> Result<reqwest::Client> {
    let mut builder = client_builder(timeout);
    if server_bypasses_proxy(base) {
        builder = builder.no_proxy();
    }
    Ok(builder.build()?)
}

fn server_bypasses_proxy(base: &str) -> bool {
    url::Url::parse(base).is_ok_and(|u| crate::netpolicy::url_host_class(&u).plain_http_ok())
}

fn client_builder(timeout: Duration) -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10).min(timeout))
        .timeout(timeout)
        .user_agent(crate::USER_AGENT)
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

#[cfg(test)]
mod server_proxy_tests {
    use super::server_bypasses_proxy;

    #[test]
    fn loopback_and_tailscale_servers_skip_proxies() {
        for direct in [
            "http://127.0.0.1:8787",
            "http://localhost:8787",
            "http://[::1]:8787",
            "http://100.101.102.103:8787",
            "https://metro.tail1234.ts.net",
            "http://[fd7a:115c:a1e0::1]:8787",
        ] {
            assert!(server_bypasses_proxy(direct), "{direct}");
        }
        for proxied in [
            "https://metro.example.com",
            "http://192.168.1.5:8787",
            "http://localhost.:8787",
            "not a url",
        ] {
            assert!(!server_bypasses_proxy(proxied), "{proxied}");
        }
    }
}
