//! The desktop app's private local workspace: a real `metro-server`, shipped
//! next to the launcher, started on demand on IPv4 loopback with its own
//! database, keys and one local account, and stopped when the app exits.
//!
//! The launcher builds one [`LocalServer`] and hands it to the bridge as a
//! [`LocalWorkspaceHost`] (see `crate::desktop::local`).
//!
//! # Data (`data_root`, private: mode 700, Windows protected DACL)
//!
//! ```text
//! local.lock               OS file lock: one launcher per data root
//! state.json               {"version":1,"port":P}: the stable local origin
//! db/metro.sqlite3         the local server's database
//! keys/provider-master.key provider master key (base64, mode 600), made once
//! run/dev-users.json       bootstrap: {"local":"<token>"}, deleted once ready
//! run/ready.json           written by the server after it has bound
//! run/server.log           the server's output, truncated at every start
//! ```
//!
//! # Rules
//!
//! - **Stable origin.** The first start binds `127.0.0.1:0` and the port the
//!   server got is saved. Every later start binds exactly that port, so the
//!   server URL (and anything bound to it, such as paired device
//!   connections) stays the same. A port taken by another program is a
//!   clear error; the origin never moves silently.
//! - **One account.** The server runs in its own dev identity namespace with
//!   the single stable account [`LOCAL_USER`], who is its organization admin
//!   (so managed users and their tokens work through the same server). The
//!   bearer is fresh at every server start, held only in memory here, and
//!   handed to the server through a mode-600 file deleted once it is ready.
//!   It is never in argv, the environment or a log.
//! - **Verified before any bearer is sent.** The server writes the ready
//!   file into the private `run/` directory after binding; it counts only if
//!   its pid is our child's. Then `GET /v1/me` must answer [`LOCAL_USER`].
//! - **No strays.** The child's environment is rebuilt from an allowlist.
//!   Its stdin is a pipe it watches (`METRO_EXIT_ON_STDIN_EOF`), so it shuts
//!   down gracefully when the launcher exits or crashes. Linux adds
//!   `PR_SET_PDEATHSIG`, Windows a kill-on-close Job Object. [`shutdown`]
//!   closes stdin, waits, then kills.
//!
//! [`shutdown`]: LocalWorkspaceHost::shutdown

use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rand::RngCore;
use serde::{Deserialize, Serialize};

use crate::desktop::local::{BoxFut, LocalState, LocalWorkspace, LocalWorkspaceHost};
use crate::fsutil::{ensure_private_dir, read_private, remove_private, write_private};
use crate::{SafeError, Secret};

/// The one account of every local workspace.
pub const LOCAL_USER: &str = "local";
/// How long a start may take (the first one creates the database).
const START_TIMEOUT: Duration = Duration::from_secs(45);
/// How long a graceful stop may take before the server is killed.
const STOP_GRACE: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(50);

/// Variables the local server inherits; everything else is dropped, so a
/// developer's `METRO_*`, OIDC or model settings never reach it.
const INHERITED_ENV: &[&str] = &[
    "PATH",
    "HOME",
    "LANG",
    "LC_ALL",
    "TZ",
    "TMPDIR",
    "TEMP",
    "TMP",
    "SystemRoot",
    "SYSTEMROOT",
    "windir",
    "USERPROFILE",
    "LOCALAPPDATA",
    "APPDATA",
    "ProgramData",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    // Outbound model calls may need the user's proxy; loopback is added to
    // NO_PROXY below.
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "http_proxy",
    "https_proxy",
];

/// The process manager behind the desktop's "Use locally".
pub struct LocalServer {
    shared: Arc<Shared>,
}

struct Shared {
    binary: PathBuf,
    root: PathBuf,
    state: Mutex<LocalState>,
    /// Serializes open and shutdown; holds the running server.
    running: tokio::sync::Mutex<Option<Running>>,
    /// Set by shutdown so a start in progress gives up at once.
    closing: AtomicBool,
    /// The running server's URL and bearer, for in-process readers; `None`
    /// unless it runs (cleared on stop and when it dies).
    current: Mutex<Option<(String, Secret)>>,
}

