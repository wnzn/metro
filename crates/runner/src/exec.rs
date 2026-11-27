//! Subprocess execution for operator-defined argv, on the host or inside an
//! opt-in docker sandbox.
//!
//! - argv is executed directly (`execve`), never through a shell.
//! - The environment is cleared; only a small non-secret allowlist is passed.
//! - Host children run in their own process group with `PR_SET_PDEATHSIG`,
//!   and timeouts kill the whole group.
//! - Docker children are `docker run --rm` with a unique container name; on
//!   timeout/abort the container is force-removed (`docker rm -f`) before the
//!   client process group is killed. There is never a fallback to the host.

use std::collections::BTreeMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::config::{DockerSandbox, Sandbox};

/// Non-secret variables passed to host-mode tool processes.
pub const HOST_ENV_ALLOWLIST: &[&str] = &[
    "PATH", "HOME", "USER", "LOGNAME", "LANG", "LC_ALL", "LC_CTYPE", "TZ", "TMPDIR",
];

/// Variables passed to the *docker CLI* (runner side, never into the
/// container): needed to reach the daemon the operator configured.
pub const DOCKER_CLI_ENV_ALLOWLIST: &[&str] = &[
    "PATH",
    "HOME",
    "DOCKER_HOST",
    "DOCKER_CONTEXT",
    "DOCKER_CONFIG",
    "DOCKER_CERT_PATH",
    "DOCKER_TLS_VERIFY",
    "XDG_RUNTIME_DIR",
];

pub const CONTAINER_WORKDIR: &str = "/workspace";
pub const MAX_CAPTURE_BYTES: usize = 256 * 1024;
const DOCKER_RM_TIMEOUT: Duration = Duration::from_secs(30);

static CONTAINER_SEQ: AtomicU64 = AtomicU64::new(0);

/// Variables a `workspace.shell` command keeps from the runner's own
/// environment. `TERM=dumb` and `METRO_SPACE_ROOT` are added.
pub const SHELL_ENV_ALLOWLIST: &[&str] = &["PATH", "HOME", "LANG"];
/// Bound on each of a shell command's stdout and stderr.
pub const SHELL_CAPTURE_BYTES: usize = 64 * 1024;

#[derive(Debug)]
pub enum ExecError {
    Spawn(String),
    TimedOut,
    Shutdown,
    Cancelled,
    Io(io::Error),
}

impl std::fmt::Display for ExecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExecError::Spawn(m) => write!(f, "failed to start process: {m}"),
            ExecError::TimedOut => f.write_str("process timed out and was killed"),
            ExecError::Shutdown => f.write_str("runner is shutting down; process was killed"),
            ExecError::Cancelled => f.write_str(
                "the server cancelled the job (run stopped or runner changed); process was killed",
            ),
            ExecError::Io(e) => write!(f, "I/O error: {e}"),
        }
    }
}

impl std::error::Error for ExecError {}

/// A command ready to spawn, plus what's needed to tear it down.
#[derive(Debug)]
pub struct Prepared {
    pub command: Command,
    pub container: Option<ContainerRef>,
}

#[derive(Debug, Clone)]
pub struct ContainerRef {
    pub docker_bin: String,
    pub name: String,
}

fn filtered_env(allow: &[&str]) -> BTreeMap<String, String> {
    allow
        .iter()
        .filter_map(|k| std::env::var(k).ok().map(|v| (k.to_string(), v)))
        .collect()
}

/// Build the command for `argv`. `interactive` keeps stdin open (MCP).
pub fn prepare(
    argv: &[String],
    workspace: &Path,
    sandbox: &Sandbox,
    interactive: bool,
) -> Result<Prepared, ExecError> {
    let Some((exe, args)) = argv.split_first() else {
        return Err(ExecError::Spawn("empty argv".into()));
    };
    match sandbox {
        Sandbox::None => {
            let mut c = Command::new(exe);
            c.args(args)
                .current_dir(workspace)
                .env_clear()
                .envs(filtered_env(HOST_ENV_ALLOWLIST));
            Ok(Prepared {
                command: c,
                container: None,
            })
        }
        Sandbox::Docker(d) => {
            let name = container_name();
            let mut c = Command::new(&d.docker_bin);
            c.env_clear().envs(filtered_env(DOCKER_CLI_ENV_ALLOWLIST));
            // The docker CLI's cwd is irrelevant to the container; use "/".
            c.current_dir("/");
            c.args(docker_run_args(d, &name, workspace, exe, args, interactive));
            Ok(Prepared {
                command: c,
                container: Some(ContainerRef {
                    docker_bin: d.docker_bin.clone(),
                    name,
                }),
            })
        }
    }
}

