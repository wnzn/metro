use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, bail};
use metro_server::{AppState, Config, db::Db, router};
use tokio::sync::watch;
use tracing_subscriber::EnvFilter;

/// A mode-600 file holding the `METRO_DEV_USERS` JSON, instead of the
/// variable itself (used by the desktop app's private local server, so no
/// token is ever in its environment). Exclusive with `METRO_DEV_USERS`.
const DEV_USERS_FILE: &str = "METRO_DEV_USERS_FILE";
/// Where to write `{"pid":…,"addr":"…"}` once the listener is bound, so the
/// process that started this server learns the port it actually got
/// (`METRO_BIND` may use port 0) and that it is this process holding it.
const READY_FILE: &str = "METRO_READY_FILE";
/// `1`: shut down gracefully when stdin reaches end of file, so a parent
/// that exits or crashes never leaves this server running.
const EXIT_ON_STDIN_EOF: &str = "METRO_EXIT_ON_STDIN_EOF";
/// Unix: the number of an inherited, already listening loopback TCP socket to
/// serve on instead of binding `METRO_BIND` (the desktop launcher owns the
/// port for its whole lifetime and hands the same socket to every start).
const LISTEN_FD: &str = "METRO_LISTEN_FD";
/// Windows: `stdin`: the first line of stdin is the hex of a
/// `WSAPROTOCOL_INFOW` the launcher made for this process with
/// `WSADuplicateSocketW`; the socket it names is served instead of binding.
const LISTEN_PROTOCOL_INFO: &str = "METRO_LISTEN_PROTOCOL_INFO";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let config = Config::from_env()?;
    let (auth, dev_users_from_file) = authenticator().await?;
    let db = Db::open(&config.db_path)?;
    tracing::info!(identity_namespace = %auth.namespace(), "authentication configured");
    let bind = config.bind;
    tracing::info!(
        db = %config.db_path.display(),
        model = ?config.model,
        "starting metro-server"
    );
    if config.model.is_none() {
        tracing::warn!("METRO_MODEL is not set: Ask agent is unavailable, Demo still works");
    }
    let mut state = AppState::new(config, db, Arc::new(auth)).await?;
    if let Some(names) = dev_users_from_file {
        // Managed accounts must not take an env user's name; with the file
        // there is no METRO_DEV_USERS for AppState to read them from.
        state = state.with_env_user_ids(names);
    }
    // Scheduled prompts are dispatched by this one task, started once.
    metro_server::schedules::spawn(state.clone());
    // The daily verified release check (release builds, METRO_UPDATE_CHECK on).
    metro_server::updates::spawn(state.clone());

    // Read before the stdin watcher starts: on Windows the inherited
    // socket's description is the first line of stdin.
    let inherited = inherited_listener()?;
    let (stop_tx, stop_rx) = watch::channel(false);
    if flag(EXIT_ON_STDIN_EOF) {
        watch_stdin(stop_tx);
    }

    let inherited_used = inherited.is_some();
    let listener = match inherited {
        Some(listener) => {
            listener
                .set_nonblocking(true)
                .context("preparing the inherited listener")?;
            tokio::net::TcpListener::from_std(listener).context("using the inherited listener")?
        }
        None => tokio::net::TcpListener::bind(bind)
            .await
            .with_context(|| format!("binding {bind}"))?,
    };
    let addr = listener.local_addr().context("reading the bound address")?;
    tracing::info!(addr = %addr, handed_over = inherited_used, "listening");
    if let Some(path) = env_path(READY_FILE) {
        write_ready(&path, &addr.to_string()).with_context(|| format!("writing {READY_FILE}"))?;
    }
    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown(stop_rx))
        .await?;
    Ok(())
}

