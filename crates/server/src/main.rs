use std::sync::Arc;

use anyhow::Context;
use metro_server::{AppState, Config, db::Db, router};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let config = Config::from_env()?;
    let auth = metro_auth::Authenticator::from_env()
        .await
        .map_err(|e| anyhow::anyhow!("configuring authentication: {e}"))?;
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
    let state = AppState::new(config, db, Arc::new(auth)).await?;

    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("binding {bind}"))?;
    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown())
        .await?;
    Ok(())
}

async fn shutdown() {
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
    tokio::select! {
        _ = ctrl_c => {},
        _ = term => {},
    }
    tracing::info!("shutting down");
}
