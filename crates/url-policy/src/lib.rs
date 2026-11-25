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
        let doc: serde_json::Value =
            serde_json::from_str(include_str!("../testdata/host_vectors.json")).unwrap();
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
