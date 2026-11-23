//! The per-connection profile directory: a private directory, atomic private
//! JSON files, and a cross-process lock file.
//!
//! - Unix: a mode-0700 directory of mode-0600 files, checked on every open.
//! - Windows: a directory with a protected DACL (this user and SYSTEM only,
//!   inherited by its files), re-applied whenever a sign-in opens it, and every
//!   file sealed with DPAPI for the current user, so a copied or mis-shared
//!   file still can't be read by another account. Nothing is ever written in
//!   plain text there.

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
                check_dir(&meta, dir, create)?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if !create {
                    return Ok(None);
                }
                create_private_dir(dir)?;
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
        let bytes = unseal(bytes).with_context(|| format!("reading {name}"))?;
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
        let bytes = seal(serde_json::to_vec_pretty(value).context("encoding profile file")?)
            .with_context(|| format!("writing {name}"))?;
        let tmp = self
            .dir
            .join(format!(".{name}.tmp-{}", hex(&random_bytes::<8>())));
        let result = (|| -> Result<()> {
            let mut f = create_new_private(&tmp)?;
            f.write_all(&bytes)?;
            f.sync_all()?;
            drop(f);
            replace_profile_file(&tmp, &self.dir.join(name))?;
            sync_dir(&self.dir)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result.with_context(|| format!("writing {name}"))
    }
}

#[cfg(not(windows))]
fn replace_profile_file(from: &Path, to: &Path) -> std::io::Result<()> {
    fs::rename(from, to)
}

#[cfg(windows)]
fn replace_profile_file(from: &Path, to: &Path) -> std::io::Result<()> {
    retry_replace(|| fs::rename(from, to))
}

#[cfg(windows)]
fn retry_replace(mut replace: impl FnMut() -> std::io::Result<()>) -> std::io::Result<()> {
    use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_SHARING_VIOLATION};

    // A concurrent status read can briefly block replacement. Windows can
    // report either sharing violation or access denied for a busy target.
    // Retry only the atomic rename of the same flushed, sealed file, with
    // 150 ms of total backoff. Persistent denial is still returned to the
    // caller, leaving the existing file in place.
    for ms in [10, 20, 40, 80] {
        match replace() {
            Err(e)
                if matches!(
                    e.raw_os_error().map(|code| code as u32),
                    Some(ERROR_SHARING_VIOLATION | ERROR_ACCESS_DENIED)
                ) =>
            {
                std::thread::sleep(Duration::from_millis(ms));
            }
            result => return result,
        }
    }
    replace()
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

