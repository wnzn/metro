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
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.dir, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }

    /// The trusted root a previous refresh verified, if any.
    pub fn cached_verifier(&self) -> Option<SigstoreVerifier> {
        let raw = std::fs::read(self.dir.join(TARGET)).ok()?;
        if raw.len() as u64 > MAX_TRUSTED_ROOT {
            return None;
        }
        SigstoreVerifier::from_trusted_root_json(std::str::from_utf8(&raw).ok()?).ok()
    }

    fn last_attempt(&self) -> Option<i64> {
        std::fs::read_to_string(self.dir.join("last_refresh"))
            .ok()?
            .trim()
            .parse()
            .ok()
    }

    /// Whether a network refresh is allowed now (none in the last week).
    pub fn refresh_due(&self, now: jiff::Timestamp) -> bool {
        self.last_attempt()
            .is_none_or(|t| now.as_second() - t >= REFRESH_INTERVAL_SECS || now.as_second() < t)
    }

    /// Run the TUF refresh and return a verifier over the new trusted root.
    /// Records the attempt first, so a failing refresh is also limited to
    /// once a week.
    pub async fn refresh(&self, now: jiff::Timestamp) -> Result<SigstoreVerifier, Error> {
        let io =
            |e: std::io::Error| Error::new(ErrorCode::Io, format!("trust cache: {}", e.kind()));
        self.private_dir().map_err(io)?;
        crate::write_atomic(
            &self.dir.join("last_refresh"),
            now.as_second().to_string().as_bytes(),
        )
        .map_err(io)?;
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
        Ok(verifier)
    }
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
        let verifier = LayeredVerifier::new(SigstoreVerifier::vendored()?, cache.cached_verifier());
        Ok(TrustRefresh {
            cache,
            verifier: std::sync::Arc::new(verifier),
        })
    }

    /// After a `trust_root_stale` failure: refresh if due. `true` when a
    /// new root is in place and the caller should retry once.
    pub async fn refresh_if_due(&self) -> bool {
        let now = jiff::Timestamp::now();
        if !self.cache.refresh_due(now) {
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
