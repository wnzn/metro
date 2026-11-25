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
//! - **Stage**: script installs extract into `<root>/versions/<V>` under the
//!   installers' `.lock`; nothing visible changes. The MSI is only verified.
//! - **Apply** is the launcher's job (`crates/desktop/src/update.rs`): trial
//!   start under a watchdog, then the pointer switch, or a rollback. The
//!   primitives it uses live in [`install`].
//!
//! Portable copies (not installed by a Metro script or the MSI) get a notice
//! and the release page only: Metro never rewrites a folder it didn't
//! install.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use metro_release::fetch::{AsyncFetcher, CheckOutcome, Source};
use metro_release::{Channel, Offer, ReleaseVerifier, Version};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub mod apply;
pub mod install;

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
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Status {
    pub version: u32,
    pub last_checked_at: Option<i64>,
    /// `never`, `up_to_date`, `available`, `error`.
    pub last_result: Option<String>,
    pub error: Option<String>,
    pub highest_seen: Option<String>,
    pub latest: Option<LatestInfo>,
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
        let own = Version::parse(env!("CARGO_PKG_VERSION"))?;
        let channel = Channel::current();
        let kind = install::detect(install_dir, &own, channel, trial.as_deref());
        let trust = Arc::new(
            metro_release::trust_refresh::TrustRefresh::new(
                &desktop_root.join("update").join("trust-cache"),
            )
            .ok()?,
        );
        let verifier: Arc<dyn ReleaseVerifier> = trust.verifier.clone();
        Some(UpdaterConfig {
            dir: desktop_root.join("update"),
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

pub struct Updater {
    cfg: UpdaterConfig,
    inner: Mutex<Inner>,
    checking: tokio::sync::Mutex<()>,
    apply_tx: tokio::sync::watch::Sender<Option<ApplyRequest>>,
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    let raw = std::fs::read(path).ok()?;
    if raw.len() > 512 * 1024 {
        return None;
    }
    serde_json::from_slice(&raw).ok()
}

impl Updater {
    pub fn new(cfg: UpdaterConfig) -> Arc<Updater> {
        let _ = crate::desktop::store::prepare_private_dir(&cfg.dir);
        let settings: Settings = read_json(&cfg.dir.join("settings.json")).unwrap_or_default();
        let mut status: Status = read_json(&cfg.dir.join("status.json")).unwrap_or_default();
        status.version = 1;
        // A journal that ended since the last start becomes `last_update`.
        if let Some(done) = install::take_finished(&cfg.kind, &cfg.dir, &cfg.own) {
            status.last_update = Some(done);
        }
        // A staged version is offered again while it is still the latest.
        // Otherwise (an older or superseded version) it is deleted.
        let found = install::staged(&cfg.kind, &cfg.dir);
        let staged = found.clone().filter(|(v, _)| {
            *v > cfg.own
                && status
                    .latest
                    .as_ref()
                    .is_some_and(|l| l.version == v.to_string())
        });
        if found.is_some() && staged.is_none() {
            install::discard_staged(&cfg.kind, &cfg.dir);
        }
        let phase = if staged.is_some() {
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
            }),
            checking: tokio::sync::Mutex::new(()),
            apply_tx,
        });
        updater.save_status();
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
        self.can_check()
            && matches!(
                self.cfg.kind,
                InstallKind::LinuxInstaller { .. }
                    | InstallKind::WindowsScript { .. }
                    | InstallKind::WindowsMsi { .. }
            )
    }

    fn save_status(&self) {
        let (settings, status) = {
            let inner = self.lock();
            (inner.settings.clone(), inner.status.clone())
        };
        let _ = install::write_atomic(
            &self.cfg.dir.join("settings.json"),
            &serde_json::to_vec(&settings).unwrap_or_default(),
        );
        let _ = install::write_atomic(
            &self.cfg.dir.join("status.json"),
            &serde_json::to_vec(&status).unwrap_or_default(),
        );
    }

    /// Record how the last update ended (the launcher, after a rollback).
    pub fn record_outcome(&self, outcome: LastUpdate) {
        self.lock().status.last_update = Some(outcome);
        self.save_status();
    }

    /// The `UpdateState` the UI reads (`GET /v1/update`).
    pub fn state(&self, busy: Busy) -> Value {
        let inner = self.lock();
        let reason = match (&self.cfg.channel, &self.cfg.kind) {
            (Channel::Source, _) => Some("development_build"),
            (Channel::Preview, _) => Some("preview_build"),
            (_, InstallKind::Unsupported(_)) => Some("unsupported_platform"),
            (_, InstallKind::Portable) => Some("portable"),
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
        if inner.status.last_result.as_deref() == Some("available")
            && let Some(l) = &inner.status.latest
        {
            v["latest"] = serde_json::to_value(l).unwrap_or(Value::Null);
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
        if matches!(self.cfg.kind, InstallKind::WindowsMsi { .. }) {
            v["msi_note"] = "no_automatic_rollback".into();
        }
        v
    }

    pub fn set_auto_check(&self, on: bool) {
        self.lock().settings.auto_check = on;
        self.save_status();
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
            && e.code == metro_release::ErrorCode::TrustRootStale
            && let Some(trust) = &self.cfg.trust
            && trust.refresh_if_due().await
        {
            result = self.check_inner().await;
        }
        let mut inner = self.lock();
        if inner.phase == Phase::Checking {
            inner.phase = Phase::Idle;
        }
        inner.status.last_checked_at = Some(now_ms());
        match result {
            Ok((latest, available, highest)) => {
                inner.status.error = None;
                inner.status.last_result =
                    Some(if available { "available" } else { "up_to_date" }.into());
                if let Some(h) = highest {
                    inner.status.highest_seen = Some(h);
                }
                if let Some(l) = latest {
                    inner.status.latest = Some(l);
                }
            }
            Err(e) => {
                inner.status.last_result = Some("error".into());
                inner.status.error = Some(e.code().to_string());
                if let Some(wait) = e.retry_after_secs {
                    inner.status.not_before = Some(now_ms() + (wait as i64) * 1000);
                }
            }
        }
        drop(inner);
        self.save_status();
        Ok(())
    }

    async fn check_inner(
        &self,
    ) -> Result<(Option<LatestInfo>, bool, Option<String>), metro_release::Error> {
        let fetcher = AsyncFetcher::new(self.cfg.source.clone(), Duration::from_secs(60))?;
        match fetcher
            .check(self.cfg.verifier.as_ref(), &self.cfg.own)
            .await?
        {
            CheckOutcome::NotNewer { .. } => Ok((None, false, None)),
            CheckOutcome::Newer(release) => {
                let highest = self
                    .lock()
                    .status
                    .highest_seen
                    .as_deref()
                    .and_then(Version::parse);
                match metro_release::version::offer(
                    &self.cfg.own,
                    self.cfg.channel,
                    &release,
                    highest.as_ref(),
                ) {
                    Offer::Available(v) => {
                        let notes = fetcher.notes(&v).await;
                        let info = LatestInfo {
                            version: v.to_string(),
                            signed_at: release.signed_at,
                            commit: release.commit.clone(),
                            notes: notes.as_ref().map(|n| n.body.clone()),
                            notes_truncated: notes.as_ref().is_some_and(|n| n.truncated),
                            release_url: metro_release::release_url(&v),
                        };
                        Ok((Some(info), true, Some(v.to_string())))
                    }
                    _ => {
                        eprintln!(
                            "metro-desktop: a release older than one verified before was served as latest; ignored"
                        );
                        Ok((None, false, None))
                    }
                }
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
        let latest = inner.status.latest.as_ref().map(|l| l.version.clone());
        if inner.status.last_result.as_deref() != Some("available")
            || latest.as_deref() != Some(version)
        {
            return Err(refuse(409, "not_latest", message_for("not_latest")));
        }
        let Some(target) = Version::parse(version) else {
            return Err(refuse(400, "bad_request", "version must be X.Y.Z"));
        };
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
                Ok(id) => {
                    inner.phase = Phase::Ready;
                    inner.staged = Some((target, id));
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
        let release = fetcher
            .fetch_release(self.cfg.verifier.as_ref(), &target)
            .await
            .map_err(|e| release_error(&e))?;
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
        let dl = self.cfg.dir.join("dl").join(target.to_string());
        let _ = std::fs::remove_dir_all(self.cfg.dir.join("dl"));
        std::fs::create_dir_all(&dl).map_err(|_| Failure {
            code: "io".into(),
            message: message_for("io").into(),
        })?;
        let url = fetcher
            .source
            .asset_url(&target, &name)
            .map_err(|e| release_error(&e))?;
        let me = self.clone();
        let progress = move |done: u64, total: u64| {
            me.lock().progress = Some((done, total));
        };
        let dest = dl.join(&name);
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
            release
                .check_archive_signature(self.cfg.verifier.as_ref(), &name, &digest, &bundle)
                .map_err(|e| release_error(&e))?;
        }
        std::fs::rename(&part, &dest).map_err(|_| Failure {
            code: "io".into(),
            message: message_for("io").into(),
        })?;
        if let Some(f) = cancelled() {
            return Err(f);
        }
        self.lock().phase = Phase::Staging;
        let kind = self.cfg.kind.clone();
        let (dir, install_dir, own, ldd) = (
            self.cfg.dir.clone(),
            self.cfg.install_dir.clone(),
            self.cfg.own,
            self.cfg.ldd.clone(),
        );
        let staged = tokio::task::spawn_blocking(move || {
            install::stage(
                &kind,
                &dir,
                &install_dir,
                &own,
                &target,
                (&dest, &digest),
                &ldd,
            )
        })
        .await
        .map_err(|_| Failure {
            code: "io".into(),
            message: message_for("io").into(),
        })?;
        if !matches!(self.cfg.kind, InstallKind::WindowsMsi { .. }) {
            let _ = std::fs::remove_dir_all(self.cfg.dir.join("dl"));
        }
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
        let mut inner = self.lock();
        let Some((staged, id)) = inner.staged.clone() else {
            return Err(refuse(
                409,
                "not_ready",
                "The update isn't downloaded and verified yet.",
            ));
        };
        if inner.phase != Phase::Ready || staged.to_string() != version {
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

    /// `POST /v1/update/started`: the UI rendered (trial health signal).
    pub fn started(&self) -> bool {
        match &self.cfg.trial {
            Some(id) => install::mark_trial_ok(&self.cfg.dir, id).is_ok(),
            None => false,
        }
    }
}
