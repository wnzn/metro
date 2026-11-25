//! Refreshing the Sigstore trusted root through TUF (desktop and server).
//!
//! Metro verifies releases against its vendored `trusted_root.json`. When
//! Sigstore rotates keys or logs, an older app can no longer verify new
//! releases (`trust_root_stale`). Then, **only then and at most once a week**,
//! Metro fetches the current `trusted_root.json` from
//! `https://tuf-repo-cdn.sigstore.dev` and verifies it with TUF, starting
//! from the vendored TUF root (v16):
//!
//! - root rotation, each new root signed by a threshold of both the old and
//!   the new root's keys; timestamp → snapshot → targets, with signature
//!   thresholds, expiry, version rollback protection (the private cache
//!   keeps the version floors across runs) and length/hash pinning of the
//!   target. This is `sigstore-tuf` (pinned `=0.17.0`, sigstore-rust, the same
//!   release as `sigstore-verify`), driven over Metro's own fetcher.
//! - The fetcher only talks to `tuf-repo-cdn.sigstore.dev` (HTTPS, no other
//!   redirect host), anonymously, with size caps.
//!
//! A refreshed root only replaces Sigstore's *trust material* (CAs, logs,
//! timestamp authorities). The release signer identity and the GitHub
//! claims stay pinned in [`crate::sigstore`] and [`crate::policy`]: nothing
//! here can make a different signer acceptable.

use std::fs::{File, Metadata};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use sigstore_tuf::client::Updater;
use sigstore_tuf::transport::{FetchFuture, Repository};

use crate::error::{Error, ErrorCode};
use crate::fetch::{AsyncFetcher, Source};
use crate::release::{ReleaseVerifier, Signed, SignedFacts};
use crate::sigstore::{SigstoreVerifier, VENDORED_TUF_ROOT};

pub use crate::fetch::{TUF_HOST, TUF_REPOSITORY};

/// At most one network refresh per this period.
pub const REFRESH_INTERVAL_SECS: i64 = 7 * 24 * 3600;
/// A cached refreshed root is used for at most this long after it verified.
pub const CACHE_MAX_AGE_SECS: i64 = 90 * 24 * 3600;
const TARGET: &str = "trusted_root.json";
const MAX_TRUSTED_ROOT: u64 = 256 * 1024;
const MAX_ROOT_ROTATIONS: u64 = 128;

/// `Repository` over Metro's fetcher (pinned host list, caps, no cookies).
struct MetroRepository {
    fetcher: AsyncFetcher,
}

impl Repository for MetroRepository {
    fn fetch_metadata<'a>(&'a self, name: &'a str, max_length: u64) -> FetchFuture<'a> {
        Box::pin(self.fetch(name, max_length))
    }

    fn fetch_target<'a>(&'a self, path: &'a str, max_length: u64) -> FetchFuture<'a> {
        Box::pin(async move { self.fetch(&format!("targets/{path}"), max_length).await })
    }
}

impl MetroRepository {
    async fn fetch(&self, name: &str, max_length: u64) -> sigstore_tuf::Result<Option<Vec<u8>>> {
        let url = self
            .fetcher
            .source
            .tuf_url(name)
            .map_err(|e| sigstore_tuf::Error::transport(e.to_string()))?;
        let cap = usize::try_from(max_length.min(8 * 1024 * 1024)).unwrap_or(usize::MAX);
        self.fetcher
            .get_optional(&url, cap)
            .await
            .map_err(|e| sigstore_tuf::Error::transport(e.to_string()))
    }
}

/// The private refresh cache: `<dir>/tuf/` (TUF metadata and version
/// floors), `<dir>/trusted_root.json` (the last verified target) and
/// `<dir>/last_refresh` (the last attempt, for the weekly limit).
pub struct TrustCache {
    dir: PathBuf,
    source: Source,
    tuf_root: Vec<u8>,
}

/// Trust material plus the time established when replaying the signed chain.
/// Only a successful replay can construct one.
#[derive(Debug)]
pub struct FreshVerifier {
    verifier: SigstoreVerifier,
    verified_at: jiff::Timestamp,
    loaded_at: std::time::Instant,
    age_when_loaded: u64,
}

