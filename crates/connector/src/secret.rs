//! Secret values and safe error text.
//!
//! A [`Secret`] never prints its value through `Debug` or `Display` and is
//! wiped from memory on drop. A [`SafeError`] is the only error text that
//! leaves this machine (in a failed job result). Everything else Metro sees
//! is a fixed, generic message.

use std::fmt;

use zeroize::Zeroize;

/// A token or key. `Debug`/`Display` print `<redacted>`.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Secret(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// Maximum length of an error string sent to Metro.
pub const MAX_SAFE_ERROR: usize = 200;

/// An error whose text is safe to send to Metro: short, single line, written
/// by this crate (or an adapter) without request or response payloads,
/// tokens or keys.
///
/// Adapters return `Err(SafeError::new("...").into())` from their
/// `anyhow::Result` functions when the person should see the reason in
/// Metro. Any other error is reported to Metro as a generic message and the
/// detail stays in the local log (redacted).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SafeError(String);

impl SafeError {
    pub fn new(msg: impl AsRef<str>) -> Self {
        SafeError(clean_line(msg.as_ref(), MAX_SAFE_ERROR))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SafeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SafeError {}

/// One line, no control characters, at most `max` characters.
pub fn clean_line(s: &str, max: usize) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if out.chars().count() >= max {
            break;
        }
        out.push(if c.is_control() { ' ' } else { c });
    }
    out.trim().to_string()
}

/// The message sent to Metro for an error: the `SafeError` text if the chain
/// holds one, else a generic message.
pub fn metro_error_text(err: &anyhow::Error) -> String {
    for cause in err.chain() {
        if let Some(safe) = cause.downcast_ref::<SafeError>() {
            return safe.0.clone();
        }
    }
    "the connector could not complete this request (see the connector log on the device)".into()
}

/// Replace every occurrence of a known secret, plus anything shaped like a
/// bearer credential, with `<redacted>`. Used for local log lines.
pub fn redact(text: &str, secrets: &[&Secret]) -> String {
    let mut out = text.to_string();
    for s in secrets {
        let v = s.expose();
        if v.len() >= 4 {
            out = out.replace(v, "<redacted>");
        }
    }
    redact_token_shapes(&out)
}

/// Prefixes of credentials that must never appear in a log line even when the
/// connector doesn't know the value (for example a vendor token echoed back
/// in an error).
const TOKEN_PREFIXES: &[&str] = &[
    "metro_pc_",
    "metro_rt_",
    "sk-",
    "sess-",
    "eyJ",
    "Bearer ",
    "bearer ",
];

fn redact_token_shapes(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    'outer: while !rest.is_empty() {
        for p in TOKEN_PREFIXES {
            if rest.starts_with(p) {
                let tail = &rest[p.len()..];
                let n = tail
                    .find(|c: char| !(c.is_ascii_alphanumeric() || "-_.~+/=".contains(c)))
                    .unwrap_or(tail.len());
                if n >= 6 {
                    out.push_str("<redacted>");
                    rest = &tail[n..];
                    continue 'outer;
                }
            }
        }
        let c = rest.chars().next().unwrap();
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    out
}

/// A local log line for an error: redacted and bounded.
pub fn log_text(err: &anyhow::Error, secrets: &[&Secret]) -> String {
    clean_line(&redact(&format!("{err:#}"), secrets), 400)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_never_prints() {
        let s = Secret::new("metro_pc_abcdef123456");
        assert_eq!(format!("{s:?}"), "Secret(<redacted>)");
        assert_eq!(format!("{s}"), "<redacted>");
    }

    #[test]
    fn redaction() {
        let k = Secret::new("my-local-key-123");
        let t = redact(
            "failed with my-local-key-123 and Bearer abcdefghijk and metro_pc_zzzzzzzz1 sk-proj-1234567 ok",
            &[&k],
        );
        assert!(!t.contains("my-local-key-123"), "{t}");
        assert!(!t.contains("abcdefghijk"), "{t}");
        assert!(!t.contains("metro_pc_zzzz"), "{t}");
        assert!(!t.contains("sk-proj"), "{t}");
        assert!(t.ends_with(" ok"), "{t}");
        // Short or ordinary words are left alone.
        assert_eq!(redact("sk- is fine, eyJ too", &[]), "sk- is fine, eyJ too");
    }

    #[test]
    fn safe_error_is_short_single_line() {
        let e = SafeError::new(format!("a\nb\r{}", "x".repeat(500)));
        assert!(!e.as_str().contains('\n'));
        assert!(e.as_str().chars().count() <= MAX_SAFE_ERROR);
        let any: anyhow::Error = anyhow::Error::new(e.clone()).context("outer detail");
        assert_eq!(metro_error_text(&any), e.as_str());
        let generic = anyhow::anyhow!("secret payload sk-abcdefgh");
        assert!(!metro_error_text(&generic).contains("sk-"));
    }
}
