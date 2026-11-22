//! Local connection profiles.
//!
//! ```text
//! <root>/                          mode 700  ($METRO_CONNECTOR_HOME, else
//!                                            $XDG_CONFIG_HOME/metro-connector,
//!                                            else ~/.config/metro-connector;
//!                                            %APPDATA%\metro-connector on Windows)
//!   profiles/<name>/               mode 700  one per personal provider connection
//!     profile.json                 mode 600  server URL, kind, model allowlist,
//!                                            base URL (endpoint kinds only:
//!                                            openai-compatible, systemone,
//!                                            embeddings)
//!     device-token                 mode 600  the metro_pc_ credential
//!     provider-key                 mode 600  BYOK key (endpoint kinds only)
//!     vendor/                      mode 700  the adapter's own private state
//!                                            (sign-in tokens, isolated CLI home)
//! ```
//!
//! Profile names are checked (`[a-z0-9][a-z0-9_-]{0,31}`) before they become
//! a path, so a name can't escape `profiles/`.

use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use zeroize::Zeroize;

use crate::fsutil::{ensure_private_dir, read_private, remove_private, write_private};
use crate::netpolicy::validate_base_url;
use crate::{SafeError, Secret};

pub const ENV_HOME: &str = "METRO_CONNECTOR_HOME";
pub const DEFAULT_PROFILE: &str = "default";
pub const DEVICE_TOKEN_PREFIX: &str = "metro_pc_";
pub const MAX_MODELS: usize = 64;
pub const MAX_MODEL_LEN: usize = 128;
/// Metro's relay abandons a claimed device job this long after the claim
/// (`DeviceLimits::run_timeout` in metro-server). Work finishing later is
/// wasted.
pub const SERVER_RUN_TIMEOUT_SECS: u64 = 300;
/// Time kept back from the server's limit for posting the result (and its
/// retries) and for clock skew between the claim and our timer.
pub const RESULT_MARGIN_SECS: u64 = 30;
/// Longest a job may run locally: the server limit minus the margin.
pub const MAX_REQUEST_TIMEOUT_SECS: u64 = SERVER_RUN_TIMEOUT_SECS - RESULT_MARGIN_SECS;
pub const MIN_REQUEST_TIMEOUT_SECS: u64 = 5;
pub const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 240;

/// The device connection kinds Metro knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    #[serde(rename = "device-openai-oauth")]
    OpenAiOAuth,
    #[serde(rename = "device-claude-code")]
    ClaudeCode,
    #[serde(rename = "device-openai-compatible")]
    OpenAiCompatible,
    /// A System One endpoint (`POST {base}/systemone`): TypeSafe's API with
    /// the person's own key, or a self-hosted OpenJev helper.
    #[serde(rename = "device-systemone")]
    SystemOne,
    /// An OpenAI-compatible embeddings endpoint (`POST {base}/embeddings`).
    #[serde(rename = "device-embeddings")]
    Embeddings,
    /// A media embedding service (`POST {base}/media/embeddings` for
    /// documents and media, `POST {base}/embeddings` for text), such as a
    /// self-hosted EmbeddingGemma 2 server.
    #[serde(rename = "device-embeddinggemma-2")]
    MediaEmbeddings,
}

/// What a connection serves. A job for another capability is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    Chat,
    SystemOne,
    Embeddings,
}