impl FreshVerifier {
    fn fresh_at(&self, now: jiff::Timestamp) -> bool {
        now >= self.verified_at
            && now.as_second().saturating_sub(self.verified_at.as_second()) < CACHE_MAX_AGE_SECS
            && self
                .age_when_loaded
                .saturating_add(self.loaded_at.elapsed().as_secs())
                < CACHE_MAX_AGE_SECS as u64
    }

    #[doc(hidden)]
    pub fn verify_at(
        &self,
        artifact: Signed<'_>,
        bundle_json: &[u8],
        now: jiff::Timestamp,
    ) -> Result<SignedFacts, Error> {
        if !self.fresh_at(now) {
            return Err(Error::new(
                ErrorCode::TrustRootStale,
                "cached Sigstore trust expired",
            ));
        }
        self.verifier.verify(artifact, bundle_json)
    }
}

impl ReleaseVerifier for FreshVerifier {
    fn verify(&self, artifact: Signed<'_>, bundle_json: &[u8]) -> Result<SignedFacts, Error> {
        self.verify_at(artifact, bundle_json, jiff::Timestamp::now())
    }
}

impl TrustCache {
    /// Production: Sigstore's CDN, anchored at the vendored TUF root.
    pub fn new(dir: &Path) -> TrustCache {
        TrustCache {
            dir: dir.to_path_buf(),
            source: Source::sigstore_tuf(),
            tuf_root: VENDORED_TUF_ROOT.to_vec(),
        }
    }

    /// Tests: a loopback fixture repository and a given bootstrap root.
    #[doc(hidden)]
    pub fn with_source(dir: &Path, source: Source, tuf_root: &[u8]) -> TrustCache {
        TrustCache {
            dir: dir.to_path_buf(),
            source,
            tuf_root: tuf_root.to_vec(),
        }
    }

    fn private_dir(&self, create: bool) -> io::Result<CacheDir> {
        CacheDir::root(&self.dir, create)
    }

    /// The trusted root a previous refresh verified, accepted only after the
    /// cached signed TUF chain is replayed from the vendored root: every root
    /// rotation (thresholds of old and new keys), timestamp → snapshot →
    /// targets (signatures, versions, pinned lengths and hashes), with expiry
    /// evaluated at the recorded verification time, which must be in the
    /// past and at most [`CACHE_MAX_AGE_SECS`] old. The cache folder and files
    /// must be private (owned by this user, not writable by others, no
    /// links). Anything else: the cache is ignored.
    pub fn cached_verifier(&self, now: jiff::Timestamp) -> Option<FreshVerifier> {
        match self.replay(now) {
            Ok(v) => Some(v),
            Err(why) => {
                if why.kind() != io::ErrorKind::NotFound {
                    eprintln!("metro: the cached Sigstore trusted root was not used: {why}");
                }
                None
            }
        }
    }

    fn replay(&self, now: jiff::Timestamp) -> io::Result<FreshVerifier> {
        let loaded_at = std::time::Instant::now();
        let dir = self.private_dir(false)?;
        let at = parse_time(&dir.read("verified_at", 32)?)?;
        if at > now.as_second() || now.as_second().saturating_sub(at) >= CACHE_MAX_AGE_SECS {
            return Err(invalid(
                "the cached verification is from the future or too old",
            ));
        }
        let at = jiff::Timestamp::from_second(at).map_err(io::Error::other)?;
        let tuf = io::Error::other;
        let mut set =
            sigstore_tuf::trusted::TrustedMetadataSet::from_root(&self.tuf_root).map_err(tuf)?;
        let tuf_dir = dir.child("tuf", false)?;
        let history = tuf_dir.child("root_history", false)?;
        for rotation in 0..=MAX_ROOT_ROTATIONS {
            let next = set
                .root()
                .version
                .checked_add(1)
                .ok_or_else(|| invalid("root version overflow"))?;
            match optional(history.read(&format!("{next}.root.json"), 512_000))? {
                Some(raw) if rotation < MAX_ROOT_ROTATIONS => set.update_root(&raw).map_err(tuf)?,
                Some(_) => return Err(invalid("too many cached root rotations")),
                None => break,
            }
        }
        set.check_root_expired(at).map_err(tuf)?;
        set.update_timestamp(&tuf_dir.read("timestamp.json", 16_384)?, at)
            .map_err(tuf)?;
        set.update_snapshot(&tuf_dir.read("snapshot.json", 2_000_000)?, at)
            .map_err(tuf)?;
        let targets = set
            .update_targets(&tuf_dir.read("targets.json", 5_000_000)?, at)
            .map_err(tuf)?;
        let pinned = targets
            .targets
            .get(TARGET)
            .ok_or_else(|| invalid("trusted_root.json isn't a TUF target"))?;
        let raw = dir.read(TARGET, MAX_TRUSTED_ROOT)?;
        if pinned.length != raw.len() as u64
            || pinned.hashes.get("sha256").map(String::as_str)
                != Some(crate::hex(&crate::sha256(&raw)).as_str())
        {
            return Err(invalid(
                "the cached trusted root doesn't match its TUF metadata",
            ));
        }
        let text = std::str::from_utf8(&raw).map_err(io::Error::other)?;
        let verifier = SigstoreVerifier::from_trusted_root_json(text).map_err(io::Error::other)?;
        Ok(FreshVerifier {
            verifier,
            verified_at: at,
            loaded_at,
            age_when_loaded: now.as_second().saturating_sub(at.as_second()) as u64,
        })
    }