/// `/bin/sh -c command` for `workspace.shell`, in `workspace`. The only
/// place the runner uses a shell. On the host it runs as the runner user
/// with a minimal environment; with docker it runs in the locked-down
/// container with only `workspace` mounted, read-write.
pub fn prepare_shell(
    command: &str,
    workspace: &Path,
    sandbox: &Sandbox,
) -> Result<Prepared, ExecError> {
    let args = ["-c".to_owned(), command.to_owned()];
    match sandbox {
        Sandbox::None => {
            let mut c = Command::new("/bin/sh");
            c.args(args)
                .current_dir(workspace)
                .env_clear()
                .envs(filtered_env(SHELL_ENV_ALLOWLIST))
                .env("TERM", "dumb")
                .env("METRO_SPACE_ROOT", workspace);
            Ok(Prepared {
                command: c,
                container: None,
            })
        }
        Sandbox::Docker(d) => {
            let name = container_name();
            let mut c = Command::new(&d.docker_bin);
            c.env_clear().envs(filtered_env(DOCKER_CLI_ENV_ALLOWLIST));
            c.current_dir("/");
            c.args(docker_args(
                d,
                &name,
                workspace,
                "/bin/sh",
                &args,
                Mount {
                    interactive: false,
                    writable: true,
                    env: &["TERM=dumb", "METRO_SPACE_ROOT=/workspace"],
                },
            ));
            Ok(Prepared {
                command: c,
                container: Some(ContainerRef {
                    docker_bin: d.docker_bin.clone(),
                    name,
                }),
            })
        }
    }
}

fn container_name() -> String {
    format!(
        "metro-runner-{}-{}-{:08x}",
        std::process::id(),
        CONTAINER_SEQ.fetch_add(1, Ordering::Relaxed),
        crate::util::random_u64() as u32
    )
}

/// How a sandbox container gets its workspace and environment.
pub struct Mount<'a> {
    pub interactive: bool,
    /// Only `workspace.shell` mounts the Space root read-write.
    pub writable: bool,
    pub env: &'a [&'a str],
}

/// The exact `docker run` argument list. Public for tests/docs.
pub fn docker_run_args(
    d: &DockerSandbox,
    name: &str,
    workspace: &Path,
    exe: &str,
    args: &[String],
    interactive: bool,
) -> Vec<String> {
    docker_args(
        d,
        name,
        workspace,
        exe,
        args,
        Mount {
            interactive,
            writable: false,
            env: &[],
        },
    )
}

pub fn docker_args(
    d: &DockerSandbox,
    name: &str,
    workspace: &Path,
    exe: &str,
    args: &[String],
    mount: Mount<'_>,
) -> Vec<String> {
    let interactive = mount.interactive;
    let mut v: Vec<String> = vec!["run".into(), "--rm".into()];
    if interactive {
        v.push("-i".into());
    }
    v.extend(
        [
            "--name",
            name,
            "--label",
            "dev.metro.runner=1",
            "--pull=never",
            "--network=none",
            "--read-only",
            "--cap-drop=ALL",
            "--security-opt=no-new-privileges",
            "--ipc=none",
            "--init",
            "--pids-limit",
            &d.pids.to_string(),
            "--memory",
            &d.memory,
            "--memory-swap",
            &d.memory,
            "--cpus",
            &d.cpus,
            "--user",
            &d.user,
            "--tmpfs",
            &format!("/tmp:rw,nosuid,nodev,size={}", d.tmpfs_size),
            "--mount",
            &format!(
                "type=bind,source={},target={CONTAINER_WORKDIR}{}",
                workspace.display(),
                if mount.writable { "" } else { ",readonly" }
            ),
            "--workdir",
            CONTAINER_WORKDIR,
            "--env",
            "HOME=/tmp",
            "--env",
            "TMPDIR=/tmp",
        ]
        .iter()
        .map(|s| s.to_string()),
    );
    for e in mount.env {
        v.push("--env".into());
        v.push((*e).to_owned());
    }
    v.extend(["--entrypoint".into(), exe.to_owned(), d.image.clone()]);
    v.extend(args.iter().cloned());
    v
}

