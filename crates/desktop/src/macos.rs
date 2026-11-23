//! The macOS app layout and the UI guardian.
//!
//! ```text
//! ~/Applications/Metro Desktop/        private install folder; its optional
//!   enable-local-runner                marker is checked by
//!                                      `desktop_tools::runner::installer_marker`
//!   Metro.app/                         outer bundle (dev.metro.desktop)
//!     Contents/MacOS/Metro             this launcher (the bundle's executable)
//!     Contents/MacOS/metro-server      bundled local server (optional)
//!     Contents/Helpers/MetroUI.app/    the unchanged NSH app (dev.metro.desktop.ui)
//!       Contents/MacOS/Metro           its executable: a fixed name, never read
//!                                      from a plist, never found by bundle id
//!       Contents/Resources/nsh/        its own resources, as NSH packaged them
//! ```
//!
//! Everything is resolved from the launcher's own canonical path. Each name
//! below `Contents` is checked with `symlink_metadata`: a symlink anywhere on
//! the way to the UI is refused, as is a final component that is not a
//! regular executable file. Nothing is launched through LaunchServices.
//!
//! **Lifetime.** macOS has no parent-death signal, so the launcher does not
//! start the UI itself. It re-runs its own executable as a guardian
//! (`Metro --metro-ui-guardian`, no other arguments; nothing secret in argv)
//! whose stdin is a pipe only the launcher holds open. The guardian starts
//! the UI in the UI's own process group, with stdin from `/dev/null` and the
//! environment it inherited from the launcher (bridge URL and token,
//! `NO_PROXY`, `NSH_APP_DIR` removed). Then it waits for one of:
//!
//! - the UI exits: the guardian reaps it and exits with its code (a signal
//!   becomes 128 + signal), so the launcher sees the UI's result;
//! - the pipe closes (the launcher exited, crashed or was killed, even with
//!   SIGKILL): the guardian ends the UI's process group;
//! - SIGTERM, SIGHUP or SIGINT (the launcher's normal stop sends SIGTERM to
//!   the guardian's group): the guardian ends the UI's process group.
//!
//! Ending the group is SIGTERM, up to [`GRACE`], then SIGKILL, always before
//! the UI is reaped, so the group id cannot have been reused. The guardian
//! waits in `poll` on the pipe and a self-pipe its signal handlers (SIGCHLD
//! included) write to: no busy polling. Its handlers are real functions, not
//! `SIG_IGN`, so the UI starts with default signal dispositions.
//!
//! **No recursion, no launcher below the guardian.** Three independent
//! guards, so neither a missing dispatch in `main` nor a mispackaged bundle
//! (the launcher shipped as the UI's executable) can make launchers start
//! launchers:
//!
//! - the launcher sets [`GUARDED_ENV`] on the guardian; the guardian and
//!   everything below it (the UI, its helpers) inherit it, and `main`
//!   refuses the launcher role in any process that has it;
//! - [`guardian_command`] refuses in a process started with
//!   [`GUARDIAN_ARG`];
//! - the guardian refuses to start a UI that is its own executable file.
//!
//! The one case it can't cover: the guardian itself killed with SIGKILL
//! while the launcher lives. The launcher then sees its "UI" exit and stops
//! the bridge, so a UI left behind has nothing to talk to.
//!
//! Compiled on every Unix so its tests run in Linux CI; only `main.rs`'s
//! wiring is macOS-only.

#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use std::ffi::OsStr;
use std::os::fd::RawFd;
use std::path::{Path, PathBuf};
use std::process::{Child, ExitCode, ExitStatus};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::time::{Duration, Instant};