    fn last_attempt(dir: &CacheDir) -> io::Result<Option<i64>> {
        optional(dir.read("last_refresh", 32))?
            .as_deref()
            .map(parse_time)
            .transpose()
    }

    /// Whether a network refresh would be allowed now (read-only).
    pub fn refresh_due(&self, now: jiff::Timestamp) -> bool {
        match optional(self.private_dir(false)) {
            Ok(None) => true,
            Ok(Some(dir)) => Self::last_attempt(&dir).is_ok_and(|last| refresh_due(last, now)),
            Err(_) => false,
        }
    }

    /// Reserve this week's refresh, atomically across processes (an
    /// kernel lock around read-decide-write). Never lower the recorded time
    /// when the clock goes backwards.
    pub fn reserve(&self, now: jiff::Timestamp) -> Result<bool, Error> {
        let io =
            |e: std::io::Error| Error::new(ErrorCode::Io, format!("trust cache: {}", e.kind()));
        let dir = self.private_dir(true).map_err(io)?;
        let Some(_lock) = dir.try_lock().map_err(io)? else {
            return Ok(false);
        };
        if !refresh_due(Self::last_attempt(&dir).map_err(io)?, now) {
            return Ok(false);
        }
        dir.write("last_refresh", now.as_second().to_string().as_bytes())
            .map_err(io)?;
        Ok(true)
    }

    /// Run the TUF refresh and return a verifier over the new trusted root
    /// (callers reserve first, see [`TrustCache::reserve`]).
    pub async fn refresh(&self, now: jiff::Timestamp) -> Result<FreshVerifier, Error> {
        let io =
            |e: std::io::Error| Error::new(ErrorCode::Io, format!("trust cache: {}", e.kind()));
        let dir = self.private_dir(true).map_err(io)?;
        let _lock = dir.try_lock().map_err(io)?.ok_or_else(|| {
            Error::new(ErrorCode::Io, "another trust refresh holds the cache lock")
        })?;
        let tuf_dir = dir.child("tuf", true).map_err(io)?;
        let history = tuf_dir.child("root_history", true).map_err(io)?;
        let tuf = |e: sigstore_tuf::Error| {
            Error::new(ErrorCode::NotVerified, format!("trust root refresh: {e}"))
        };
        let fetcher = AsyncFetcher::new(self.source.clone(), std::time::Duration::from_secs(60))?;
        let base = sigstore_tuf::trusted::TrustedMetadataSet::from_root(&self.tuf_root)
            .map_err(tuf)?
            .root()
            .version;
        let store = CacheStore {
            tuf: tuf_dir,
            history,
            base,
        };
        let mut updater = Updater::new(MetroRepository { fetcher }, &self.tuf_root)
            .map_err(tuf)?
            .with_store(store);
        updater.refresh(now).await.map_err(tuf)?;
        let bytes = updater.get_target(TARGET, now).await.map_err(tuf)?;
        if bytes.len() as u64 > MAX_TRUSTED_ROOT {
            return Err(Error::new(ErrorCode::TooLarge, "trusted root is too large"));
        }
        dir.write(TARGET, &bytes).map_err(io)?;
        dir.write("verified_at", now.as_second().to_string().as_bytes())
            .map_err(io)?;
        self.replay(now).map_err(io)
    }
}

