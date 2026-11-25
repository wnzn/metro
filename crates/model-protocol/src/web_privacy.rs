//! Which hosts a web search or fetch must never name, shared by Metro's
//! server (`web_tools`) and the device (`metro-connector`'s `web`), so both
//! sides refuse exactly the same URLs.
//!
//! The vendor fetches the page, so this is about privacy, not SSRF: a
//! private address or an intranet name would leak to a third party. A host
//! is private when it is an IP literal in a non-public range (including
//! IPv6 forms that embed a private IPv4 address) or a name that only
//! resolves locally.

use std::net::{Ipv4Addr, Ipv6Addr};

/// Whether `ip` is loopback, private, link-local, CGNAT (Tailscale),
/// benchmarking, documentation, reserved, multicast or broadcast.
pub fn private_ipv4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_documentation()
        // 0.0.0.0/8 "this network".
        || o[0] == 0
        // 100.64.0.0/10 CGNAT, including Tailscale addresses.
        || (o[0] == 100 && (64..=127).contains(&o[1]))
        // 192.0.0.0/24 IETF protocol assignments.
        || (o[0] == 192 && o[1] == 0 && o[2] == 0)
        // 198.18.0.0/15 benchmarking.
        || (o[0] == 198 && (o[1] & 0xfe) == 18)
        // 240.0.0.0/4 reserved.
        || o[0] >= 240
}

/// Whether `ip` is loopback, unspecified, multicast, unique local, link-local,
/// site-local, documentation, NAT64 (well-known or local-use), or a form
/// that embeds a private IPv4 address (mapped, compatible, 6to4).
pub fn private_ipv6(ip: Ipv6Addr) -> bool {
    let s = ip.segments();
    let embedded = |hi: u16, lo: u16| {
        private_ipv4(Ipv4Addr::new(
            (hi >> 8) as u8,
            hi as u8,
            (lo >> 8) as u8,
            lo as u8,
        ))
    };
    if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() {
        return true;
    }
    // ::ffff:a.b.c.d (mapped) and ::a.b.c.d (deprecated compatible).
    if s[..5] == [0; 5] && (s[5] == 0xffff || s[5] == 0) {
        return embedded(s[6], s[7]);
    }
    // 64:ff9b::/96 well-known NAT64 and 64:ff9b:1::/48 local-use NAT64:
    // a translator on the asker's own network.
    if s[0] == 0x0064 && s[1] == 0xff9b && (s[2..6] == [0; 4] || s[2] == 1) {
        return true;
    }
    // 2002::/16 6to4, embedding an IPv4 address in bits 16..48.
    if s[0] == 0x2002 && embedded(s[1], s[2]) {
        return true;
    }
    // fc00::/7 unique local, fe80::/10 link-local, fec0::/10 site-local.
    (s[0] & 0xfe00) == 0xfc00
        || (s[0] & 0xffc0) == 0xfe80
        || (s[0] & 0xffc0) == 0xfec0
        // 2001:db8::/32 documentation.
        || (s[0] == 0x2001 && s[1] == 0x0db8)
}

/// Whether a host name (lowercase, without brackets) only makes sense on a
/// local network: one label, `localhost`, or a local-only suffix.
pub fn private_name(host: &str) -> bool {
    let host = host.trim_end_matches('.');
    !host.contains('.')
        || host == "localhost"
        || [
            ".localhost",
            ".local",
            ".internal",
            ".lan",
            ".home.arpa",
            ".intranet",
            ".corp",
        ]
        .iter()
        .any(|s| host.ends_with(s))
}

/// URLs both sides must refuse. For tests: each side runs its own URL check
/// over this table.
#[doc(hidden)]
pub const PRIVATE_URL_SAMPLES: &[&str] = &[
    "http://localhost:8080",
    "http://127.0.0.1",
    "http://10.1.2.3",
    "http://192.168.1.1",
    "http://172.16.0.1",
    "http://169.254.169.254/latest",
    "http://100.100.100.100",
    "http://0.0.0.0/",
    "http://192.0.0.8/",
    "http://198.18.0.1/",
    "http://198.19.255.1/",
    "http://240.0.0.1/",
    "http://255.255.255.255/",
    "http://[::1]/",
    "http://[fd7a:115c:a1e0::1]/",
    "http://[fe80::1]/",
    "http://[fec0::1]/",
    "http://[::ffff:10.0.0.1]/",
    "http://[::10.0.0.1]/",
    "http://[64:ff9b::808:808]/",
    "http://[64:ff9b:1::1]/",
    "http://[2002:c0a8:101::1]/",
    "http://[2002:7f00:1::]/",
    "http://[2001:db8::1]/",
    "http://intranet/",
    "http://printer.local/",
    "http://db.internal/",
    "http://nas.home.arpa/",
    "http://router.lan/",
];

/// URLs both sides must accept.
#[doc(hidden)]
pub const PUBLIC_URL_SAMPLES: &[&str] = &[
    "https://example.com",
    "http://news.example.org/x?y=1",
    "https://8.8.8.8/",
    "https://198.20.0.1/",
    "https://[2606:4700::1]/",
    "https://[2002:808:808::1]/",
    "https://[::ffff:8.8.8.8]/",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges() {
        for ip in [
            "198.18.0.0",
            "198.19.255.255",
            "240.0.0.1",
            "192.0.0.1",
            "0.1.2.3",
        ] {
            assert!(private_ipv4(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["198.17.255.255", "198.20.0.0", "239.255.255.254", "1.1.1.1"] {
            // 239/8 is multicast (private), the rest public.
            assert_eq!(
                private_ipv4(ip.parse().unwrap()),
                ip.starts_with("239."),
                "{ip}"
            );
        }
        for ip in [
            "fec0::1",
            "feff::1",
            "64:ff9b::1.1.1.1",
            "64:ff9b:1:2::3",
            "2002:0a00:0001::",
            "::192.168.0.1",
            "::ffff:127.0.0.1",
        ] {
            assert!(private_ipv6(ip.parse().unwrap()), "{ip}");
        }
        for ip in [
            "2002:0808:0808::",
            "::ffff:1.1.1.1",
            "2606:4700::1",
            "64:ff9c::1",
        ] {
            assert!(!private_ipv6(ip.parse().unwrap()), "{ip}");
        }
    }
}
