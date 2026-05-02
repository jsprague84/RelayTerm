use std::sync::Arc;

use anyhow::Context;
use relayterm_api::{AppState, router};
use relayterm_core::ids::UserId;
use relayterm_core::repository::{
    CreateUser, SessionEventRepository, TerminalSessionRepository, UserRepository,
};
use relayterm_db::Db;
use relayterm_ssh::{
    HostKeyPreflightService, RusshAuthChecker, RusshHostKeyProbe, RusshPtyBridge,
    SshAuthCheckService, SshPtyBridge,
};
use relayterm_terminal::TerminalSessionManager;
use relayterm_vault::VaultService;
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

    let mut cfg = config::Config::load().context("load config")?;
    // Boot-time auth gate. Runs BEFORE any irreversible work (db connect,
    // ssh services, listener bind) so a misconfigured deploy fails fast and
    // never opens a socket without a valid auth posture. Today this rejects
    // `auth.mode = production` (the production code path is not implemented
    // yet) AND the `auth.mode = production` + `dev_auth.enabled = true`
    // mutual-exclusion violation. See `Config::validate_auth` for the
    // matrix.
    cfg.validate_auth().context("validate auth config")?;
    info!(
        addr = %cfg.server.bind,
        auth_mode = cfg.auth.mode.as_str(),
        "relayterm-backend starting",
    );

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

    // Resolve the vault master key. Failure here is fatal — we will not
    // boot a backend that silently disables encrypted-private-key storage.
    // The error message names the source ("file" / "b64") but never echoes
    // the configured value or any prefix of it.
    let vault = match cfg.vault_master_key().context("resolve vault master key")? {
        Some(master_key) => {
            info!("vault master key loaded; backend-generated SSH identities enabled");
            Some(VaultService::new(master_key))
        }
        None => {
            warn!(
                "vault.enabled = false — POST /api/v1/ssh-identities returns 503 until a \
                 master key is configured",
            );
            None
        }
    };

    // Host-key preflight service. The probe is the russh-backed
    // implementation; tests inject a fake via `AppState` directly. Held
    // behind `Arc` so clones of `AppState` share one instance.
    //
    // SCOPE: this attests to host-key reachability classification only —
    // not SSH authentication or PTY readiness. See the doc-comment on
    // `HostKeyPreflightService` for the full "proves vs does not prove"
    // contract.
    let preflight = Arc::new(HostKeyPreflightService::new(Arc::new(
        RusshHostKeyProbe::new(),
    )));

    // Authenticated credential-check service. Verifies a saved
    // (profile, host, ssh-identity) trio's host-key trust state and
    // attempts SSH public-key authentication, disconnecting before any
    // PTY/command/shell. Tests inject a fake checker via `AppState`
    // directly. SCOPE: no interactive session, no command execution.
    let auth_check = Arc::new(SshAuthCheckService::new(Arc::new(RusshAuthChecker::new())));

    // Live SSH PTY bridge. Production: russh-backed; tests inject a
    // fake via AppState directly. SCOPE: minimal interactive PTY path
    // — no replay-buffer recovery yet.
    let pty_bridge: Arc<dyn SshPtyBridge> = Arc::new(RusshPtyBridge::new());

    // Terminal session orchestrator. Owns the in-memory runtime registry
    // and writes session metadata + lifecycle events to Postgres. The
    // registry is NOT durable — a backend restart leaves any pre-restart
    // metadata rows operator-visible as stale records until they're
    // explicitly closed via `POST /api/v1/terminal-sessions/:id/close`.
    //
    // SCOPE: this slice manages session lifecycle metadata only. Real
    // PTY allocation, SSH channel ownership, and replay-buffer state are
    // future slices.
    let terminal_sessions = Arc::new(TerminalSessionManager::new(
        Arc::new(db.terminal_sessions()) as Arc<dyn TerminalSessionRepository>,
        Arc::new(db.session_events()) as Arc<dyn SessionEventRepository>,
    ));

    let state = AppState {
        db,
        vault,
        preflight,
        auth_check,
        pty_bridge,
        terminal_sessions,
        dev_user_id,
    };
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