fn invalid(reason: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, reason)
}

fn optional<T>(result: io::Result<T>) -> io::Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

fn parse_time(bytes: &[u8]) -> io::Result<i64> {
    std::str::from_utf8(bytes)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .filter(|t| jiff::Timestamp::from_second(*t).is_ok())
        .ok_or_else(|| invalid("bad trust cache time"))
}

fn refresh_due(last: Option<i64>, now: jiff::Timestamp) -> bool {
    last.is_none_or(|t| now.as_second().saturating_sub(t) >= REFRESH_INTERVAL_SECS)
}

struct CacheDir {
    #[cfg(unix)]
    handle: File,
    #[cfg(not(unix))]
    path: PathBuf,
    #[cfg(windows)]
    _pinned: Vec<File>,
}

impl CacheDir {
    #[cfg(unix)]
    fn root(path: &Path, create: bool) -> io::Result<Self> {
        let parent = crate::unix_fs::open_dir(path.parent().unwrap_or(Path::new(".")))?;
        let name = path
            .file_name()
            .ok_or_else(|| invalid("invalid trust cache path"))?;
        Self::from_dir(crate::unix_fs::child_dir(&parent, name, create)?)
    }

    #[cfg(not(unix))]
    fn root(path: &Path, create: bool) -> io::Result<Self> {
        let parent = path
            .parent()
            .ok_or_else(|| invalid("invalid trust cache path"))?;
        let _parent = crate::archive::pin_windows_dirs(parent)?;
        if create {
            match std::fs::create_dir(path) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e),
            }
        }
        let pinned = crate::archive::pin_windows_dirs(path)?;
        win::check_private(
            pinned
                .last()
                .ok_or_else(|| invalid("invalid trust cache path"))?,
        )?;
        Ok(Self {
            path: path.to_path_buf(),
            _pinned: pinned,
        })
    }

    #[cfg(unix)]
    fn from_dir(handle: File) -> io::Result<Self> {
        validate(&handle.metadata()?, true)?;
        Ok(Self { handle })
    }

    fn child(&self, name: &str, create: bool) -> io::Result<Self> {
        #[cfg(unix)]
        {
            Self::from_dir(crate::unix_fs::child_dir(
                &self.handle,
                name.as_ref(),
                create,
            )?)
        }
        #[cfg(not(unix))]
        {
            Self::root(&self.path.join(name), create)
        }
    }

    fn open(&self, name: &str, lock: bool) -> io::Result<File> {
        #[cfg(unix)]
        let file = {
            use rustix::fs::{Mode, OFlags, openat};
            let flags = if lock {
                crate::unix_fs::READ.difference(OFlags::RDONLY) | OFlags::RDWR | OFlags::CREATE
            } else {
                crate::unix_fs::READ
            };
            File::from(openat(&self.handle, name, flags, Mode::RUSR | Mode::WUSR)?)
        };
        #[cfg(not(unix))]
        let file = {
            let mut options = std::fs::OpenOptions::new();
            options.read(true).write(lock).create(lock);
            #[cfg(windows)]
            {
                use std::os::windows::fs::OpenOptionsExt;
                options
                    .custom_flags(0x0020_0000)
                    .share_mode(if lock { 3 } else { 1 });
            }
            options.open(self.path.join(name))?
        };
        validate(&file.metadata()?, false)?;
        #[cfg(windows)]
        win::check_private(&file)?;
        Ok(file)
    }

    fn read(&self, name: &str, cap: u64) -> io::Result<Vec<u8>> {
        let mut data = Vec::new();
        self.open(name, false)?
            .take(cap + 1)
            .read_to_end(&mut data)?;
        if data.len() as u64 > cap {
            return Err(invalid("a trust cache file is too large"));
        }
        Ok(data)
    }

    fn write(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        // An unsafe existing file is not "repaired" or followed.
        drop(optional(self.open(name, false))?);
        #[cfg(unix)]
        return crate::unix_fs::write_atomic(&self.handle, name, bytes);
        #[cfg(not(unix))]
        return crate::write_atomic(&self.path.join(name), bytes);
    }

    fn try_lock(&self) -> io::Result<Option<File>> {
        let lock = self.open("refresh.lock", true)?;
        match lock.try_lock() {
            Ok(()) => Ok(Some(lock)),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(e)) => Err(e),
        }
    }
}

