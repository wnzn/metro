//! One error type with stable codes for UIs and logs (no paths, no secrets).

use std::fmt;

/// Stable error codes shown to people (via the UI's wording) and logs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    /// No network, DNS failure, timeout, connection reset.
    Offline,
    /// GitHub answered 403/429 with a rate limit.
    RateLimited,
    /// Signature, identity, GitHub claims or a file hash didn't verify.
    NotVerified,
    /// The bundle's trust material is newer than this build's trust root.
    TrustRootStale,
    /// Files of one release disagree (manifest vs bundle, archive vs manifest).
    Inconsistent,
    /// A redirect left the allowed HTTPS hosts.
    RedirectRefused,
    /// A response or archive exceeded its size cap.
    TooLarge,
    /// An unexpected HTTP status.
    Http,
    /// The signed manifest isn't a complete stable release (or is malformed).
    Manifest,
    /// The release is a prerelease or carries build metadata.
    NotOffered,
    /// An archive broke the extraction rules or its RELEASE.json.
    Archive,
    /// A local file operation failed.
    Io,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::Offline => "offline",
            ErrorCode::RateLimited => "rate_limited",
            ErrorCode::NotVerified => "not_verified",
            ErrorCode::TrustRootStale => "trust_root_stale",
            ErrorCode::Inconsistent => "inconsistent",
            ErrorCode::RedirectRefused => "redirect_refused",
            ErrorCode::TooLarge => "too_large",
            ErrorCode::Http => "http",
            ErrorCode::Manifest => "manifest",
            ErrorCode::NotOffered => "not_offered",
            ErrorCode::Archive => "archive",
            ErrorCode::Io => "io",
        }
    }
}

/// An error with a stable code and a short detail (no secrets, no full paths).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub code: ErrorCode,
    pub detail: String,
    /// For [`ErrorCode::RateLimited`]: seconds to wait, when the server said.
    pub retry_after_secs: Option<u64>,
}

impl Error {
    pub fn new(code: ErrorCode, detail: impl Into<String>) -> Self {
        let mut detail = detail.into();
        if detail.len() > 300 {
            let mut cut = 300;
            while !detail.is_char_boundary(cut) {
                cut -= 1;
            }
            detail.truncate(cut);
        }
        Error {
            code,
            detail,
            retry_after_secs: None,
        }
    }

    pub fn code(&self) -> &'static str {
        self.code.as_str()
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.detail)
    }
}

impl std::error::Error for Error {}

pub(crate) fn require(condition: bool, code: ErrorCode, detail: &str) -> Result<(), Error> {
    if condition {
        Ok(())
    } else {
        Err(Error::new(code, detail))
    }
}