/// The only argument of guardian mode.
pub const GUARDIAN_ARG: &str = "--metro-ui-guardian";
/// Private sentinel, never secret: set by the launcher on its guardian and
/// inherited below it. Its presence (any value) forbids the launcher role.
pub const GUARDED_ENV: &str = "METRO_DESKTOP_UNDER_GUARDIAN";
/// From `Contents/` of the outer bundle to the UI's executable.
const UI_PATH: [&str; 5] = ["Helpers", "MetroUI.app", "Contents", "MacOS", "Metro"];
/// How long the UI gets between SIGTERM and SIGKILL. Below the launcher's
/// own 3 s, so the guardian finishes first on a normal stop.
pub const GRACE: Duration = Duration::from_secs(2);

// ------------------------------------------------------------------ layout

/// `<X>.app/Contents` for a launcher at `<X>.app/Contents/MacOS/<exe>`.
fn bundle_contents(macos_dir: &Path) -> Option<&Path> {
    let contents = macos_dir.parent()?;
    let app = contents.parent()?;
    (macos_dir.file_name() == Some(OsStr::new("MacOS"))
        && contents.file_name() == Some(OsStr::new("Contents"))
        && app.extension() == Some(OsStr::new("app")))
    .then_some(contents)
}

fn real_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink())
}

/// The UI's executable inside the outer bundle, from the launcher's
/// canonical directory (`.../Metro.app/Contents/MacOS`).
pub fn ui_executable(macos_dir: &Path) -> Result<PathBuf, String> {
    use std::os::unix::fs::PermissionsExt;
    let contents = bundle_contents(macos_dir).ok_or_else(|| {
        "Metro must run from inside Metro.app. Download Metro again and open Metro.app.".to_string()
    })?;
    let incomplete = || {
        "the app files are incomplete: Metro.app/Contents/Helpers/MetroUI.app is missing or damaged. Download Metro again and replace Metro.app.".to_string()
    };
    let (exe, dirs) = UI_PATH.split_last().expect("UI_PATH is not empty");
    let mut path = contents.to_path_buf();
    for name in dirs {
        path.push(name);
        if !real_dir(&path) {
            return Err(incomplete());
        }
    }
    path.push(exe);
    let ui = crate::component(&path).ok_or_else(incomplete)?;
    let executable =
        std::fs::symlink_metadata(&ui).is_ok_and(|m| m.permissions().mode() & 0o111 != 0);
    executable.then_some(ui).ok_or_else(incomplete)
}

// ---------------------------------------------------------------- launcher

/// The guardian command for the launcher to start in place of the UI. The
/// caller adds the environment and the process group, and must make stdin a
/// pipe it keeps open for as long as it runs.
pub fn guardian_command(macos_dir: &Path) -> Result<tokio::process::Command, String> {
    // A guardian never starts another one, even if `main` failed to
    // dispatch it: re-running ourselves must not be able to recurse.
    if guardian_requested(std::env::args_os()) {
        return Err("a Metro window guardian cannot start another one".to_string());
    }
    let exe = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|_| "cannot find the Metro program file".to_string())?;
    if exe.parent() != Some(macos_dir) {
        return Err("cannot find the Metro program file".to_string());
    }
    let mut cmd = tokio::process::Command::new(exe);
    cmd.arg(GUARDIAN_ARG).env(GUARDED_ENV, "1");
    Ok(cmd)
}

/// Whether this process runs below a guardian, where it must never act as
/// the launcher. `main` checks this on every Unix, right after guardian
/// dispatch.
pub fn under_guardian() -> bool {
    sentinel_set(std::env::var_os(GUARDED_ENV))
}

fn sentinel_set(value: Option<std::ffi::OsString>) -> bool {
    value.is_some()
}

// ---------------------------------------------------------------- guardian

/// Guardian mode, if this process was started as one. Call first in `main`,
/// before any logging, monitoring or desktop-root setup: the guardian
/// touches none of it.
pub fn guardian_main() -> Option<ExitCode> {
    if !guardian_requested(std::env::args_os()) {
        return None;
    }
    if std::env::args_os().nth(2).is_some() {
        eprintln!("Metro: {GUARDIAN_ARG} takes no other arguments");
        return Some(ExitCode::from(2));
    }
    Some(ExitCode::from(run_guardian()))
}