impl Kind {
    pub const ALL: [Kind; 6] = [
        Kind::OpenAiOAuth,
        Kind::ClaudeCode,
        Kind::OpenAiCompatible,
        Kind::SystemOne,
        Kind::Embeddings,
        Kind::MediaEmbeddings,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::OpenAiOAuth => "device-openai-oauth",
            Kind::ClaudeCode => "device-claude-code",
            Kind::OpenAiCompatible => "device-openai-compatible",
            Kind::SystemOne => "device-systemone",
            Kind::Embeddings => "device-embeddings",
            Kind::MediaEmbeddings => "device-embeddinggemma-2",
        }
    }

    /// The one capability this kind serves.
    pub fn capability(self) -> Capability {
        match self {
            Kind::OpenAiOAuth | Kind::ClaudeCode | Kind::OpenAiCompatible => Capability::Chat,
            Kind::SystemOne => Capability::SystemOne,
            Kind::Embeddings | Kind::MediaEmbeddings => Capability::Embeddings,
        }
    }

    /// Kinds that call a base URL saved in the profile, with an optional
    /// key bound to it (no vendor sign-in).
    pub fn uses_endpoint(self) -> bool {
        matches!(
            self,
            Kind::OpenAiCompatible | Kind::SystemOne | Kind::Embeddings | Kind::MediaEmbeddings
        )
    }

    /// The one kind that runs `media_embed` (documents and media).
    pub fn embeds_media(self) -> bool {
        self == Kind::MediaEmbeddings
    }

    pub fn describe(self) -> &'static str {
        match self {
            Kind::OpenAiOAuth => "OpenAI sign-in on this computer",
            Kind::ClaudeCode => {
                "Claude (local CLI) with your Anthropic Console API key (API billing, no consumer subscription)"
            }
            Kind::OpenAiCompatible => {
                "OpenAI-compatible endpoint with your own key (or a local server)"
            }
            Kind::SystemOne => {
                "System One endpoint (TypeSafe with your own key, or a self-hosted OpenJev server)"
            }
            Kind::Embeddings => {
                "OpenAI-compatible embeddings endpoint with your own key (or a local server)"
            }
            Kind::MediaEmbeddings => {
                "Media embedding service (EmbeddingGemma 2 or compatible) for documents, images, audio and video"
            }
        }
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Kind {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        Kind::ALL
            .into_iter()
            .find(|k| k.as_str() == s.trim())
            .with_context(|| {
                format!(
                    "unknown connection kind {s:?}; use one of device-openai-oauth, device-claude-code, \
                     device-openai-compatible, device-systemone, device-embeddings, \
                     device-embeddinggemma-2"
                )
            })
    }
}

/// `profile.json`. Unknown fields are rejected so a typo can't silently
/// weaken anything.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileConfig {
    pub version: u32,
    pub kind: Kind,
    pub server_url: String,
    /// Locally approved models. `complete` refuses any other model and
    /// `models` reports only these.
    pub models: Vec<String>,
    /// The endpoint (`.../v1`), only for the endpoint kinds
    /// ([`Kind::uses_endpoint`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default = "default_timeout")]
    pub request_timeout_secs: u64,
}

fn default_timeout() -> u64 {
    DEFAULT_REQUEST_TIMEOUT_SECS
}

impl ProfileConfig {
    pub fn validate(&self) -> Result<()> {
        if self.version != 1 {
            bail!("profile.json: unsupported version {}", self.version);
        }
        if validate_base_url(&self.server_url, "server_url")? != self.server_url {
            bail!("profile.json: server_url is not normalized");
        }
        match (self.kind.uses_endpoint(), &self.base_url) {
            (true, Some(b)) => {
                validate_base_url(b, "base_url")?;
            }
            (true, None) => bail!("profile.json: base_url is required"),
            (false, Some(_)) => bail!(
                "profile.json: base_url is only for device-openai-compatible, device-systemone, device-embeddings and device-embeddinggemma-2"
            ),
            _ => {}
        }
        validate_models(&self.models)?;
        if !(MIN_REQUEST_TIMEOUT_SECS..=MAX_REQUEST_TIMEOUT_SECS)
            .contains(&self.request_timeout_secs)
        {
            bail!(
                "profile.json: request_timeout_secs must be {MIN_REQUEST_TIMEOUT_SECS}..={MAX_REQUEST_TIMEOUT_SECS} \
                 (Metro gives a device job {SERVER_RUN_TIMEOUT_SECS}s, and {RESULT_MARGIN_SECS}s are kept for sending the result)"
            );
        }
        Ok(())
    }
}

pub fn validate_model_name(m: &str) -> Result<()> {
    if m.is_empty()
        || m.len() > MAX_MODEL_LEN
        || m.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        bail!("model name {m:?} is invalid (1-{MAX_MODEL_LEN} characters, no spaces)");
    }
    Ok(())
}

pub fn validate_models(models: &[String]) -> Result<()> {
    if models.len() > MAX_MODELS {
        bail!("at most {MAX_MODELS} models can be allowed");
    }
    for (i, m) in models.iter().enumerate() {
        validate_model_name(m)?;
        if models[..i].contains(m) {
            bail!("model {m:?} is listed twice");
        }
    }
    Ok(())
}

pub fn validate_profile_name(name: &str) -> Result<()> {
    let b = name.as_bytes();
    let ok = !b.is_empty()
        && b.len() <= 32
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'-' | b'_'));
    if !ok {
        bail!(
            "profile name {name:?} is invalid: use 1-32 lowercase letters, digits, - or _, starting with a letter or digit"
        );
    }
    Ok(())
}

