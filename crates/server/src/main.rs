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
        bind = %bind,
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

    let (stop_tx, stop_rx) = watch::channel(false);
    if flag(EXIT_ON_STDIN_EOF) {
        watch_stdin(stop_tx);
    }

    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("binding {bind}"))?;
    if let Some(path) = env_path(READY_FILE) {
        let addr = listener.local_addr().context("reading the bound address")?;
        write_ready(&path, &addr.to_string()).with_context(|| format!("writing {READY_FILE}"))?;
        tracing::info!(addr = %addr, "listening");
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
