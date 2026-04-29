use anyhow::Context;
use relayterm_api::{AppState, router};
use relayterm_core::ids::UserId;
use relayterm_core::repository::{CreateUser, UserRepository};
use relayterm_db::Db;
use tokio::{net::TcpListener, signal};
use tracing::{info, warn};

mod config;

/// Email used by the temporary single-user dev context.
///
/// Replaced by real auth in a future slice; see `relayterm_api::dev_user`.
const DEV_USER_EMAIL: &str = "dev@relayterm.local";
const DEV_USER_DISPLAY_NAME: &str = "RelayTerm Dev User";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    relayterm_observability::init();

    let cfg = config::Config::load().context("load config")?;
    info!(addr = %cfg.server.bind, "relayterm-backend starting");

    let db = Db::connect(&cfg.database.url, cfg.database.max_connections)
        .await
        .context("connect to postgres")?;

    // STOPGAP — see `bootstrap_dev_user_for_unimplemented_auth` below.
    //
    // Two-phase removal of this shim:
    //   1. Land real auth alongside the shim. While both are wired the
    //      shim wins and tags requests with the dev user.
    //   2. Flip `dev_auth.enabled = false`. The backend keeps starting;
    //      `DevUser`-guarded routes return 401 until each handler is
    //      ported to the real auth extractor.
    //   3. Delete the bootstrap call, the `DevUser` module, and the
    //      `dev_auth` config field in the same change that retires the
    //      last `DevUser` use site.
    let dev_user_id = if cfg.dev_auth.enabled {
        let id = bootstrap_dev_user_for_unimplemented_auth(&db)
            .await
            .context("bootstrap dev user for unimplemented auth")?;
        warn!(
            dev_user_id = %id,
            "AUTH NOT IMPLEMENTED — every request is attributed to the hardcoded dev user; \
             flip dev_auth.enabled to false once real auth is wired",
        );
        Some(id)
    } else {
        warn!(
            "dev_auth.enabled = false — DevUser-guarded routes will return 401 until \
             every handler is ported to the real auth extractor, then this whole shim \
             can be deleted",
        );
        None
    };

    let state = AppState { db, dev_user_id };
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

/// **DELETE WHEN REAL AUTH LANDS.**
///
/// Find-or-create the single hardcoded dev user that every request is
/// attributed to while authentication is unimplemented. The function name
/// is intentionally long and unambiguous so a code search for `unimplemented_auth`
/// surfaces this and the matching `DevUser` extractor in one shot.
///
/// Removal sequence is in the `main()` doc-comment above; the fixture is
/// idempotent so a re-deploy behaves the same as a fresh container.
async fn bootstrap_dev_user_for_unimplemented_auth(db: &Db) -> anyhow::Result<UserId> {
    let users = db.users();
    if let Some(existing) = users.get_by_email(DEV_USER_EMAIL).await? {
        return Ok(existing.id);
    }
    let created = users
        .create(CreateUser {
            email: DEV_USER_EMAIL.to_owned(),
            display_name: DEV_USER_DISPLAY_NAME.to_owned(),
        })
        .await?;
    Ok(created.id)
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
