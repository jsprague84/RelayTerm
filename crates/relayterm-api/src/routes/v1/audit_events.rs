//! `/api/v1/audit-events` routes.
//!
//! Read-only audit feed scoped to the authenticated current user.
//!
//! ## Scope (security-critical)
//!
//! - **Current-user only.** Rows are filtered by the caller's
//!   [`UserId`](relayterm_core::ids::UserId) at the SQL layer via
//!   [`AuditEventRepository::recent_for_actor`]. Pre-auth events with
//!   `actor_id IS NULL` (e.g. failed login attempts) are NOT visible
//!   here — an admin surface that wants those uses
//!   [`AuditEventRepository::recent`] directly.
//! - **No cross-user access.** There is no `actor_id` query parameter,
//!   no admin route, no aggregation, no search.
//! - **No raw payload.** Responses go through
//!   [`AuditEventResponse::from_event`], which maps each known
//!   [`AuditEventKind`] onto a closed allow-list of safe public fields.
//!   Unknown kinds collapse to a generic summary that carries no
//!   payload data at all.
//!
//! ## Limit clamping
//!
//! `?limit=N` is clamped to `1..=MAX_LIMIT`. The default is
//! [`DEFAULT_LIMIT`]. Out-of-range values are clamped silently rather
//! than 400'd — the limit is a UI hint, not load-bearing input.

use axum::{
    Json, Router,
    extract::{Query, State},
    routing::get,
};
use relayterm_core::repository::AuditEventRepository;
use serde::Deserialize;

use crate::AppState;
use crate::auth::AuthenticatedUser;
use crate::dto::audit_event::AuditEventResponse;
use crate::error::ApiError;

/// Default page size when the caller omits `?limit`.
const DEFAULT_LIMIT: u32 = 20;
/// Hard cap on `?limit`. Larger values are clamped silently — see the
/// module docs.
const MAX_LIMIT: u32 = 100;

pub(super) fn router() -> Router<AppState> {
    Router::new().route("/recent", get(recent))
}

#[derive(Debug, Deserialize, Default)]
struct RecentQuery {
    /// Optional client-supplied page size. Clamped to `1..=MAX_LIMIT`;
    /// defaults to [`DEFAULT_LIMIT`].
    limit: Option<u32>,
}

/// `GET /api/v1/audit-events/recent[?limit=N]`.
///
/// Returns the most-recent-first audit feed for the authenticated
/// current user. Foreign-actor and `actor_id IS NULL` rows are filtered
/// out at the SQL layer. The response is a list of redaction-safe DTOs
/// — see [`AuditEventResponse`] for the contract.
async fn recent(
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Query(q): Query<RecentQuery>,
) -> Result<Json<Vec<AuditEventResponse>>, ApiError> {
    let limit = clamp_limit(q.limit);
    let events = state
        .db
        .audit_events()
        .recent_for_actor(user.user_id(), limit)
        .await?;
    Ok(Json(
        events
            .into_iter()
            .map(AuditEventResponse::from_event)
            .collect(),
    ))
}

/// Clamp a caller-supplied `limit` into `1..=MAX_LIMIT`. `None` returns
/// the default; `Some(0)` clamps up to `1` so a malformed client query
/// never produces an empty page silently.
fn clamp_limit(raw: Option<u32>) -> u32 {
    raw.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limit_clamping_table() {
        assert_eq!(clamp_limit(None), DEFAULT_LIMIT);
        assert_eq!(clamp_limit(Some(0)), 1);
        assert_eq!(clamp_limit(Some(1)), 1);
        assert_eq!(clamp_limit(Some(50)), 50);
        assert_eq!(clamp_limit(Some(MAX_LIMIT)), MAX_LIMIT);
        assert_eq!(clamp_limit(Some(MAX_LIMIT + 1)), MAX_LIMIT);
        assert_eq!(clamp_limit(Some(u32::MAX)), MAX_LIMIT);
    }
}
