//! Metro release verification.
//!
//! This crate is the one place that decides whether a published Metro release
//! is genuine. A release counts only when:
//!
//! 1. `SHA256SUMS` carries a Sigstore v0.3 bundle that verifies against the
//!    vendored Sigstore public-good trusted root, signed by the pinned release
//!    workflow identity ([`SIGNER`], [`ISSUER`]) with the GitHub claims
//!    `packaging/release-signatures/release_signatures.py` checks as well
//!    ([`policy`]);
//! 2. the signed `SHA256SUMS` names exactly one stable version and the
//!    complete release file set ([`manifest`]);
//! 3. every file Metro uses matches its signed row, and every archive
//!    matches its own `RELEASE.json` ([`archive`]).
//!
//! The core has no network code. The optional fetchers in [`fetch`] only build
//! URLs from constants and verified data.

pub mod archive;
pub mod channel;
#[cfg(feature = "e2e")]
pub mod e2e;
pub mod error;
#[cfg(any(feature = "blocking", feature = "async"))]
pub mod fetch;
pub mod images;
pub mod manifest;
pub mod policy;
pub mod release;
pub mod sigstore;
#[cfg(feature = "testing")]
pub mod testing;
#[cfg(feature = "async")]
pub mod trust_refresh;
pub mod version;

pub use channel::Channel;
pub use error::{Error, ErrorCode};
pub use release::{ReleaseVerifier, Signed, SignedFacts, VerifiedRelease};
pub use version::{Offer, Version};

/// The only repository whose releases count.
pub const REPO_URL: &str = "https://github.com/wnzn/metro";
/// GitHub's numeric id for [`REPO_URL`]; survives renames, not re-creation.
pub const REPO_ID: &str = "1467654868";
/// GitHub Actions' OIDC issuer.
pub const ISSUER: &str = "https://token.actions.githubusercontent.com";
/// The reusable workflow that signs release assets (certificate SAN).
pub const SIGNER: &str =
    "https://github.com/wnzn/metro/.github/workflows/sign-release-assets.yml@refs/heads/main";
/// The top-level workflow that calls [`SIGNER`] on a protected `main` push.
pub const CALLER: &str = "https://github.com/wnzn/metro/.github/workflows/ci.yml@refs/heads/main";
/// The only ref releases are signed from.
pub const SOURCE_REF: &str = "refs/heads/main";
/// Release page prefix; a version is appended (`v1.2.3`).
pub const RELEASE_PAGE_PREFIX: &str = "https://github.com/wnzn/metro/releases/tag/v";

/// Size caps (bytes).
pub const MAX_MANIFEST: usize = 64 * 1024;
pub const MAX_BUNDLE: usize = 64 * 1024;
pub const MAX_IMAGES: usize = 16 * 1024;
pub const MAX_NOTES_JSON: usize = 256 * 1024;
pub const MAX_ARCHIVE: u64 = 1024 * 1024 * 1024;

/// Lowercase hex of a SHA-256 digest.
pub fn hex(digest: &[u8; 32]) -> String {
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push(char::from_digit(u32::from(byte >> 4), 16).unwrap_or('0'));
        out.push(char::from_digit(u32::from(byte & 15), 16).unwrap_or('0'));
    }
    out
}

/// Parse exactly 64 lowercase hex characters.
pub fn parse_hex32(text: &str) -> Option<[u8; 32]> {
    let bytes = text.as_bytes();
    if bytes.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (index, pair) in bytes.as_chunks::<2>().0.iter().enumerate() {
        let nibble = |c: u8| match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            _ => None,
        };
        out[index] = (nibble(pair[0])? << 4) | nibble(pair[1])?;
    }
    Some(out)
}

/// SHA-256 of a byte slice.
pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    use sha2::Digest;
    sha2::Sha256::digest(bytes).into()
}

/// The release page for a version (built from a constant and a parsed version).
pub fn release_url(version: &Version) -> String {
    format!("{RELEASE_PAGE_PREFIX}{version}")
}

/// A 40-character lowercase hex commit.
pub fn is_commit(text: &str) -> bool {
    text.len() == 40 && text.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
}

/// Write a file atomically: a new, uniquely named temp file next to it
/// (created exclusively, never following a link), fsync, rename over the
/// target, fsync the parent folder. Errors are returned, never swallowed.
pub fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64)
        ^ COUNTER.fetch_add(1, Ordering::Relaxed).rotate_left(32);
    let tmp = path.with_file_name(format!(".{name}.{}.{nonce:016x}.tmp", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let result = (|| {
        let mut f = options.open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)?;
        sync_parent(path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// fsync a path's parent folder (Unix; Windows has no folder handles to sync).
pub fn sync_parent(path: &std::path::Path) -> std::io::Result<()> {
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        let dir = if parent.as_os_str().is_empty() {
            std::path::Path::new(".")
        } else {
            parent
        };
        std::fs::File::open(dir)?.sync_all()?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}