/// Whether argv (program name first) asks for guardian mode: its first
/// argument is exactly [`GUARDIAN_ARG`].
fn guardian_requested(args: impl IntoIterator<Item = std::ffi::OsString>) -> bool {
    args.into_iter().nth(1).as_deref() == Some(OsStr::new(GUARDIAN_ARG))
}

/// Same device and inode (a hard link counts); unreadable counts as same.
fn same_file(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => true,
    }
}

fn stdin_is_pipe() -> bool {
    // SAFETY: fstat writes into a zeroed stat of the right type.
    unsafe {
        let mut st: libc::stat = std::mem::zeroed();
        libc::fstat(0, &mut st) == 0 && (st.st_mode & libc::S_IFMT) == libc::S_IFIFO
    }
}

fn run_guardian() -> u8 {
    // Only the launcher starts a guardian, always with a pipe it holds.
    if !stdin_is_pipe() || !under_guardian() {
        eprintln!("Metro: {GUARDIAN_ARG} is started by Metro itself");
        return 2;
    }
    let ui = match crate::install_dir().and_then(|dir| ui_executable(&dir)) {
        Ok(ui) => ui,
        Err(e) => {
            eprintln!("Metro: cannot start the Metro window: {e}");
            return 127;
        }
    };
    // A bundle with the launcher in the UI's place must not start itself.
    let own = std::env::current_exe().and_then(std::fs::canonicalize);
    if !own.is_ok_and(|own| !same_file(&own, &ui)) {
        eprintln!(
            "Metro: the app files are damaged: the Metro window is the launcher itself. Download Metro again and replace Metro.app."
        );
        return 2;
    }
    if let Err(e) = install_handlers() {
        eprintln!("Metro: cannot watch the Metro window: {e}");
        return 1;
    }
    let mut cmd = std::process::Command::new(&ui);
    if let Some(dir) = ui.parent() {
        cmd.current_dir(dir);
    }
    cmd.stdin(std::process::Stdio::null());
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    match cmd.spawn() {
        Ok(child) => supervise(child, 0),
        Err(e) => {
            eprintln!("Metro: cannot start the Metro window: {e}");
            127
        }
    }
}

/// Set by SIGTERM, SIGHUP and SIGINT.
static STOP: AtomicBool = AtomicBool::new(false);
/// Self-pipe: the handlers write a byte (non-blocking), `poll` wakes.
static WAKE_R: AtomicI32 = AtomicI32::new(-1);
static WAKE_W: AtomicI32 = AtomicI32::new(-1);

#[cfg(target_vendor = "apple")]
unsafe fn errno_ptr() -> *mut libc::c_int {
    // SAFETY: always valid for the calling thread.
    unsafe { libc::__error() }
}
#[cfg(not(target_vendor = "apple"))]
unsafe fn errno_ptr() -> *mut libc::c_int {
    // SAFETY: always valid for the calling thread.
    unsafe { libc::__errno_location() }
}

extern "C" fn on_signal(sig: libc::c_int) {
    if sig != libc::SIGCHLD {
        STOP.store(true, Ordering::SeqCst);
    }
    let fd = WAKE_W.load(Ordering::SeqCst);
    if fd >= 0 {
        // SAFETY: async-signal-safe calls only; errno is restored.
        unsafe {
            let saved = *errno_ptr();
            let byte = 0u8;
            libc::write(fd, (&raw const byte).cast(), 1);
            *errno_ptr() = saved;
        }
    }
}