#[cfg(windows)]
fn platform_check() -> Result<()> {
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn platform_check() -> Result<()> {
    bail!("the OpenAI device connection has no private credential store on this platform")
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

/// Windows files carry no mode: they inherit the directory's protected DACL
/// and their contents are DPAPI-sealed (see `seal`).
#[cfg(not(unix))]
fn check_mode(_: &fs::Metadata, _: u32, _: &Path) -> Result<()> {
    platform_check()
}

#[cfg(unix)]
fn check_dir(meta: &fs::Metadata, dir: &Path, _create: bool) -> Result<()> {
    check_mode(meta, 0o700, dir)
}

/// A sign-in (`create`) re-applies the protected DACL, so a directory whose
/// ACL was widened is narrowed before anything is written into it. Reads
/// don't rewrite ACLs: every file is DPAPI-sealed for this user anyway.
#[cfg(windows)]
fn check_dir(_meta: &fs::Metadata, dir: &Path, create: bool) -> Result<()> {
    if create {
        crate::desktop::store::protect_private_dir(dir)
            .with_context(|| format!("securing {}", dir.display()))?;
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn check_dir(_: &fs::Metadata, _: &Path, _: bool) -> Result<()> {
    platform_check()
}

#[cfg(unix)]
fn create_private_dir(dir: &Path) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    fs::DirBuilder::new()
        .mode(0o700)
        .create(dir)
        .with_context(|| format!("creating {}", dir.display()))?;
    // Enforce it regardless of umask.
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(windows)]
fn create_private_dir(dir: &Path) -> Result<()> {
    fs::create_dir(dir).with_context(|| format!("creating {}", dir.display()))?;
    crate::desktop::store::protect_private_dir(dir)
        .with_context(|| format!("securing {}", dir.display()))
}

#[cfg(not(any(unix, windows)))]
fn create_private_dir(_: &Path) -> Result<()> {
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

/// The lock file holds nothing secret; it only has to be a plain file, never
/// a link, inside the protected directory.
#[cfg(windows)]
fn open_private(path: &Path, create: bool) -> Result<fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    // FILE_FLAG_OPEN_REPARSE_POINT: open a link itself, never its target.
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    if let Ok(meta) = fs::symlink_metadata(path)
        && (!meta.is_file() || meta.file_type().is_symlink())
    {
        bail!("{} is not a regular file", path.display());
    }
    let f = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(create)
        .truncate(false)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    if !f.metadata()?.is_file() {
        bail!("{} is not a regular file", path.display());
    }
    Ok(f)
}

#[cfg(not(any(unix, windows)))]
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

#[cfg(windows)]
fn create_new_private(path: &Path) -> Result<fs::File> {
    Ok(fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?)
}

#[cfg(not(any(unix, windows)))]
fn create_new_private(_: &Path) -> Result<fs::File> {
    platform_check()?;
    unreachable!()
}

/// Unix files are already private by mode: stored as they are.
#[cfg(not(windows))]
fn seal(plain: Vec<u8>) -> Result<Vec<u8>> {
    Ok(plain)
}

#[cfg(not(windows))]
fn unseal(stored: Vec<u8>) -> Result<Vec<u8>> {
    Ok(stored)
}

#[cfg(windows)]
fn seal(mut plain: Vec<u8>) -> Result<Vec<u8>> {
    let r = dpapi::protect(&plain);
    zeroize::Zeroize::zeroize(&mut plain);
    r
}

#[cfg(windows)]
fn unseal(stored: Vec<u8>) -> Result<Vec<u8>> {
    dpapi::unprotect(&stored)
}

/// DPAPI for the current Windows user (`CryptProtectData` without
/// `CRYPTPROTECT_LOCAL_MACHINE`): only this account on this computer can
/// unseal what it seals.
#[cfg(windows)]
mod dpapi {
    use anyhow::{Result, bail};
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
    };

    /// Marks a sealed file. A file without it is refused, never read as plain text.
    const MAGIC: &[u8] = b"metro-dpapi-v1\n";
    /// Binds the blobs to this store: another DPAPI user can't reuse them.
    const ENTROPY: &[u8] = b"metro-connector openai_oauth profile v1";

    fn blob(b: &[u8]) -> CRYPT_INTEGER_BLOB {
        CRYPT_INTEGER_BLOB {
            cbData: b.len() as u32,
            pbData: b.as_ptr() as *mut u8,
        }
    }

    /// Copies DPAPI's output, wipes it and frees it.
    ///
    /// SAFETY: `out` must be a blob DPAPI allocated with LocalAlloc.
    unsafe fn take(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
        unsafe {
            let s = std::slice::from_raw_parts_mut(out.pbData, out.cbData as usize);
            let v = s.to_vec();
            s.fill(0);
            LocalFree(out.pbData.cast());
            v
        }
    }

    pub fn protect(plain: &[u8]) -> Result<Vec<u8>> {
        let input = blob(plain);
        let entropy = blob(ENTROPY);
        let mut out = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };
        // SAFETY: the input blobs borrow live slices; DPAPI allocates `out`.
        let ok = unsafe {
            CryptProtectData(
                &input,
                std::ptr::null(),
                &entropy,
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        };
        if ok == 0 || out.pbData.is_null() {
            bail!("Windows could not protect the sign-in data for this user (DPAPI)");
        }
        // SAFETY: allocated by CryptProtectData.
        let sealed = unsafe { take(out) };
        let mut v = MAGIC.to_vec();
        v.extend_from_slice(&sealed);
        Ok(v)
    }

    pub fn unprotect(stored: &[u8]) -> Result<Vec<u8>> {
        let Some(sealed) = stored.strip_prefix(MAGIC) else {
            bail!("the file is not sealed for this Windows user; sign in again");
        };
        let input = blob(sealed);
        let entropy = blob(ENTROPY);
        let mut out = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };
        // SAFETY: as in `protect`.
        let ok = unsafe {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                &entropy,
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        };
        if ok == 0 || out.pbData.is_null() {
            bail!("the file can't be unsealed by this Windows user; sign in again");
        }
        // SAFETY: allocated by CryptUnprotectData.
        Ok(unsafe { take(out) })
    }
}

fn sync_dir(_dir: &Path) -> Result<()> {
    #[cfg(unix)]
    fs::File::open(_dir)?.sync_all()?;
    Ok(())
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_SHARING_VIOLATION};

    #[test]
    fn replace_retries_when_a_reader_releases_the_account_file() {
        const FILE_SHARE_READ: u32 = 1;

        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("profile");
        let profile = Profile::open(&dir, true).unwrap().unwrap();
        let target = dir.join(ACCOUNT_FILE);
        let next = dir.join("next.json");
        profile.write(ACCOUNT_FILE, &"old").unwrap();
        profile.write("next.json", &"new").unwrap();
        let mut reader = Some(
            fs::OpenOptions::new()
                .read(true)
                // Deliberately omit delete sharing to force the first failure.
                .share_mode(FILE_SHARE_READ)
                .open(&target)
                .unwrap(),
        );
        let mut tries = 0;
        retry_replace(|| {
            tries += 1;
            let result = fs::rename(&next, &target);
            if tries == 1 {
                assert!(matches!(
                    result
                        .as_ref()
                        .unwrap_err()
                        .raw_os_error()
                        .map(|code| code as u32),
                    Some(ERROR_SHARING_VIOLATION | ERROR_ACCESS_DENIED)
                ));
                assert_eq!(
                    profile.read::<String>(ACCOUNT_FILE).unwrap().as_deref(),
                    Some("old")
                );
                // Release only after the real failed rename; no timing race.
                drop(reader.take());
            }
            result
        })
        .unwrap();
        assert_eq!(tries, 2);
        assert_eq!(
            profile.read::<String>(ACCOUNT_FILE).unwrap().as_deref(),
            Some("new")
        );
        assert!(!next.exists());
    }

    #[test]
    fn write_preserves_the_sealed_account_when_replacement_is_denied() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("profile");
        let profile = Profile::open(&dir, true).unwrap().unwrap();
        let target = dir.join(ACCOUNT_FILE);
        profile.write(ACCOUNT_FILE, &"old").unwrap();
        let original = fs::read(&target).unwrap();
        let perms = target.metadata().unwrap().permissions();
        let mut readonly = perms.clone();
        readonly.set_readonly(true);
        fs::set_permissions(&target, readonly).unwrap();
        let result = profile.write(ACCOUNT_FILE, &"new");
        fs::set_permissions(&target, perms).unwrap();

        let err = result.unwrap_err();
        assert_eq!(
            err.downcast_ref::<std::io::Error>()
                .and_then(std::io::Error::raw_os_error),
            Some(ERROR_ACCESS_DENIED as i32)
        );
        assert!(fs::read(&target).unwrap() == original);
        assert_eq!(
            profile.read::<String>(ACCOUNT_FILE).unwrap().as_deref(),
            Some("old")
        );
        assert_eq!(
            fs::read_dir(&dir).unwrap().count(),
            1,
            "no abandoned temp file"
        );
    }
}
