//! SQLx-error → `RepositoryError` mapping.
//!
//! Keep this layer narrow: callers shouldn't see raw SQL or driver-specific
//! errors. We classify into the three structured outcomes the domain cares
//! about (not-found, conflict, generic database error) and turn everything
//! else into a short, sanitized string.

use relayterm_core::repository::RepositoryError;

/// Map a SQLx error into the public repository error.
///
/// `entity` is the human-readable noun for the row this query was about
/// (e.g. `"user"`, `"host"`). It appears in `NotFound` and `Conflict`.
#[must_use]
pub fn map_sqlx_error(entity: &'static str, err: sqlx::Error) -> RepositoryError {
    match err {
        sqlx::Error::RowNotFound => RepositoryError::NotFound { entity },
        sqlx::Error::Database(db_err) if db_err.is_unique_violation() => {
            RepositoryError::Conflict {
                entity,
                constraint: db_err
                    .constraint()
                    .map(ToOwned::to_owned)
                    .unwrap_or_else(|| "unique".to_owned()),
            }
        }
        sqlx::Error::Database(db_err) => {
            // Foreign-key, check-constraint, etc. Surface a short message
            // tagged with the constraint name when present so operators can
            // grep logs, but never include the SQL or parameter values.
            let constraint = db_err.constraint().unwrap_or("unknown");
            RepositoryError::Database(format!(
                "{entity}: database constraint failed ({constraint})"
            ))
        }
        other => RepositoryError::Database(format!("{entity}: {other}")),
    }
}