fn cloexec_nonblock(fd: RawFd) -> std::io::Result<()> {
    // SAFETY: plain fcntl on a descriptor we own.
    unsafe {
        let fl = libc::fcntl(fd, libc::F_GETFL);
        if fl < 0
            || libc::fcntl(fd, libc::F_SETFL, fl | libc::O_NONBLOCK) < 0
            || libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) < 0
        {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

/// The self-pipe (close-on-exec, so the UI never inherits it) and handlers
/// for SIGCHLD, SIGTERM, SIGHUP and SIGINT. Once per process.
fn install_handlers() -> std::io::Result<()> {
    if WAKE_W.load(Ordering::SeqCst) >= 0 {
        return Ok(());
    }
    let mut fds = [0; 2];
    // SAFETY: pipe writes two descriptors into the array.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    cloexec_nonblock(fds[0])?;
    cloexec_nonblock(fds[1])?;
    WAKE_R.store(fds[0], Ordering::SeqCst);
    WAKE_W.store(fds[1], Ordering::SeqCst);
    for sig in [libc::SIGCHLD, libc::SIGTERM, libc::SIGHUP, libc::SIGINT] {
        // SAFETY: a zeroed sigaction with a valid handler and empty mask.
        unsafe {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
            sa.sa_flags = libc::SA_RESTART;
            libc::sigemptyset(&mut sa.sa_mask);
            if libc::sigaction(sig, &sa, std::ptr::null_mut()) != 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
    }
    Ok(())
}

fn drain_wake() {
    let fd = WAKE_R.load(Ordering::SeqCst);
    let mut buf = [0u8; 64];
    // SAFETY: reads into a local buffer from our non-blocking pipe.
    while unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) } > 0 {}
}

/// Waits up to `timeout` (`None`: forever) for the self-pipe or, when
/// given, `liveness`. True if `liveness` is readable, hung up or failed.
fn wait(liveness: Option<RawFd>, timeout: Option<Duration>) -> bool {
    let mut fds = vec![libc::pollfd {
        fd: WAKE_R.load(Ordering::SeqCst),
        events: libc::POLLIN,
        revents: 0,
    }];
    if let Some(fd) = liveness {
        fds.push(libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        });
    }
    let ms = timeout.map_or(-1, |t| t.as_millis().min(60_000) as libc::c_int);
    // SAFETY: fds is a valid array of its length.
    let n = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, ms) };
    drain_wake();
    n > 0
        && fds.get(1).is_some_and(|p| {
            p.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0
        })
}

