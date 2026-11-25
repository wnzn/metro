//! `metro-desktop`: the Metro desktop app's entry point (shipped as
//! `Metro.exe` / `Metro`).
//!
//! 1. Finds the packaged UI at `<own dir>/ui/Metro[.exe]` (its own canonical
//!    path; never PATH, argv or the environment) and the optional bundled
//!    `<own dir>/metro-server[.exe]`.
//! 2. Prepares the private desktop root and takes the per-user instance lock
//!    (a second instance still opens a UI but serves no connections).
//! 3. Starts the loopback bridge (`metro_connector::desktop::bridge`) with
//!    the linked on-device connection service and local workspace manager.
//! 4. Starts the UI with only `METRO_DESKTOP_BRIDGE_URL` and
//!    `METRO_DESKTOP_BRIDGE_TOKEN` added to its environment (and `127.0.0.1`
//!    added to `NO_PROXY`), `NSH_APP_DIR` removed. The UI dies with the
//!    launcher (Linux parent-death signal, Windows kill-on-close Job Object).
//! 5. When the UI exits (or the launcher is told to stop), stops every
//!    worker and sign-in, drops the session token, stops the local
//!    workspace, and exits with the UI's exit code.
//!
//! Nothing secret is written to argv, logs or disk by the launcher.
//!
//! On macOS the launcher is `Metro.app/Contents/MacOS/Metro`, the server sits
//! next to it, and the UI is the nested, unchanged NSH app; a guardian keeps
//! the UI's lifetime tied to the launcher's (see [`macos`]).

#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(unix)]
mod macos;
mod update;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use metro_connector::desktop::bridge::{self, ENV_TOKEN, ENV_URL};
use metro_connector::desktop::core::{Desktop, DesktopConfig};
use metro_connector::desktop::local::{LocalWorkspaceHost, ToolsHook};
use metro_connector::desktop::monitoring::{EventKind, Events};
use metro_connector::desktop::store;
use metro_connector::desktop::update::{ApplyRequest, Updater, UpdaterConfig};
use metro_connector::desktop_local::LocalServer;
use metro_connector::desktop_tools::DesktopTools;

#[cfg(windows)]
const UI_NAME: &str = "Metro.exe";
#[cfg(not(any(windows, target_os = "macos")))]
const UI_NAME: &str = "Metro";
#[cfg(windows)]
const SERVER_NAME: &str = "metro-server.exe";
#[cfg(not(windows))]
const SERVER_NAME: &str = "metro-server";

/// A message for the person, shown in a dialog on Windows (no console).
fn fatal(msg: &str) {
    eprintln!("Metro: {msg}");
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
        let w = |s: &str| {
            s.encode_utf16()
                .chain(std::iter::once(0))
                .collect::<Vec<u16>>()
        };
        let (text, title) = (w(msg), w("Metro"));
        // SAFETY: both strings are NUL-terminated and outlive the call.
        unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                text.as_ptr(),
                title.as_ptr(),
                MB_OK | MB_ICONERROR,
            );
        }
    }
}

/// `\\?\C:\...` → `C:\...` (verbatim paths confuse some programs); UNC and
/// other forms are kept as they are.
fn plain(p: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        let s = p.to_string_lossy();
        if let Some(rest) = s.strip_prefix(r"\\?\")
            && rest.as_bytes().get(1) == Some(&b':')
        {
            return PathBuf::from(rest.to_string());
        }
    }
    p
}

/// The directory holding this launcher, from its canonical path.
fn install_dir() -> Result<PathBuf, String> {
    let exe =
        std::env::current_exe().map_err(|_| "cannot find the Metro program file".to_string())?;
    let exe =
        std::fs::canonicalize(exe).map_err(|_| "cannot find the Metro program file".to_string())?;
    exe.parent()
        .map(|d| plain(d.to_path_buf()))
        .ok_or_else(|| "cannot find the Metro program folder".to_string())
}

/// A shipped component: a regular file (not a link) at exactly this path.
fn component(path: &Path) -> Option<PathBuf> {
    let m = std::fs::symlink_metadata(path).ok()?;
    (m.is_file() && !m.file_type().is_symlink()).then(|| path.to_path_buf())
}

