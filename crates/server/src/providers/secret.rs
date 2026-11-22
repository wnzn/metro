//! Provider API keys at rest: XChaCha20-Poly1305 under the operator's master
//! key, with a fresh random 192-bit nonce per seal and the owner's user id
//! and the connection id as associated data, so a sealed key moved to
//! another row or another user does not open.
//!
//! The master key is 32 bytes, RFC 4648 base64 (standard alphabet, padded),
//! from exactly one of `METRO_PROVIDER_MASTER_KEY` or a file named by
//! `METRO_PROVIDER_KEY_FILE`. Without either, keys cannot be stored or used
//! (never stored in plaintext); keyless connections still work.

use std::path::Path;

use anyhow::{Context, bail};
use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};

pub const MASTER_KEY_ENV: &str = "METRO_PROVIDER_MASTER_KEY";
pub const KEY_FILE_ENV: &str = "METRO_PROVIDER_KEY_FILE";

/// Format tag of a sealed key: `v1 || nonce(24) || ciphertext+tag`.
const VERSION: &[u8; 2] = b"v1";
const NONCE_LEN: usize = 24;
/// Longest API key accepted, in bytes.
pub const MAX_API_KEY_BYTES: usize = 8 * 1024;

pub struct MasterKey([u8; 32]);

impl std::fmt::Debug for MasterKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MasterKey(<redacted>)")
    }
}

impl Drop for MasterKey {
    fn drop(&mut self) {
        self.0 = [0; 32];
    }
}

/// Why a sealed key did not open. Neither variant carries key material.
#[derive(Debug, PartialEq, Eq)]
pub enum OpenError {
    /// Not something this build sealed.
    Malformed,
    /// Wrong master key, or the row or owner does not match.
    Rejected,
}

impl MasterKey {
    /// Parse base64 of exactly 32 bytes. The error never echoes the input.
    pub fn parse(text: &str) -> anyhow::Result<Self> {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(text.trim())
            .map_err(|_| anyhow::anyhow!("the provider master key is not valid base64"))?;
        let key: [u8; 32] = bytes.try_into().map_err(|b: Vec<u8>| {
            anyhow::anyhow!(
                "the provider master key must decode to exactly 32 bytes (got {})",
                b.len()
            )
        })?;
        Ok(Self(key))
    }

    /// The master key from the environment, `None` when neither variable is
    /// set. Both set, an unreadable file or a malformed key is an error:
    /// there is no fallback from one source to another.
    pub fn from_env(db_path: &Path) -> anyhow::Result<Option<Self>> {
        Self::from_sources(non_blank(MASTER_KEY_ENV), non_blank(KEY_FILE_ENV), db_path)
    }

    /// [`Self::from_env`] with the two variables' values given.
    pub fn from_sources(
        inline: Option<String>,
        file: Option<String>,
        db_path: &Path,
    ) -> anyhow::Result<Option<Self>> {
        match (inline, file) {
            (Some(_), Some(_)) => {
                bail!("set only one of {MASTER_KEY_ENV} and {KEY_FILE_ENV}, not both")
            }
            (Some(text), None) => Self::parse(&text)
                .with_context(|| format!("{MASTER_KEY_ENV} is invalid"))
                .map(Some),
            (None, Some(path)) => {
                let path = Path::new(&path);
                warn_about_file(path, db_path);
                let text = std::fs::read_to_string(path)
                    .with_context(|| format!("reading {KEY_FILE_ENV}={}", path.display()))?;
                Self::parse(&text)
                    .with_context(|| format!("{KEY_FILE_ENV}={} is invalid", path.display()))
                    .map(Some)
            }
            (None, None) => Ok(None),
        }
    }

    fn cipher(&self) -> XChaCha20Poly1305 {
        XChaCha20Poly1305::new(&self.0.into())
    }

    /// Seal `plaintext` for `connection_id` of `user_id`.
    pub fn seal(
        &self,
        user_id: &str,
        connection_id: &str,
        plaintext: &str,
    ) -> anyhow::Result<Vec<u8>> {
        let mut nonce = [0u8; NONCE_LEN];
        getrandom::fill(&mut nonce).map_err(|_| anyhow::anyhow!("no randomness for a nonce"))?;
        let aad = associated_data(user_id, connection_id);
        let sealed = self
            .cipher()
            .encrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: plaintext.as_bytes(),
                    aad: &aad,
                },
            )
            .map_err(|_| anyhow::anyhow!("the API key could not be encrypted"))?;
        let mut out = Vec::with_capacity(VERSION.len() + NONCE_LEN + sealed.len());
        out.extend_from_slice(VERSION);
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&sealed);
        Ok(out)
    }

    /// Open a key sealed for `connection_id` of `user_id`.
    pub fn open(
        &self,
        user_id: &str,
        connection_id: &str,
        sealed: &[u8],
    ) -> Result<String, OpenError> {
        let rest = sealed.strip_prefix(VERSION).ok_or(OpenError::Malformed)?;
        if rest.len() < NONCE_LEN + 16 {
            return Err(OpenError::Malformed);
        }
        let (nonce, ciphertext) = rest.split_at(NONCE_LEN);
        let nonce: [u8; NONCE_LEN] = nonce.try_into().map_err(|_| OpenError::Malformed)?;
        let aad = associated_data(user_id, connection_id);
        let plain = self
            .cipher()
            .decrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: ciphertext,
                    aad: &aad,
                },
            )
            .map_err(|_| OpenError::Rejected)?;
        String::from_utf8(plain).map_err(|_| OpenError::Malformed)
    }
}

/// Binds a sealed key to its owner and row. Lengths are prefixed so no two
/// (user, connection) pairs share an encoding.
fn associated_data(user_id: &str, connection_id: &str) -> Vec<u8> {
    let mut aad = b"metro.provider-key.v1".to_vec();
    for part in [user_id, connection_id] {
        aad.extend_from_slice(&(part.len() as u64).to_be_bytes());
        aad.extend_from_slice(part.as_bytes());
    }
    aad
}

fn non_blank(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}

/// Warnings only: a key file other users can read, or one kept beside the
/// database (so a copy of the data directory carries its own key).
fn warn_about_file(path: &Path, db_path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(path)
            && meta.permissions().mode() & 0o077 != 0
        {
            tracing::warn!(
                "{KEY_FILE_ENV} is readable by other users; restrict it to mode 600 or 400"
            );
        }
    }
    let dir = |p: &Path| {
        p.parent()
            .map(|d| {
                if d.as_os_str().is_empty() {
                    Path::new(".")
                } else {
                    d
                }
            })
            .and_then(|d| std::fs::canonicalize(d).ok())
    };
    if let (Some(a), Some(b)) = (dir(path), dir(db_path))
        && a == b
    {
        tracing::warn!(
            "{KEY_FILE_ENV} is in the same directory as METRO_DB; keep the master key outside \
             the data directory so a copy of the database does not carry its own key"
        );
    }
}