fn validate(m: &Metadata, dir: bool) -> io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if m.file_attributes() & 0x400 != 0 {
            return Err(invalid("trust cache contains a reparse point"));
        }
    }
    if (dir && !m.is_dir()) || (!dir && !m.is_file()) {
        return Err(invalid("trust cache entry has the wrong type"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // SAFETY: geteuid has no preconditions.
        if m.uid() != unsafe { libc::geteuid() } || m.mode() & 0o077 != 0 {
            return Err(invalid("trust cache entry is not private to this user"));
        }
    }
    Ok(())
}

#[cfg(windows)]
mod win {
    use std::fs::File;
    use std::io;
    use std::os::windows::io::AsRawHandle;
    use std::ptr::{addr_of, null_mut};

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LocalFree};
    use windows_sys::Win32::Security::Authorization::{GetSecurityInfo, SE_FILE_OBJECT};
    use windows_sys::Win32::Security::{
        ACCESS_ALLOWED_ACE, ACE_HEADER, DACL_SECURITY_INFORMATION, EqualSid, GetAce,
        GetTokenInformation, IsWellKnownSid, OWNER_SECURITY_INFORMATION, TOKEN_QUERY, TOKEN_USER,
        TokenUser, WinLocalSystemSid,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    pub fn check_private(file: &File) -> io::Result<()> {
        let unsafe_acl = || super::invalid("trust cache is not private to this Windows account");
        // SAFETY: the access token and descriptor are system allocations
        // with checked returns. Owner/ACL/ACE pointers live in the descriptor;
        // the token-user SID lives in the aligned buffer.
        unsafe {
            let mut token: HANDLE = null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                return Err(io::Error::last_os_error());
            }
            let mut len = 0;
            GetTokenInformation(token, TokenUser, null_mut(), 0, &mut len);
            let mut buf = vec![0u64; (len as usize).div_ceil(8).max(1)];
            let ok = GetTokenInformation(token, TokenUser, buf.as_mut_ptr().cast(), len, &mut len);
            CloseHandle(token);
            if ok == 0 {
                return Err(unsafe_acl());
            }
            let user = (*buf.as_ptr().cast::<TOKEN_USER>()).User.Sid;
            let (mut owner, mut dacl, mut descriptor) = (null_mut(), null_mut(), null_mut());
            let rc = GetSecurityInfo(
                file.as_raw_handle().cast(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | OWNER_SECURITY_INFORMATION,
                &mut owner,
                null_mut(),
                &mut dacl,
                null_mut(),
                &mut descriptor,
            );
            if rc != 0 {
                return Err(io::Error::from_raw_os_error(rc as i32));
            }
            let accepted = !dacl.is_null()
                && !owner.is_null()
                && EqualSid(owner, user) != 0
                && (0..u32::from((*dacl).AceCount)).all(|index| {
                    let mut ace = null_mut();
                    if GetAce(dacl, index, &mut ace) == 0 || ace.is_null() {
                        return false;
                    }
                    let header = &*ace.cast::<ACE_HEADER>();
                    match header.AceType {
                        0 if usize::from(header.AceSize) >= size_of::<ACCESS_ALLOWED_ACE>() => {
                            let allowed = ace.cast::<ACCESS_ALLOWED_ACE>();
                            let sid = addr_of!((*allowed).SidStart).cast_mut().cast();
                            EqualSid(sid, user) != 0 || IsWellKnownSid(sid, WinLocalSystemSid) != 0
                        }
                        1 => true, // deny-only ACE cannot widen access
                        _ => false,
                    }
                });
            LocalFree(descriptor);
            if accepted { Ok(()) } else { Err(unsafe_acl()) }
        }
    }
}

struct CacheStore {
    tuf: CacheDir,
    history: CacheDir,
    base: u64,
}

impl CacheStore {
    fn entry<'a>(&self, name: &'a str) -> Option<(&CacheDir, &'a str, u64)> {
        let max = match name {
            "root.json" => 512_000,
            "timestamp.json" => 16_384,
            "snapshot.json" => 2_000_000,
            "targets.json" => 5_000_000,
            _ => {
                let file = name.strip_prefix("root_history/")?;
                let version: u64 = file.strip_suffix(".root.json")?.parse().ok()?;
                return (version >= self.base && version - self.base <= MAX_ROOT_ROTATIONS)
                    .then_some((&self.history, file, 512_000));
            }
        };
        Some((&self.tuf, name, max))
    }
}