/// The UI's `NO_PROXY`: the person's own entries plus this machine's exact
/// hosts in ureq's syntax (`metro_url_policy::loopback_no_proxy_entries`).
/// NSH's fetch (ureq 3) honours `HTTP_PROXY` and friends, which must never
/// see the bridge bearer or a local server's token. No prefixes (`127.`,
/// `100.64.`): those would send ordinary names around the person's proxy.
/// The UI gets this value as `METRO_PROXY_DIRECT_FOR` and the route as
/// `METRO_PROXY_ROUTE` (never the proxy URLs, which can hold credentials),
/// and only skips the plain-HTTP question for a loopback or Tailscale server
/// it will reach directly (apps/desktop/src/api/proxyEnv.ts).
fn no_proxy() -> String {
    ui_no_proxy(&metro_connector::http::no_proxy_with_loopback())
}

fn ui_no_proxy(base: &str) -> String {
    let mut parts: Vec<String> = base
        .split(',')
        .map(str::to_string)
        .filter(|p| !p.is_empty())
        .collect();
    if !parts.iter().any(|p| p == "*") {
        for e in metro_url_policy::loopback_no_proxy_entries() {
            if !parts.iter().any(|p| p.eq_ignore_ascii_case(&e)) {
                parts.push(e);
            }
        }
    }
    parts.join(",")
}