struct Running {
    url: String,
    token: Secret,
    child: Arc<ChildProc>,
    /// Held while the server runs: one launcher per data root.
    _lock: fs::File,
}

#[derive(Default, Serialize, Deserialize)]
struct SavedState {
    version: u32,
    port: Option<u16>,
}

#[derive(Deserialize)]
struct Ready {
    pid: u32,
    addr: String,
}

impl LocalServer {
    /// `server_binary`: the absolute path of the bundled `metro-server[.exe]`.
    /// `data_root`: the private directory for this OS user's local workspace.
    pub fn new(server_binary: PathBuf, data_root: PathBuf) -> Self {
        LocalServer {
            shared: Arc::new(Shared {
                binary: server_binary,
                root: data_root,
                state: Mutex::new(LocalState::Stopped),
                running: tokio::sync::Mutex::new(None),
                closing: AtomicBool::new(false),
                current: Mutex::new(None),
            }),
        }
    }

    /// The running local server's URL, `None` unless it runs. In-process
    /// only (for example to tell whether a session is this hosted server).
    pub fn service_url(&self) -> Option<String> {
        self.current().map(|w| w.server_url)
    }

    /// The running local server's bearer, `None` unless it runs. In-process
    /// only: never log, persist or send it anywhere but that server.
    pub fn token(&self) -> Option<Secret> {
        self.current().map(|w| w.token)
    }

    /// Both together (one consistent snapshot).
    pub fn current(&self) -> Option<LocalWorkspace> {
        let cur = self
            .shared
            .current
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        cur.as_ref().map(|(url, token)| LocalWorkspace {
            server_url: url.clone(),
            token: token.clone(),
        })
    }

    async fn open_inner(&self) -> Result<LocalWorkspace, SafeError> {
        let mut slot = self.shared.running.lock().await;
        if let Some(running) = slot.as_ref() {
            if running.child.alive() {
                return Ok(LocalWorkspace {
                    server_url: running.url.clone(),
                    token: running.token.clone(),
                });
            }
            // It stopped on its own (crash, killed): start a fresh one.
            self.shared.set_current(None);
            if let Some(old) = slot.take() {
                let child = old.child.clone();
                drop(old);
                let _ = tokio::task::spawn_blocking(move || child.stop(Duration::ZERO)).await;
            }
        }
        self.shared.set_state(LocalState::Starting);
        match self.start().await {
            Ok(running) => {
                let out = LocalWorkspace {
                    server_url: running.url.clone(),
                    token: running.token.clone(),
                };
                self.shared
                    .set_current(Some((running.url.clone(), running.token.clone())));
                *slot = Some(running);
                self.shared.set_state(LocalState::Running);
                Ok(out)
            }
            Err(e) => {
                self.shared.set_state(LocalState::Failed);
                Err(e)
            }
        }
    }

    async fn shutdown_inner(&self) {
        self.shared.closing.store(true, Ordering::SeqCst);
        let mut slot = self.shared.running.lock().await;
        self.shared.set_current(None);
        if let Some(running) = slot.take() {
            let child = running.child.clone();
            drop(running); // token wiped, lock released after the stop below
            let _ = tokio::task::spawn_blocking(move || child.stop(STOP_GRACE)).await;
        }
        self.shared.set_state(LocalState::Stopped);
        self.shared.closing.store(false, Ordering::SeqCst);
    }

