//! Which URLs the connector may talk to.
//!
//! Both the Metro server URL and a custom provider base URL are set by the
//! person on this machine, never by Metro. Each must be `https://`, or plain
//! `http://` to a loopback host (`localhost`, `127.0.0.0/8`, `::1`). No
//! userinfo, query or fragment. Redirects are never followed (see `http.rs`).

use std::net::IpAddr;

use anyhow::{Result, bail};
use url::{Host, Url};

/// Validate and normalize a base URL (no trailing slash).
pub fn validate_base_url(raw: &str, what: &str) -> Result<String> {
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > 2048 || raw.chars().any(|c| c.is_control()) {
        bail!("{what}: enter a URL like https://metro.example.com");
    }
    let url = match Url::parse(raw) {
        Ok(u) => u,
        Err(_) => bail!("{what}: {raw:?} is not a valid URL"),
    };
    if !url.username().is_empty() || url.password().is_some() {
        bail!("{what}: the URL must not contain a user name or password");
    }
    if url.query().is_some() || url.fragment().is_some() {
        bail!("{what}: the URL must not contain ? or #");
    }
    let host = match url.host() {
        Some(h) => h,
        None => bail!("{what}: the URL has no host"),
    };
    match url.scheme() {
        "https" => {}
        "http" if is_loopback(&host) => {}
        "http" => bail!(
            "{what}: plain http is only allowed to this machine (localhost, 127.0.0.1, ::1); use https"
        ),
        other => bail!("{what}: scheme {other:?} is not supported; use https"),
    }
    Ok(url.as_str().trim_end_matches('/').to_string())
}

pub fn is_loopback(host: &Host<&str>) -> bool {
    match host {
        Host::Domain(d) => d.eq_ignore_ascii_case("localhost"),
        Host::Ipv4(ip) => IpAddr::V4(*ip).is_loopback(),
        Host::Ipv6(ip) => IpAddr::V6(*ip).is_loopback(),
    }
}

/// True for an http(s) URL whose host is loopback.
pub fn url_is_loopback(base: &str) -> bool {
    Url::parse(base)
        .ok()
        .and_then(|u| u.host().map(|h| is_loopback(&h)))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        assert_eq!(
            validate_base_url("https://metro.example.com/", "x").unwrap(),
            "https://metro.example.com"
        );
        assert_eq!(
            validate_base_url("http://127.0.0.1:11434/v1", "x").unwrap(),
            "http://127.0.0.1:11434/v1"
        );
        for ok in [
            "http://localhost:8787",
            "http://[::1]:8080",
            "http://127.5.0.1",
        ] {
            validate_base_url(ok, "x").unwrap();
        }
        for bad in [
            "",
            "metro.example.com",
            "http://metro.example.com",
            "http://10.0.0.2:8787",
            "http://localhost.evil.com",
            "https://user:pw@metro.example.com",
            "https://metro.example.com/?a=1",
            "https://metro.example.com/#x",
            "ftp://metro.example.com",
            "file:///etc/passwd",
            "https://metro\n.example.com",
        ] {
            assert!(validate_base_url(bad, "x").is_err(), "{bad:?}");
        }
    }
}
