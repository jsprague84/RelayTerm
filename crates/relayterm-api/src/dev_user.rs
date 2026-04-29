//! Temporary, development-only user context.
//!
//! **THIS IS A STOPGAP.** RelayTerm has no auth yet, but every persisted
//! entity (host, server profile, ssh identity) is owned by a `UserId` for
//! foreign-key reasons. To unblock the first product API slice we inject a
//! single hardcoded user id from [`AppState::dev_user_id`] into every
//! handler via the [`DevUser`] extractor.
//!
//! ## Coexistence with real auth
//!
//! `AppState::dev_user_id` is `Option<UserId>` so this shim can be turned
//! off (via `dev_auth.enabled = false`) WITHOUT removing the bootstrap or
//! the extractor in the same change. While disabled the extractor returns
//! `401 Unauthorized`, leaving room to wire a real auth extractor onto the
//! same routes during the transition window. When real auth lands the
//! removal is:
//!
//! 1. Replace every `DevUser` parameter with the real auth extractor.
//! 2. Delete this module and the bootstrap call in `apps/backend/src/main.rs`.
//! 3. Drop `AppState::dev_user_id` and the `dev_auth` config field.
//!
//! Searches for `DevUser` and `unimplemented_auth` should turn up every
//! call site that needs to be re-pointed.

use axum::{
    extract::{FromRef, FromRequestParts},
    http::request::Parts,
};
use relayterm_core::ids::UserId;

use crate::error::ApiError;

/// Identity context injected for every handler while auth is unimplemented.
///
/// Wraps a [`UserId`]; obtain via the axum extractor:
///
/// ```ignore
/// async fn handler(DevUser(user_id): DevUser) { ... }
/// ```
#[derive(Debug, Clone, Copy)]
pub struct DevUser(pub UserId);

impl<S> FromRequestParts<S> for DevUser
where
    S: Send + Sync,
    Option<UserId>: FromRef<S>,
{
    type Rejection = ApiError;

    async fn from_request_parts(_parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        Option::<UserId>::from_ref(state).map(Self).ok_or_else(|| {
            ApiError::Unauthorized(
                "dev auth is disabled and no real auth backend is wired up yet".to_owned(),
            )
        })
    }
}