    async fn start(&self) -> Result<Running, SafeError> {
        let root = &self.shared.root;
        check_binary(&self.shared.binary)?;
        let dirs = Dirs::new(root);
        for dir in [root, &dirs.db, &dirs.keys, &dirs.run] {
            ensure_private_dir(dir).map_err(|e| fs_error("prepare its folder", e))?;
        }
        let lock = take_lock(&root.join("local.lock"))?;
        let saved = load_state(&root.join("state.json"))?;
        if let Some(port) = saved.port {
            check_port_free(port)?;
        }
        ensure_master_key(&dirs.master_key)?;

        let token = new_token();
        let bootstrap = dirs.run.join("dev-users.json");
        let users = serde_json::json!({ LOCAL_USER: token.expose() }).to_string();
        write_private(&bootstrap, users.as_bytes()).map_err(|e| fs_error("prepare sign-in", e))?;
        let ready_path = dirs.run.join("ready.json");
        let _ = fs::remove_file(&ready_path);
        let log_path = dirs.run.join("server.log");
        let log = open_log(&log_path)?;

        let bind = SocketAddrV4::new(Ipv4Addr::LOCALHOST, saved.port.unwrap_or(0));
        let mut cmd = Command::new(&self.shared.binary);
        cmd.current_dir(&dirs.run)
            .env_clear()
            .envs(
                INHERITED_ENV
                    .iter()
                    .filter_map(|k| std::env::var_os(k).map(|v| (*k, v))),
            )
            .env("NO_PROXY", no_proxy("NO_PROXY"))
            .env("no_proxy", no_proxy("no_proxy"))
            .env("NO_COLOR", "1")
            .env("RUST_LOG", "info")
            .env("METRO_BIND", bind.to_string())
            .env("METRO_DB", dirs.db.join("metro.sqlite3"))
            .env("METRO_DEV_USERS_FILE", &bootstrap)
            .env("METRO_ORG_ADMINS", format!("[\"{LOCAL_USER}\"]"))
            .env("METRO_PROVIDER_KEY_FILE", &dirs.master_key)
            .env("METRO_READY_FILE", &ready_path)
            .env("METRO_EXIT_ON_STDIN_EOF", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::from(
                log.try_clone()
                    .map_err(|e| fs_error("open its log", e.into()))?,
            ))
            .stderr(Stdio::from(log));
        platform::configure(&mut cmd);

        let child = ChildProc::spawn(cmd).map_err(|e| {
            let _ = remove_private(&bootstrap);
            SafeError::new(format!("Couldn't start the local Metro server: {e}"))
        })?;

        let started = self
            .await_ready(&child, &ready_path, &log_path, saved.port)
            .await;
        // The server has read its bootstrap file (or failed): it goes now.
        let _ = remove_private(&bootstrap);
        let result = match started {
            Ok(addr) => verify(&addr, &token).await.map(|()| addr),
            Err(e) => Err(e),
        };
        let addr = match result {
            Ok(addr) => addr,
            Err(e) => {
                let c = child.clone();
                let _ = tokio::task::spawn_blocking(move || c.stop(STOP_GRACE)).await;
                return Err(e);
            }
        };
        if saved.port != Some(addr.port()) {
            let next = SavedState {
                version: 1,
                port: Some(addr.port()),
            };
            let body = serde_json::to_vec(&next).expect("state serializes");
            if let Err(e) = write_private(&root.join("state.json"), &body) {
                let c = child.clone();
                let _ = tokio::task::spawn_blocking(move || c.stop(STOP_GRACE)).await;
                return Err(fs_error("save its settings", e));
            }
        }
        child.watch(self.shared.clone());
        Ok(Running {
            url: format!("http://{addr}"),
            token,
            child,
            _lock: lock,
        })
    }