/// Owns a spawned child; kills it (and its container) on drop unless reaped.
pub struct ChildGuard {
    child: Child,
    container: Option<ContainerRef>,
    reaped: bool,
}

impl ChildGuard {
    pub fn spawn(mut p: Prepared, stdin: Stdio) -> Result<Self, ExecError> {
        p.command
            .stdin(stdin)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        // SAFETY: prctl is async-signal-safe; no allocation in the closure.
        unsafe {
            p.command.pre_exec(|| {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = p
            .command
            .spawn()
            .map_err(|e| ExecError::Spawn(e.to_string()))?;
        Ok(ChildGuard {
            child,
            container: p.container,
            reaped: false,
        })
    }

    pub fn child_mut(&mut self) -> &mut Child {
        &mut self.child
    }

    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        let r = self.child.try_wait()?;
        if r.is_some() {
            self.reaped = true;
        }
        Ok(r)
    }

    /// Wait until `deadline`, polling the shutdown flag.
    pub fn wait_until(&mut self, deadline: Instant) -> Result<ExitStatus, ExecError> {
        loop {
            if let Some(s) = self.try_wait().map_err(ExecError::Io)? {
                return Ok(s);
            }
            if crate::shutdown_requested() {
                self.terminate();
                return Err(ExecError::Shutdown);
            }
            if crate::job_cancelled() {
                self.terminate();
                return Err(ExecError::Cancelled);
            }
            if Instant::now() >= deadline {
                self.terminate();
                return Err(ExecError::TimedOut);
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    /// Force-stop: remove the container (if any), then SIGKILL the process
    /// group and reap.
    pub fn terminate(&mut self) {
        if self.reaped {
            return;
        }
        if let Some(c) = &self.container {
            force_remove_container(c);
        }
        let pid = self.child.id() as libc::pid_t;
        // SAFETY: signalling our own child's process group.
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.reaped = true;
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        self.terminate();
    }
}

/// `docker rm -f <name>`, bounded in time. Best effort; logs on failure.
pub fn force_remove_container(c: &ContainerRef) {
    let mut cmd = Command::new(&c.docker_bin);
    cmd.env_clear()
        .envs(filtered_env(DOCKER_CLI_ENV_ALLOWLIST))
        .args(["rm", "-f", &c.name])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    match cmd.spawn() {
        Ok(mut ch) => {
            let deadline = Instant::now() + DOCKER_RM_TIMEOUT;
            loop {
                match ch.try_wait() {
                    Ok(Some(_)) => return,
                    Ok(None) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(50))
                    }
                    _ => {
                        let _ = ch.kill();
                        let _ = ch.wait();
                        eprintln!(
                            "metro-runner: warning: docker rm -f {} did not finish in time",
                            c.name
                        );
                        return;
                    }
                }
            }
        }
        Err(e) => eprintln!(
            "metro-runner: warning: could not run docker rm -f {}: {e}",
            c.name
        ),
    }
}

/// Reads a stream to EOF, keeping at most `cap` bytes; reports the total.
pub fn spawn_capture<R: Read + Send + 'static>(mut r: R, cap: usize) -> Receiver<(Vec<u8>, u64)> {
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut kept = Vec::new();
        let mut total: u64 = 0;
        let mut buf = [0u8; 8192];
        loop {
            match r.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    total += n as u64;
                    let room = cap.saturating_sub(kept.len());
                    kept.extend_from_slice(&buf[..n.min(room)]);
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        let _ = tx.send((kept, total));
    });
    rx
}

fn collect(rx: &Receiver<(Vec<u8>, u64)>, wait: Duration) -> (Vec<u8>, u64) {
    // A daemonized grandchild could hold the pipe open; don't wait forever.
    rx.recv_timeout(wait).unwrap_or_default()
}

#[derive(Debug)]
pub struct Completed {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stdout_total: u64,
    pub stderr: Vec<u8>,
    pub stderr_total: u64,
}

/// What a command printed, kept even when it was killed.
#[derive(Debug, Default)]
pub struct Captured {
    pub stdout: Vec<u8>,
    pub stdout_total: u64,
    pub stderr: Vec<u8>,
    pub stderr_total: u64,
}

/// Run a non-interactive command to completion with a timeout.
pub fn run_bounded(p: Prepared, timeout: Duration) -> Result<Completed, ExecError> {
    let (status, c) = run_capped(p, timeout, MAX_CAPTURE_BYTES)?;
    Ok(Completed {
        status: status?,
        stdout: c.stdout,
        stdout_total: c.stdout_total,
        stderr: c.stderr,
        stderr_total: c.stderr_total,
    })
}

/// Like [`run_bounded`], keeping at most `cap` bytes of each stream, and
/// the output so far when the wait fails (timeout, cancel, shutdown). The
/// outer error is a failure to start.
pub fn run_capped(
    p: Prepared,
    timeout: Duration,
    cap: usize,
) -> Result<(Result<ExitStatus, ExecError>, Captured), ExecError> {
    let mut g = ChildGuard::spawn(p, Stdio::null())?;
    let out = spawn_capture(g.child_mut().stdout.take().expect("piped"), cap);
    let err = spawn_capture(g.child_mut().stderr.take().expect("piped"), cap);
    let status = g.wait_until(Instant::now() + timeout);
    let (stdout, stdout_total) = collect(&out, Duration::from_secs(2));
    let (stderr, stderr_total) = collect(&err, Duration::from_secs(2));
    Ok((
        status,
        Captured {
            stdout,
            stdout_total,
            stderr,
            stderr_total,
        },
    ))
}

/// Line-oriented interactive process (used for stdio MCP).
pub struct Interactive {
    pub guard: ChildGuard,
    writer: Option<SyncSender<Vec<u8>>>,
    writer_handle: Option<JoinHandle<()>>,
    lines: Receiver<io::Result<String>>,
    stderr: Receiver<(Vec<u8>, u64)>,
}

pub enum LineRecv {
    Line(String),
    Eof,
    TimedOut,
    Shutdown,
    Err(String),
}

impl Interactive {
    pub fn spawn(p: Prepared, max_line: usize) -> Result<Self, ExecError> {
        let mut guard = ChildGuard::spawn(p, Stdio::piped())?;
        let stdin = guard.child_mut().stdin.take().expect("piped");
        let stdout = guard.child_mut().stdout.take().expect("piped");
        let stderr = spawn_capture(guard.child_mut().stderr.take().expect("piped"), 64 * 1024);

        // Writes happen on their own thread so a peer that stops reading can
        // never block the runner past its deadline.
        let (wtx, wrx) = mpsc::sync_channel::<Vec<u8>>(8);
        let writer_handle = std::thread::spawn(move || writer_loop(stdin, wrx));

        // Bounded channel gives backpressure against a flooding peer.
        let (ltx, lrx) = mpsc::sync_channel(16);
        std::thread::spawn(move || reader_loop(stdout, ltx, max_line));

        Ok(Interactive {
            guard,
            writer: Some(wtx),
            writer_handle: Some(writer_handle),
            lines: lrx,
            stderr,
        })
    }

