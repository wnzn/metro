//! The desktop app's update check and in-app update.
//!
//! - **Check**: anonymous, from the launcher only (the UI never contacts
//!   GitHub): one `GET …/releases/latest/download/SHA256SUMS`; only when it
//!   names a newer version are its Sigstore bundle and release notes fetched.
//!   No cookies, no auth, `User-Agent: Metro`, no version or id. Automatic
//!   check on by default (about 2 minutes after start, then daily; skipped
//!   within 20 h of the last one), switchable; a manual check always works.
//! - **Builds**: only `METRO_RELEASE_CHANNEL=release` builds check at all.
//! - **What's offered**: a stable version newer than this build and not
//!   older than the highest version verified before ([`metro_release`]).
//! - **Download** happens only after the person clicks Update: the release is
//!   fetched and verified again for exactly that version, then the file's
//!   signed row (and for Linux the archive's own bundle) and `RELEASE.json`.
//! - **Stage**: migrated script installs (Linux and Windows) extract into
//!   `<root>/versions/<V>` under the installer locks; nothing visible changes.
//! - **Apply** is the launcher's job (`crates/desktop/src/update.rs`): trial
//!   start under a watchdog, then the pointer switch, or a rollback. The
//!   primitives it uses live in [`install`].
//!
//! Portable copies, unmigrated script installs and MSI installs get a
//! notice and the release page only.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use metro_release::fetch::{AsyncFetcher, CheckOutcome, Source};
use metro_release::{Channel, ReleaseVerifier, VerifiedRelease, Version};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub mod apply;
pub mod install;
pub mod msi;

pub use install::{InstallKind, Journal};

const FIRST_CHECK_DELAY: Duration = Duration::from_secs(120);
const CHECK_INTERVAL_MS: i64 = 24 * 3600 * 1000;
const MIN_AUTO_GAP_MS: i64 = 20 * 3600 * 1000;

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub version: u32,
    pub auto_check: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            version: 1,
            auto_check: true,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LatestInfo {
    pub version: String,
    pub signed_at: i64,
    pub commit: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(default)]
    pub notes_truncated: bool,
    pub release_url: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LastUpdate {
    pub from: String,
    pub to: String,
    /// `updated`, `rolled_back`, `install_failed`, `not_confirmed`.
    pub outcome: String,
    pub at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_code: Option<InstallFailure>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InstallFailure {
    InstallerBusy,
    StageVerificationFailed,
    BackupFailed,
    // [msi lane] Windows Installer outcomes (never block a retry).
    InstallerCancelled,
    InstallerFailed,
}

impl LastUpdate {
    fn blocks(&self, own: Version, target: Version) -> bool {
        self.outcome == "install_failed"
            // Old launchers did not distinguish verification from other failures.
            && matches!(self.failure_code, None | Some(InstallFailure::StageVerificationFailed))
            && Version::parse(&self.from) == Some(own)
            && Version::parse(&self.to) == Some(target)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Status {
    pub version: u32,
    pub last_checked_at: Option<i64>,
    /// `never`, `up_to_date`, `available`, `error`.
    pub last_result: Option<String>,
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anomaly: Option<String>,
    pub highest_seen: Option<String>,
    pub latest: Option<LatestInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<metro_release::release::Evidence>,
    /// No automatic or manual check before this (rate limit), ms.
    pub not_before: Option<i64>,
    pub last_update: Option<LastUpdate>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub code: String,
    pub message: String,
}

/// What a restart would interrupt (the UI asks before applying).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Busy {
    pub serving_connections: usize,
    pub local_workspace: bool,
}

impl Busy {
    pub fn any(&self) -> bool {
        self.serving_connections > 0 || self.local_workspace
    }
}

/// A request to restart into a staged version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApplyRequest {
    pub version: Version,
    pub journal_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    Idle,
    Checking,
    Downloading,
    Verifying,
    Staging,
    Ready,
    Applying,
    Failed,
}

impl Phase {
    fn as_str(&self) -> &'static str {
        match self {
            Phase::Idle => "idle",
            Phase::Checking => "checking",
            Phase::Downloading => "downloading",
            Phase::Verifying => "verifying",
            Phase::Staging => "staging",
            Phase::Ready => "ready",
            Phase::Applying => "applying",
            Phase::Failed => "failed",
        }
    }
}

struct Inner {
    settings: Settings,
    status: Status,
    phase: Phase,
    progress: Option<(u64, u64)>,
    failure: Option<Failure>,
    /// The staged version and its journal id.
    staged: Option<(Version, String)>,
    cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
    settings_error: bool,
    status_error: bool,
    floor_unreadable: bool,
}

/// An error answer for the bridge: HTTP status, stable code, message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub status: u16,
    pub code: &'static str,
    pub message: String,
}

fn refuse(status: u16, code: &'static str, message: impl Into<String>) -> Refusal {
    Refusal {
        status,
        code,
        message: message.into(),
    }
}