/// The human authenticator, from the environment or `METRO_DEV_USERS_FILE`.
/// With the file, also the dev user names (never their tokens).
async fn authenticator() -> anyhow::Result<(metro_auth::Authenticator, Option<Vec<String>>)> {
    let configure = |e: metro_auth::ConfigError| anyhow::anyhow!("configuring authentication: {e}");
    let Some(path) = env_path(DEV_USERS_FILE) else {
        return Ok((
            metro_auth::Authenticator::from_env()
                .await
                .map_err(configure)?,
            None,
        ));
    };
    if std::env::var_os(metro_auth::env::DEV_USERS).is_some_and(|v| !v.is_empty()) {
        bail!(
            "set only one of {} and {DEV_USERS_FILE}, not both",
            metro_auth::env::DEV_USERS
        );
    }
    let raw =
        read_private_file(&path, 64 * 1024).with_context(|| format!("reading {DEV_USERS_FILE}"))?;
    let names = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&raw)
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    let auth = metro_auth::Authenticator::from_lookup(|name| {
        if name == metro_auth::env::DEV_USERS {
            return Ok(Some(raw.clone()));
        }
        match std::env::var(name) {
            Ok(v) => Ok(Some(v)),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(std::env::VarError::NotUnicode(_)) => Err(()),
        }
    })
    .await
    .map_err(configure)?;
    Ok((auth, Some(names)))
}

/// The listening socket handed over by the process that started this server,
/// if it handed one over. It must be a listening TCP socket on a loopback
/// address: anything else is refused (never a public fallback).
fn inherited_listener() -> anyhow::Result<Option<std::net::TcpListener>> {
    let listener = if let Some(fd) = std::env::var_os(LISTEN_FD) {
        Some(inherit_fd(fd.to_str().unwrap_or_default())?)
    } else if let Some(how) = std::env::var_os(LISTEN_PROTOCOL_INFO) {
        if how != "stdin" {
            bail!("{LISTEN_PROTOCOL_INFO} must be \"stdin\"");
        }
        Some(inherit_from_stdin()?)
    } else {
        None
    };
    if let Some(l) = &listener {
        let addr = l
            .local_addr()
            .context("the inherited socket has no address")?;
        if !addr.ip().is_loopback() || addr.port() == 0 {
            bail!("the inherited socket is on {addr}, not a loopback address; refusing it");
        }
    }
    Ok(listener)
}

#[cfg(unix)]
fn inherit_fd(raw: &str) -> anyhow::Result<std::net::TcpListener> {
    use std::os::fd::FromRawFd;
    let fd: i32 = raw
        .trim()
        .parse()
        .ok()
        .filter(|fd| *fd > 2)
        .with_context(|| format!("{LISTEN_FD} must be a descriptor number above 2"))?;
    let int_opt = |level, name| -> Option<i32> {
        let mut v: libc::c_int = 0;
        let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
        // SAFETY: getsockopt writes at most `len` bytes into `v`.
        let r = unsafe { libc::getsockopt(fd, level, name, (&raw mut v).cast(), &mut len) };
        (r == 0).then_some(v)
    };
    if int_opt(libc::SOL_SOCKET, libc::SO_TYPE) != Some(libc::SOCK_STREAM) {
        bail!("{LISTEN_FD}={fd} is not an inherited stream socket");
    }
    #[cfg(any(target_os = "linux", target_os = "android"))]
    if int_opt(libc::SOL_SOCKET, libc::SO_ACCEPTCONN) != Some(1) {
        bail!("{LISTEN_FD}={fd} is not a listening socket");
    }
    // Nothing this server starts may inherit it.
    // SAFETY: plain fcntl on a descriptor we now own.
    unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
    // SAFETY: checked above to be an open socket; this process owns it now.
    Ok(unsafe { std::net::TcpListener::from_raw_fd(fd) })
}

#[cfg(not(unix))]
fn inherit_fd(_: &str) -> anyhow::Result<std::net::TcpListener> {
    bail!("{LISTEN_FD} is for Unix; on Windows use {LISTEN_PROTOCOL_INFO}")
}

