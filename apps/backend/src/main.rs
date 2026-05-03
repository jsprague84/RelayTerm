use std::sync::Arc;

use anyhow::{Context, bail};
use relayterm_api::{AppState, AuthRoutesConfig, router};
use relayterm_auth::{AuthService, LoginThrottleConfig, LoginThrottler, PasswordHasher};
use relayterm_core::repository::{
    PasswordCredentialRepository, SessionEventRepository, TerminalRecordingRepository,
    TerminalSessionRepository, UserSessionRepository,
};
use relayterm_db::Db;
use relayterm_ssh::{
    HostKeyPreflightService, RusshAuthChecker, RusshHostKeyProbe, RusshPtyBridge,
    SshAuthCheckService, SshPtyBridge,
};
use relayterm_terminal::{
    RecordingRuntime, RecordingWriterConfig, TerminalSessionManager,
    run_recording_retention_startup_sweep,
};
use relayterm_vault::VaultService;
use tokio::{net::TcpListener, signal};
use tracing::{info, warn};
use zeroize::Zeroizing;

mod config;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    relayterm_observability::init();

    let mut cfg = config::Config::load().context("load config")?;
    // Boot-time auth gate. Runs BEFORE any irreversible work (db connect,
    // ssh services, listener bind) so a misconfigured deploy fails fast and
    // never opens a socket without a valid auth posture. See
    // `Config::validate_auth` for the matrix (production requires a
    // signing key, non-empty `allowed_origins`, and `cookie_secure =
    // true`; dev relaxes all three for local convenience).
    cfg.validate_auth().context("validate auth config")?;
    // Recording config foundation. Step 1b (this slice) wires typed
    // config + boot validation only — there is no chunk writer, no
    // replay API, no UI yet. Validation runs alongside auth so a
    // misconfigured production deploy fails fast (e.g. recording
    // enabled in production with no master key) before binding the
    // listener. See `docs/terminal-recording.md` Section 13 for the
    // staged plan and what each later slice will add.
    cfg.validate_terminal_recording()
        .context("validate terminal recording config")?;
    info!(
        addr = %cfg.server.bind,
        auth_mode = cfg.auth.mode.as_str(),
        recording_enabled = cfg.terminal_recording.enabled,
        "relayterm-backend starting",
    );

    let db = Db::connect(&cfg.database.url, cfg.database.max_connections)
        .await
        .context("connect to postgres")?;

    // Backend startup reconciliation for terminal sessions. Any
    // `terminal_sessions` row left in `starting`/`active`/`detached`
    // is an orphan: its in-memory `TerminalSessionManager` runtime
    // entry, live `russh::Channel`, broadcast fanout, and replay ring
    // were lost when the previous process exited. The row would
    // otherwise stay operator-visible as a "live" session the UI can
    // never resume. Sweep them to `closed` BEFORE the listener binds
    // and BEFORE the manager is constructed, so the registry starts
    // from a clean slate. Each transitioned row gets one
    // `session_events { kind: closed, payload: { reason:
    // "startup_reconciliation", previous_status, reconciled_at } }`
    // row inside the same database transaction; reconciled sessions
    // that have at least one durable chunk row ALSO get one
    // `terminal_recording_markers { kind: closed, seq: MAX(seq_end),
    // payload: { reason: "startup_reconciliation", previous_status,
    // reconciled_at } }` row appended in the same transaction. The
    // marker INSERT uses `ON CONFLICT DO NOTHING` against the partial
    // unique index `terminal_recording_markers_session_closed_seq_uidx`
    // so a pre-existing equivalent marker is preserved at the schema
    // layer. No `audit_events`, no row deletion, no chunk writes
    // (chunks are append-only and survive the restart).
    // Idempotent across reruns: a second startup with no orphans is
    // a no-op. See `docs/terminal-recording.md` Section 9.3 and
    // SPEC.md "Durable terminal recording and replay architecture"
    // for the policy.
    //
    // Fails the boot on a DB error: a partial reconciliation that
    // leaves the orchestrator running with stale "live" rows is the
    // worse failure mode (operator-invisible, UI-misleading) than
    // refusing to serve.
    {
        let now = chrono::Utc::now();
        let reconciled = db
            .terminal_sessions()
            .reconcile_orphaned_on_startup(now)
            .await
            .context("reconcile orphaned terminal sessions on startup")?;
        if reconciled.is_empty() {
            info!("terminal session startup reconciliation: nothing to do");
        } else {
            // Log the count only — never the session ids. They're
            // owner-scoped resources the operator log doesn't need
            // and the count is enough to spot a regression.
            info!(
                count = reconciled.len(),
                "terminal session startup reconciliation: \
                 swept orphaned sessions to closed",
            );
        }
    }

    // Recording retention startup sweep (Stage A — `docs/terminal-recording.md`
    // Section 12.7). Runs AFTER terminal-session reconciliation and
    // BEFORE the HTTP listener binds. Bounded to one batch
    // (`cleanup.batch_size`) so a long retention backlog does not
    // block boot indefinitely; remaining work is picked up by the
    // future Stage B periodic worker (not landed yet) or by the next
    // backend restart.
    //
    // Cleanup is independent of `terminal_recording.enabled`
    // (Section 12.6 independence rule): an operator who turned
    // recording off after running it for some time MUST NOT have
    // their existing recording corpus become immortal. The sweep
    // therefore runs whenever `cleanup.enabled` AND
    // `cleanup.startup_sweep_enabled` are both true, regardless of
    // whether the chunk writer is active in this process.
    //
    // Failure semantics — fail-soft. Unlike Section 9.3
    // reconciliation (which is fail-fast), a retention sweep failure
    // is a `warn!` and the boot continues to the listener bind.
    // Missing one sweep cycle is operationally undesirable but is not
    // a security-relevant correctness issue — the corpus has not
    // grown past its existing on-disk footprint by skipping a single
    // sweep.
    {
        let cleanup = &cfg.terminal_recording.cleanup;
        if cleanup.enabled && cleanup.startup_sweep_enabled {
            let now = chrono::Utc::now();
            let repo: Arc<dyn TerminalRecordingRepository> = Arc::new(db.terminal_recordings());
            // The sweep is fully self-contained: it logs its own
            // summary line and never returns an error. We capture the
            // summary purely for the test seam — no operator-side
            // log line should be emitted from main.rs.
            let _summary = run_recording_retention_startup_sweep(
                repo,
                cfg.terminal_recording.retention_days,
                cleanup.batch_size,
                now,
            )
            .await;
        }
    }

    // Production deploys must be reachable as a real user. Without a
    // first user AND without a `first_user_bootstrap_token`, no operator
    // path exists to create one. Reject before binding the listener so a
    // misconfigured production deploy never starts serving 401s with no
    // recovery affordance. Dev mode is exempt — local development can
    // mint users via the bootstrap route at any time, or hit the DB
    // directly. SPEC.md "Security properties to test" property 1.
    if matches!(cfg.auth.mode, config::AuthMode::Production)
        && cfg.auth.first_user_bootstrap_token.is_none()
        && !db
            .password_credentials()
            .any_exists()
            .await
            .context("check first-user state")?
    {
        bail!(
            "auth.mode = production with no existing user requires \
             auth.first_user_bootstrap_token to be set so the operator \
             can bootstrap the first account"
        );
    }

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
    //
    // Recording wiring: when `terminal_recording.enabled = true` AND the
    // configured encryption mode is supported by the writer (currently
    // only `disabled` — i.e. plaintext-at-rest), the manager fans every
    // PTY output frame into a bounded background writer that appends
    // chunks/markers to Postgres. The writer is a tee, not a gate: a
    // full queue or DB outage drops frames for recording only and the
    // live wire is unaffected. See `docs/terminal-recording.md` Section
    // 6.1 + Section 13 step 3.
    //
    // Encryption.required is config-valid (the validator accepts it for
    // production) but the writer for that mode has not landed; we
    // refuse to start in that combination so an operator who configured
    // encryption.required does not silently get plaintext.
    let recording_runtime = build_recording_runtime(&cfg, &db)?;
    let terminal_sessions = {
        let mut mgr = TerminalSessionManager::new(
            Arc::new(db.terminal_sessions()) as Arc<dyn TerminalSessionRepository>,
            Arc::new(db.session_events()) as Arc<dyn SessionEventRepository>,
        );
        if let Some(runtime) = recording_runtime {
            mgr = mgr.with_recording(runtime);
            info!("terminal recording writer enabled (plaintext-at-rest)");
        }
        Arc::new(mgr)
    };

    // Compose the auth service from the existing repositories. The
    // hasher uses production parameters (`PasswordHasher::default()` =
    // `PasswordHasherConfig::OWASP_2023`); tests construct their own
    // tuned-down hasher. Every protected route runs through this
    // service (cookie-backed `AuthenticatedUser` extractor in
    // `relayterm-api::auth`).
    let auth = Arc::new(AuthService::new(
        Arc::new(db.password_credentials()) as Arc<dyn PasswordCredentialRepository>,
        Arc::new(db.user_sessions()) as Arc<dyn UserSessionRepository>,
        PasswordHasher::default(),
    ));

    // Auth-routes policy. Bootstrap token is moved into a `Zeroizing`
    // wrapper here so the heap copy wipes itself when `AppState`
    // drops; the typed config field on `AuthConfig` is consumed via
    // `take()` so a copy is not retained on `cfg.auth` after the
    // shared state is built.
    let auth_routes = Arc::new(AuthRoutesConfig {
        cookie_secure: cfg.auth.cookie_secure,
        cookie_domain: cfg.auth.cookie_domain.clone(),
        allowed_origins: cfg.auth.allowed_origins.clone(),
        bootstrap_token: cfg
            .auth
            .first_user_bootstrap_token
            .take()
            .map(Zeroizing::new),
    });

    // Login throttler. v1 ships an in-memory leaky bucket keyed on
    // the normalized email; SPEC.md "Password authentication (v1)" →
    // "Throttling" pins the policy. State is local to this process —
    // a multi-instance deploy SHOULD layer reverse-proxy rate-limiting
    // on top until a distributed limiter lands. See
    // `docs/production-auth.md` for the operator-facing caveat.
    let login_throttler = Arc::new(LoginThrottler::new(LoginThrottleConfig::V1_DEFAULT));

    let state = AppState {
        db,
        vault,
        preflight,
        auth_check,
        pty_bridge,
        terminal_sessions,
        auth,
        auth_routes,
        login_throttler,
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

/// Resolve the recording writer runtime, or return `None` when recording
/// is configured-disabled.
///
/// Failure modes:
/// * `enabled = true` AND `encryption.mode = required`: the writer for
///   that mode has not landed yet. We return an explicit error so the
///   backend fails to boot rather than silently degrade to plaintext —
///   the validator already accepted this combination on the assumption
///   that the writer would respect it.
fn build_recording_runtime(
    cfg: &config::Config,
    db: &Db,
) -> anyhow::Result<Option<RecordingRuntime>> {
    let rec = &cfg.terminal_recording;
    if !rec.enabled {
        return Ok(None);
    }
    match rec.encryption.mode {
        config::TerminalRecordingEncryptionMode::Disabled => {
            let writer_cfg = RecordingWriterConfig {
                chunk_target_bytes: rec.chunk_target_bytes,
                chunk_hard_cap_bytes: rec.chunk_hard_cap_bytes,
            };
            let repo: Arc<dyn TerminalRecordingRepository> = Arc::new(db.terminal_recordings());
            Ok(Some(RecordingRuntime::new(repo, writer_cfg)))
        }
        config::TerminalRecordingEncryptionMode::Required => bail!(
            "terminal_recording.enabled = true with encryption.mode = required, but the \
             encryption-aware recording writer has not landed yet. Either set \
             encryption.mode = disabled (dev only — operator accepts plaintext-at-rest) or \
             leave terminal_recording.enabled = false until the encryption writer lands"
        ),
    }
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