/// Plain-words messages for failure codes (no paths, no tokens).
pub fn message_for(code: &str) -> &'static str {
    match code {
        "offline" => "Metro couldn't reach GitHub. Check your connection and try again.",
        "rate_limited" => "GitHub asked Metro to wait before checking again.",
        "not_verified" => {
            "The download didn't pass Metro's signature and checksum checks, so it wasn't used."
        }
        "trust_root_stale" => {
            "This copy of Metro is too old to verify new releases. Download the latest version from the release page."
        }
        "inconsistent" => {
            "The release files didn't match each other, so nothing was used. Try again later."
        }
        "redirect_refused" => {
            "The download was redirected to a site Metro doesn't trust, so it stopped."
        }
        "too_large" => "A release file was larger than expected, so it wasn't used.",
        "http" => "GitHub answered with an error. Try again later.",
        "manifest" | "not_offered" => {
            "The latest release isn't a complete published release, so it isn't offered."
        }
        "archive" => "The downloaded package didn't match its own file list, so it wasn't used.",
        "installer_busy" => {
            "Another Metro install or update is running. Try again when it has finished."
        }
        install::JOURNAL_UNCERTAIN => {
            "Metro couldn't verify the update journal. Keep the installation and workspace data unchanged and contact the Metro maintainers."
        }
        "missing_libraries" => "This version needs system libraries that aren't installed.",
        "not_latest" => "That version isn't the latest verified release anymore. Check again.",
        _ => "The update couldn't be completed. Nothing was changed.",
    }
}

fn release_error(e: &metro_release::Error) -> Failure {
    let code = e.code().to_string();
    Failure {
        message: message_for(&code).to_string(),
        code,
    }
}

/// Configuration for an [`Updater`] (tests inject the source and verifier).
pub struct UpdaterConfig {
    /// `<desktop_root>/update`.
    pub dir: PathBuf,
    /// The launcher's own folder.
    pub install_dir: PathBuf,
    pub own: Version,
    pub channel: Channel,
    pub kind: InstallKind,
    pub source: Source,
    pub verifier: Arc<dyn ReleaseVerifier>,
    /// `--update-trial <id>`: this start is a trial of an update.
    pub trial: Option<String>,
    /// The library checker for Linux stages (`ldd`).
    pub ldd: PathBuf,
    /// Sigstore trust-root refresh (weekly at most, only after
    /// `trust_root_stale`); `None` in tests.
    pub trust: Option<Arc<metro_release::trust_refresh::TrustRefresh>>,
}

impl UpdaterConfig {
    /// Production settings for this build.
    pub fn for_app(
        desktop_root: &Path,
        install_dir: &Path,
        trial: Option<String>,
    ) -> Option<UpdaterConfig> {
        let own = own_version()?;
        let (channel, test) = test_release();
        let kind = install::detect(install_dir, &own, channel, trial.as_deref());
        let dir = desktop_root.join("update");
        crate::desktop::store::prepare_private_dir(&dir).ok()?;
        if let Some((source, verifier)) = test {
            return Some(UpdaterConfig {
                dir,
                install_dir: install_dir.to_path_buf(),
                own,
                channel,
                kind,
                source,
                verifier,
                trial,
                ldd: PathBuf::from("ldd"),
                trust: None,
            });
        }
        let trust = Arc::new(
            metro_release::trust_refresh::TrustRefresh::new(&dir.join("trust-cache")).ok()?,
        );
        let verifier: Arc<dyn ReleaseVerifier> = trust.verifier.clone();
        Some(UpdaterConfig {
            dir,
            install_dir: install_dir.to_path_buf(),
            own,
            channel,
            kind,
            source: Source::github(),
            verifier,
            trial,
            ldd: PathBuf::from("ldd"),
            trust: Some(trust),
        })
    }
}

pub type TestRelease = (Source, Arc<dyn ReleaseVerifier>);

/// This build's channel, and in an e2e test build (feature
/// `e2e-test-release`, never in a release build) with
/// `METRO_TEST_RELEASE_BASE` set, the loopback fixture source and test
/// verifier (it then counts as a release build, like the server's).
pub fn test_release() -> (Channel, Option<TestRelease>) {
    #[cfg(feature = "e2e-test-release")]
    if let Some((source, verifier)) = metro_release::e2e::release_source() {
        return (channel(), Some((source, Arc::from(verifier))));
    }
    (channel(), None)
}

pub struct Updater {
    cfg: UpdaterConfig,
    inner: Mutex<Inner>,
    checking: tokio::sync::Mutex<()>,
    apply_tx: tokio::sync::watch::Sender<Option<ApplyRequest>>,
}