#[cfg(windows)]
fn inherit_from_stdin() -> anyhow::Result<std::net::TcpListener> {
    use std::io::{BufRead, Read};
    use std::os::windows::io::FromRawSocket;
    use windows_sys::Win32::Networking::WinSock::{
        FROM_PROTOCOL_INFO, INVALID_SOCKET, WSA_FLAG_NO_HANDLE_INHERIT, WSA_FLAG_OVERLAPPED,
        WSADATA, WSAGetLastError, WSAPROTOCOL_INFOW, WSASocketW, WSAStartup,
    };
    let size = std::mem::size_of::<WSAPROTOCOL_INFOW>();
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .take((size * 2 + 2) as u64)
        .read_line(&mut line)
        .context("reading the inherited socket from stdin")?;
    let hex = line.trim();
    if hex.len() != size * 2 {
        bail!("stdin did not start with an inherited socket description");
    }
    let bytes = (0..size)
        .map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16))
        .collect::<Result<Vec<u8>, _>>()
        .context("the inherited socket description is not hex")?;
    // SAFETY: WSAPROTOCOL_INFOW is plain data of exactly `size` bytes; WinSock
    // calls get valid pointers; the new socket is owned by the listener.
    unsafe {
        let info: WSAPROTOCOL_INFOW = std::ptr::read_unaligned(bytes.as_ptr().cast());
        let mut data: WSADATA = std::mem::zeroed();
        if WSAStartup(0x0202, &mut data) != 0 {
            bail!("WinSock did not start");
        }
        let s = WSASocketW(
            FROM_PROTOCOL_INFO,
            FROM_PROTOCOL_INFO,
            FROM_PROTOCOL_INFO,
            &info,
            0,
            WSA_FLAG_OVERLAPPED | WSA_FLAG_NO_HANDLE_INHERIT,
        );
        if s == INVALID_SOCKET {
            bail!(
                "the inherited socket could not be opened (WinSock error {})",
                WSAGetLastError()
            );
        }
        Ok(std::net::TcpListener::from_raw_socket(s as _))
    }
}

#[cfg(not(windows))]
fn inherit_from_stdin() -> anyhow::Result<std::net::TcpListener> {
    bail!("{LISTEN_PROTOCOL_INFO} is for Windows; on Unix use {LISTEN_FD}")
}

/// A regular file (not a symlink), on Unix readable by its owner only.
fn read_private_file(path: &Path, cap: u64) -> anyhow::Result<String> {
    let meta = std::fs::symlink_metadata(path).with_context(|| format!("{}", path.display()))?;
    if !meta.is_file() {
        bail!("{} is not a regular file", path.display());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            bail!(
                "{} is readable by other users; restrict it to mode 600",
                path.display()
            );
        }
    }
    if meta.len() > cap {
        bail!("{} is too large", path.display());
    }
    std::fs::read_to_string(path).with_context(|| format!("{}", path.display()))
}

/// Writes the ready file next to its final name, then renames it into place,
/// so a reader never sees a partial file.
fn write_ready(path: &Path, addr: &str) -> anyhow::Result<()> {
    let body = serde_json::json!({ "pid": std::process::id(), "addr": addr }).to_string();
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    {
        use std::io::Write;
        let mut file = opts.open(&tmp)?;
        file.write_all(body.as_bytes())?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Reads stdin on its own thread until end of file (or an error), then asks
/// the server to stop.
fn watch_stdin(stop: watch::Sender<bool>) {
    std::thread::Builder::new()
        .name("stdin-eof".into())
        .spawn(move || {
            use std::io::Read;
            let mut stdin = std::io::stdin().lock();
            let mut buf = [0u8; 256];
            while matches!(stdin.read(&mut buf), Ok(n) if n > 0) {}
            tracing::info!("stdin closed");
            let _ = stop.send(true);
        })
        .expect("spawning the stdin watcher");
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|v| v.trim() == "1")
}

async fn shutdown(mut stop: watch::Receiver<bool>) {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    let stopped = async {
        if stop.wait_for(|stop| *stop).await.is_err() {
            std::future::pending::<()>().await;
        }
    };
    tokio::select! {
        _ = ctrl_c => {},
        _ = term => {},
        _ = stopped => {},
    }
    tracing::info!("shutting down");
}