impl sigstore_tuf::cache::MetadataStore for CacheStore {
    fn load(&self, name: &str) -> sigstore_tuf::Result<Option<Vec<u8>>> {
        let Some((dir, file, cap)) = self.entry(name) else {
            return Ok(None);
        };
        optional(dir.read(file, cap)).map_err(|e| sigstore_tuf::Error::io("reading trust cache", e))
    }

    fn store(&self, name: &str, bytes: &[u8]) -> sigstore_tuf::Result<()> {
        if let Some((dir, file, cap)) = self.entry(name) {
            if bytes.len() as u64 > cap {
                return Err(sigstore_tuf::Error::transport("TUF cache entry too large"));
            }
            dir.write(file, bytes)
                .map_err(|e| sigstore_tuf::Error::io("writing trust cache", e))?;
        }
        Ok(())
    }
}

/// The vendored root first; a refreshed root only when the vendored one is
/// too old (`trust_root_stale`). Same pinned signer policy either way.
pub struct LayeredVerifier {
    vendored: SigstoreVerifier,
    refreshed: Mutex<Option<FreshVerifier>>,
}

impl LayeredVerifier {
    pub fn new(vendored: SigstoreVerifier, refreshed: Option<FreshVerifier>) -> LayeredVerifier {
        LayeredVerifier {
            vendored,
            refreshed: Mutex::new(refreshed),
        }
    }

    pub fn set_refreshed(&self, v: FreshVerifier) {
        *self.refreshed.lock().unwrap_or_else(|p| p.into_inner()) = Some(v);
    }

    #[doc(hidden)]
    pub fn verify_at(
        &self,
        artifact: Signed<'_>,
        bundle_json: &[u8],
        now: jiff::Timestamp,
    ) -> Result<SignedFacts, Error> {
        self.verify_with_clock(artifact, bundle_json, || now)
    }

    fn verify_with_clock(
        &self,
        artifact: Signed<'_>,
        bundle_json: &[u8],
        clock: impl FnOnce() -> jiff::Timestamp,
    ) -> Result<SignedFacts, Error> {
        match self.vendored.verify(artifact, bundle_json) {
            Err(e) if e.code == ErrorCode::TrustRootStale => {
                let refreshed = self.refreshed.lock().unwrap_or_else(|p| p.into_inner());
                match refreshed.as_ref() {
                    Some(r) => r.verify_at(artifact, bundle_json, clock()),
                    _ => Err(e),
                }
            }
            other => other,
        }
    }
}

impl ReleaseVerifier for LayeredVerifier {
    fn verify(&self, artifact: Signed<'_>, bundle_json: &[u8]) -> Result<SignedFacts, Error> {
        self.verify_with_clock(artifact, bundle_json, jiff::Timestamp::now)
    }
}

/// What an app holds: the layered verifier plus its refresh cache.
pub struct TrustRefresh {
    pub cache: TrustCache,
    pub verifier: std::sync::Arc<LayeredVerifier>,
}

impl TrustRefresh {
    /// Production: vendored root plus any root a previous refresh verified.
    pub fn new(dir: &Path) -> Result<TrustRefresh, Error> {
        let cache = TrustCache::new(dir);
        let verifier = LayeredVerifier::new(
            SigstoreVerifier::vendored()?,
            cache.cached_verifier(jiff::Timestamp::now()),
        );
        Ok(TrustRefresh {
            cache,
            verifier: std::sync::Arc::new(verifier),
        })
    }

    /// After a `trust_root_stale` failure: refresh if due. `true` when a
    /// new root is in place and the caller should retry once.
    pub async fn refresh_if_due(&self) -> bool {
        self.refresh_if_due_at(jiff::Timestamp::now()).await
    }

    #[doc(hidden)]
    pub async fn refresh_if_due_at(&self, now: jiff::Timestamp) -> bool {
        if !matches!(self.cache.reserve(now), Ok(true)) {
            return false;
        }
        match self.cache.refresh(now).await {
            Ok(v) => {
                self.verifier.set_refreshed(v);
                true
            }
            Err(e) => {
                eprintln!("metro: Sigstore trust root refresh failed: {}", e.code());
                false
            }
        }
    }
}
