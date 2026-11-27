//! `workspace.shell` for this computer's runner: one command through the
//! platform shell (`/bin/sh -c` on Unix, `cmd.exe /d /s /c` on Windows) in
//! the Space's folder.
//!
//! - The environment is cleared except `PATH`, `HOME` (Windows:
//!   `USERPROFILE`), `LANG`, `TERM=dumb` and `METRO_SPACE_ROOT`. Windows also
//!   keeps `SystemRoot`, `windir`, `ComSpec`, `PATHEXT`, `TEMP` and `TMP`,
//!   without which `cmd.exe` and most programs don't start. No Metro token
//!   is ever passed.
//! - stdout and stderr are each kept up to [`MAX_STREAM_BYTES`], with a note
//!   giving the full size.
//! - The command runs in its own process group (Windows: a job object).
//!   The whole group is killed on timeout, when the server says the job is
//!   over (the run stopped, or the Space's runner changed), when the runner
//!   stops, and once the shell itself exits (so nothing it started keeps
//!   running).
//!
//! The server decides whether a command may run before it queues the job;
//! this module only runs it.

use std::fs::File;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Map, Value};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::watch;

pub const DEFAULT_TIMEOUT_SECS: u64 = 120;
pub const MAX_TIMEOUT_SECS: u64 = 600;
pub const MAX_STREAM_BYTES: usize = 64 * 1024;
pub const MAX_COMMAND_BYTES: usize = 32 * 1024;
/// How long output is still read after the process group was killed.
const DRAIN: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub command: String,
    pub timeout: Duration,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    command: String,
    #[serde(default)]
    timeout_secs: Option<u64>,
}

/// `{command, timeout_secs?}`.
pub fn parse(args: &Value) -> Result<Request, String> {
    let Some(m) = args.as_object() else {
        return Err("arguments must be a JSON object".into());
    };
    let a: Args = serde_json::from_value(Value::Object(Map::clone(m)))
        .map_err(|e| format!("invalid arguments: {e}"))?;
    if a.command.trim().is_empty() {
        return Err("invalid arguments: `command` is empty".into());
    }
    if a.command.len() > MAX_COMMAND_BYTES {
        return Err(format!(
            "invalid arguments: `command` is longer than {MAX_COMMAND_BYTES} bytes"
        ));
    }
    if a.command.contains('\0') {
        return Err("invalid arguments: `command` contains a NUL character".into());
    }
    let secs = a.timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS);
    if !(1..=MAX_TIMEOUT_SECS).contains(&secs) {
        return Err(format!(
            "invalid arguments: `timeout_secs` must be 1 to {MAX_TIMEOUT_SECS}"
        ));
    }
    Ok(Request {
        command: a.command,
        timeout: Duration::from_secs(secs),
    })
}

#[derive(Default)]
struct Capture {
    kept: Vec<u8>,
    total: u64,
}

/// Read to EOF, keeping at most `cap` bytes in `into`.
async fn capture<R: AsyncRead + Unpin>(mut r: R, cap: usize, into: Arc<Mutex<Capture>>) {
    let mut buf = [0u8; 8192];
    loop {
        let n = match r.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        let Ok(mut c) = into.lock() else { return };
        c.total += n as u64;
        let room = cap.saturating_sub(c.kept.len()).min(n);
        c.kept.extend_from_slice(&buf[..room]);
    }
}

fn section(out: &mut String, name: &str, c: &Mutex<Capture>) {
    let c = c.lock().map(|c| {
        (
            String::from_utf8_lossy(&c.kept).into_owned(),
            c.total,
            c.kept.len(),
        )
    });
    let (text, total, kept) = c.unwrap_or_default();
    out.push_str(&format!("--- {name} ---\n"));
    out.push_str(&text);
    if total > kept as u64 {
        out.push_str(&format!(
            "\n[metro-desktop: {name} truncated, {total} bytes total]"
        ));
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }
}

