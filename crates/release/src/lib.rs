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
pub mod installer_lock;
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
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let tmp = path.with_file_name(atomic_temp_name(&name));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut f = options.open(&tmp)?;
    let result = (|| {
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        rename_durable(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

fn atomic_temp_name(name: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    let time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!(".{name}.{}.{time:x}.{count:x}.tmp", std::process::id())
}

#[cfg(unix)]
mod unix_fs;

#[cfg(windows)]
mod win_rename {
    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use std::path::Path;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_RENAME_INFO, FileRenameInfoEx, FlushFileBuffers, SetFileInformationByHandle,
    };

    const DELETE: u32 = 0x0001_0000;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const REPLACE_IF_EXISTS: u32 = 1;
    const POSIX_SEMANTICS: u32 = 2;

    /// Rename the file `from` over the file `to` by handle with POSIX
    /// semantics (Windows 10 1607+ on NTFS), then flush the moved file.
    pub fn replace_posix(from: &Path, to: &Path) -> io::Result<()> {
        let file = std::fs::OpenOptions::new()
            .access_mode(DELETE | GENERIC_WRITE)
            .share_mode(0)
            .custom_flags(OPEN_REPARSE_POINT)
            .open(from)?;
        if !file.metadata()?.is_file() {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let target: Vec<u16> = std::path::absolute(to)?.as_os_str().encode_wide().collect();
        if target.contains(&0) {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let header = std::mem::size_of::<FILE_RENAME_INFO>();
        let size = header + target.len() * 2;
        // u64 words: FILE_RENAME_INFO's alignment (it holds a HANDLE).
        let mut buf = vec![0u64; size.div_ceil(8)];
        let info = buf.as_mut_ptr().cast::<FILE_RENAME_INFO>();
        // SAFETY: `buf` is zeroed, aligned and large enough for the header
        // and the name (FileName is its last field); the handle is open.
        unsafe {
            (*info).Anonymous.Flags = REPLACE_IF_EXISTS | POSIX_SEMANTICS;
            (*info).FileNameLength = (target.len() * 2) as u32;
            std::ptr::copy_nonoverlapping(
                target.as_ptr(),
                std::ptr::addr_of_mut!((*info).FileName).cast::<u16>(),
                target.len(),
            );
            let h = file.as_raw_handle();
            if SetFileInformationByHandle(h, FileRenameInfoEx, info.cast(), size as u32) == 0 {
                return Err(io::Error::last_os_error());
            }
            if FlushFileBuffers(h) == 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use std::os::windows::fs::OpenOptionsExt;

        #[test]
        fn replaces_a_file_its_reader_shares_delete_on() {
            let dir = tempfile::tempdir().unwrap();
            let (from, to) = (dir.path().join("new"), dir.path().join("journal"));
            std::fs::write(&to, b"old").unwrap();
            std::fs::write(&from, b"new").unwrap();
            let held = std::fs::OpenOptions::new()
                .read(true)
                .share_mode(7)
                .open(&to)
                .unwrap();
            super::replace_posix(&from, &to).unwrap();
            drop(held);
            assert_eq!(std::fs::read(&to).unwrap(), b"new");
            assert!(!from.exists());
        }
    }
}

/// Same-filesystem rename, with metadata made durable before returning.
pub fn rename_durable(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::io;
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::{
            MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
        };
        let wide = |path: &std::path::Path| -> io::Result<Vec<u16>> {
            let mut encoded: Vec<u16> = path.as_os_str().encode_wide().collect();
            if encoded.contains(&0) {
                return Err(io::ErrorKind::InvalidInput.into());
            }
            encoded.push(0);
            Ok(encoded)
        };
        // Replacing a file that another process has open is denied by
        // MoveFileExW even when that reader shares delete (classic rename
        // semantics). Then, for a file target only: rename by handle with
        // POSIX semantics (replaces a target its readers share delete on;
        // they keep reading the old file), flushed like WRITE_THROUGH; and
        // retry for a bounded time, for readers that don't share delete.
        // Anything else fails at once.
        let replaces_file = std::fs::symlink_metadata(to).is_ok_and(|m| m.is_file());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let (wfrom, wto) = (wide(from)?, wide(to)?);
        loop {
            // SAFETY: both paths are NUL-terminated UTF-16; no cross-volume copy.
            if unsafe {
                MoveFileExW(
                    wfrom.as_ptr(),
                    wto.as_ptr(),
                    MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
                )
            } != 0
            {
                return Ok(());
            }
            let e = io::Error::last_os_error();
            // ERROR_ACCESS_DENIED, ERROR_SHARING_VIOLATION
            if !(replaces_file && matches!(e.raw_os_error(), Some(5 | 32))) {
                return Err(e);
            }
            if win_rename::replace_posix(from, to).is_ok() {
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                return Err(e);
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    #[cfg(not(windows))]
    {
        std::fs::rename(from, to)?;
        sync_parent(to)?;
        if from.parent() != to.parent() {
            sync_parent(from)?;
        }
        Ok(())
    }
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
