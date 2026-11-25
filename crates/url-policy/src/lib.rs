//! How Metro clients treat a plain-HTTP **Metro server URL**.
//!
//! HTTPS is not required. Plain `http://` works to every host, and the host
//! decides how:
//!
//! - **Loopback** (`localhost`, `127.0.0.0/8`, `::1`): never leaves the
//!   machine; used silently.
//! - **Tailscale** (`100.64.0.0/10`, `fd7a:115c:a1e0::/48`, and MagicDNS
//!   names ending in `.ts.net`): Tailscale encrypts traffic between devices
//!   itself; used silently.
//! - **Anything else**: allowed, but whoever runs it is told plainly that the
//!   token crosses the network unencrypted (a log line for the runner and
//!   CLI tools; a one-time confirmation in the apps).
//!
//! This applies only to the Metro server URL a person or operator chose. It
//! is not a rule for model-provider base URLs, OAuth redirects, or the
//! loopback-only bindings of local helpers: those keep their own policies.
//!
//! The same classification is implemented for the desktop app in
//! `apps/desktop/src/api/serverUrl.ts` (`classifyHost`); keep them in step.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Where a host is, as far as an unencrypted connection is concerned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostClass {
    Loopback,
    Tailscale,
    Other,
}

impl HostClass {
    /// Plain HTTP to this host needs no warning.
    pub fn plain_http_ok(self) -> bool {
        matches!(self, Self::Loopback | Self::Tailscale)
    }
}

const TS_NET: &str = ".ts.net";

/// Classify a URL host: a domain name, an IPv4 address, or an IPv6 address
/// with or without brackets. No port. Anything unrecognised is `Other`.
///
/// The host is taken exactly as given: a trailing dot (`100.64.0.1.`,
/// `localhost.`) makes it a name that goes through the resolver, so it is
/// `Other`. Only a MagicDNS name may end in a dot (`box.tailnet.ts.net.`).
/// Shared test vectors: `testdata/host_vectors.json`.
pub fn classify_host(host: &str) -> HostClass {
    let host = host.to_ascii_lowercase();
    let bare = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(&host);
    if bare.is_empty() {
        return HostClass::Other;
    }
    if bare == "localhost" {
        return HostClass::Loopback;
    }
    if is_ts_net_name(bare.strip_suffix('.').unwrap_or(bare)) {
        return HostClass::Tailscale;
    }
    match bare.parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => classify_v4(ip),
        Ok(IpAddr::V6(ip)) => classify_v6(ip),
        Err(_) => HostClass::Other,
    }
}

/// `<label>[.<label>...].ts.net`, each label letters, digits and inner
/// hyphens (lowercase input).
fn is_ts_net_name(name: &str) -> bool {
    match name.strip_suffix(TS_NET) {
        Some(labels) => !labels.is_empty() && labels.split('.').all(valid_label),
        None => false,
    }
}