    /// Waits for the ready file our child wrote, and returns its address.
    async fn await_ready(
        &self,
        child: &ChildProc,
        ready: &Path,
        log: &Path,
        port: Option<u16>,
    ) -> Result<SocketAddrV4, SafeError> {
        let deadline = Instant::now() + START_TIMEOUT;
        loop {
            if self.shared.closing.load(Ordering::SeqCst) {
                return Err(SafeError::new(
                    "Metro is closing; the local workspace wasn't opened.",
                ));
            }
            if !child.alive() {
                // Another program took the saved port between the check and
                // the server's own bind: say so, as the check would have.
                if let Some(p) = port
                    && let Err(e) = check_port_free(p)
                {
                    return Err(e);
                }
                return Err(SafeError::new(format!(
                    "The local Metro server stopped while starting: {}",
                    log_error(log)
                )));
            }
            if let Ok(Some(bytes)) = read_private(ready, 4096)
                && let Ok(r) = serde_json::from_slice::<Ready>(&bytes)
            {
                if r.pid != child.pid {
                    return Err(SafeError::new(
                        "The local Metro server couldn't be verified (another process answered).",
                    ));
                }
                return match r.addr.parse::<SocketAddr>() {
                    Ok(SocketAddr::V4(a))
                        if *a.ip() == Ipv4Addr::LOCALHOST
                            && a.port() != 0
                            && port.is_none_or(|p| p == a.port()) =>
                    {
                        Ok(a)
                    }
                    _ => Err(SafeError::new(
                        "The local Metro server reported an unexpected address.",
                    )),
                };
            }
            if Instant::now() >= deadline {
                return Err(SafeError::new(format!(
                    "The local Metro server didn't start within {} seconds. {}",
                    START_TIMEOUT.as_secs(),
                    log_error(log)
                )));
            }
            tokio::time::sleep(POLL).await;
        }
    }
}

impl LocalWorkspaceHost for LocalServer {
    fn open(&self) -> BoxFut<'_, Result<LocalWorkspace, SafeError>> {
        Box::pin(self.open_inner())
    }

    fn state(&self) -> LocalState {
        *self.shared.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn shutdown(&self) -> BoxFut<'_, ()> {
        Box::pin(self.shutdown_inner())
    }
}

impl Drop for LocalServer {
    /// Last resort if the launcher never called `shutdown`: stop at once.
    fn drop(&mut self) {
        self.shared.set_current(None);
        if let Ok(mut slot) = self.shared.running.try_lock()
            && let Some(running) = slot.take()
        {
            running.child.stop(Duration::from_secs(2));
        }
    }
}

impl Shared {
    fn set_state(&self, state: LocalState) {
        *self.state.lock().unwrap_or_else(|e| e.into_inner()) = state;
    }

    fn set_current(&self, value: Option<(String, Secret)>) {
        *self.current.lock().unwrap_or_else(|e| e.into_inner()) = value;
    }
}

struct Dirs {
    db: PathBuf,
    keys: PathBuf,
    run: PathBuf,
    master_key: PathBuf,
}

impl Dirs {
    fn new(root: &Path) -> Self {
        let keys = root.join("keys");
        Dirs {
            db: root.join("db"),
            master_key: keys.join("provider-master.key"),
            keys,
            run: root.join("run"),
        }
    }
}

// ---------------------------------------------------------------- the child

/// The server process. The thread that spawned it waits for it (so Linux's
/// parent-death signal stays tied to a thread that lives as long as it).
struct ChildProc {
    pid: u32,
    child: Mutex<Child>,
    stdin: Mutex<Option<ChildStdin>>,
    exited: AtomicBool,
    /// Set when we asked it to stop: its exit is then not a failure.
    stopping: AtomicBool,
    #[cfg(windows)]
    _job: platform::Job,
}

impl ChildProc {
    fn spawn(mut cmd: Command) -> std::io::Result<Arc<Self>> {
        let (tx, rx) = std::sync::mpsc::sync_channel::<std::io::Result<Arc<ChildProc>>>(1);
        std::thread::Builder::new()
            .name("metro-local-server".into())
            .spawn(move || {
                let proc = match cmd.spawn() {
                    Ok(mut child) => {
                        #[cfg(windows)]
                        let job = match platform::Job::assign(&child) {
                            Ok(job) => job,
                            Err(e) => {
                                let _ = child.kill();
                                let _ = child.wait();
                                let _ = tx.send(Err(e));
                                return;
                            }
                        };
                        let stdin = child.stdin.take();
                        Arc::new(ChildProc {
                            pid: child.id(),
                            child: Mutex::new(child),
                            stdin: Mutex::new(stdin),
                            exited: AtomicBool::new(false),
                            stopping: AtomicBool::new(false),
                            #[cfg(windows)]
                            _job: job,
                        })
                    }
                    Err(e) => {
                        let _ = tx.send(Err(e));
                        return;
                    }
                };
                let _ = tx.send(Ok(proc.clone()));
                // Reap it here; `stop` takes the same lock to signal it, and
                // std never signals a child it has already reaped.
                loop {
                    let done = proc
                        .child
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .try_wait()
                        .map(|s| s.is_some())
                        .unwrap_or(true);
                    if done {
                        proc.exited.store(true, Ordering::SeqCst);
                        return;
                    }
                    std::thread::sleep(POLL);
                }
            })?;
        rx.recv()
            .unwrap_or_else(|_| Err(std::io::Error::other("the server thread ended")))
    }