pub fn validate_device_token(t: &Secret) -> Result<()> {
    let v = t.expose();
    let rest = match v.strip_prefix(DEVICE_TOKEN_PREFIX) {
        Some(r) => r,
        None => bail!(
            "that isn't a Metro device token (it should start with {DEVICE_TOKEN_PREFIX}); copy it again from Metro"
        ),
    };
    if rest.len() < 16
        || v.len() > 512
        || !rest
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
    {
        bail!("the device token looks damaged; copy it again from Metro");
    }
    Ok(())
}

pub fn validate_provider_key(k: &Secret) -> Result<()> {
    let v = k.expose();
    if v.len() > 4096 || v.chars().any(|c| c.is_whitespace() || c.is_control()) {
        bail!("the API key contains spaces or control characters, or is too long");
    }
    Ok(())
}

/// The configuration root.
pub fn root_dir() -> Result<PathBuf> {
    if let Some(h) = std::env::var_os(ENV_HOME).filter(|v| !v.is_empty()) {
        let p = PathBuf::from(h);
        if !p.is_absolute() {
            bail!("{ENV_HOME} must be an absolute path");
        }
        return Ok(p);
    }
    #[cfg(windows)]
    if let Some(a) = std::env::var_os("APPDATA").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(a).join("metro-connector"));
    }
    if let Some(x) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        let p = PathBuf::from(x);
        if p.is_absolute() {
            return Ok(p.join("metro-connector"));
        }
    }
    let home = std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .context("cannot find your home directory; set METRO_CONNECTOR_HOME")?;
    Ok(PathBuf::from(home).join(".config").join("metro-connector"))
}

/// On-disk form of `device-token` and `provider-key`: the secret together
/// with the one destination (normalized URL) it may be sent to, written in a
/// single atomic file write. Lookups compare the destination with the
/// profile's current URL, so a secret can never be paired with a different
/// host, whatever order files were written in or where a setup stopped.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SecretRecord {
    version: u32,
    destination: String,
    secret: String,
}

impl Drop for SecretRecord {
    fn drop(&mut self) {
        self.secret.zeroize();
    }
}

/// One profile directory.
#[derive(Debug, Clone)]
pub struct Profile {
    pub name: String,
    pub dir: PathBuf,
}

impl Profile {
    pub fn open(root: &Path, name: &str) -> Result<Self> {
        validate_profile_name(name)?;
        Ok(Profile {
            name: name.to_string(),
            dir: root.join("profiles").join(name),
        })
    }

    pub fn exists(&self) -> bool {
        self.dir.join("profile.json").is_file()
    }

    /// Create the directory tree with private permissions.
    pub fn ensure(&self) -> Result<()> {
        let root = self
            .dir
            .parent()
            .and_then(Path::parent)
            .context("bad profile path")?;
        ensure_private_dir(root)?;
        ensure_private_dir(&root.join("profiles"))?;
        ensure_private_dir(&self.dir)?;
        ensure_private_dir(&self.vendor_dir())?;
        Ok(())
    }

    /// The adapter's private directory. Adapters keep their sign-in state
    /// here and nowhere else.
    pub fn vendor_dir(&self) -> PathBuf {
        self.dir.join("vendor")
    }

