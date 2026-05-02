//! Postgres connection pool and SQLx-backed repository implementations.
//!
//! Schema lives in `apps/backend/migrations/`. This crate exposes the typed
//! [`Db`] handle plus per-entity repositories that implement the contracts
//! defined in `relayterm_core::repository`.
//!
//! Note on compile-time query checking: this crate uses the *runtime* SQLx
//! API (`sqlx::query` / `sqlx::query_as::<_, RowType>`) instead of the
//! `query!` / `query_as!` macros so that `cargo check` does not require a
//! live `DATABASE_URL` or a populated `.sqlx/` offline cache. When a
//! Postgres test environment is wired up (see the integration-test note in
//! `repositories/mod.rs`), the team can migrate hot queries to the macros
//! and run `cargo sqlx prepare --workspace`.

mod error;
mod rows;

pub mod repositories;

use sqlx::postgres::{PgPool, PgPoolOptions};
use std::time::Duration;

pub use error::map_sqlx_error;
pub use repositories::{
    PgAuditEventRepository, PgHostRepository, PgKnownHostEntryRepository,
    PgPasswordCredentialRepository, PgServerProfileRepository, PgSessionEventRepository,
    PgSshIdentityRepository, PgTerminalRecordingRepository, PgTerminalSessionRepository,
    PgUserRepository, PgUserSessionRepository,
};

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("connection failed: {0}")]
    Connect(#[from] sqlx::Error),
    #[error("migration failed: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
}

/// Thin handle around a `PgPool`. Cheap to clone (Arc inside).
#[derive(Debug, Clone)]
pub struct Db {
    pool: PgPool,
}

impl Db {
    /// Connect with sane defaults for a single-tenant deployment.
    pub async fn connect(database_url: &str, max_connections: u32) -> Result<Self, DbError> {
        let pool = PgPoolOptions::new()
            .max_connections(max_connections)
            .acquire_timeout(Duration::from_secs(5))
            .connect(database_url)
            .await?;
        Ok(Self { pool })
    }

    #[must_use]
    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    #[must_use]
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    // Repository constructors. Each returns a thin wrapper that borrows
    // nothing from `self` beyond a cloned pool, so the caller can share
    // them freely behind `Arc<dyn ...>` if desired.

    #[must_use]
    pub fn users(&self) -> PgUserRepository {
        PgUserRepository::new(self.pool.clone())
    }

    #[must_use]
    pub fn hosts(&self) -> PgHostRepository {
        PgHostRepository::new(self.pool.clone())
    }

    #[must_use]
    pub fn ssh_identities(&self) -> PgSshIdentityRepository {
        PgSshIdentityRepository::new(self.pool.clone())
    }

    #[must_use]
    pub fn server_profiles(&self) -> PgServerProfileRepository {
        PgServerProfileRepository::new(self.pool.clone())
    }

    #[must_use]
    pub fn known_host_entries(&self) -> PgKnownHostEntryRepository {
        PgKnownHostEntryRepository::new(self.pool.clone())
    }

    #[must_use]
    pub fn terminal_sessions(&self) -> PgTerminalSessionRepository {
        PgTerminalSessionRepository::new(self.pool.clone())
    }

    #[must_use]
    pub fn session_events(&self) -> PgSessionEventRepository {
        PgSessionEventRepository::new(self.pool.clone())
    }

    #[must_use]
    pub fn audit_events(&self) -> PgAuditEventRepository {
        PgAuditEventRepository::new(self.pool.clone())
    }

    #[must_use]
    pub fn password_credentials(&self) -> PgPasswordCredentialRepository {
        PgPasswordCredentialRepository::new(self.pool.clone())
    }

    #[must_use]
    pub fn user_sessions(&self) -> PgUserSessionRepository {
        PgUserSessionRepository::new(self.pool.clone())
    }

    #[must_use]
    pub fn terminal_recordings(&self) -> PgTerminalRecordingRepository {
        PgTerminalRecordingRepository::new(self.pool.clone())
    }
}
