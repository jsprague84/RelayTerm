//! HTTP-layer auth glue: cookie parsing, the shared CSRF / `Origin`
//! guard, and the `AuthenticatedUser` extractor.
//!
//! The crypto and persistence primitives live in
//! [`relayterm_auth`](::relayterm_auth). This module is the thin shim
//! that bridges them to axum's request lifecycle.
//!
//! The cookie helper is shared between the
//! [`/api/v1/auth/*`](crate::routes::v1::auth) routes and the
//! extractor so both agree on the cookie name and the parser policy
//! (exact-match, empty-value-as-absent, prefix/suffix-confusion-safe).
//!
//! The CSRF helper ([`csrf::check_origin`]) and extractor
//! ([`csrf::CsrfGuard`]) are shared between the auth routes (today)
//! and every browser-write route that migrates off
//! [`crate::DevUser`] (SPEC step 7). Both shapes implement the same
//! policy so a handler that switches from one to the other does not
//! change the wire response.

pub(crate) mod cookie;
pub(crate) mod csrf;
pub(crate) mod user;

pub use csrf::CsrfGuard;
pub use user::AuthenticatedUser;