    pub fn send_line(&mut self, mut line: Vec<u8>) -> Result<(), String> {
        line.push(b'\n');
        match &self.writer {
            Some(w) => w.send(line).map_err(|_| "peer stdin closed".to_string()),
            None => Err("stdin already closed".into()),
        }
    }

    pub fn recv_line(&self, deadline: Instant) -> LineRecv {
        loop {
            if crate::shutdown_requested() {
                return LineRecv::Shutdown;
            }
            let now = Instant::now();
            if now >= deadline {
                return LineRecv::TimedOut;
            }
            match self
                .lines
                .recv_timeout((deadline - now).min(Duration::from_millis(100)))
            {
                Ok(Ok(l)) => return LineRecv::Line(l),
                Ok(Err(e)) => return LineRecv::Err(e.to_string()),
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => return LineRecv::Eof,
            }
        }
    }

    /// Close stdin and give the process `grace` to exit on its own, then kill.
    pub fn shutdown(mut self, grace: Duration) -> String {
        self.writer.take();
        if let Some(h) = self.writer_handle.take() {
            let _ = h.join();
        }
        let deadline = Instant::now() + grace;
        loop {
            match self.guard.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline && !crate::shutdown_requested() => {
                    std::thread::sleep(Duration::from_millis(25))
                }
                _ => {
                    self.guard.terminate();
                    break;
                }
            }
        }
        self.stderr_tail()
    }

    pub fn stderr_tail(&self) -> String {
        match self.stderr.recv_timeout(Duration::from_millis(500)) {
            Ok((b, _)) => crate::util::lossy(&b),
            Err(_) => String::new(),
        }
    }
}