    fn alive(&self) -> bool {
        !self.exited.load(Ordering::SeqCst)
    }

    /// After a successful start: a later exit we didn't ask for is a failure.
    fn watch(self: &Arc<Self>, shared: Arc<Shared>) {
        let me = Arc::downgrade(self);
        std::thread::Builder::new()
            .name("metro-local-watch".into())
            .spawn(move || {
                loop {
                    std::thread::sleep(Duration::from_millis(200));
                    let Some(me) = me.upgrade() else { return };
                    if !me.alive() {
                        if !me.stopping.load(Ordering::SeqCst) {
                            shared.set_current(None);
                            shared.set_state(LocalState::Failed);
                        }
                        return;
                    }
                }
            })
            .ok();
    }

    /// Closes stdin (a graceful stop), waits up to `grace`, then kills.
    /// Blocking; idempotent.
    fn stop(&self, grace: Duration) {
        self.stopping.store(true, Ordering::SeqCst);
        drop(self.stdin.lock().unwrap_or_else(|e| e.into_inner()).take());
        let deadline = Instant::now() + grace;
        while self.alive() && Instant::now() < deadline {
            std::thread::sleep(POLL);
        }
        if self.alive() {
            let _ = self.child.lock().unwrap_or_else(|e| e.into_inner()).kill();
            let deadline = Instant::now() + Duration::from_secs(5);
            while self.alive() && Instant::now() < deadline {
                std::thread::sleep(POLL);
            }
        }
    }
}

// ---------------------------------------------------------------- steps

fn check_binary(path: &Path) -> Result<(), SafeError> {
    match fs::metadata(path) {
        Ok(m) if m.is_file() => Ok(()),
        _ => Err(SafeError::new(
            "This Metro install has no local server (metro-server is missing next to Metro). Reinstall Metro to use it locally.",
        )),
    }
}

fn take_lock(path: &Path) -> Result<fs::File, SafeError> {
    let mut opts = fs::OpenOptions::new();
    opts.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = opts
        .open(path)
        .map_err(|e| fs_error("lock its folder", e.into()))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(fs::TryLockError::WouldBlock) => Err(SafeError::new(
            "Your local workspace is already open in another Metro window. Use that window, or close it first.",
        )),
        Err(fs::TryLockError::Error(e)) => Err(fs_error("lock its folder", e.into())),
    }
}

fn load_state(path: &Path) -> Result<SavedState, SafeError> {
    match read_private(path, 4096) {
        Ok(None) => Ok(SavedState::default()),
        Ok(Some(bytes)) => match serde_json::from_slice::<SavedState>(&bytes) {
            Ok(s) if s.version == 1 && s.port != Some(0) => Ok(s),
            _ => Err(SafeError::new(format!(
                "The local workspace settings file {} is damaged.",
                path.display()
            ))),
        },
        Err(e) => Err(fs_error("read its settings", e)),
    }
}

/// Fails clearly when another program holds the saved port.
fn check_port_free(port: u16) -> Result<(), SafeError> {
    match TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)) {
        Ok(listener) => {
            drop(listener);
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => Err(SafeError::new(format!(
            "Your local workspace uses port {port} on this computer, and another program is using it now. Close that program, then try again."
        ))),
        Err(e) => Err(SafeError::new(format!(
            "Couldn't use port {port} for the local workspace: {e}"
        ))),
    }
}