    pub fn load(&self) -> Result<ProfileConfig> {
        let bytes =
            read_private(&self.dir.join("profile.json"), 64 * 1024)?.with_context(|| {
                format!(
                    "profile {:?} is not set up; run `metro-connector setup --profile {}`",
                    self.name, self.name
                )
            })?;
        let mut cfg: ProfileConfig =
            serde_json::from_slice(&bytes).context("profile.json is not valid")?;
        // Profiles written by earlier builds default to 300s, which Metro
        // never honours; anything above the effective limit means "as long
        // as possible".
        cfg.request_timeout_secs = cfg.request_timeout_secs.min(MAX_REQUEST_TIMEOUT_SECS);
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn save(&self, cfg: &ProfileConfig) -> Result<()> {
        cfg.validate()?;
        self.ensure()?;
        let mut json = serde_json::to_vec_pretty(cfg)?;
        json.push(b'\n');
        write_private(&self.dir.join("profile.json"), &json)
    }

    fn read_record(&self, file: &str) -> Result<Option<SecretRecord>> {
        let Some(mut bytes) = read_private(&self.dir.join(file), 16 * 1024)? else {
            return Ok(None);
        };
        let parsed = serde_json::from_slice::<SecretRecord>(&bytes);
        bytes.zeroize();
        match parsed {
            Ok(r) if r.version == 1 => Ok(Some(r)),
            // Plain-text files from builds before destination binding, or
            // anything else: never usable, never guessed at.
            _ => bail!(
                "{file} in profile {:?} is not bound to a destination; run `metro-connector setup --profile {}` again",
                self.name,
                self.name
            ),
        }
    }

    fn write_record(&self, file: &str, destination: &str, secret: &Secret) -> Result<()> {
        self.ensure()?;
        let rec = SecretRecord {
            version: 1,
            destination: destination.to_string(),
            secret: secret.expose().to_string(),
        };
        let mut bytes = serde_json::to_vec(&rec)?;
        let r = write_private(&self.dir.join(file), &bytes);
        bytes.zeroize();
        r
    }

    /// The server the saved device token belongs to (not secret).
    pub fn device_token_destination(&self) -> Result<Option<String>> {
        Ok(self
            .read_record("device-token")?
            .map(|r| r.destination.clone()))
    }

    /// The device token, only if it was saved for exactly `server_url`.
    pub fn device_token_for(&self, server_url: &str) -> Result<Secret> {
        let rec = self.read_record("device-token")?.with_context(|| {
            format!(
                "no device token saved for profile {:?}; run `metro-connector setup --profile {}`",
                self.name, self.name
            )
        })?;
        if rec.destination != server_url {
            bail!(
                "the saved device token belongs to {}, not {server_url}; it will not be sent there. \
                 Run `metro-connector setup --profile {}` and enter the token for {server_url}",
                rec.destination,
                self.name
            );
        }
        let t = Secret::new(rec.secret.as_str());
        validate_device_token(&t)?;
        Ok(t)
    }

    /// Save the device token bound to `server_url` (one atomic write).
    pub fn save_device_token(&self, server_url: &str, t: &Secret) -> Result<()> {
        validate_device_token(t)?;
        validate_base_url(server_url, "server_url")?;
        self.write_record("device-token", server_url, t)
    }

    /// The endpoint the saved BYOK key belongs to, if a key is saved.
    pub fn provider_key_destination(&self) -> Result<Option<String>> {
        Ok(self
            .read_record("provider-key")?
            .map(|r| r.destination.clone()))
    }

    /// The BYOK key for exactly `base_url`. `Ok(None)` if no key is saved
    /// (keyless endpoint). A key saved for another endpoint is an error, not
    /// "no key", so a half-finished re-setup can't silently change behaviour.
    pub fn provider_key_for(&self, base_url: &str) -> Result<Option<Secret>> {
        let Some(rec) = self.read_record("provider-key")? else {
            return Ok(None);
        };
        if rec.destination != base_url {
            bail!(SafeError::new(
                "the API key saved on the device belongs to a different endpoint; run `metro-connector setup` on the device"
            ));
        }
        let k = Secret::new(rec.secret.as_str());
        validate_provider_key(&k)?;
        Ok(Some(k))
    }

    /// Save (or with `None`, delete) the BYOK key bound to `base_url`.
    pub fn save_provider_key(&self, base_url: &str, k: Option<&Secret>) -> Result<()> {
        self.ensure()?;
        match k {
            Some(k) if !k.is_empty() => {
                validate_provider_key(k)?;
                validate_base_url(base_url, "base_url")?;
                self.write_record("provider-key", base_url, k)
            }
            _ => remove_private(&self.dir.join("provider-key")).map(|_| ()),
        }
    }

    /// Every saved secret value, whatever it is bound to. Only for redacting
    /// local log lines.
    pub fn secrets_for_redaction(&self) -> Vec<Secret> {
        ["device-token", "provider-key"]
            .iter()
            .filter_map(|f| self.read_record(f).ok().flatten())
            .map(|r| Secret::new(r.secret.as_str()))
            .collect()
    }

    pub fn forget_provider_key(&self) -> Result<bool> {
        remove_private(&self.dir.join("provider-key"))
    }

    /// Delete the whole profile directory: device token, key, vendor state.
    pub fn forget_all(&self) -> Result<bool> {
        match std::fs::symlink_metadata(&self.dir) {
            Ok(m) if m.file_type().is_symlink() => {
                bail!("{} is a symlink; remove it by hand", self.dir.display())
            }
            Ok(_) => {
                for f in ["device-token", "provider-key"] {
                    let _ = remove_private(&self.dir.join(f));
                }
                std::fs::remove_dir_all(&self.dir)
                    .with_context(|| format!("removing {}", self.dir.display()))?;
                Ok(true)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e.into()),
        }
    }
}

/// Profile names under `root`, sorted.
pub fn list_profiles(root: &Path) -> Result<Vec<String>> {
    let dir = root.join("profiles");
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e.into()),
    };
    for e in entries {
        let e = e?;
        if let Some(n) = e.file_name().to_str()
            && validate_profile_name(n).is_ok()
            && e.path().join("profile.json").is_file()
        {
            out.push(n.to_string());
        }
    }
    out.sort();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_names() {
        for ok in ["default", "work", "a", "0", "my-claude_2"] {
            validate_profile_name(ok).unwrap();
        }
        for bad in [
            "",
            ".",
            "..",
            "../x",
            "a/b",
            "A",
            "-a",
            "a b",
            "é",
            &"a".repeat(33),
        ] {
            assert!(validate_profile_name(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn device_tokens() {
        validate_device_token(&Secret::new("metro_pc_0123456789abcdefXYZ_-")).unwrap();
        for bad in [
            "",
            "metro_pc_",
            "metro_pc_short",
            "metro_rt_v2_0123456789abcdef",
            "sk-0123456789abcdef0123",
            "metro_pc_0123456789abcdef with space",
        ] {
            assert!(validate_device_token(&Secret::new(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn roundtrip() {
        let t = tempfile::tempdir().unwrap();
        let p = Profile::open(t.path(), "work").unwrap();
        assert!(p.load().is_err());
        let cfg = ProfileConfig {
            version: 1,
            kind: Kind::OpenAiCompatible,
            server_url: "https://metro.example.com".into(),
            models: vec!["llama3".into()],
            base_url: Some("http://127.0.0.1:11434/v1".into()),
            request_timeout_secs: 240,
        };
        p.save(&cfg).unwrap();
        assert_eq!(p.load().unwrap(), cfg);
        let metro = "https://metro.example.com";
        let base = "http://127.0.0.1:11434/v1";
        p.save_device_token(metro, &Secret::new("metro_pc_0123456789abcdef0123"))
            .unwrap();
        assert_eq!(
            p.device_token_for(metro).unwrap().expose(),
            "metro_pc_0123456789abcdef0123"
        );
        // Bound to its server: never handed out for another one.
        assert!(p.device_token_for("https://other.example.com").is_err());
        assert_eq!(
            p.device_token_destination().unwrap().as_deref(),
            Some(metro)
        );
        assert!(p.provider_key_for(base).unwrap().is_none());
        p.save_provider_key(base, Some(&Secret::new("k-123")))
            .unwrap();
        assert_eq!(p.provider_key_for(base).unwrap().unwrap().expose(), "k-123");
        assert!(p.provider_key_for("https://api.example.com/v1").is_err());
        assert_eq!(p.secrets_for_redaction().len(), 2);
        // Earlier builds wrote 300; values above the effective limit load as
        // the limit, and saving a value above it is refused.
        let mut v: serde_json::Value =
            serde_json::from_slice(&std::fs::read(p.dir.join("profile.json")).unwrap()).unwrap();
        v["request_timeout_secs"] = 3600.into();
        std::fs::write(p.dir.join("profile.json"), v.to_string()).unwrap();
        assert_eq!(
            p.load().unwrap().request_timeout_secs,
            MAX_REQUEST_TIMEOUT_SECS
        );
        let mut too_long = p.load().unwrap();
        too_long.request_timeout_secs = 300;
        assert!(p.save(&too_long).is_err());
        // A plain-text token from an older build is never used.
        std::fs::write(p.dir.join("device-token"), "metro_pc_0123456789abcdef0123").unwrap();
        assert!(p.device_token_for(metro).is_err());
        assert_eq!(list_profiles(t.path()).unwrap(), vec!["work".to_string()]);
        assert!(p.forget_all().unwrap());
        assert!(!p.exists());

        let mut bad = cfg.clone();
        bad.base_url = None;
        assert!(bad.validate().is_err());
        let mut bad = cfg.clone();
        bad.kind = Kind::ClaudeCode;
        assert!(bad.validate().is_err());
        let mut bad = cfg;
        bad.models = vec!["a".into(), "a".into()];
        assert!(bad.validate().is_err());
        assert!(
            serde_json::from_str::<ProfileConfig>(
                r#"{"version":1,"kind":"device-claude-code","server_url":"https://m","models":[],"extra":1}"#
            )
            .is_err()
        );
    }
}