fn writer_loop(mut stdin: ChildStdin, rx: Receiver<Vec<u8>>) {
    for msg in rx {
        if stdin.write_all(&msg).and_then(|_| stdin.flush()).is_err() {
            return;
        }
    }
    // Dropping stdin closes the pipe (EOF for the peer).
}

fn reader_loop<R: Read>(r: R, tx: SyncSender<io::Result<String>>, max_line: usize) {
    let mut reader = BufReader::new(r);
    loop {
        let mut buf = Vec::new();
        let n = match (&mut reader)
            .take(max_line as u64 + 1)
            .read_until(b'\n', &mut buf)
        {
            Ok(n) => n,
            Err(e) => {
                let _ = tx.send(Err(e));
                return;
            }
        };
        if n == 0 {
            return;
        }
        if buf.len() > max_line && buf.last() != Some(&b'\n') {
            let _ = tx.send(Err(io::Error::other(format!(
                "line exceeds {max_line} bytes"
            ))));
            return;
        }
        while matches!(buf.last(), Some(b'\n' | b'\r')) {
            buf.pop();
        }
        if buf.is_empty() {
            continue;
        }
        let line = match String::from_utf8(buf) {
            Ok(s) => s,
            Err(_) => {
                let _ = tx.send(Err(io::Error::other("non-UTF-8 output")));
                return;
            }
        };
        if tx.send(Ok(line)).is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str) -> Vec<String> {
        // Tests only: the production path never uses a shell. Using one here
        // is the simplest way to create specific child behaviour.
        vec!["/bin/sh".into(), "-c".into(), script.into()]
    }

    #[test]
    fn runs_in_workspace_with_clean_env() {
        let _spawning = crate::test_gate::spawning();
        let ws = tempfile::tempdir().unwrap();
        // SAFETY: test-only; set before spawning, single var.
        unsafe { std::env::set_var("METRO_RUNNER_TOKEN", "leak-me") };
        let p = prepare(&sh("pwd; env"), ws.path(), &Sandbox::None, false).unwrap();
        let c = run_bounded(p, Duration::from_secs(10)).unwrap();
        let out = String::from_utf8(c.stdout).unwrap();
        assert!(c.status.success());
        assert!(
            out.starts_with(&format!("{}\n", ws.path().display())),
            "{out}"
        );
        assert!(
            !out.contains("leak-me"),
            "token leaked into child env: {out}"
        );
        assert!(!out.contains("METRO_"), "{out}");
    }

    #[test]
    fn argv_is_not_shell_interpreted() {
        let _spawning = crate::test_gate::spawning();
        let ws = tempfile::tempdir().unwrap();
        let argv = vec![
            "/bin/echo".to_string(),
            "$(id); rm -rf /".to_string(),
            "`x`".to_string(),
        ];
        let p = prepare(&argv, ws.path(), &Sandbox::None, false).unwrap();
        let c = run_bounded(p, Duration::from_secs(10)).unwrap();
        assert_eq!(
            String::from_utf8(c.stdout).unwrap(),
            "$(id); rm -rf / `x`\n"
        );
    }

    #[test]
    fn timeout_kills_process_group() {
        let _spawning = crate::test_gate::spawning();
        let ws = tempfile::tempdir().unwrap();
        let marker = ws.path().join("pid");
        let script = format!("sleep 30 & echo $! > {}; wait", marker.display());
        let p = prepare(&sh(&script), ws.path(), &Sandbox::None, false).unwrap();
        let t = Instant::now();
        let r = run_bounded(p, Duration::from_millis(500));
        assert!(matches!(r, Err(ExecError::TimedOut)));
        assert!(t.elapsed() < Duration::from_secs(5));
        let pid: i32 = std::fs::read_to_string(&marker)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        std::thread::sleep(Duration::from_millis(200));
        // The background grandchild must be gone too (ESRCH), or a zombie
        // already reparented and reaped by init.
        let alive = unsafe { libc::kill(pid, 0) } == 0;
        let zombie = std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .map(|s| s.contains(") Z "))
            .unwrap_or(true);
        assert!(!alive || zombie, "grandchild {pid} survived timeout");
    }

    #[test]
    fn capture_is_bounded() {
        let _spawning = crate::test_gate::spawning();
        let ws = tempfile::tempdir().unwrap();
        let p = prepare(
            &sh("head -c 1000000 /dev/zero"),
            ws.path(),
            &Sandbox::None,
            false,
        )
        .unwrap();
        let c = run_bounded(p, Duration::from_secs(10)).unwrap();
        assert_eq!(c.stdout.len(), MAX_CAPTURE_BYTES);
        assert_eq!(c.stdout_total, 1_000_000);
    }

    #[test]
    fn missing_executable_is_spawn_error() {
        let _spawning = crate::test_gate::spawning();
        let ws = tempfile::tempdir().unwrap();
        let p = prepare(
            &["/nonexistent/bin".to_string()],
            ws.path(),
            &Sandbox::None,
            false,
        )
        .unwrap();
        assert!(matches!(
            run_bounded(p, Duration::from_secs(5)),
            Err(ExecError::Spawn(_))
        ));
    }

    #[test]
    fn docker_args_are_hardened() {
        let _spawning = crate::test_gate::spawning();
        let d = DockerSandbox {
            docker_bin: "docker".into(),
            image: "img@sha256:0000000000000000000000000000000000000000000000000000000000000000"
                .into(),
            memory: "512m".into(),
            cpus: "1".into(),
            pids: 64,
            user: "65534:65534".into(),
            tmpfs_size: "64m".into(),
        };
        let a = docker_run_args(&d, "n", Path::new("/w"), "cargo", &["check".into()], false);
        let j = a.join(" ");
        for must in [
            "--rm",
            "--pull=never",
            "--network=none",
            "--read-only",
            "--cap-drop=ALL",
            "--security-opt=no-new-privileges",
            "--pids-limit 64",
            "--memory 512m",
            "--memory-swap 512m",
            "--cpus 1",
            "--user 65534:65534",
            "type=bind,source=/w,target=/workspace,readonly",
            "--entrypoint cargo",
        ] {
            assert!(j.contains(must), "missing {must}: {j}");
        }
        assert!(!j.contains("docker.sock"));
        assert!(!j.contains("--privileged"));
        assert!(!a.contains(&"-i".to_string()));
        // Image comes right before the args, after --entrypoint.
        let img_pos = a.iter().position(|s| s == &d.image).unwrap();
        assert_eq!(&a[img_pos + 1..], &["check".to_string()]);
        assert!(
            docker_run_args(&d, "n", Path::new("/w"), "x", &[], true).contains(&"-i".to_string())
        );
    }

    #[test]
    fn shell_container_mounts_the_root_read_write_and_keeps_hardening() {
        let d = DockerSandbox {
            docker_bin: "docker".into(),
            image: "img@sha256:0000000000000000000000000000000000000000000000000000000000000000"
                .into(),
            memory: "512m".into(),
            cpus: "1".into(),
            pids: 64,
            user: "65534:65534".into(),
            tmpfs_size: "64m".into(),
        };
        let p = prepare_shell("echo hi", Path::new("/w"), &Sandbox::Docker(d.clone())).unwrap();
        let a: Vec<String> = p
            .command
            .get_args()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        let j = a.join(" ");
        assert!(j.contains("type=bind,source=/w,target=/workspace "), "{j}");
        assert!(!j.contains("readonly"), "{j}");
        for must in [
            "--network=none",
            "--read-only",
            "--cap-drop=ALL",
            "--user 65534:65534",
            "--env TERM=dumb",
            "--env METRO_SPACE_ROOT=/workspace",
            "--entrypoint /bin/sh",
        ] {
            assert!(j.contains(must), "missing {must}: {j}");
        }
        assert_eq!(
            &a[a.len() - 3..],
            &[d.image.clone(), "-c".into(), "echo hi".into()]
        );
        // Check and MCP stay read-only.
        let ro = docker_run_args(&d, "n", Path::new("/w"), "x", &[], false).join(" ");
        assert!(ro.contains("target=/workspace,readonly"));
    }

    #[test]
    fn docker_missing_binary_fails_closed() {
        let _spawning = crate::test_gate::spawning();
        let ws = tempfile::tempdir().unwrap();
        let d = Sandbox::Docker(DockerSandbox {
            docker_bin: "/nonexistent/docker".into(),
            image: "img@sha256:0000000000000000000000000000000000000000000000000000000000000000"
                .into(),
            memory: "512m".into(),
            cpus: "1".into(),
            pids: 64,
            user: "65534:65534".into(),
            tmpfs_size: "64m".into(),
        });
        let p = prepare(&["/bin/true".to_string()], ws.path(), &d, false).unwrap();
        // Must not silently run /bin/true on the host.
        assert!(matches!(
            run_bounded(p, Duration::from_secs(5)),
            Err(ExecError::Spawn(_))
        ));
    }
}
