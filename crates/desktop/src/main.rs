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

#![cfg_attr(windows, windows_subsystem = "windows")]

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use metro_connector::desktop::bridge::{self, ENV_TOKEN, ENV_URL};
use metro_connector::desktop::core::{Desktop, DesktopConfig};
use metro_connector::desktop::local::{LocalWorkspaceHost, ToolsHook};
use metro_connector::desktop::monitoring::{EventKind, Events};
use metro_connector::desktop::store;
use metro_connector::desktop_local::LocalServer;
use metro_connector::desktop_tools::DesktopTools;

#[cfg(windows)]
const UI_NAME: &str = "Metro.exe";
#[cfg(not(windows))]
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

/// `NO_PROXY` with the loopback hosts added: NSH's fetch (and our own
/// clients) honour `HTTP_PROXY`, which must never see the bridge bearer or a
/// local server's token.
fn no_proxy() -> String {
    metro_connector::http::no_proxy_with_loopback()
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

async fn launch(events: Option<&Events>) -> Result<u8, (EventKind, String)> {
    let start_failed = |msg| (EventKind::LauncherStartFailed, msg);
    let dir = install_dir().map_err(start_failed)?;
    let ui = component(&dir.join("ui").join(UI_NAME)).ok_or_else(|| {
        start_failed(format!(
            "the app files are incomplete: ui{}{UI_NAME} is missing. Extract the whole Metro archive again and keep the ui folder next to Metro.",
            std::path::MAIN_SEPARATOR
        ))
    })?;
    let server = component(&dir.join(SERVER_NAME));

    let root = store::desktop_root()
        .map_err(|e| start_failed(format!("cannot find a place for Metro's data: {e}")))?;
    store::prepare_private_dir(&root)
        .map_err(|e| start_failed(format!("cannot prepare Metro's data folder: {e}")))?;
    let lock = store::try_lock_instance(&root)
        .map_err(|e| start_failed(format!("cannot prepare Metro's data folder: {e}")))?;
    let primary = lock.is_some();

    let local: Option<Arc<dyn LocalWorkspaceHost>> = match (&server, primary) {
        (Some(bin), true) => {
            let data = root.join("local");
            match store::prepare_private_dir(&data) {
                Ok(()) => Some(Arc::new(LocalServer::with_events(
                    bin.clone(),
                    data,
                    events.cloned(),
                ))),
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
    let mut cfg = DesktopConfig::new(root.clone(), primary, local);
    cfg.tools = tools_hook(&root, primary);
    let desktop = Desktop::new(cfg);
    let bridge = bridge::start(desktop.clone()).await.map_err(|e| {
        start_failed(format!(
            "cannot start Metro's local connection service: {e}"
        ))
    })?;

    let mut cmd = tokio::process::Command::new(&ui);
    if let Some(ui_dir) = ui.parent() {
        cmd.current_dir(ui_dir);
    }
    cmd.env_remove("NSH_APP_DIR")
        .env(ENV_URL, &bridge.url)
        .env(ENV_TOKEN, bridge.token.expose())
        .env("NO_PROXY", no_proxy())
        .env("no_proxy", no_proxy())
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
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
    };
    // The UI is gone: nothing may keep serving on its behalf.
    bridge.stop().await;
    if tokio::time::timeout(Duration::from_secs(15), desktop.shutdown())
        .await
        .is_err()
    {
        eprintln!("Metro: some on-device work did not stop in time; ending it");
    }
    drop(desktop);
    drop(lock);
    Ok(exit_code(status))
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
    #[cfg(unix)]
    // SAFETY: umask has no preconditions.
    unsafe {
        libc::umask(0o077);
    }
    // SAFETY: first thing in main, before the runtime starts any thread.
    unsafe {
        metro_connector::http::exempt_loopback_from_proxy();
    }
    let events = store::desktop_root().ok().map(|root| Events::new(&root));
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            if let Some(events) = &events {
                events.record(EventKind::LauncherStartFailed);
            }
            fatal(&format!("Metro can't start: {e}"));
            return ExitCode::from(1);
        }
    };
    let code = match rt.block_on(launch(events.as_ref())) {
        Ok(code) => code,
        Err((kind, msg)) => {
            if let Some(events) = &events {
                events.record(kind);
            }
            fatal(&format!("Metro can't start: {msg}"));
            1
        }
    };
    // Don't wait for stuck provider calls; everything was told to stop.
    rt.shutdown_timeout(Duration::from_secs(2));
    ExitCode::from(code)
}
