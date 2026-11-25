//! Who this computer searches for: one private record that holds the Metro
//! server, the owner, the web device id and its token together.
//!
//! It's published in one atomic write (temp file, fsync, rename), and
//! sealed with DPAPI on Windows. Readers take the identity and the
//! credential from the same record, so an interrupted or concurrent re-pair
//! can never pair one person's token with another person's keys: a reader
//! sees the old record or the new one, never a mix.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

use crate::fsutil::{read_private, remove_private, write_private};
use crate::netpolicy::validate_canonical_server_url;
use crate::openai_oauth::store::{seal, unseal};
use crate::profile::validate_device_token;
use crate::{SafeError, Secret};

pub const FILE: &str = "web-binding.json";

/// One web device binding.
pub struct Binding {
    pub server_url: String,
    /// `account:<stable id>` (Metro confirmed it) or `device:<fingerprint>`.
    pub owner: String,
    pub device_id: Option<String>,
    token: Secret,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    v: u32,
    server_url: String,
    owner: String,
    #[serde(default)]
    device_id: Option<String>,
    token: String,
}

impl Drop for Wire {
    fn drop(&mut self) {
        self.token.zeroize();
    }
}

impl Binding {
    pub fn new(
        server_url: &str,
        owner: &str,
        device_id: Option<String>,
        token: Secret,
    ) -> Result<Self> {
        // The URL is part of the identity. Don't silently change which
        // server a stored credential and the per-owner data are bound to.
        validate_canonical_server_url(server_url, "web binding server_url")?;
        validate_device_token(&token)?;
        if owner.is_empty() || owner.len() > 300 || owner.chars().any(char::is_control) {
            bail!("invalid owner");
        }
        Ok(Self {
            server_url: server_url.to_owned(),
            owner: owner.to_owned(),
            device_id,
            token,
        })
    }

    /// The token, only for exactly this server and owner.
    pub fn token_for(&self, server_url: &str, owner: &str) -> Result<&Secret> {
        if self.server_url != server_url || self.owner != owner {
            bail!(SafeError::new(
                "this computer's web search is set up for another server or account; set it up again"
            ));
        }
        Ok(&self.token)
    }

    /// The token and the identity it belongs to, from this one record.
    pub fn token(&self) -> &Secret {
        &self.token
    }
}

fn path(dir: &Path) -> PathBuf {
    dir.join(FILE)
}

/// The record in `dir`, if any. A record that can't be read is an error,
/// never a partial identity.
pub fn load(dir: &Path) -> Result<Option<Binding>> {
    let Some(stored) = read_private(&path(dir), 64 * 1024)? else {
        return Ok(None);
    };
    let mut plain = unseal(stored).context("the web search binding can't be read")?;
    let parsed = serde_json::from_slice::<Wire>(&plain);
    plain.zeroize();
    let w = parsed.context("the web search binding is not valid")?;
    if w.v != 1 {
        bail!("the web search binding has an unknown format");
    }
    Binding::new(
        &w.server_url,
        &w.owner,
        w.device_id.clone(),
        Secret::new(w.token.as_str()),
    )
    .map(Some)
}

/// Publish `b` in one atomic write.
pub fn save(dir: &Path, b: &Binding) -> Result<()> {
    let w = Wire {
        v: 1,
        server_url: b.server_url.clone(),
        owner: b.owner.clone(),
        device_id: b.device_id.clone(),
        token: b.token.expose().to_owned(),
    };
    let plain = serde_json::to_vec(&w)?;
    drop(w);
    write_private(&path(dir), &seal(plain)?)
}

pub fn remove(dir: &Path) -> Result<bool> {
    remove_private(&path(dir))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_binding_requires_a_canonical_metro_server_url() {
        let dir = tempfile::tempdir().unwrap();
        crate::fsutil::ensure_private_dir(dir.path()).unwrap();
        for server_url in [
            "http://metro.invalid/",
            "http://metro.invalid/?query",
            "http://metro.invalid/#fragment",
            "http://user:password@metro.invalid",
            "ftp://metro.invalid",
        ] {
            let wire = Wire {
                v: 1,
                server_url: server_url.into(),
                owner: "account:alice".into(),
                device_id: Some("wd_fixture".into()),
                token: format!("metro_pc_{:064x}", 1),
            };
            let sealed = seal(serde_json::to_vec(&wire).unwrap()).unwrap();
            write_private(&path(dir.path()), &sealed).unwrap();
            assert!(load(dir.path()).is_err(), "accepted an invalid binding URL");
        }
    }
}