/// Runs until the UI exits, the launcher's pipe `liveness` closes, or a
/// stop signal arrives; returns the UI's exit code.
fn supervise(mut child: Child, liveness: RawFd) -> u8 {
    let pid = child.id() as libc::pid_t;
    let code = |s: ExitStatus| crate::exit_code(Some(s));
    loop {
        match child.try_wait() {
            Ok(Some(s)) => return code(s),
            Ok(None) => {}
            Err(_) => break,
        }
        if STOP.load(Ordering::SeqCst) || wait(Some(liveness), None) {
            break;
        }
    }
    // Still unreaped, so `pid` is still this UI's process group.
    // SAFETY: signalling our own child's process group.
    unsafe { libc::kill(-pid, libc::SIGTERM) };
    let deadline = Instant::now() + GRACE;
    loop {
        if let Ok(Some(s)) = child.try_wait() {
            return code(s);
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        wait(None, Some(left));
    }
    // SAFETY: as above; the child is not reaped yet.
    unsafe { libc::kill(-pid, libc::SIGKILL) };
    crate::exit_code(child.wait().ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::{AsRawFd, OwnedFd};
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::process::{Command, Stdio};
    use std::sync::Mutex;

    /// Signal handlers and STOP are process-wide.
    static GUARD: Mutex<()> = Mutex::new(());

    struct Temp(PathBuf);
    impl Temp {
        fn new(tag: &str) -> Temp {
            let p = std::env::temp_dir().join(format!(
                "metro-desktop-macos-{tag}-{}-{:?}",
                std::process::id(),
                Instant::now()
            ));
            std::fs::create_dir_all(&p).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700)).unwrap();
            Temp(std::fs::canonicalize(&p).unwrap())
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// `<root>/Metro Desktop/Metro.app` with the full layout; returns the
    /// outer `Contents/MacOS`. A space in the path on purpose.
    fn bundle(root: &Path) -> PathBuf {
        let parent = root.join("Metro Desktop");
        let macos = parent.join("Metro.app/Contents/MacOS");
        std::fs::create_dir_all(&macos).unwrap();
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700)).unwrap();
        let ui_dir = parent.join("Metro.app/Contents/Helpers/MetroUI.app/Contents/MacOS");
        std::fs::create_dir_all(&ui_dir).unwrap();
        let ui = ui_dir.join("Metro");
        std::fs::write(&ui, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&ui, std::fs::Permissions::from_mode(0o755)).unwrap();
        macos
    }

    #[test]
    fn resolves_the_fixed_ui_path() {
        let t = Temp::new("ok");
        let macos = bundle(&t.0);
        let ui = ui_executable(&macos).unwrap();
        assert_eq!(
            ui,
            t.0.join("Metro Desktop/Metro.app/Contents/Helpers/MetroUI.app/Contents/MacOS/Metro")
        );
    }

    #[test]
    fn refuses_symlinks_on_the_way_to_the_ui() {
        let names = ["Helpers", "MetroUI.app", "Contents", "MacOS", "Metro"];
        for depth in 0..names.len() {
            let t = Temp::new("link");
            let macos = bundle(&t.0);
            let mut p = macos.parent().unwrap().to_path_buf();
            for n in &names[..=depth] {
                p.push(n);
            }
            let moved = t.0.join("moved");
            std::fs::rename(&p, &moved).unwrap();
            symlink(&moved, &p).unwrap();
            assert!(ui_executable(&macos).is_err(), "symlinked {}", names[depth]);
        }
    }

    #[test]
    fn refuses_a_missing_or_non_executable_ui_and_other_layouts() {
        let t = Temp::new("bad");
        let macos = bundle(&t.0);
        let ui = macos.join("../Helpers/MetroUI.app/Contents/MacOS/Metro");
        std::fs::set_permissions(&ui, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(ui_executable(&macos).is_err());
        std::fs::remove_file(&ui).unwrap();
        std::fs::create_dir(&ui).unwrap();
        assert!(ui_executable(&macos).is_err());
        // A launcher outside a bundle (the Linux/Windows layout).
        assert!(ui_executable(&t.0).is_err());
        let not_app = t.0.join("Metro/Contents/MacOS");
        std::fs::create_dir_all(&not_app).unwrap();
        assert!(ui_executable(&not_app).is_err());
    }

    #[test]
    fn guardian_mode_is_only_the_exact_first_argument() {
        let argv = |a: &[&str]| a.iter().map(std::ffi::OsString::from).collect::<Vec<_>>();
        assert!(guardian_requested(argv(&["Metro", GUARDIAN_ARG])));
        assert!(guardian_requested(argv(&["Metro", GUARDIAN_ARG, "x"])));
        assert!(!guardian_requested(argv(&["Metro"])));
        assert!(!guardian_requested(argv(&["Metro", "-psn_0_1234"])));
        assert!(!guardian_requested(argv(&["Metro", "x", GUARDIAN_ARG])));
        assert!(!guardian_requested(argv(&[GUARDIAN_ARG])));
    }

    #[test]
    fn the_sentinel_forbids_the_launcher_role_whatever_its_value() {
        assert!(!sentinel_set(None));
        assert!(sentinel_set(Some("1".into())));
        assert!(sentinel_set(Some("".into())));
        assert!(sentinel_set(Some("0".into())));
    }

    #[test]
    fn same_file_sees_hard_links_and_refuses_unreadable() {
        let t = Temp::new("same");
        let (a, b, c) = (t.0.join("a"), t.0.join("b"), t.0.join("c"));
        std::fs::write(&a, "x").unwrap();
        std::fs::write(&c, "x").unwrap();
        std::fs::hard_link(&a, &b).unwrap();
        assert!(same_file(&a, &b));
        assert!(!same_file(&a, &c)); // a copy is caught by the sentinel instead
        assert!(same_file(&a, &t.0.join("missing")));
    }

    fn pipe() -> (OwnedFd, OwnedFd) {
        let mut fds = [0; 2];
        // SAFETY: pipe writes two descriptors that we then own.
        unsafe {
            assert_eq!(libc::pipe(fds.as_mut_ptr()), 0);
            use std::os::fd::FromRawFd;
            libc::fcntl(fds[0], libc::F_SETFD, libc::FD_CLOEXEC);
            libc::fcntl(fds[1], libc::F_SETFD, libc::FD_CLOEXEC);
            (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1]))
        }
    }

    fn spawn(script: &str) -> Child {
        use std::os::unix::process::CommandExt;
        Command::new("/bin/sh")
            .args(["-c", script])
            .stdin(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap()
    }

    fn alive(pid: libc::pid_t) -> bool {
        // SAFETY: signal 0 only checks existence.
        unsafe { libc::kill(pid, 0) == 0 }
    }

    fn setup() -> std::sync::MutexGuard<'static, ()> {
        let g = GUARD.lock().unwrap_or_else(|e| e.into_inner());
        install_handlers().unwrap();
        STOP.store(false, Ordering::SeqCst);
        g
    }

    #[test]
    fn passes_the_ui_exit_code_through() {
        let _g = setup();
        let (r, _w) = pipe();
        assert_eq!(supervise(spawn("sleep 0.2; exit 7"), r.as_raw_fd()), 7);
        assert_eq!(supervise(spawn("kill -KILL $$"), r.as_raw_fd()), 128 + 9);
    }

    #[test]
    fn ends_the_ui_group_when_the_launcher_goes_away() {
        let _g = setup();
        let (r, w) = pipe();
        let child = spawn("sleep 30");
        let start = Instant::now();
        drop(w); // what the kernel does when the launcher dies
        let code = supervise(child, r.as_raw_fd());
        assert_eq!(code, 128 + libc::SIGTERM as u8);
        assert!(start.elapsed() < GRACE);
    }

    #[test]
    fn kills_a_ui_that_ignores_sigterm_and_its_helpers() {
        let _g = setup();
        let (r, w) = pipe();
        let t = Temp::new("stubborn");
        let pidfile = t.0.join("helper pid");
        let child = {
            use std::os::unix::process::CommandExt;
            Command::new("/bin/sh")
                .args([
                    "-c",
                    "trap '' TERM; sleep 30 & echo $! > \"$1\"; while :; do sleep 1; done",
                    "sh",
                ])
                .arg(&pidfile)
                .stdin(Stdio::null())
                .process_group(0)
                .spawn()
                .unwrap()
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        let helper = loop {
            if let Ok(s) = std::fs::read_to_string(&pidfile)
                && let Ok(pid) = s.trim().parse::<libc::pid_t>()
            {
                break pid;
            }
            assert!(Instant::now() < deadline, "helper did not start");
            std::thread::sleep(Duration::from_millis(20));
        };
        let start = Instant::now();
        drop(w);
        assert_eq!(supervise(child, r.as_raw_fd()), 128 + 9);
        assert!(start.elapsed() >= GRACE && start.elapsed() < GRACE + Duration::from_secs(2));
        // The helper (same group) is gone too; it may linger as a zombie of
        // init for a moment, so allow a short wait.
        let deadline = Instant::now() + Duration::from_secs(2);
        while alive(helper) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!alive(helper), "the UI's helper survived");
    }

    #[test]
    fn a_stop_signal_ends_the_ui() {
        let _g = setup();
        let (r, _w) = pipe();
        let child = spawn("sleep 30");
        let pid = child.id() as libc::pid_t;
        let stopper = std::thread::spawn(|| {
            std::thread::sleep(Duration::from_millis(200));
            // What the launcher's SIGTERM to the guardian does.
            on_signal(libc::SIGTERM);
        });
        assert_eq!(supervise(child, r.as_raw_fd()), 128 + libc::SIGTERM as u8);
        stopper.join().unwrap();
        STOP.store(false, Ordering::SeqCst);
        assert!(!alive(pid));
    }
}