#[cfg(windows)]
mod job {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    };

    /// Kills its processes when closed (when the launcher exits or dies).
    pub struct Job(HANDLE);

    // SAFETY: the handle is only used to close it.
    unsafe impl Send for Job {}

    impl Job {
        pub fn assign(process: HANDLE) -> Option<Job> {
            // SAFETY: plain Win32 calls with checked results; the info
            // struct is zeroed and sized correctly.
            unsafe {
                let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if job.is_null() {
                    return None;
                }
                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let ok = SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    (&info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                ) != 0
                    && AssignProcessToJobObject(job, process) != 0;
                if !ok {
                    CloseHandle(job);
                    return None;
                }
                Some(Job(job))
            }
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            // SAFETY: we own the handle.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

/// Resolves when the launcher is asked to stop (Ctrl-C, SIGTERM, SIGHUP).
/// The Windows GUI launcher has no console: it ends with its UI.
async fn stop_requested() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let (Ok(mut term), Ok(mut hup), Ok(mut int)) = (
            signal(SignalKind::terminate()),
            signal(SignalKind::hangup()),
            signal(SignalKind::interrupt()),
        ) else {
            return std::future::pending().await;
        };
        tokio::select! {
            _ = term.recv() => {}
            _ = hup.recv() => {}
            _ = int.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Ask the UI to close, then make sure it is gone.
async fn end_ui(child: &mut tokio::process::Child) -> Option<std::process::ExitStatus> {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        // SAFETY: signalling our own child's process group.
        unsafe {
            libc::kill(-(pid as i32), libc::SIGTERM);
        }
        if let Ok(Ok(s)) = tokio::time::timeout(Duration::from_secs(3), child.wait()).await {
            return Some(s);
        }
        // SAFETY: as above.
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }
    let _ = child.kill().await;
    child.wait().await.ok()
}

fn exit_code(status: Option<std::process::ExitStatus>) -> u8 {
    let Some(s) = status else { return 1 };
    if let Some(c) = s.code() {
        // NTSTATUS values (for example a missing DLL) are negative as i32.
        // The launcher cannot forward the full value, but must still fail.
        return u8::try_from(c).unwrap_or(1);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = s.signal() {
            return (128 + sig).clamp(0, 255) as u8;
        }
    }
    1
}

/// How one run of [`launch`] ended.
enum Launched {
    Exit(u8),
    /// An update was rolled back: run the launcher again (this version).
    Relaunch,
}

/// The next apply request from the updater; never resolves without one.
async fn next_apply(
    rx: &mut Option<tokio::sync::watch::Receiver<Option<ApplyRequest>>>,
) -> ApplyRequest {
    let Some(rx) = rx else {
        return std::future::pending().await;
    };
    loop {
        if let Some(req) = rx.borrow_and_update().clone() {
            return req;
        }
        if rx.changed().await.is_err() {
            return std::future::pending().await;
        }
    }
}

async fn launch(
    events: Option<&Events>,
    trial: Option<String>,
    dir: &Path,
    root: &Path,
) -> Result<Launched, (EventKind, String)> {
    let start_failed = |msg| (EventKind::LauncherStartFailed, msg);
    #[cfg(target_os = "macos")]
    let ui = macos::ui_executable(dir).map_err(start_failed)?;
    #[cfg(not(target_os = "macos"))]
    let ui = component(&dir.join("ui").join(UI_NAME)).ok_or_else(|| {
        start_failed(format!(
            "the app files are incomplete: ui{}{UI_NAME} is missing. Extract the whole Metro archive again and keep the ui folder next to Metro.",
            std::path::MAIN_SEPARATOR
        ))
    })?;
    let server = component(&dir.join(SERVER_NAME));

    store::prepare_private_dir(root)
        .map_err(|e| start_failed(format!("cannot prepare Metro's data folder: {e}")))?;
    update::gate_msi_at_start(root).map_err(start_failed)?;
    let lock = if let Some(id) = &trial {
        Some(update::trial_lock(id, dir, root).ok_or_else(|| {
            start_failed("this trial did not inherit the active workspace lock".into())
        })?)
    } else {
        store::try_lock_instance(root)
            .map_err(|e| start_failed(format!("cannot prepare Metro's data folder: {e}")))?
    }
    .map(Arc::new);
    let primary = lock.is_some();
    update::gate_msi_at_start(root).map_err(start_failed)?;
    if trial.is_none() {
        update::recover_at_start(dir, root, lock.as_deref()).map_err(start_failed)?;
    }

    // Updates: primary instance only. Startup above reconciles safe
    // cleanup or blocks if a prior trial lost its watchdog.
    let updater: Option<Arc<Updater>> = if primary {
        UpdaterConfig::for_app(root, dir, trial.clone()).map(Updater::new)
    } else {
        None
    };
    if let Some(u) = &updater
        && trial.is_none()
    {
        u.spawn_schedule();
    }
    let mut apply_rx = updater.as_ref().map(|u| u.apply_requests());

    let local: Option<Arc<dyn LocalWorkspaceHost>> = match (&server, primary) {
        (Some(bin), true) => {
            let data = root.join("local");
            match store::prepare_private_dir(&data) {
                Ok(()) => {
                    let server = LocalServer::with_events(bin.clone(), data, events.cloned());
                    #[cfg(target_os = "linux")]
                    let server = server.with_instance(lock.clone().expect("primary instance"));
                    Some(Arc::new(server))
                }
                Err(e) => {
                    if let Some(events) = events {
                        events.record(EventKind::LocalServerStartFailed);
                    }
                    eprintln!("Metro: the local workspace is unavailable: {e}");
                    None
                }
            }
        }
        _ => None,
    };
    let mut cfg = DesktopConfig::new(root.to_path_buf(), primary, local);
    cfg.tools = tools_hook(root, primary);
    cfg.updater = updater.clone();
    let desktop = Desktop::new(cfg);
    let bridge = bridge::start(desktop.clone()).await.map_err(|e| {
        start_failed(format!(
            "cannot start Metro's local connection service: {e}"
        ))
    })?;

    #[cfg(not(target_os = "macos"))]
    let mut cmd = tokio::process::Command::new(&ui);
    // The guardian starts `ui` itself and passes this environment on.
    #[cfg(target_os = "macos")]
    let mut cmd = match macos::guardian_command(dir) {
        Ok(cmd) => cmd,
        Err(e) => {
            bridge.stop().await;
            desktop.shutdown().await;
            return Err((
                EventKind::UiSpawnFailed,
                format!("cannot start the Metro window: {e}"),
            ));
        }
    };
    if let Some(ui_dir) = ui.parent() {
        cmd.current_dir(ui_dir);
    }
    cmd.env_remove("NSH_APP_DIR")
        .env(ENV_URL, &bridge.url)
        .env(ENV_TOKEN, bridge.token.expose())
        .env("NO_PROXY", no_proxy())
        .env("no_proxy", no_proxy())
        .env(
            "METRO_PROXY_ROUTE",
            metro_url_policy::proxy_route(|k| std::env::var(k).ok(), cfg!(windows)),
        )
        .env("METRO_PROXY_DIRECT_FOR", no_proxy())
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    // The guardian's stdin is its liveness pipe: when it closes (this
    // process ends, however it ends) the guardian ends the UI. It is never
    // SIGKILLed on drop, so it always gets to do that.
    #[cfg(target_os = "macos")]
    cmd.stdin(std::process::Stdio::piped()).kill_on_drop(false);
    #[cfg(unix)]
    {
        // Own process group (so stopping reaches its helpers), and Linux
        // ends it if the launcher dies.
        cmd.process_group(0);
        #[cfg(target_os = "linux")]
        {
            // SAFETY: getpid has no preconditions.
            let parent = unsafe { libc::getpid() };
            // SAFETY: only async-signal-safe calls in the child.
            unsafe {
                cmd.pre_exec(move || {
                    if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    if libc::getppid() != parent {
                        libc::_exit(1);
                    }
                    Ok(())
                });
            }
        }
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            bridge.stop().await;
            desktop.shutdown().await;
            return Err((
                EventKind::UiSpawnFailed,
                format!("cannot start the Metro window: {e}"),
            ));
        }
    };
    #[cfg(windows)]
    let _job = child.raw_handle().and_then(|h| job::Job::assign(h as _));
    // Held, never written, until this function returns.
    #[cfg(target_os = "macos")]
    let _liveness = child.stdin.take();

    let mut apply_request = None;
    let status = tokio::select! {
        s = child.wait() => {
            let failure = match &s {
                Err(_) => Some(EventKind::UiWaitFailed),
                Ok(s) if !s.success() => Some(match s.code() {
                    #[cfg(unix)]
                    Some(127) => EventKind::UiExit127,
                    #[cfg(windows)]
                    Some(c) if c as u32 == 0xc000_0135 => EventKind::UiMissingDll,
                    _ => EventKind::UiUnexpectedExit,
                }),
                _ => None,
            };
            if let (Some(events), Some(kind)) = (events, failure) {
                events.record(kind);
            }
            s.ok()
        },
        _ = stop_requested() => end_ui(&mut child).await,
        req = next_apply(&mut apply_rx) => {
            apply_request = Some(req);
            end_ui(&mut child).await
        }
    };
    // The UI is gone: nothing may keep serving on its behalf.
    bridge.stop().await;
    let stopped = matches!(
        tokio::time::timeout(Duration::from_secs(15), desktop.shutdown_confirmed()).await,
        Ok(true)
    );
    if !stopped {
        eprintln!("Metro: some on-device work did not stop in time; ending it");
    }
    drop(desktop);
    if let (Some(req), Some(u)) = (apply_request, updater) {
        if !stopped {
            return Err((EventKind::LauncherStartFailed, "the update was not started because the local workspace did not stop. Close Metro and try again.".into()));
        }
        return Ok(
            match update::apply(&u, req, root, lock.expect("primary updater")).await {
                update::After::Exit(code) => Launched::Exit(code),
                update::After::Relaunch => Launched::Relaunch,
            },
        );
    }
    drop(lock);
    Ok(Launched::Exit(exit_code(status)))
}

/// Local workspace tools (read-only folders per hosted Space), primary
/// instance only. With the installer's `enable-local-runner` marker (next to
/// this launcher; on macOS beside the outer Metro.app in its private install
/// folder), also this computer's personal registered runner on the own
/// hosted server.
fn tools_hook(root: &Path, primary: bool) -> Option<Arc<dyn ToolsHook>> {
    use metro_connector::desktop_tools::runner;
    primary.then(|| {
        let marker = install_dir()
            .map(|d| runner::installer_marker(&d))
            .unwrap_or(runner::Marker::Absent);
        Arc::new(DesktopTools::new(root.join("local-tools")).with_local_runner(marker))
            as Arc<dyn ToolsHook>
    })
}

fn main() -> ExitCode {
    // First: a guardian does none of the launcher's setup (no desktop root,
    // monitoring, proxy changes or runtime).
    #[cfg(target_os = "macos")]
    if let Some(code) = macos::guardian_main() {
        return code;
    }
    // Below a guardian (a missing dispatch, or the launcher shipped as the
    // UI) this binary never acts as the launcher: no recursion.
    #[cfg(unix)]
    if macos::under_guardian() {
        fatal("Metro can't start: this copy of Metro is damaged. Download Metro again.");
        return ExitCode::from(2);
    }
    let mode = update::mode();
    if mode == update::Mode::LegacyHelper {
        fatal(
            "In-app MSI updates are no longer supported. Use a verified manual installer; any existing backup was kept.",
        );
        return ExitCode::from(2);
    }
    #[cfg(unix)]
    // SAFETY: umask has no preconditions.
    unsafe {
        libc::umask(0o077);
    }
    // SAFETY: first thing in main, before the runtime starts any thread.
    unsafe {
        metro_connector::http::exempt_loopback_from_proxy();
    }
    let paths = || -> Result<_, String> {
        let dir = install_dir()?;
        let root = store::desktop_root()
            .map_err(|e| format!("cannot find a place for Metro's data: {e}"))?;
        update::preflight_at_start(&dir, &root, matches!(mode, update::Mode::Trial(_)))?;
        Ok((dir, root))
    };
    let (dir, root) = match paths() {
        Ok(paths) => paths,
        Err(msg) => {
            // Refuse without even creating a monitoring record or lock file.
            fatal(&format!("Metro can't start: {msg}"));
            return ExitCode::from(1);
        }
    };
    let events = Events::new(&root);
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            events.record(EventKind::LauncherStartFailed);
            fatal(&format!("Metro can't start: {e}"));
            return ExitCode::from(1);
        }
    };
    let mut trial = match &mode {
        update::Mode::Trial(id) => Some(id.clone()),
        _ => None,
    };
    let code = loop {
        match rt.block_on(launch(Some(&events), trial.take(), &dir, &root)) {
            Ok(Launched::Exit(code)) => break code,
            Ok(Launched::Relaunch) => {
                if let Err(msg) = update::preflight_at_start(&dir, &root, false) {
                    fatal(&format!("Metro can't start: {msg}"));
                    break 1;
                }
            }
            Err((kind, msg)) => {
                events.record(kind);
                fatal(&format!("Metro can't start: {msg}"));
                break 1;
            }
        }
    };
    // Don't wait for stuck provider calls; everything was told to stop.
    rt.shutdown_timeout(Duration::from_secs(2));
    ExitCode::from(code)
}