fn ensure_master_key(path: &Path) -> Result<(), SafeError> {
    match read_private(path, 1024) {
        Ok(Some(_)) => Ok(()),
        Ok(None) => {
            let mut key = [0u8; 32];
            rand::rng().fill_bytes(&mut key);
            use base64::Engine;
            let mut text = base64::engine::general_purpose::STANDARD.encode(key);
            zeroize::Zeroize::zeroize(&mut key);
            let result = write_private(path, text.as_bytes());
            zeroize::Zeroize::zeroize(&mut text);
            result.map_err(|e| fs_error("create its key", e))
        }
        Err(e) => Err(fs_error("read its key", e)),
    }
}

/// 256 random bits as lowercase hex.
fn new_token() -> Secret {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    let mut out = String::with_capacity(64);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    zeroize::Zeroize::zeroize(&mut bytes);
    Secret::new(out)
}

fn open_log(path: &Path) -> Result<fs::File, SafeError> {
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    opts.open(path)
        .map_err(|e| fs_error("open its log", e.into()))
}

fn no_proxy(name: &str) -> String {
    match std::env::var(name) {
        Ok(v) if !v.trim().is_empty() => format!("{v},127.0.0.1,localhost"),
        _ => "127.0.0.1,localhost".into(),
    }
}

/// `GET /v1/me` with the new bearer must answer the local account. Called
/// only once the ready file proved our child holds the port.
async fn verify(addr: &SocketAddrV4, token: &Secret) -> Result<(), SafeError> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .user_agent(crate::USER_AGENT)
        .build()
        .map_err(|_| SafeError::new("Couldn't check the local Metro server."))?;
    let resp = client
        .get(format!("http://{addr}/v1/me"))
        .bearer_auth(token.expose())
        .send()
        .await
        .map_err(|_| SafeError::new("The local Metro server started but didn't answer."))?;
    if !resp.status().is_success() {
        return Err(SafeError::new(format!(
            "The local Metro server refused its own sign-in (HTTP {}).",
            resp.status().as_u16()
        )));
    }
    let me = crate::http::read_json(resp, 64 * 1024)
        .await
        .map_err(|_| SafeError::new("The local Metro server gave an unexpected answer."))?;
    if me.get("id").and_then(|v| v.as_str()) != Some(LOCAL_USER) {
        return Err(SafeError::new(
            "The local Metro server signed in someone other than the local account.",
        ));
    }
    Ok(())
}

/// The server's own error from the end of its log (it never logs tokens),
/// as one short line.
fn log_error(path: &Path) -> String {
    let tail = (|| -> std::io::Result<String> {
        let mut f = fs::File::open(path)?;
        let len = f.metadata()?.len();
        f.seek(SeekFrom::Start(len.saturating_sub(4096)))?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf)?;
        Ok(String::from_utf8_lossy(&buf).into_owned())
    })()
    .unwrap_or_default();
    summarize_log(&tail)
}

fn summarize_log(tail: &str) -> String {
    let lines: Vec<&str> = tail
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    // anyhow's `Error: …` report, with its causes, is what main returns.
    let text = match lines.iter().rposition(|l| l.starts_with("Error:")) {
        Some(i) => lines[i..]
            .iter()
            .map(|l| l.trim_start_matches("Error:").trim())
            .filter(|l| *l != "Caused by:")
            .map(|l| {
                l.trim_start_matches(|c: char| c.is_ascii_digit() || c == ':')
                    .trim()
            })
            .collect::<Vec<_>>()
            .join(": "),
        None => lines.last().copied().unwrap_or("no details").to_string(),
    };
    let text = if text.is_empty() {
        "no details".into()
    } else {
        text
    };
    crate::secret::clean_line(&redact_log(&text), 160)
}

