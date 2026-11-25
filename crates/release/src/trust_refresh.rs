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

    fn private_dir(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(self.dir.join("tuf"))?;
        tighten(&self.dir)
    }

    /// The trusted root a previous refresh verified, accepted only after the
    /// cached signed TUF chain is replayed from the vendored root: every root
    /// rotation (thresholds of old and new keys), timestamp → snapshot →
    /// targets (signatures, versions, pinned lengths and hashes), with expiry
    /// evaluated at the recorded verification time, which must be in the
    /// past and at most [`CACHE_MAX_AGE_SECS`] old. The cache folder and files
    /// must be private (owned by this user, not writable by others, no
    /// links). Anything else: the cache is ignored.
    pub fn cached_verifier(&self, now: jiff::Timestamp) -> Option<SigstoreVerifier> {
        match self.replay(now) {
            Ok(v) => Some(v),
            Err(why) => {
                if self.dir.join(TARGET).exists() {
                    eprintln!("metro: the cached Sigstore trusted root was not used: {why}");
                }
                None
            }
        }
    }

    fn replay(&self, now: jiff::Timestamp) -> Result<SigstoreVerifier, String> {
        let tuf_dir = self.dir.join("tuf");
        safe_entry(&self.dir, true, true)?;
        safe_entry(&tuf_dir, true, false)?;
        let read = |path: &Path, cap: u64| -> Result<Vec<u8>, String> {
            safe_entry(path, false, false)?;
            let raw =
                std::fs::read(path).map_err(|e| format!("{}: {}", path.display(), e.kind()))?;
            if raw.len() as u64 > cap {
                return Err("a cached file is too large".into());
            }
            Ok(raw)
        };
        let at: i64 = String::from_utf8(read(&self.dir.join("verified_at"), 32)?)
            .map_err(|_| "bad verification time")?
            .trim()
            .parse()
            .map_err(|_| "bad verification time")?;
        if at > now.as_second() + 300 || now.as_second() - at > CACHE_MAX_AGE_SECS {
            return Err("the cached verification is from the future or too old".into());
        }
        let at = jiff::Timestamp::from_second(at).map_err(|_| "bad verification time")?;
        let tuf = |e: sigstore_tuf::Error| e.to_string();
        let mut set =
            sigstore_tuf::trusted::TrustedMetadataSet::from_root(&self.tuf_root).map_err(tuf)?;
        let history = tuf_dir.join("root_history");
        loop {
            let next = history.join(format!("{}.root.json", set.root().version + 1));
            if std::fs::symlink_metadata(&next).is_err() {
                break;
            }
            safe_entry(&history, true, false)?;
            set.update_root(&read(&next, 512_000)?).map_err(tuf)?;
        }
        set.check_root_expired(at).map_err(tuf)?;
        set.update_timestamp(&read(&tuf_dir.join("timestamp.json"), 16_384)?, at)
            .map_err(tuf)?;
        set.update_snapshot(&read(&tuf_dir.join("snapshot.json"), 2_000_000)?, at)
            .map_err(tuf)?;
        let targets = set
            .update_targets(&read(&tuf_dir.join("targets.json"), 5_000_000)?, at)
            .map_err(tuf)?;
        let pinned = targets
            .targets
            .get(TARGET)
            .ok_or("trusted_root.json isn't a TUF target")?;
        let raw = read(&self.dir.join(TARGET), MAX_TRUSTED_ROOT)?;
        if pinned.length != raw.len() as u64
            || pinned.hashes.get("sha256").map(String::as_str)
                != Some(crate::hex(&crate::sha256(&raw)).as_str())
        {
            return Err("the cached trusted root doesn't match its TUF metadata".into());
        }
        let text = std::str::from_utf8(&raw).map_err(|_| "trusted root is not UTF-8")?;
        SigstoreVerifier::from_trusted_root_json(text).map_err(|e| e.to_string())
    }

    fn last_attempt(&self) -> Option<i64> {
        std::fs::read_to_string(self.dir.join("last_refresh"))
            .ok()?
            .trim()
            .parse()
            .ok()
    }

    /// Whether a network refresh would be allowed now (read-only).
    pub fn refresh_due(&self, now: jiff::Timestamp) -> bool {
        self.last_attempt()
            .is_none_or(|t| t <= now.as_second() && now.as_second() - t >= REFRESH_INTERVAL_SECS)
    }

    /// Reserve this week's refresh, atomically across processes (an
    /// exclusive lock file around read-decide-write). Conservative about
    /// clock changes: a last attempt in the future (the clock went back) is
    /// reset to now and nothing is reserved, so at most one refresh happens
    /// per week of the current clock.
    pub fn reserve(&self, now: jiff::Timestamp) -> Result<bool, Error> {
        let io =
            |e: std::io::Error| Error::new(ErrorCode::Io, format!("trust cache: {}", e.kind()));
        self.private_dir().map_err(io)?;
        let lock = self.dir.join("refresh.lock");
        let held = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock)
        {
            Ok(_) => true,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // Another process is deciding; a lock older than an hour was
                // left by a crash and is replaced.
                let old = std::fs::metadata(&lock)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.elapsed().ok())
                    .is_some_and(|age| age.as_secs() > 3600);
                if !old {
                    return Ok(false);
                }
                let _ = std::fs::remove_file(&lock);
                std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&lock)
                    .is_ok()
            }
            Err(e) => return Err(io(e)),
        };
        if !held {
            return Ok(false);
        }
        let decision = (|| {
            let now_s = now.as_second();
            match self.last_attempt() {
                Some(t) if t > now_s + 60 => {
                    crate::write_atomic(
                        &self.dir.join("last_refresh"),
                        now_s.to_string().as_bytes(),
                    )
                    .map_err(io)?;
                    Ok(false)
                }
                Some(t) if now_s - t < REFRESH_INTERVAL_SECS => Ok(false),
                _ => {
                    crate::write_atomic(
                        &self.dir.join("last_refresh"),
                        now_s.to_string().as_bytes(),
                    )
                    .map_err(io)?;
                    Ok(true)
                }
            }
        })();
        let _ = std::fs::remove_file(&lock);
        decision
    }

    /// Run the TUF refresh and return a verifier over the new trusted root
    /// (callers reserve first, see [`TrustCache::reserve`]).
    pub async fn refresh(&self, now: jiff::Timestamp) -> Result<SigstoreVerifier, Error> {
        let io =
            |e: std::io::Error| Error::new(ErrorCode::Io, format!("trust cache: {}", e.kind()));
        self.private_dir().map_err(io)?;
        let tuf = |e: sigstore_tuf::Error| {
            Error::new(ErrorCode::NotVerified, format!("trust root refresh: {e}"))
        };
        let fetcher = AsyncFetcher::new(self.source.clone(), std::time::Duration::from_secs(60))?;
        let mut updater = Updater::new(MetroRepository { fetcher }, &self.tuf_root)
            .map_err(tuf)?
            .with_store(sigstore_tuf::cache::FileStore::new(self.dir.join("tuf")));
        updater.refresh(now).await.map_err(tuf)?;
        let bytes = updater.get_target(TARGET, now).await.map_err(tuf)?;
        if bytes.len() as u64 > MAX_TRUSTED_ROOT {
            return Err(Error::new(ErrorCode::TooLarge, "trusted root is too large"));
        }
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| Error::new(ErrorCode::NotVerified, "trusted root"))?;
        let verifier = SigstoreVerifier::from_trusted_root_json(text)?;
        crate::write_atomic(&self.dir.join(TARGET), &bytes).map_err(io)?;
        crate::write_atomic(
            &self.dir.join("verified_at"),
            now.as_second().to_string().as_bytes(),
        )
        .map_err(io)?;
        // sigstore-tuf's store uses default permissions: make it private.
        tighten(&self.dir).map_err(io)?;
        Ok(verifier)
    }
}

