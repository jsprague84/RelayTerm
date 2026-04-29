use anyhow::Context;
use relayterm_api::{AppState, router};
use relayterm_db::Db;
use tokio::{net::TcpListener, signal};
use tracing::info;

mod config;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    relayterm_observability::init();

    let cfg = config::Config::load().context("load config")?;
    info!(addr = %cfg.server.bind, "relayterm-backend starting");

    let db = Db::connect(&cfg.database.url, cfg.database.max_connections)
        .await
        .context("connect to postgres")?;

    let state = AppState { db };
    let app = router(state);

    let listener = TcpListener::bind(cfg.server.bind)
        .await
        .with_context(|| format!("bind {}", cfg.server.bind))?;

    info!(addr = %cfg.server.bind, "listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("axum::serve")?;

    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c().await.expect("install ctrl_c handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }

    info!("shutdown signal received");
}
