//! SQLx-backed repository implementations for the contracts in
//! `relayterm_core::repository`.
//!
//! Each `PgXxxRepository` holds a cloned `PgPool` and is cheap to construct
//! via `Db::xxx()`. They are `Send + Sync + Clone` so they can sit behind
//! `Arc<dyn ...>` in `AppState` once the routes that need them land.
//!
//! ## Integration tests
//!
//! These implementations are not yet covered by integration tests because
//! the project does not yet have a Postgres-spinning test harness. The
//! recommended path:
//!
//! 1. Add a `dev-dependencies` block here for `sqlx` with the `migrate`
//!    feature, plus `tokio` with `rt-multi-thread` and `macros`.
//! 2. Use `sqlx::test` (which provisions a per-test database against a
//!    `DATABASE_URL` set in the environment, e.g. via `docker compose up
//!    postgres` from `deploy/`).
//! 3. Place tests under `crates/relayterm-db/tests/` so each test gets a
//!    fresh schema by running migrations from `apps/backend/migrations/`.
//! 4. Once integration tests are reliable, switch the hot queries from the
//!    runtime API to the `query!` / `query_as!` macros and run
//!    `cargo sqlx prepare --workspace` to populate `.sqlx/`.

mod audit_event;
mod host;
mod known_host_entry;
mod password_credential;
mod retention_advisory_lock;
mod server_profile;
mod session_event;
mod ssh_identity;
mod terminal_recording;
mod terminal_session;
mod user;
mod user_session;

pub use audit_event::PgAuditEventRepository;
pub use host::PgHostRepository;
pub use known_host_entry::PgKnownHostEntryRepository;
pub use password_credential::PgPasswordCredentialRepository;
pub use retention_advisory_lock::PgRetentionAdvisoryLock;
pub use server_profile::PgServerProfileRepository;
pub use session_event::PgSessionEventRepository;
pub use ssh_identity::PgSshIdentityRepository;
pub use terminal_recording::PgTerminalRecordingRepository;
pub use terminal_session::PgTerminalSessionRepository;
pub use user::PgUserRepository;
pub use user_session::PgUserSessionRepository;