/// Shared token shapes, plus managed `metro_ut_` tokens and any long hex run
/// (the local bearer is bare hex).
fn redact_log(text: &str) -> String {
    let text = crate::secret::redact(&text.replace("metro_ut_", "metro_pc_"), &[]);
    let text = text.replace("metro_pc_", "metro_ut_");
    let mut out = String::with_capacity(text.len());
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut String| {
        if run.len() >= 32 {
            out.push_str("<redacted>");
        } else {
            out.push_str(run);
        }
        run.clear();
    };
    for c in text.chars() {
        if c.is_ascii_hexdigit() {
            run.push(c);
        } else {
            flush(&mut run, &mut out);
            out.push(c);
        }
    }
    flush(&mut run, &mut out);
    out
}

fn fs_error(what: &str, e: anyhow::Error) -> SafeError {
    let detail = e.chain().last().map(|c| c.to_string()).unwrap_or_default();
    SafeError::new(format!("The local workspace couldn't {what}: {detail}"))
}

// ---------------------------------------------------------------- platform

#[cfg(unix)]
mod platform {
    use std::process::Command;

    pub fn configure(cmd: &mut Command) {
        use std::os::unix::process::CommandExt;
        // Its own process group: a terminal's Ctrl+C doesn't reach it; we stop it.
        cmd.process_group(0);
        #[cfg(target_os = "linux")]
        // SAFETY: only async-signal-safe calls between fork and exec.
        unsafe {
            cmd.pre_exec(|| {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                // The launcher may already have died between fork and prctl.
                if libc::getppid() == 1 {
                    return Err(std::io::Error::other("launcher exited"));
                }
                Ok(())
            });
        }
    }
}

#[cfg(windows)]
mod platform {
    use std::os::windows::io::AsRawHandle;
    use std::os::windows::process::CommandExt;
    use std::process::{Child, Command};

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    };

    /// No console window for the server (the launcher is a GUI app).
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    pub fn configure(cmd: &mut Command) {
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    /// A Job Object that kills the server when the launcher's handle to it
    /// closes (the launcher exits or crashes).
    pub struct Job(HANDLE);

    // SAFETY: a job handle may be used and closed from any thread.
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}

    impl Job {
        pub fn assign(child: &Child) -> std::io::Result<Job> {
            // SAFETY: plain Win32 calls with valid arguments; the handle is
            // owned by the returned `Job` and closed on drop.
            unsafe {
                let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if job.is_null() {
                    return Err(std::io::Error::last_os_error());
                }
                let job = Job(job);
                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                if SetInformationJobObject(
                    job.0,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const _,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                ) == 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                if AssignProcessToJobObject(job.0, child.as_raw_handle() as HANDLE) == 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(job)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_summary_takes_the_error_report() {
        let log = "2026 INFO starting\nError: binding 127.0.0.1:5\n\nCaused by:\n    Address already in use (os error 98)\n";
        assert_eq!(
            summarize_log(log),
            "binding 127.0.0.1:5: Address already in use (os error 98)"
        );
        assert_eq!(summarize_log(""), "no details");
        assert_eq!(summarize_log("a\nlast line\n"), "last line");
        assert!(
            !summarize_log("Error: bad metro_ut_0123456789abcdef").contains("0123456789abcdef")
        );
        let hex = "ab".repeat(32);
        assert_eq!(
            summarize_log(&format!("Error: token {hex} refused")),
            "token <redacted> refused"
        );
    }

    #[test]
    fn tokens_are_fresh_hex() {
        let a = new_token();
        let b = new_token();
        assert_eq!(a.expose().len(), 64);
        assert!(
            a.expose()
                .bytes()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
        assert_ne!(a.expose(), b.expose());
    }

    #[test]
    fn saved_port_in_use_is_a_clear_error() {
        // Held for the whole assertion: nothing else can take it meanwhile.
        let taken = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = taken.local_addr().unwrap().port();
        let err = check_port_free(port).unwrap_err();
        assert!(err.as_str().contains(&port.to_string()), "{err}");
        assert!(err.as_str().contains("another program"), "{err}");
        drop(taken);
    }
}
