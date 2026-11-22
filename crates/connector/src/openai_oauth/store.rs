//! The per-connection profile directory: mode-0700 dir, atomic mode-0600
//! JSON files, and a cross-process lock file.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use rand::RngCore;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use super::Endpoints;

const HOST_FILE: &str = "host.json";
const ACCOUNT_FILE: &str = "account.json";
const TOKENS_FILE: &str = "tokens.json";
const LOCK_FILE: &str = ".lock";
const MAX_FILE: u64 = 1 << 20;
const LOCK_WAIT: Duration = Duration::from_secs(90);

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Host {
    pub ext_agent_host_id: String,
    pub created_at: u64,
}

/// The registration this profile is bound to. It outlives sign-out.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Account {
    pub endpoints: Endpoints,
    /// The issued client ID (never `dynamic_agent_client`).
    pub client_id: String,
    /// Validated ID-token `sub`. It's `None` only while a registration that
    /// got its client ID but never completed a code exchange is incomplete.
    pub subject: Option<String>,
    pub email: Option<String>,
    pub plan_usage_enabled: bool,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Tokens {
    pub id_token: String,
    pub access_token: String,
    pub refresh_token: String,
    pub token_type: String,
    /// Unix seconds.
    pub expires_at: u64,
    pub earliest_refresh_at: Option<u64>,
    pub scopes: Vec<String>,
    pub saved_at: u64,
}

impl std::fmt::Debug for Tokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tokens")
            .field("expires_at", &self.expires_at)
            .field("scopes", &self.scopes)
            .finish_non_exhaustive()
    }
}

pub(crate) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub(crate) struct Profile {
    dir: PathBuf,
}

/// Holds the profile's exclusive lock until it's dropped.
pub(crate) struct Lock {
    _file: fs::File,
}

impl Profile {
    /// Opens an existing profile. With `create`, a missing leaf directory is
    /// created with mode 0700. An existing directory must already be 0700 and
    /// must not be a symlink.
    pub fn open(dir: &Path, create: bool) -> Result<Option<Self>> {
        platform_check()?;
        match fs::symlink_metadata(dir) {
            Ok(meta) => {
                if meta.file_type().is_symlink() || !meta.is_dir() {
                    bail!(
                        "connection profile {} is not a plain directory",
                        dir.display()
                    );
                }
                check_mode(&meta, 0o700, dir)?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if !create {
                    return Ok(None);
                }
                create_dir_700(dir)?;
            }
            Err(e) => return Err(e).with_context(|| format!("reading {}", dir.display())),
        }
        Ok(Some(Self {
            dir: dir.to_path_buf(),
        }))
    }