enum End {
    Exited(std::process::ExitStatus),
    TimedOut,
    Cancelled,
    Stopped,
    Failed,
}

/// Run `req` in `dir` (its path is `dir_path`). `stop` turning true (or
/// its sender going away) or `cancelled` finishing ends the command.
/// `(ok, output)`: ok when the command ran and exited with 0.
pub async fn run(
    req: &Request,
    dir: &File,
    dir_path: &Path,
    mut stop: watch::Receiver<bool>,
    cancelled: impl std::future::Future<Output = ()>,
) -> (bool, String) {
    let mut cmd = platform::command(&req.command, dir, dir_path);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return (
                false,
                format!("workspace.shell: couldn't start the shell ({:?})", e.kind()),
            );
        }
    };
    let tree = match platform::Tree::new(&child) {
        Ok(t) => t,
        Err(_) => {
            let _ = child.kill().await;
            return (
                false,
                "workspace.shell: couldn't contain the command's processes; nothing was run".into(),
            );
        }
    };
    let (out, err) = (
        Arc::<Mutex<Capture>>::default(),
        Arc::<Mutex<Capture>>::default(),
    );
    let readers = [
        child
            .stdout
            .take()
            .map(|s| tokio::spawn(capture(s, MAX_STREAM_BYTES, out.clone()))),
        child
            .stderr
            .take()
            .map(|s| tokio::spawn(capture(s, MAX_STREAM_BYTES, err.clone()))),
    ];
    let end = if *stop.borrow() {
        End::Stopped
    } else {
        tokio::select! {
            s = child.wait() => s.map(End::Exited).unwrap_or(End::Failed),
            _ = tokio::time::sleep(req.timeout) => End::TimedOut,
            _ = stop.changed() => End::Stopped,
            _ = cancelled => End::Cancelled,
        }
    };
    // Whatever is still running in the group goes, in every case.
    tree.kill();
    let _ = child.kill().await;
    let _ = child.wait().await;
    for r in readers.into_iter().flatten() {
        let abort = r.abort_handle();
        if tokio::time::timeout(DRAIN, r).await.is_err() {
            abort.abort();
        }
    }
    drop(tree);
    let secs = req.timeout.as_secs();
    let (ok, mut text) = match end {
        End::Exited(s) => (s.success(), format!("exit: {}\n", exit_text(&s))),
        End::TimedOut => (
            false,
            format!("workspace.shell: timed out after {secs}s and was killed\n"),
        ),
        End::Cancelled => (
            false,
            "workspace.shell: the job ended on the server (the run stopped or the Space's \
             runner changed) and the command was killed\n"
                .into(),
        ),
        End::Stopped => (
            false,
            "workspace.shell: stopped because this computer's runner stopped\n".into(),
        ),
        End::Failed => (false, "workspace.shell: lost track of the command\n".into()),
    };
    section(&mut text, "stdout", &out);
    section(&mut text, "stderr", &err);
    (ok, text)
}

fn exit_text(s: &std::process::ExitStatus) -> String {
    if let Some(c) = s.code() {
        return c.to_string();
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = s.signal() {
            return format!("signal {sig}");
        }
    }
    "unknown".into()
}

fn keep(cmd: &mut tokio::process::Command, name: &str) {
    if let Some(v) = std::env::var_os(name).filter(|v| !v.is_empty()) {
        cmd.env(name, v);
    }
}

#[cfg(unix)]
mod platform {
    use std::fs::File;
    use std::os::fd::AsRawFd;
    use std::path::Path;

