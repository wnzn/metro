//! Web search provider keys, on this computer only.
//!
//! `web-keys.json` in the person's web profile: mode 600 in a mode-700
//! directory on Unix (checked on every open); on Windows the profile has a
//! protected DACL and the file is sealed with DPAPI for the current user.
//! Keys are held as [`Secret`] (zeroized, redacted in Debug), accepted
//! write-only, never logged, never in an error text, never returned to the
//! UI (only whether one is saved) and never sent to Metro.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

use super::ProviderId;
use crate::fsutil::{ensure_private_dir, read_private, remove_private, write_private};
use crate::openai_oauth::store::{seal, unseal};
use crate::{SafeError, Secret};

pub const FILE: &str = "web-keys.json";
/// Held across every read-modify-write of the key file, in any process.
pub const LOCK_FILE: &str = "web-keys.lock";

/// The saved keys, by provider id.
#[derive(Default)]
pub struct Keys(BTreeMap<String, Secret>);

impl Keys {
    pub fn get(&self, provider: &str) -> Option<&Secret> {
        self.0.get(provider)
    }

    pub fn has(&self, provider: &str) -> bool {
        self.0.contains_key(provider)
    }

    /// Every key, only for redacting local log lines.
    pub fn all(&self) -> impl Iterator<Item = &Secret> {
        self.0.values()
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    v: u32,
    keys: BTreeMap<String, String>,
}

impl Drop for Wire {
    fn drop(&mut self) {
        for v in self.keys.values_mut() {
            v.zeroize();
        }
    }
}

/// A key's shape: 8..=256 printable ASCII characters, no whitespace.
pub fn validate_key(k: &Secret) -> Result<(), SafeError> {
    let s = k.expose();
    if !(8..=256).contains(&s.len()) || !s.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(SafeError::new(
            "that doesn't look like an API key (8 to 256 printable characters, no spaces)",
        ));
    }
    Ok(())
}

/// A short, non-reversible fingerprint (to remember that a key was rejected
/// without storing the key twice).
pub fn fingerprint(k: &Secret) -> String {
    Sha256::digest(k.expose().as_bytes())[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn unreadable() -> anyhow::Error {
    SafeError::new(
        "the saved web search keys can't be read; reset them (this deletes every saved key) and add them again",
    )
    .into()
}

pub struct KeyStore {
    dir: PathBuf,
}

impl KeyStore {
    pub fn new(dir: &Path) -> Self {
        Self {
            dir: dir.to_path_buf(),
        }
    }

    fn path(&self) -> PathBuf {
        self.dir.join(FILE)
    }

    fn locked<T>(&self, f: impl FnOnce() -> Result<T>) -> Result<T> {
        ensure_private_dir(&self.dir)?;
        let _lock = crate::fsutil::lock_exclusive(&self.dir.join(LOCK_FILE))?;
        f()
    }

    /// The saved keys; none when nothing is saved. A file that can't be read
    /// is an error, and no key from it is used.
    pub fn load(&self) -> Result<Keys> {
        // Only a missing file is "no keys"; anything else is a fixed error.
        let Some(stored) = read_private(&self.path(), 64 * 1024).map_err(|_| unreadable())? else {
            return Ok(Keys::default());
        };
        let mut plain = unseal(stored).map_err(|_| unreadable())?;
        let parsed = serde_json::from_slice::<Wire>(&plain);
        plain.zeroize();
        let w = parsed.map_err(|_| unreadable())?;
        if w.v != 1 {
            bail!(SafeError::new(
                "the saved web search keys have an unknown format"
            ));
        }
        let mut out = BTreeMap::new();
        for (p, k) in &w.keys {
            if ProviderId::parse(p).is_some() {
                let secret = Secret::new(k.as_str());
                if validate_key(&secret).is_ok() {
                    out.insert(p.clone(), secret);
                }
            }
        }
        Ok(Keys(out))
    }

    fn store(&self, keys: &BTreeMap<String, Secret>) -> Result<()> {
        ensure_private_dir(&self.dir)?;
        if keys.is_empty() {
            remove_private(&self.path())?;
            return Ok(());
        }
        let w = Wire {
            v: 1,
            keys: keys
                .iter()
                .map(|(p, k)| (p.clone(), k.expose().to_owned()))
                .collect(),
        };
        let plain = serde_json::to_vec(&w).context("encoding keys")?;
        drop(w);
        let sealed = seal(plain)?;
        write_private(&self.path(), &sealed)
    }

    /// Save `key` for `p` (replacing any). The key is checked first. A key
    /// file that can't be read is left untouched (an error); [`Self::reset`]
    /// is the explicit way out.
    pub fn set(&self, p: ProviderId, key: Secret) -> Result<()> {
        validate_key(&key)?;
        self.locked(|| {
            let mut keys = self.load()?.0;
            keys.insert(p.as_str().into(), key);
            self.store(&keys)
        })
    }

    /// Delete every saved key, readable or not: the explicit recovery from a
    /// key file that can't be read. `true` if there was a file.
    pub fn reset(&self) -> Result<bool> {
        self.locked(|| remove_private(&self.path()))
    }

    /// Delete `p`'s key; `true` if there was one. The provider goes back to
    /// its free mode.
    pub fn delete(&self, p: ProviderId) -> Result<bool> {
        self.locked(|| {
            let mut keys = self.load()?.0;
            let had = keys.remove(p.as_str()).is_some();
            self.store(&keys)?;
            Ok(had)
        })
    }
}