    /// Takes the exclusive lock. It serializes refresh, sign-in commits and
    /// sign-out across processes.
    pub async fn lock(&self) -> Result<Lock> {
        let path = self.dir.join(LOCK_FILE);
        let file = open_private(&path, true)?;
        let started = Instant::now();
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Lock { _file: file }),
                Err(fs::TryLockError::WouldBlock) => {
                    if started.elapsed() > LOCK_WAIT {
                        bail!("timed out waiting for another Metro process using this connection");
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                Err(fs::TryLockError::Error(e)) => {
                    return Err(e).context("locking the connection profile");
                }
            }
        }
    }

    pub fn host(&self) -> Result<Option<Host>> {
        self.read(HOST_FILE)
    }

    /// The host ID, created once and kept for the profile's lifetime. The
    /// caller must hold the lock.
    pub fn host_or_create(&self, _lock: &Lock) -> Result<Host> {
        if let Some(h) = self.host()? {
            valid_host_id(&h.ext_agent_host_id)?;
            return Ok(h);
        }
        let host = Host {
            ext_agent_host_id: format!("urn:uuid:{}", uuid_v4()),
            created_at: now(),
        };
        self.write(HOST_FILE, &host)?;
        Ok(host)
    }

    pub fn account(&self) -> Result<Option<Account>> {
        self.read(ACCOUNT_FILE)
    }

    pub fn write_account(&self, _lock: &Lock, account: &Account) -> Result<()> {
        self.write(ACCOUNT_FILE, account)
    }

    pub fn tokens(&self) -> Result<Option<Tokens>> {
        self.read(TOKENS_FILE)
    }

    pub fn write_tokens(&self, _lock: &Lock, tokens: &Tokens) -> Result<()> {
        self.write(TOKENS_FILE, tokens)
    }

    pub fn clear_tokens(&self, _lock: &Lock) -> Result<()> {
        match fs::remove_file(self.dir.join(TOKENS_FILE)) {
            Ok(()) => sync_dir(&self.dir),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).context("clearing local tokens"),
        }
    }

    fn read<T: DeserializeOwned>(&self, name: &str) -> Result<Option<T>> {
        let path = self.dir.join(name);
        let meta = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).with_context(|| format!("reading {name}")),
        };
        if !meta.is_file() {
            bail!("{name} in the connection profile is not a regular file");
        }
        check_mode(&meta, 0o600, &path)?;
        if meta.len() > MAX_FILE {
            bail!("{name} in the connection profile is too large");
        }
        let bytes = fs::read(&path).with_context(|| format!("reading {name}"))?;
        // The error never includes file contents.
        let value = serde_json::from_slice(&bytes).map_err(|e| {
            anyhow::anyhow!(
                "{name} in the connection profile is malformed (line {})",
                e.line()
            )
        })?;
        Ok(Some(value))
    }

    fn write<T: Serialize>(&self, name: &str, value: &T) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(value).context("encoding profile file")?;
        let tmp = self
            .dir
            .join(format!(".{name}.tmp-{}", hex(&random_bytes::<8>())));
        let result = (|| -> Result<()> {
            let mut f = create_new_private(&tmp)?;
            f.write_all(&bytes)?;
            f.sync_all()?;
            drop(f);
            fs::rename(&tmp, self.dir.join(name))?;
            sync_dir(&self.dir)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result.with_context(|| format!("writing {name}"))
    }
}

fn valid_host_id(id: &str) -> Result<()> {
    let ok = (id.starts_with("urn:uuid:")
        || id.starts_with("urn:ietf:params:oauth:jwk-thumbprint:")
        || id.starts_with("did:key:"))
        && id.len() <= 256
        && id.bytes().all(|b| b.is_ascii_graphic());
    if !ok {
        bail!("host.json holds an unsupported ext_agent_host_id format");
    }
    Ok(())
}

pub(crate) fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    rand::rng().fill_bytes(&mut b);
    b
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub(crate) fn uuid_v4() -> String {
    let mut b = random_bytes::<16>();
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h = hex(&b);
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

#[cfg(unix)]
fn platform_check() -> Result<()> {
    Ok(())
}

#[cfg(not(unix))]
fn platform_check() -> Result<()> {
    bail!("the OpenAI device connection needs Unix file permissions for its credential store")
}

#[cfg(unix)]
fn check_mode(meta: &fs::Metadata, want: u32, path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = meta.permissions().mode() & 0o777;
    if mode != want {
        bail!(
            "{} must have mode {want:o} (it has {mode:o}); fix it with chmod {want:o}",
            path.display()
        );
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_mode(_: &fs::Metadata, _: u32, _: &Path) -> Result<()> {
    platform_check()
}

#[cfg(unix)]
fn create_dir_700(dir: &Path) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    fs::DirBuilder::new()
        .mode(0o700)
        .create(dir)
        .with_context(|| format!("creating {}", dir.display()))?;
    // Enforce it regardless of umask.
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn create_dir_700(_: &Path) -> Result<()> {
    platform_check()
}

#[cfg(unix)]
fn open_private(path: &Path, create: bool) -> Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    if let Ok(meta) = fs::symlink_metadata(path)
        && !meta.is_file()
    {
        bail!("{} is not a regular file", path.display());
    }
    let f = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(create)
        .truncate(false)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    Ok(f)
}

#[cfg(not(unix))]
fn open_private(_: &Path, _: bool) -> Result<fs::File> {
    platform_check()?;
    unreachable!()
}

#[cfg(unix)]
fn create_new_private(path: &Path) -> Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    Ok(fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?)
}

#[cfg(not(unix))]
fn create_new_private(_: &Path) -> Result<fs::File> {
    platform_check()?;
    unreachable!()
}

fn sync_dir(dir: &Path) -> Result<()> {
    #[cfg(unix)]
    fs::File::open(dir)?.sync_all()?;
    Ok(())
}
