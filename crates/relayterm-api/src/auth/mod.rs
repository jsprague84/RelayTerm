//! HTTP-layer auth glue: cookie parsing + the `AuthenticatedUser`
//! extractor.
//!
//! The crypto and persistence primitives live in
//! [`relayterm_auth`](::relayterm_auth). This module is the thin shim
//! that bridges them to axum's request lifecycle.
//!
//! The cookie helper is shared between the
//! [`/api/v1/auth/*`](crate::routes::v1::auth) routes and the
//! extractor so both agree on the cookie name and the parser policy
//! (exact-match, empty-value-as-absent, prefix/suffix-confusion-safe).

pub(crate) mod cookie;
pub(crate) mod user;

pub use user::AuthenticatedUser;