#[cfg(test)]
mod no_proxy_tests {
    use super::ui_no_proxy;
    use metro_url_policy::ureq_no_proxy_matches;

    #[test]
    fn keeps_the_persons_entries_and_adds_only_exact_loopback_hosts() {
        let v = ui_no_proxy("corp.example,box.tail1234.ts.net,127.0.0.1,localhost,::1");
        assert!(
            v.starts_with("corp.example,box.tail1234.ts.net,127.0.0.1,localhost,::1,"),
            "{v}"
        );
        for direct in [
            "127.0.0.1",
            "[::1]",
            "localhost",
            "box.tail1234.ts.net",
            "corp.example",
        ] {
            assert!(ureq_no_proxy_matches(&v, direct), "{direct}");
        }
        // Not listed by the person: through the proxy (the UI then asks).
        for proxied in [
            "100.101.102.103",
            "[fd7a:115c:a1e0::1]",
            "other.tail1234.ts.net",
            "100.64.office.example",
            "127.foo.example",
            "192.168.1.5",
        ] {
            assert!(!ureq_no_proxy_matches(&v, proxied), "{proxied}");
        }
        assert_eq!(v.matches("localhost").count(), 1, "no duplicates");
        assert_eq!(ui_no_proxy("*"), "*");
    }
}