/// Make the cache private: folders 0700, files 0600, links left alone (and
/// then refused by [`safe_entry`]).
fn tighten(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let m = std::fs::symlink_metadata(dir)?;
        if m.file_type().is_symlink() {
            return Ok(());
        }
        if m.is_dir() {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
            for e in std::fs::read_dir(dir)? {
                tighten(&e?.path())?;
            }
        } else if m.is_file() {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o600))?;
        }
    }
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

/// A cache entry Metro may trust: a plain folder or file (no link), owned by
/// this user and not writable by anyone else; `private` folders also not
/// readable by others.
fn safe_entry(path: &Path, dir: bool, private: bool) -> Result<(), String> {
    let m =
        std::fs::symlink_metadata(path).map_err(|e| format!("{}: {}", path.display(), e.kind()))?;
    if m.file_type().is_symlink() || (dir && !m.is_dir()) || (!dir && !m.is_file()) {
        return Err(format!(
            "{} is not a plain {}",
            path.display(),
            if dir { "folder" } else { "file" }
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // SAFETY: geteuid has no preconditions.
        let me = unsafe { libc::geteuid() };
        let forbidden = if private { 0o077 } else { 0o022 };
        if m.uid() != me || m.mode() & forbidden != 0 {
            return Err(format!("{} is not private to this user", path.display()));
        }
    }
    #[cfg(not(unix))]
    let _ = private;
    Ok(())
}

/// The vendored root first; a refreshed root only when the vendored one is
/// too old (`trust_root_stale`). Same pinned signer policy either way.
pub struct LayeredVerifier {
    vendored: SigstoreVerifier,
    refreshed: Mutex<Option<SigstoreVerifier>>,
}

impl LayeredVerifier {
    pub fn new(vendored: SigstoreVerifier, refreshed: Option<SigstoreVerifier>) -> LayeredVerifier {
        LayeredVerifier {
            vendored,
            refreshed: Mutex::new(refreshed),
        }
    }

    pub fn set_refreshed(&self, v: SigstoreVerifier) {
        *self.refreshed.lock().unwrap_or_else(|p| p.into_inner()) = Some(v);
    }
}

impl ReleaseVerifier for LayeredVerifier {
    fn verify(&self, artifact: Signed<'_>, bundle_json: &[u8]) -> Result<SignedFacts, Error> {
        match self.vendored.verify(artifact, bundle_json) {
            Err(e) if e.code == ErrorCode::TrustRootStale => {
                let refreshed = self.refreshed.lock().unwrap_or_else(|p| p.into_inner());
                match refreshed.as_ref() {
                    Some(r) => r.verify(artifact, bundle_json),
                    None => Err(e),
                }
            }
            other => other,
        }
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
        let now = jiff::Timestamp::now();
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
