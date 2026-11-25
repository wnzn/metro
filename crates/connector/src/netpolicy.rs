//! Which URLs the connector may talk to.
//!
//! Both the Metro server URL and a custom provider base URL are set by the
//! person on this machine, never by Metro. No userinfo, query or fragment,
//! and redirects are never followed (see `http.rs`). The schemes differ:
//!
//! - The **Metro server URL** (`validate_server_url`) may be `https://` or
//!   plain `http://` to any host. Loopback and Tailscale hosts are used
//!   silently; any other plain-HTTP host gets one clear warning
//!   (`server_plain_http_warning`), because the device token crosses the
//!   network unencrypted. See the `metro-url-policy` crate.
//! - A **provider base URL** (`validate_base_url`) must be `https://`, or
//!   plain `http://` to a loopback host (`localhost`, `127.0.0.0/8`, `::1`).
//!   That rule is about where model keys and prompts may go and is unchanged.

use std::net::IpAddr;

use anyhow::{Result, bail};
use url::{Host, Url};

/// Validate and normalize a provider base URL (no trailing slash): https, or
/// http to loopback only.
pub fn validate_base_url(raw: &str, what: &str) -> Result<String> {
    let (url, host) = parse_url(raw_input(raw, what)?, what)?;
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

/// Validate and normalize a Metro server URL (no trailing slash): https, or
/// plain http to any host. Callers that start talking to it report
/// `server_plain_http_warning` once.
pub fn validate_server_url(raw: &str, what: &str) -> Result<String> {
    normalize_server_url(raw_input(raw, what)?, what)
}

/// Check a saved Metro server URL without reapplying the raw-input limit:
/// percent encoding can expand a valid URL. The record reader bounds its size.
pub(crate) fn validate_canonical_server_url(server: &str, what: &str) -> Result<()> {
    if normalize_server_url(server, what)? != server {
        bail!("{what} is not normalized");
    }
    Ok(())
}

fn normalize_server_url(input: &str, what: &str) -> Result<String> {
    let (url, _) = parse_url(input, what)?;
    match url.scheme() {
        "https" | "http" => {}
        other => bail!("{what}: scheme {other:?} is not supported; use https:// or http://"),
    }
    Ok(url.as_str().trim_end_matches('/').to_string())
}

/// One warning line for a Metro server URL that uses plain HTTP to a host
/// that is neither loopback nor Tailscale; `None` otherwise. Never contains
/// a secret.
pub fn server_plain_http_warning(server_url: &str, what: &str) -> Option<String> {
    let url = Url::parse(server_url).ok()?;
    let host = url.host_str()?;
    metro_url_policy::plain_http_warning(url.scheme(), host, what)
}

/// Loopback, Tailscale or other, for an http(s) URL's host.
pub fn url_host_class(url: &Url) -> metro_url_policy::HostClass {
    url.host_str().map_or(
        metro_url_policy::HostClass::Other,
        metro_url_policy::classify_host,
    )
}

fn raw_input<'a>(raw: &'a str, what: &str) -> Result<&'a str> {
    let raw = raw.trim();
    if raw.len() > 2048 {
        bail!("{what}: enter a URL like https://metro.example.com");
    }
    Ok(raw)
}

fn parse_url(input: &str, what: &str) -> Result<(Url, Host<String>)> {
    if input.is_empty() || input.chars().any(|c| c.is_control()) {
        bail!("{what}: enter a URL like https://metro.example.com");
    }
    let url = match Url::parse(input) {
        Ok(u) => u,
        // The input may contain credentials, including when parsing fails.
        Err(e) => bail!("{what}: invalid URL ({e})"),
    };
    if !url.username().is_empty() || url.password().is_some() {
        bail!("{what}: the URL must not contain a user name or password");
    }
    if url.query().is_some() || url.fragment().is_some() {
        bail!("{what}: the URL must not contain ? or #");
    }
    let host = match url.host() {
        Some(h) => h.to_owned(),
        None => bail!("{what}: the URL has no host"),
    };
    Ok((url, host))
}

pub fn is_loopback<S: AsRef<str>>(host: &Host<S>) -> bool {
    match host {
        Host::Domain(d) => d.as_ref().eq_ignore_ascii_case("localhost"),
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
        // Provider base URLs keep the loopback-only rule for plain http,
        // Tailscale included.
        for bad in [
            "http://100.101.102.103:11434/v1",
            "http://box.tailnet.ts.net/v1",
            "http://192.168.1.5:11434/v1",
        ] {
            assert!(validate_base_url(bad, "x").is_err(), "{bad:?}");
        }
    }

    #[test]
    fn server_urls() {
        for (raw, norm) in [
            ("https://metro.example.com/", "https://metro.example.com"),
            ("http://127.0.0.1:8787/", "http://127.0.0.1:8787"),
            ("http://100.101.102.103:8787", "http://100.101.102.103:8787"),
            (
                "http://metro.tail1234.ts.net/",
                "http://metro.tail1234.ts.net",
            ),
            (
                "http://192.168.1.5:8787/metro/",
                "http://192.168.1.5:8787/metro",
            ),
            ("http://metro.example.com", "http://metro.example.com"),
        ] {
            assert_eq!(validate_server_url(raw, "x").unwrap(), norm, "{raw}");
        }
        for bad in [
            "",
            "metro.example.com",
            "http://user:pw@192.168.1.5",
            "http://192.168.1.5/?a=1",
            "http://192.168.1.5/#x",
            "ftp://metro.example.com",
            "file:///etc/passwd",
            "http://metro\n.example.com",
        ] {
            assert!(validate_server_url(bad, "x").is_err(), "{bad:?}");
        }
    }

    #[test]
    fn server_warnings() {
        for quiet in [
            "https://192.168.1.5",
            "http://127.0.0.1:8787",
            "http://localhost:8787",
            "http://[::1]:8787",
            "http://100.64.0.7:8787",
            "http://[fd7a:115c:a1e0::9]:8787",
            "http://metro.tail1234.ts.net",
        ] {
            assert_eq!(
                server_plain_http_warning(quiet, "device token"),
                None,
                "{quiet}"
            );
        }
        // The url crate turns 100.64.0.1. into the IP literal, and the
        // request goes to that address directly; localhost. stays a name.
        assert_eq!(
            validate_server_url("http://100.64.0.1.:8787", "x").unwrap(),
            "http://100.64.0.1:8787"
        );
        assert_eq!(
            server_plain_http_warning("http://100.64.0.1.:8787", "t"),
            None
        );
        for loud in [
            "http://localhost.:8787",
            "http://foo.localhost:8787",
            "http://192.168.1.5:8787",
            "http://metro.example.com",
            "http://100.128.0.1",
        ] {
            let w = server_plain_http_warning(loud, "device token").unwrap();
            assert!(
                w.contains("isn't encrypted") && w.contains("device token"),
                "{w}"
            );
        }
    }
}