    pub fn command(command: &str, dir: &File, dir_path: &Path) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.arg("-c").arg(command).env_clear();
        match std::env::var_os("PATH").filter(|v| !v.is_empty()) {
            Some(p) => cmd.env("PATH", p),
            None => cmd.env("PATH", "/usr/local/bin:/usr/bin:/bin"),
        };
        super::keep(&mut cmd, "HOME");
        super::keep(&mut cmd, "LANG");
        cmd.env("TERM", "dumb")
            .env("METRO_SPACE_ROOT", dir_path)
            .process_group(0);
        // The working directory is the handle that was opened without
        // following links, not the path looked up again.
        let fd = dir.as_raw_fd();
        // SAFETY: fchdir and prctl are async-signal-safe; nothing allocates.
        unsafe {
            cmd.pre_exec(move || {
                if libc::fchdir(fd) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                #[cfg(target_os = "linux")]
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        cmd
    }

    /// The child's process group (its pid, from `process_group(0)`).
    pub struct Tree(Option<libc::pid_t>);

    impl Tree {
        pub fn new(child: &tokio::process::Child) -> std::io::Result<Tree> {
            Ok(Tree(
                child.id().map(|p| p as libc::pid_t).filter(|p| *p > 1),
            ))
        }

        pub fn kill(&self) {
            if let Some(pg) = self.0 {
                // SAFETY: signals the process group created for this child.
                unsafe {
                    libc::kill(-pg, libc::SIGKILL);
                }
            }
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            self.kill();
        }
    }
}

#[cfg(windows)]
mod platform {
    use std::fs::File;
    use std::path::{Path, PathBuf};

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject,
    };

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    pub fn command(command: &str, _dir: &File, dir_path: &Path) -> tokio::process::Command {
        let system = std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        let mut cmd = tokio::process::Command::new(system.join("System32").join("cmd.exe"));
        // `/d`: no AutoRun commands. `/s /c "..."`: the command line as is.
        cmd.raw_arg(format!("/d /s /c \"{command}\""))
            .env_clear()
            .current_dir(dir_path)
            .creation_flags(CREATE_NO_WINDOW);
        for name in [
            "PATH",
            "USERPROFILE",
            "LANG",
            "SystemRoot",
            "windir",
            "ComSpec",
            "PATHEXT",
            "TEMP",
            "TMP",
        ] {
            super::keep(&mut cmd, name);
        }
        cmd.env("TERM", "dumb").env("METRO_SPACE_ROOT", dir_path);
        cmd
    }

    /// A job object holding the command and everything it starts; closing
    /// it kills them all.
    pub struct Tree(HANDLE);

    // SAFETY: a job handle may be used and closed from any thread.
    unsafe impl Send for Tree {}
    unsafe impl Sync for Tree {}

    impl Tree {
        pub fn new(child: &tokio::process::Child) -> std::io::Result<Tree> {
            let Some(process) = child.raw_handle() else {
                return Err(std::io::Error::other("the command already ended"));
            };
            // SAFETY: plain Win32 calls with valid arguments; the handle is
            // owned by the returned `Tree` and closed on drop.
            unsafe {
                let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if job.is_null() {
                    return Err(std::io::Error::last_os_error());
                }
                let tree = Tree(job);
                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                if SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const _,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                ) == 0
                    || AssignProcessToJobObject(job, process as HANDLE) == 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(tree)
            }
        }

        pub fn kill(&self) {
            // SAFETY: we own the job handle.
            unsafe {
                TerminateJobObject(self.0, 1);
            }
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            // SAFETY: we own the handle; KILL_ON_JOB_CLOSE ends what's left.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    async fn cmd(command: &str, secs: u64) -> (bool, String) {
        let t = tempfile::tempdir().unwrap();
        let f = super::super::folder::open_dir_path(t.path()).unwrap();
        let (_tx, rx) = watch::channel(false);
        let req = Request {
            command: command.into(),
            timeout: Duration::from_secs(secs),
        };
        run(&req, &f, t.path(), rx, std::future::pending()).await
    }

    #[tokio::test]
    async fn runs_with_exit_code_and_scrubbed_environment() {
        let (ok, out) = cmd("echo %TERM%& exit /b 3", 30).await;
        assert!(!ok);
        assert!(
            out.starts_with("exit: 3\n--- stdout ---\ndumb\r\n"),
            "{out}"
        );
        let (ok, out) = cmd(
            "if defined METRO_SPACE_ROOT (echo set) else (echo unset)",
            30,
        )
        .await;
        assert!(ok && out.contains("set"), "{out}");
        let (_, out) = cmd("if defined USERNAME (echo leaked) else (echo clean)", 30).await;
        assert!(out.contains("clean"), "{out}");
    }

    #[tokio::test]
    async fn timeout_ends_the_job() {
        let started = std::time::Instant::now();
        let (ok, out) = cmd("for /l %i in (0,0,1) do @rem", 1).await;
        assert!(!ok);
        assert!(
            out.starts_with("workspace.shell: timed out after 1s and was killed\n"),
            "{out}"
        );
        assert!(started.elapsed() < Duration::from_secs(15));
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use serde_json::json;

    fn dir() -> (tempfile::TempDir, File, std::path::PathBuf) {
        let t = tempfile::tempdir().unwrap();
        let p = std::fs::canonicalize(t.path()).unwrap();
        let f = super::super::folder::open_dir_path(&p).unwrap();
        (t, f, p)
    }

    /// `(ok, output, folder)`; the folder lives as long as the guard.
    async fn sh(command: &str, secs: u64) -> (bool, String, Folder) {
        let (t, f, p) = dir();
        let (_tx, rx) = watch::channel(false);
        let req = Request {
            command: command.into(),
            timeout: Duration::from_secs(secs),
        };
        let (ok, out) = run(&req, &f, &p, rx, std::future::pending()).await;
        (ok, out, Folder(t, p))
    }

    struct Folder(#[allow(dead_code)] tempfile::TempDir, std::path::PathBuf);

    impl std::ops::Deref for Folder {
        type Target = std::path::Path;
        fn deref(&self) -> &std::path::Path {
            &self.1
        }
    }

    #[test]
    fn arguments() {
        let r = parse(&json!({"command": "ls"})).unwrap();
        assert_eq!(r.timeout, Duration::from_secs(DEFAULT_TIMEOUT_SECS));
        assert_eq!(
            parse(&json!({"command": "ls", "timeout_secs": 600}))
                .unwrap()
                .timeout,
            Duration::from_secs(600)
        );
        for bad in [
            json!({"command": "ls", "timeout_secs": 601}),
            json!({"command": "ls", "timeout_secs": 0}),
            json!({"command": "ls", "timeout_secs": -1}),
            json!({"command": "  "}),
            json!({"command": "a\u{0}b"}),
            json!({"command": "ls", "cwd": "/"}),
            json!({"command": "x".repeat(MAX_COMMAND_BYTES + 1)}),
            json!({}),
            json!("ls"),
        ] {
            assert!(parse(&bad).is_err(), "{bad}");
        }
    }

    #[tokio::test]
    async fn runs_in_the_folder_with_exit_code() {
        let (ok, out, p) = sh("pwd; echo \"$METRO_SPACE_ROOT\"; echo err >&2; exit 3", 30).await;
        assert!(!ok);
        let pd = p.display().to_string();
        assert_eq!(
            out,
            format!("exit: 3\n--- stdout ---\n{pd}\n{pd}\n--- stderr ---\nerr\n")
        );
        let (ok, out, _) = sh("printf hi", 30).await;
        assert!(ok);
        assert!(out.starts_with("exit: 0\n--- stdout ---\nhi\n"), "{out}");
    }

    #[tokio::test]
    async fn environment_is_scrubbed() {
        let (ok, out, _) = sh("env", 30).await;
        assert!(ok, "{out}");
        let allowed = [
            "PATH",
            "HOME",
            "LANG",
            "TERM",
            "METRO_SPACE_ROOT",
            // Set by the shell itself.
            "PWD",
            "OLDPWD",
            "SHLVL",
            "_",
        ];
        let body = out.split("--- stdout ---\n").nth(1).unwrap();
        let body = body.split("--- stderr ---").next().unwrap();
        for line in body.lines().filter(|l| !l.is_empty()) {
            let name = line.split('=').next().unwrap();
            assert!(allowed.contains(&name), "unexpected variable {name}");
        }
        assert!(body.contains("TERM=dumb\n"), "{body}");
    }

    #[tokio::test]
    async fn output_is_bounded() {
        let (ok, out, _) = sh("head -c 100000 /dev/zero | tr '\\0' a; echo e >&2", 30).await;
        assert!(ok);
        assert!(
            out.contains("[metro-desktop: stdout truncated, 100000 bytes total]"),
            "{}",
            &out[out.len() - 200..]
        );
        assert!(out.len() < MAX_STREAM_BYTES + 300);
        assert!(out.ends_with("--- stderr ---\ne\n"));
    }

    fn gone(pid: i32) -> bool {
        // SAFETY: signal 0 only checks for existence.
        let r = unsafe { libc::kill(pid, 0) };
        r != 0 || {
            // A zombie still answers; it no longer runs.
            std::fs::read_to_string(format!("/proc/{pid}/stat"))
                .map(|s| s.contains(") Z "))
                .unwrap_or(true)
        }
    }

    async fn wait_gone(pid: i32) {
        for _ in 0..100 {
            if gone(pid) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("process {pid} still runs");
    }

    #[tokio::test]
    async fn timeout_kills_the_whole_group() {
        let started = std::time::Instant::now();
        let (ok, out, p) = sh("sleep 60 & echo $! > bg.pid; echo before; sleep 60", 1).await;
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(!ok);
        assert!(
            out.starts_with("workspace.shell: timed out after 1s and was killed\n"),
            "{out}"
        );
        assert!(out.contains("before\n"), "{out}");
        let pid: i32 = std::fs::read_to_string(p.join("bg.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        wait_gone(pid).await;
    }

    #[tokio::test]
    async fn background_leftovers_are_killed_when_the_shell_exits() {
        let started = std::time::Instant::now();
        let (ok, out, p) = sh("sleep 60 & echo $! > bg.pid; echo done", 30).await;
        assert!(started.elapsed() < Duration::from_secs(10), "{out}");
        assert!(ok, "{out}");
        let pid: i32 = std::fs::read_to_string(p.join("bg.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        wait_gone(pid).await;
    }

    #[tokio::test]
    async fn stopping_the_runner_stops_the_command() {
        let (_t, f, p) = dir();
        let (tx, rx) = watch::channel(false);
        let req = Request {
            command: "echo up; sleep 60".into(),
            timeout: Duration::from_secs(60),
        };
        let started = std::time::Instant::now();
        let job = tokio::spawn(async move { run(&req, &f, &p, rx, std::future::pending()).await });
        tokio::time::sleep(Duration::from_millis(300)).await;
        tx.send(true).unwrap();
        let (ok, out) = job.await.unwrap();
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(!ok);
        assert!(out.starts_with("workspace.shell: stopped"), "{out}");
    }

    #[tokio::test]
    async fn a_job_ended_on_the_server_is_killed() {
        let (_t, f, p) = dir();
        let (_tx, rx) = watch::channel(false);
        let req = Request {
            command: "sleep 60 & echo $! > bg.pid; sleep 60".into(),
            timeout: Duration::from_secs(60),
        };
        let started = std::time::Instant::now();
        let cancel = tokio::time::sleep(Duration::from_millis(300));
        let (ok, out) = run(&req, &f, &p, rx, cancel).await;
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(!ok);
        assert!(
            out.starts_with("workspace.shell: the job ended on the server"),
            "{out}"
        );
        let pid: i32 = std::fs::read_to_string(p.join("bg.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        wait_gone(pid).await;
    }
}