fn valid_label(label: &str) -> bool {
    !label.is_empty()
        && !label.starts_with('-')
        && !label.ends_with('-')
        && label
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Classify the authority of a URL (`host`, `host:port`, `[v6]` or
/// `[v6]:port`). Userinfo is not accepted here and makes it `Other`.
pub fn classify_authority(authority: &str) -> HostClass {
    if authority.contains('@') {
        return HostClass::Other;
    }
    let host = if let Some(rest) = authority.strip_prefix('[') {
        match rest.split_once(']') {
            Some((h, tail)) if tail.is_empty() || tail.starts_with(':') => h,
            _ => return HostClass::Other,
        }
    } else {
        match authority.rsplit_once(':') {
            Some((h, _)) if !h.contains(':') => h,
            Some(_) => return HostClass::Other, // an unbracketed IPv6 address is not a URL authority
            None => authority,
        }
    };
    classify_host(host)
}

fn classify_v4(ip: Ipv4Addr) -> HostClass {
    let [a, b, _, _] = ip.octets();
    if ip.is_loopback() {
        HostClass::Loopback
    } else if a == 100 && (64..=127).contains(&b) {
        HostClass::Tailscale // 100.64.0.0/10
    } else {
        HostClass::Other
    }
}

fn classify_v6(ip: Ipv6Addr) -> HostClass {
    if ip.is_loopback() {
        return HostClass::Loopback;
    }
    if let Some(v4) = ip.to_ipv4_mapped() {
        return classify_v4(v4);
    }
    let s = ip.segments();
    if s[0] == 0xfd7a && s[1] == 0x115c && s[2] == 0xa1e0 {
        return HostClass::Tailscale; // fd7a:115c:a1e0::/48
    }
    HostClass::Other
}

/// The warning for plain HTTP to `host` when it is neither loopback nor
/// Tailscale, or `None` (https, or a host that needs no warning). `what`
/// names the token, such as "runner token". Never contains a secret.
pub fn plain_http_warning(scheme: &str, host: &str, what: &str) -> Option<String> {
    if !scheme.eq_ignore_ascii_case("http") || classify_host(host).plain_http_ok() {
        return None;
    }
    Some(format!(
        "plain HTTP to {host} isn't encrypted: anyone on the network between this machine and {host} could read the {what}. \
         Use it only on networks you trust, such as Tailscale or a VPN, or use https://."
    ))
}

/// `NO_PROXY` entries for this machine, exact hosts in ureq 3's syntax (NSH's
/// `fetch`, and Metro runners up to 0.4.0), which matches them against the
/// URI's host as written (IPv6 in brackets, hence `[::1]` beside `::1`).
///
/// Only exact hosts: a prefix such as `127.` or `100.64.` would also send an
/// ordinary name like `100.64.office.example` around the operator's proxy.
/// So a Tailscale server is reached directly only when the person's own
/// `NO_PROXY` lists it (or no proxy is set); otherwise the apps treat it as
/// proxied and ask before sending a token over plain HTTP.
pub fn loopback_no_proxy_entries() -> Vec<String> {
    ["localhost", "127.0.0.1", "::1", "[::1]"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// The proxy variables ureq 3 reads (the first set one wins).
pub const PROXY_VARS: [&str; 6] = [
    "ALL_PROXY",
    "all_proxy",
    "HTTPS_PROXY",
    "https_proxy",
    "HTTP_PROXY",
    "http_proxy",
];

/// How a ureq 3 client in this environment routes requests, as non-secret
/// metadata for a process that must not see the proxy URLs themselves (they
/// can carry credentials): `proxied` when a proxy variable is set; `unknown`
/// on Windows otherwise (a system proxy may apply that the environment
/// doesn't show); `direct` otherwise. Hosts matched by the `NO_PROXY` value
/// are direct in every case.
pub fn proxy_route(get: impl Fn(&str) -> Option<String>, windows: bool) -> &'static str {
    if PROXY_VARS
        .iter()
        .any(|k| get(k).is_some_and(|v| !v.is_empty()))
    {
        "proxied"
    } else if windows {
        "unknown"
    } else {
        "direct"
    }
}

/// Whether ureq would skip the proxy, for a canonical ASCII `uri_host`
/// (IPv6 in brackets, no port). Source: `NoProxy::try_from_env` and
/// `NoProxyEntry` in ureq 3.4.2 `src/proxy.rs` (NSH 0.6.1). Those routines
/// are identical in ureq 3.5.2, whose public matcher checks the shared cases.
pub fn ureq_no_proxy_matches(no_proxy: &str, uri_host: &str) -> bool {
    let host = uri_host.to_ascii_lowercase();
    no_proxy.split(',').any(|entry| {
        let e = entry.to_ascii_lowercase();
        if e == "*" {
            true
        } else if let Some(suffix) = e.strip_prefix('*') {
            host.ends_with(suffix)
        } else if e.starts_with('.') {
            host.ends_with(&e)
        } else if let Some(prefix) = e.strip_suffix('*') {
            host.starts_with(prefix)
        } else if e.ends_with('.') {
            host.starts_with(&e)
        } else {
            host == e
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use HostClass::*;

    #[test]
    fn loopback() {
        for h in [
            "localhost",
            "LOCALHOST",
            "127.0.0.1",
            "127.255.0.9",
            "::1",
            "[::1]",
            "0:0:0:0:0:0:0:1",
            "::ffff:127.0.0.1",
            "[::ffff:7f00:1]",
        ] {
            assert_eq!(classify_host(h), Loopback, "{h}");
        }
    }

    #[test]
    fn tailscale() {
        for h in [
            "100.64.0.0",
            "100.64.0.1",
            "100.101.102.103",
            "100.127.255.255",
            "fd7a:115c:a1e0::1",
            "[fd7a:115c:a1e0::53]",
            "FD7A:115C:A1E0:AB12:4843:CD96:6258:B240",
            "::ffff:100.100.100.100",
            "metro.tail1234.ts.net",
            "Metro.Tail-Net.TS.NET",
            "metro.tail1234.ts.net.",
        ] {
            assert_eq!(classify_host(h), Tailscale, "{h}");
        }
    }

    #[test]
    fn other() {
        for h in [
            "",
            "localhost.",
            "127.0.0.1.",
            "100.64.0.1.",
            "[100.64.0.1.]",
            "a-.ts.net",
            "-a.ts.net",
            "a_b.ts.net",
            "100.63.255.255",
            "100.128.0.0",
            "99.64.0.1",
            "10.0.0.1",
            "192.168.1.5",
            "0.0.0.0",
            "fd7a:115c:a1e1::1",
            "fd7a:115c::1",
            "fd7b:115c:a1e0::1",
            "ts.net",
            ".ts.net",
            "a..ts.net",
            "evilts.net",
            "metro.ts.net.evil.com",
            "localhost.evil.com",
            "foo.localhost",
            "127.0.0.1.evil.com",
            "metro.example.com",
            "::ffff:c0a8:1",
            "not a host",
        ] {
            assert_eq!(classify_host(h), Other, "{h:?}");
        }
    }

    #[test]
    fn authorities() {
        assert_eq!(classify_authority("127.0.0.1:8787"), Loopback);
        assert_eq!(classify_authority("localhost"), Loopback);
        assert_eq!(classify_authority("[::1]:8787"), Loopback);
        assert_eq!(classify_authority("[::1]"), Loopback);
        assert_eq!(classify_authority("100.100.1.2:8787"), Tailscale);
        assert_eq!(classify_authority("[fd7a:115c:a1e0::1]:80"), Tailscale);
        assert_eq!(classify_authority("box.tailnet.ts.net:8787"), Tailscale);
        assert_eq!(classify_authority("192.168.1.5:8787"), Other);
        assert_eq!(classify_authority("::1"), Other);
        assert_eq!(classify_authority("[::1]x"), Other);
        assert_eq!(classify_authority("user@127.0.0.1"), Other);
    }

    #[test]
    fn shared_vectors() {
        let doc = vectors();
        let hosts = doc["hosts"].as_array().unwrap();
        assert!(hosts.len() >= 40);
        for v in hosts {
            let host = v["host"].as_str().unwrap();
            let want = match v["class"].as_str().unwrap() {
                "loopback" => Loopback,
                "tailscale" => Tailscale,
                "other" => Other,
                c => panic!("unknown class {c}"),
            };
            assert_eq!(classify_host(host), want, "{host:?}");
        }
    }

    fn vectors() -> serde_json::Value {
        serde_json::from_str(include_str!("../testdata/host_vectors.json")).unwrap()
    }

    #[test]
    fn ureq_no_proxy_cases() {
        let doc = vectors();
        let cases = doc["no_proxy"].as_array().unwrap();
        assert!(cases.len() >= 10);
        for c in cases {
            let (list, host) = (c["no_proxy"].as_str().unwrap(), c["host"].as_str().unwrap());
            let want = c["match"].as_bool().unwrap();
            assert_eq!(
                ureq_no_proxy_matches(list, host),
                want,
                "{list:?} / {host:?}"
            );
            // Same entries as NoProxy::try_from_env, without modifying the
            // process environment while other tests are running.
            let mut builder = ureq::Proxy::builder(ureq::ProxyProtocol::Http).host("proxy.invalid");
            for entry in list.split(',') {
                builder = builder.no_proxy(entry);
            }
            let uri: ureq::http::Uri = format!("http://{host}:8787/").parse().unwrap();
            assert_eq!(uri.host(), Some(host));
            assert_eq!(
                builder.build().unwrap().is_no_proxy(&uri),
                want,
                "ureq: {list:?} / {host:?}"
            );
        }
    }

    #[test]
    fn loopback_entries_are_exact_and_cover_loopback_only() {
        let list = loopback_no_proxy_entries().join(",");
        let doc = vectors();
        let listed: Vec<String> = doc["launcher_no_proxy_entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            listed,
            loopback_no_proxy_entries(),
            "update host_vectors.json"
        );
        for h in doc["loopback_uri_hosts"].as_array().unwrap() {
            let h = h.as_str().unwrap();
            assert_eq!(classify_host(h), HostClass::Loopback, "{h}");
            assert!(
                ureq_no_proxy_matches(&list, h),
                "{h} would go through a proxy"
            );
        }
        // No prefixes: ordinary names that start like a trusted range, and
        // Tailscale hosts the person didn't list, keep the proxy.
        for h in doc["not_exempted_by_default"].as_array().unwrap() {
            let h = h.as_str().unwrap();
            assert!(!ureq_no_proxy_matches(&list, h), "{h} would skip the proxy");
        }
    }

    #[test]
    fn route() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| v.to_string())
            }
        };
        assert_eq!(proxy_route(env(&[]), false), "direct");
        assert_eq!(proxy_route(env(&[]), true), "unknown");
        assert_eq!(proxy_route(env(&[("HTTP_PROXY", "")]), false), "direct");
        for k in PROXY_VARS {
            let pairs: &'static [(&'static str, &'static str)] =
                Box::leak(vec![(k, "http://user:secret@proxy:3128")].into_boxed_slice());
            assert_eq!(proxy_route(env(pairs), false), "proxied", "{k}");
            assert_eq!(proxy_route(env(pairs), true), "proxied", "{k}");
        }
    }

    #[test]
    fn warning() {
        assert_eq!(plain_http_warning("https", "192.168.1.5", "token"), None);
        assert_eq!(plain_http_warning("http", "127.0.0.1", "token"), None);
        assert_eq!(
            plain_http_warning("http", "box.tailnet.ts.net", "token"),
            None
        );
        assert_eq!(plain_http_warning("HTTP", "100.64.1.1", "token"), None);
        let w = plain_http_warning("http", "192.168.1.5", "runner token").unwrap();
        assert!(
            w.contains("192.168.1.5")
                && w.contains("runner token")
                && w.contains("isn't encrypted")
        );
    }
}