fn read_json<T: for<'de> Deserialize<'de>>(
    path: &Path,
    limit: usize,
) -> std::io::Result<Option<T>> {
    use std::io::{ErrorKind, Read};
    let f = match install::open_state(path) {
        Ok(f) => f,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let mut raw = Vec::new();
    f.take((limit + 1) as u64).read_to_end(&mut raw)?;
    if raw.len() > limit {
        return Err(ErrorKind::InvalidData.into());
    }
    serde_json::from_slice(&raw)
        .map(Some)
        .map_err(std::io::Error::other)
}

const STATUS_LIMIT: usize =
    4 * (metro_release::MAX_MANIFEST + metro_release::MAX_BUNDLE) + 512 * 1024;
const FLOOR: &str = "release-floor.json";

fn save_floor(dir: &Path, version: Version) -> std::io::Result<()> {
    let raw = serde_json::to_vec(&version.to_string()).map_err(std::io::Error::other)?;
    install::write_atomic(&dir.join(FLOOR), &raw)
}

fn offered(status: &Status, own: Version, target: Version) -> bool {
    target > own
        && !blocked_stage(status, own, target)
        && status.highest_seen.as_deref().and_then(Version::parse) == Some(target)
        && status
            .latest
            .as_ref()
            .is_some_and(|l| Version::parse(&l.version) == Some(target))
        && status.evidence.is_some()
}

fn blocked_stage(status: &Status, own: Version, target: Version) -> bool {
    status
        .last_update
        .as_ref()
        .is_some_and(|last| last.blocks(own, target))
}

/// Linux and Windows script installs with migrated locks, and registered
/// per-user MSI installs (`update::msi`).
fn supports_managed_update(kind: &InstallKind) -> bool {
    match kind {
        InstallKind::LinuxInstaller { .. } | InstallKind::WindowsScript { .. } => {
            install::managed(kind)
        }
        // [msi lane] a registered per-user MSI install
        InstallKind::WindowsMsi { .. } => true,
        InstallKind::Portable | InstallKind::Unsupported(_) => false,
    }
}

/// This build's version. In an e2e test build with the test release source
/// active, the name of its `versions/<v>` folder (script installs), so one
/// test binary can play several releases. Elsewhere (MSI) the built version.
pub fn own_version() -> Option<Version> {
    #[cfg(feature = "e2e-test-release")]
    if metro_release::e2e::active()
        && let Some(v) = std::env::current_exe()
            .and_then(|e| e.canonicalize())
            .ok()
            .and_then(|e| {
                let dir = e.parent()?;
                (dir.parent()?.file_name()? == "versions")
                    .then(|| dir.file_name()?.to_str().and_then(Version::parse))?
            })
    {
        return Some(v);
    }
    Version::parse(env!("CARGO_PKG_VERSION"))
}

/// This build's channel (an e2e test build with the test source: release).
pub fn channel() -> Channel {
    #[cfg(feature = "e2e-test-release")]
    if metro_release::e2e::active() {
        return Channel::Release;
    }
    Channel::current()
}

fn latest_info(
    release: &VerifiedRelease,
    notes: Option<String>,
    notes_truncated: bool,
) -> LatestInfo {
    LatestInfo {
        version: release.version.to_string(),
        commit: release.commit.clone(),
        signed_at: release.signed_at,
        notes,
        notes_truncated,
        release_url: metro_release::release_url(&release.version),
    }
}

enum Checked {
    NotNewer(Version),
    Verified(LatestInfo, metro_release::release::Evidence),
}

struct Download(PathBuf);

impl Drop for Download {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl Updater {
    pub fn new(cfg: UpdaterConfig) -> Arc<Updater> {
        let private = crate::desktop::store::prepare_private_dir(&cfg.dir).is_ok();
        let settings = private
            .then(|| read_json::<Settings>(&cfg.dir.join("settings.json"), 4096))
            .and_then(Result::ok)
            .and_then(|s| match s {
                Some(s) if s.version != 1 => None,
                s => Some(s.unwrap_or_default()),
            });
        let settings_error = settings.is_none();
        let settings = settings.unwrap_or(Settings {
            version: 1,
            auto_check: false,
        });
        let saved = private
            .then(|| read_json::<Status>(&cfg.dir.join("status.json"), STATUS_LIMIT))
            .and_then(Result::ok)
            .and_then(|s| match s {
                Some(s) if s.version != 1 => None,
                s => Some(s.unwrap_or_default()),
            });
        let status_error = saved.is_none();
        let mut status = saved.unwrap_or_default();
        status.version = 1;
        // A saved floor only refuses older releases. It cannot authorize
        // any version; evidence must still verify with current trust.
        let stored_floor = read_json::<String>(&cfg.dir.join(FLOOR), 64).and_then(|s| {
            s.map(|v| Version::parse(&v).ok_or(std::io::ErrorKind::InvalidData))
                .transpose()
                .map_err(Into::into)
        });
        let mut floor_unreadable = !private || stored_floor.is_err();
        let saved_floor = stored_floor.ok().flatten();
        // If the pre-migration record is damaged and there is no separate
        // floor yet, we cannot distinguish first use from a lost floor.
        floor_unreadable |= saved_floor.is_none() && status_error;
        let cached = status
            .evidence
            .as_ref()
            .map(|e| e.verify(cfg.verifier.as_ref()));
        // Migrate older status records even when their verification lease
        // has expired. Treat their recorded version only as a lower bound.
        let floor = saved_floor
            .into_iter()
            .chain(status.highest_seen.as_deref().and_then(Version::parse))
            .chain(
                cached
                    .as_ref()
                    .and_then(|c| c.as_ref().ok())
                    .map(|r| r.version),
            )
            .max();
        if let Some(floor) = floor
            && !floor_unreadable
            && saved_floor != Some(floor)
        {
            floor_unreadable = save_floor(&cfg.dir, floor).is_err();
        }
        status.highest_seen = floor.map(|v| v.to_string());
        if let Some(Ok(release)) = cached.as_ref()
            && !floor_unreadable
            && floor == Some(release.version)
        {
            let version = release.version;
            let notes = status
                .latest
                .take()
                .filter(|l| l.version == version.to_string());
            status.highest_seen = Some(version.to_string());
            if version > cfg.own
                && cfg.channel == Channel::Release
                && !matches!(cfg.kind, InstallKind::Unsupported(_))
            {
                let truncated = notes.as_ref().is_some_and(|l| l.notes_truncated);
                status.latest = Some(latest_info(release, notes.and_then(|l| l.notes), truncated));
            } else {
                status.last_result = Some("up_to_date".into());
            }
        } else {
            if let Some(Err(e)) = cached {
                status.error = Some(e.code().into());
            }
            status.latest = None;
            if status.last_result.as_deref() == Some("available") {
                status.last_result = None;
            }
        }
        // A journal that ended since the last start becomes `last_update`.
        let finished = install::take_finished(&cfg.kind, &cfg.dir, &cfg.own);
        let save_finished = finished.is_some();
        if let Some(done) = finished {
            status.last_update = Some(done);
        }
        // Keep stage and evidence while awaiting revalidation; an unknown
        // offer is not proof of supersession.
        let found =
            (private && cfg.channel == Channel::Release && supports_managed_update(&cfg.kind))
                .then(|| install::staged(&cfg.kind, &cfg.dir))
                .flatten();
        let staged = found
            .clone()
            .filter(|(v, _)| floor_unreadable || (*v > cfg.own && floor.is_none_or(|f| *v >= f)));
        if found.is_some() && staged.is_none() {
            install::discard_staged(&cfg.kind, &cfg.dir);
        }
        let phase = if staged
            .as_ref()
            .is_some_and(|(v, _)| offered(&status, cfg.own, *v))
        {
            Phase::Ready
        } else {
            Phase::Idle
        };
        let (apply_tx, _) = tokio::sync::watch::channel(None);
        let updater = Arc::new(Updater {
            cfg,
            inner: Mutex::new(Inner {
                settings,
                status,
                phase,
                progress: None,
                failure: None,
                staged,
                cancel: None,
                settings_error,
                status_error,
                floor_unreadable,
            }),
            checking: tokio::sync::Mutex::new(()),
            apply_tx,
        });
        if save_finished {
            let mut inner = updater.lock();
            updater.save_status(&mut inner);
        }
        updater
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn kind(&self) -> &InstallKind {
        &self.cfg.kind
    }

    pub fn dir(&self) -> &Path {
        &self.cfg.dir
    }

    pub fn own(&self) -> Version {
        self.cfg.own
    }

    /// Whether this build checks at all.
    pub fn can_check(&self) -> bool {
        self.cfg.channel == Channel::Release
            && !matches!(self.cfg.kind, InstallKind::Unsupported(_))
    }

    pub fn can_update_in_app(&self) -> bool {
        self.can_check() && supports_managed_update(&self.cfg.kind)
    }

    fn save_status(&self, inner: &mut Inner) {
        inner.status_error = serde_json::to_vec(&inner.status)
            .map_err(std::io::Error::other)
            .and_then(|data| install::write_atomic(&self.cfg.dir.join("status.json"), &data))
            .is_err();
    }

    fn authorizes(&self, inner: &Inner, version: Version) -> bool {
        !inner.floor_unreadable
            && !inner.status_error
            && !inner
                .failure
                .as_ref()
                .is_some_and(|f| f.code == install::JOURNAL_UNCERTAIN)
            && offered(&inner.status, self.cfg.own, version)
            && inner.status.evidence.as_ref().is_some_and(|e| {
                e.verify(self.cfg.verifier.as_ref())
                    .is_ok_and(|verified| verified.version == version)
            })
    }

    /// Record a pre-trial failure. In particular, a check of release metadata
    /// cannot make a failed stage safe; leave its files/journal untouched.
    pub fn record_install_failure(&self, target: Version, reason: InstallFailure) -> bool {
        let mut inner = self.lock();
        inner.status.last_update = Some(LastUpdate {
            from: self.cfg.own.to_string(),
            to: target.to_string(),
            outcome: "install_failed".into(),
            at: now_ms(),
            detail: None,
            failure_code: Some(reason),
        });
        if reason == InstallFailure::StageVerificationFailed {
            inner.phase = Phase::Idle;
        }
        self.save_status(&mut inner);
        !inner.status_error
    }

    /// The `UpdateState` the UI reads (`GET /v1/update`).
    pub fn state(&self, busy: Busy) -> Value {
        let inner = self.lock();
        let reason = match (&self.cfg.channel, &self.cfg.kind) {
            (Channel::Source, _) => Some("development_build"),
            (Channel::Preview, _) => Some("preview_build"),
            (_, InstallKind::Unsupported(_)) => Some("unsupported_platform"),
            (_, InstallKind::Portable) => Some("portable"),
            (_, InstallKind::LinuxInstaller { .. } | InstallKind::WindowsScript { .. })
                if !self.can_update_in_app() =>
            {
                Some("manual_migration_required")
            }
            _ => None,
        };
        let mut v = json!({
            "current": {
                "version": self.cfg.own.to_string(),
                "channel": self.cfg.channel.as_str(),
                "install": self.cfg.kind.as_str(),
            },
            "can_check": self.can_check(),
            "can_update_in_app": self.can_update_in_app(),
            "auto_check": inner.settings.auto_check,
            "last_checked_at": inner.status.last_checked_at,
            "result": inner.status.last_result.clone().unwrap_or_else(|| "never".into()),
            "phase": inner.phase.as_str(),
            "busy": busy,
        });
        if let Some(r) = reason {
            v["reason"] = r.into();
        }
        if let Some(e) = &inner.status.error {
            v["error"] = e.clone().into();
        }
        if let Some(anomaly) = &inner.status.anomaly {
            v["anomaly"] = anomaly.clone().into();
        }
        if inner.settings_error || inner.status_error || inner.floor_unreadable {
            v["persistence_error"] = if inner.settings_error {
                "settings_io"
            } else {
                "status_io"
            }
            .into();
        }
        if let Some(l) = &inner.status.latest {
            v["latest"] = serde_json::to_value(l).unwrap_or(Value::Null);
        }
        if inner.phase == Phase::Ready
            && let Some((_, id)) = &inner.staged
        {
            // An opaque comparison value, not the journal identifier or a path.
            v["stage_revision"] = metro_release::hex(&metro_release::sha256(id.as_bytes())).into();
        }
        if let Some((done, total)) = inner.progress {
            v["progress"] = json!({"done": done, "total": total});
        }
        if let Some(f) = &inner.failure {
            v["failure"] = json!({"code": f.code, "message": f.message});
        }
        if let Some(u) = &inner.status.last_update {
            v["last_update"] = serde_json::to_value(u).unwrap_or(Value::Null);
        }
        v
    }

    pub fn set_auto_check(&self, on: bool) -> Result<(), Refusal> {
        let mut inner = self.lock();
        let settings = Settings {
            version: 1,
            auto_check: on,
        };
        let result = serde_json::to_vec(&settings)
            .map_err(std::io::Error::other)
            .and_then(|data| install::write_atomic(&self.cfg.dir.join("settings.json"), &data));
        inner.settings_error = result.is_err();
        result.map_err(|_| refuse(500, "io", message_for("io")))?;
        inner.settings = settings;
        Ok(())
    }

    /// One check. `manual` ignores the 20-hour rule, never the rate-limit wait.
    pub async fn check(self: &Arc<Self>, manual: bool) -> Result<(), Refusal> {
        if !self.can_check() {
            return Err(refuse(
                409,
                "not_for_this_build",
                "Updates are only offered for published releases.",
            ));
        }
        let Ok(_guard) = self.checking.try_lock() else {
            return Err(refuse(409, "busy", "A check is already running."));
        };
        let now = now_ms();
        {
            let mut inner = self.lock();
            if inner.floor_unreadable {
                return Err(refuse(500, "io", message_for("io")));
            }
            if inner.phase == Phase::Applying {
                return Err(refuse(409, "busy", "An update is being applied."));
            }
            if inner.status.not_before.is_some_and(|nb| now < nb) {
                return Err(refuse(429, "rate_limited", message_for("rate_limited")));
            }
            if !manual
                && inner
                    .status
                    .last_checked_at
                    .is_some_and(|t| now - t < MIN_AUTO_GAP_MS)
            {
                return Ok(());
            }
            if matches!(inner.phase, Phase::Idle | Phase::Failed | Phase::Ready)
                && inner.staged.is_none()
            {
                inner.phase = Phase::Checking;
            }
        }
        let mut result = self.check_inner().await;
        if let Err(e) = &result
            && self.refresh_stale_trust(e).await
        {
            result = self.check_inner().await;
        }
        let mut inner = self.lock();
        if inner.phase == Phase::Checking {
            inner.phase = Phase::Idle;
        }
        inner.status.last_checked_at = Some(now_ms());
        inner.status.anomaly = None;
        let floor = inner
            .status
            .highest_seen
            .as_deref()
            .and_then(Version::parse);
        match result {
            Ok(checked) => {
                inner.status.error = None;
                match checked {
                    Checked::NotNewer(seen) if floor.is_some_and(|f| seen < f) => {
                        inner.status.anomaly = Some("older_release_served".into());
                    }
                    Checked::NotNewer(_) => {
                        inner.status.latest = None;
                    }
                    Checked::Verified(latest, evidence) => {
                        let seen =
                            Version::parse(&latest.version).expect("canonical verified version");
                        if floor.is_some_and(|f| seen < f) {
                            inner.status.anomaly = Some("older_release_served".into());
                        } else if floor == Some(seen)
                            && inner
                                .status
                                .evidence
                                .as_ref()
                                .is_some_and(|before| before.manifest != evidence.manifest)
                        {
                            inner.status.anomaly = Some("release_changed".into());
                        } else {
                            if floor != Some(seen) && save_floor(&self.cfg.dir, seen).is_err() {
                                inner.floor_unreadable = true;
                                inner.status.latest = None;
                                return Err(refuse(500, "io", message_for("io")));
                            }
                            inner.status.highest_seen = Some(latest.version.clone());
                            inner.status.latest = Some(latest);
                            inner.status.evidence = Some(evidence);
                        }
                    }
                }
                inner.status.last_result = Some(
                    if inner.status.latest.is_some() {
                        "available"
                    } else {
                        "up_to_date"
                    }
                    .into(),
                );
            }
            Err(e) => {
                if e.code == metro_release::ErrorCode::TrustRootStale {
                    inner.status.latest = None;
                }
                inner.status.last_result = Some("error".into());
                inner.status.error = Some(e.code().to_string());
                if let Some(wait) = e.retry_after_secs {
                    inner.status.not_before = Some(now_ms() + (wait as i64) * 1000);
                }
            }
        }
        let floor = inner
            .status
            .highest_seen
            .as_deref()
            .and_then(Version::parse);
        let superseded = inner
            .staged
            .as_ref()
            .is_some_and(|(v, _)| *v <= self.cfg.own || floor.is_some_and(|f| *v < f));
        if superseded {
            inner.staged = None;
            if inner.phase == Phase::Ready {
                inner.phase = Phase::Idle;
            }
        }
        if matches!(inner.phase, Phase::Idle | Phase::Ready | Phase::Failed)
            && let Some((v, _)) = inner.staged.as_ref()
        {
            inner.phase = if offered(&inner.status, self.cfg.own, *v) {
                Phase::Ready
            } else {
                Phase::Idle
            };
        }
        self.save_status(&mut inner);
        if superseded {
            install::discard_staged(&self.cfg.kind, &self.cfg.dir);
        }
        if inner.status_error {
            return Err(refuse(500, "io", message_for("io")));
        }
        Ok(())
    }

    async fn check_inner(&self) -> Result<Checked, metro_release::Error> {
        let fetcher = AsyncFetcher::new(self.cfg.source.clone(), Duration::from_secs(60))?;
        match fetcher
            .check(self.cfg.verifier.as_ref(), &self.cfg.own)
            .await?
        {
            CheckOutcome::NotNewer { latest } => Ok(Checked::NotNewer(latest)),
            CheckOutcome::Newer(release) => {
                let highest = self
                    .lock()
                    .status
                    .highest_seen
                    .as_deref()
                    .and_then(Version::parse);
                let v = release.version;
                let notes = if highest.is_none_or(|f| v >= f) {
                    fetcher.notes(&v).await
                } else {
                    None
                };
                let info = latest_info(
                    &release,
                    notes.as_ref().map(|n| n.body.clone()),
                    notes.as_ref().is_some_and(|n| n.truncated),
                );
                Ok(Checked::Verified(info, release.evidence))
            }
        }
    }

    /// The automatic schedule (primary instance only).
    pub fn spawn_schedule(self: &Arc<Self>) {
        if !self.can_check() {
            return;
        }
        let me = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(FIRST_CHECK_DELAY).await;
            loop {
                if me.lock().settings.auto_check {
                    let _ = me.check(false).await;
                }
                let jitter = (now_ms().rem_euclid(3_600_000)) as u64;
                tokio::time::sleep(Duration::from_millis(
                    CHECK_INTERVAL_MS as u64 - 1_800_000 + jitter,
                ))
                .await;
            }
        });
    }

    /// `POST /v1/update/download`: download, verify and stage `version`.
    pub fn start_download(self: &Arc<Self>, version: &str) -> Result<(), Refusal> {
        if !self.can_update_in_app() {
            return Err(refuse(
                409,
                "not_in_app",
                "This copy of Metro can't update itself; use the release page.",
            ));
        }
        let mut inner = self.lock();
        let Some(target) = Version::parse(version) else {
            return Err(refuse(400, "bad_request", "version must be X.Y.Z"));
        };
        if inner
            .failure
            .as_ref()
            .is_some_and(|f| f.code == install::JOURNAL_UNCERTAIN)
        {
            return Err(refuse(
                409,
                install::JOURNAL_UNCERTAIN,
                message_for(install::JOURNAL_UNCERTAIN),
            ));
        }
        if blocked_stage(&inner.status, self.cfg.own, target) {
            return Err(refuse(
                409,
                "stage_verification_failed",
                "The staged update could not be verified. Use the official installer.",
            ));
        }
        if !self.authorizes(&inner, target) {
            return Err(refuse(409, "not_latest", message_for("not_latest")));
        }
        match inner.phase {
            Phase::Downloading
            | Phase::Verifying
            | Phase::Staging
            | Phase::Applying
            | Phase::Checking => {
                return Err(refuse(409, "busy", "An update step is already running."));
            }
            Phase::Ready if inner.staged.as_ref().is_some_and(|(v, _)| *v == target) => {
                return Ok(());
            }
            _ => {}
        }
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        inner.cancel = Some(cancel.clone());
        inner.phase = Phase::Downloading;
        inner.failure = None;
        inner.progress = Some((0, 0));
        drop(inner);
        let me = self.clone();
        tokio::spawn(async move {
            let result = me.download_and_stage(target, cancel).await;
            let mut inner = me.lock();
            inner.cancel = None;
            inner.progress = None;
            match result {
                Ok(id) if offered(&inner.status, me.cfg.own, target) && !inner.status_error => {
                    inner.phase = Phase::Ready;
                    inner.staged = Some((target, id));
                }
                Ok(_) => {
                    inner.phase = Phase::Idle;
                    inner.staged = None;
                    install::discard_staged(&me.cfg.kind, &me.cfg.dir);
                }
                Err(f) => {
                    eprintln!("metro-desktop: update to {target} failed: {}", f.code);
                    inner.phase = if f.code == "cancelled" {
                        Phase::Idle
                    } else {
                        Phase::Failed
                    };
                    inner.failure = (f.code != "cancelled").then_some(f);
                }
            }
        });
        Ok(())
    }

    async fn download_and_stage(
        self: &Arc<Self>,
        target: Version,
        cancel: Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<String, Failure> {
        let cancelled = || {
            cancel
                .load(std::sync::atomic::Ordering::SeqCst)
                .then(|| Failure {
                    code: "cancelled".into(),
                    message: "Cancelled.".into(),
                })
        };
        // Slow links are fine; a stalled one fails after a minute without data.
        let fetcher = AsyncFetcher::for_downloads(
            self.cfg.source.clone(),
            Duration::from_secs(60),
            Duration::from_secs(6 * 3600),
        )
        .map_err(|e| release_error(&e))?;
        let fetch_release = || fetcher.fetch_release(self.cfg.verifier.as_ref(), &target);
        let mut release = fetch_release().await;
        if let Err(e) = &release
            && self.refresh_stale_trust(e).await
        {
            release = fetch_release().await;
        }
        let release = release.map_err(|e| release_error(&e))?;
        let current_offer = |inner: &Inner| {
            offered(&inner.status, self.cfg.own, target)
                && !inner.status_error
                && inner
                    .status
                    .evidence
                    .as_ref()
                    .is_some_and(|e| e.manifest == release.evidence.manifest)
        };
        let not_latest = || Failure {
            code: "not_latest".into(),
            message: message_for("not_latest").into(),
        };
        let io = || Failure {
            code: "io".into(),
            message: message_for("io").into(),
        };
        if !current_offer(&self.lock()) {
            return Err(not_latest());
        }
        let name = match &self.cfg.kind {
            InstallKind::LinuxInstaller { .. } => release.desktop_linux(),
            InstallKind::WindowsScript { .. } => release.desktop_windows_zip(),
            InstallKind::WindowsMsi { .. } => release.msi.clone(),
            _ => {
                return Err(Failure {
                    code: "not_in_app".into(),
                    message: message_for("").into(),
                });
            }
        };
        let dl = self
            .cfg
            .dir
            .join(format!(".download-{}", install::new_id()));
        install::private_mkdir(&dl).map_err(|_| io())?;
        let dl = Download(dl);
        let url = fetcher
            .source
            .asset_url(&target, &name)
            .map_err(|e| release_error(&e))?;
        let me = self.clone();
        let progress = move |done: u64, total: u64| {
            me.lock().progress = Some((done, total));
        };
        let dest = dl.0.join(&name);
        let (digest, part) = fetcher
            .download(&url, &dest, metro_release::MAX_ARCHIVE, &progress)
            .await
            .map_err(|e| release_error(&e))?;
        if let Some(f) = cancelled() {
            return Err(f);
        }
        self.lock().phase = Phase::Verifying;
        release
            .check_digest(&name, &digest)
            .map_err(|e| release_error(&e))?;
        if matches!(self.cfg.kind, InstallKind::LinuxInstaller { .. }) {
            let bundle = fetcher
                .get(
                    &fetcher
                        .source
                        .asset_url(&target, &format!("{name}.sigstore.json"))
                        .map_err(|e| release_error(&e))?,
                    metro_release::MAX_BUNDLE,
                )
                .await
                .map_err(|e| release_error(&e))?;
            let verify = || {
                release.check_archive_signature(self.cfg.verifier.as_ref(), &name, &digest, &bundle)
            };
            let mut result = verify();
            if let Err(e) = &result
                && self.refresh_stale_trust(e).await
            {
                result = verify();
            }
            result.map_err(|e| release_error(&e))?;
        }
        // Serialize publication with checks, so a newly checked version can
        // never be superseded by an older download finishing late.
        let _check = self.checking.lock().await;
        if let Some(f) = cancelled() {
            return Err(f);
        }
        {
            let mut inner = self.lock();
            if !current_offer(&inner) {
                return Err(not_latest());
            }
            inner.phase = Phase::Staging;
        }
        let kind = self.cfg.kind.clone();
        let (dir, install_dir, own, ldd) = (
            self.cfg.dir.clone(),
            self.cfg.install_dir.clone(),
            self.cfg.own,
            self.cfg.ldd.clone(),
        );
        let staged = tokio::task::spawn_blocking(move || {
            // Keep the private download alive even if the caller is dropped.
            let _download = dl;
            install::stage(&kind, &dir, &install_dir, &own, &release, &part, &ldd)
        })
        .await
        .map_err(|_| io())?;
        staged.map_err(|e| Failure {
            message: e.message.clone(),
            code: e.code.to_string(),
        })
    }

    /// `POST /v1/update/cancel`: stops a download or stage (not an apply).
    pub fn cancel(&self) -> Result<(), Refusal> {
        let inner = self.lock();
        match (&inner.phase, &inner.cancel) {
            (Phase::Downloading | Phase::Verifying, Some(c)) => {
                c.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            }
            (Phase::Staging, _) => Err(refuse(
                409,
                "busy",
                "Preparing the update can't be cancelled; it finishes shortly.",
            )),
            _ => Err(refuse(409, "nothing_to_cancel", "No download is running.")),
        }
    }

    /// `POST /v1/update/apply`: ask the launcher to restart into the update.
    pub fn request_apply(
        &self,
        version: &str,
        confirm_interrupt: bool,
        busy: Busy,
    ) -> Result<(), Refusal> {
        if !self.can_update_in_app() {
            return Err(refuse(
                409,
                "not_in_app",
                "Use the supported manual installer for this copy of Metro.",
            ));
        }
        let Ok(_check) = self.checking.try_lock() else {
            return Err(refuse(409, "busy", "A check is already running."));
        };
        let mut inner = self.lock();
        let Some((staged, id)) = inner.staged.clone() else {
            return Err(refuse(
                409,
                "not_ready",
                "The update isn't downloaded and verified yet.",
            ));
        };
        if inner.phase != Phase::Ready
            || staged.to_string() != version
            || !self.authorizes(&inner, staged)
        {
            return Err(refuse(
                409,
                "not_ready",
                "The update isn't downloaded and verified yet.",
            ));
        }
        if busy.any() && !confirm_interrupt {
            return Err(refuse(
                409,
                "would_interrupt",
                "Restarting now stops on-device work in progress.",
            ));
        }
        inner.phase = Phase::Applying;
        drop(inner);
        self.apply_tx.send_replace(Some(ApplyRequest {
            version: staged,
            journal_id: id,
        }));
        Ok(())
    }

    /// The launcher waits on this.
    pub fn apply_requests(&self) -> tokio::sync::watch::Receiver<Option<ApplyRequest>> {
        self.apply_tx.subscribe()
    }

    /// Revalidate the exact staged package after the launcher has stopped
    /// local work and taken the installer lock.
    pub fn verify_apply(
        &self,
        req: &ApplyRequest,
        lock: &install::Lock,
    ) -> std::io::Result<(Journal, install::CheckedFile)> {
        use std::io::{Error, ErrorKind};
        let invalid = || {
            Error::new(
                ErrorKind::InvalidData,
                "the update is no longer staged and offered",
            )
        };
        lock.check(install::lock_root(&self.cfg.kind, &self.cfg.dir))?;
        let path = install::journal_path(&self.cfg.kind, &self.cfg.dir).ok_or_else(invalid)?;
        let j = install::load_journal(&path)
            .map_err(|_| install::journal_uncertain())?
            .filter(|j| {
                j.phase == "staged"
                    && j.id == req.journal_id
                    && j.from == self.cfg.own.to_string()
                    && j.to == req.version.to_string()
            })
            .ok_or_else(install::journal_uncertain)?;
        install::check_staged_current(&self.cfg.kind, &j)?;
        if !self.can_update_in_app() {
            return Err(invalid());
        }
        let manifest = {
            let inner = self.lock();
            if inner.phase != Phase::Applying
                || inner.status_error
                || !offered(&inner.status, self.cfg.own, req.version)
                || inner.staged.as_ref() != Some(&(req.version, req.journal_id.clone()))
            {
                return Err(invalid());
            }
            inner
                .status
                .evidence
                .as_ref()
                .ok_or_else(invalid)?
                .manifest
                .clone()
        };
        let file = install::verify_staged(
            &self.cfg.kind,
            &self.cfg.dir,
            &j,
            self.cfg.verifier.as_ref(),
            Some(&manifest),
            lock,
        )?;
        Ok((j, file))
    }

    pub async fn verify_apply_with_refresh(
        &self,
        req: &ApplyRequest,
        lock: &install::Lock,
    ) -> std::io::Result<(Journal, install::CheckedFile)> {
        let result = self.verify_apply(req, lock);
        if let Err(e) = &result
            && let Some(e) = e
                .get_ref()
                .and_then(|e| e.downcast_ref::<metro_release::Error>())
            && self.refresh_stale_trust(e).await
        {
            return self.verify_apply(req, lock);
        }
        result
    }

    async fn refresh_stale_trust(&self, e: &metro_release::Error) -> bool {
        if e.code != metro_release::ErrorCode::TrustRootStale {
            return false;
        }
        match &self.cfg.trust {
            Some(trust) => trust.refresh_if_due().await,
            None => false,
        }
    }

    /// `POST /v1/update/started`: the UI rendered (trial health signal).
    pub fn started(&self) -> bool {
        // [msi lane] dispatch hook
        if matches!(self.cfg.kind, InstallKind::WindowsMsi { .. }) {
            return self.msi_started();
        }
        let Some(id) = &self.cfg.trial else {
            return false;
        };
        let Some(path) = install::journal_path(&self.cfg.kind, &self.cfg.dir) else {
            return false;
        };
        install::read_journal(&path).is_some_and(|j| {
            j.phase == "trial"
                && j.id == *id
                && j.to == self.cfg.own.to_string()
                && j.watchdog_pid.is_some_and(install::pid_alive)
                && self
                    .cfg
                    .kind
                    .root()
                    .is_none_or(|root| install::script_state(root, &j.from))
        }) && install::mark_trial_ok(&self.cfg.dir, id).is_ok()
    }
}
