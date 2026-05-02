//! Shared CSRF / `Origin` guard foundation for browser-write routes.
//!
//! Cookie-bearing browser requests are vulnerable to CSRF. SPEC.md
//! "CSRF posture" pins three layers of defense; this module is the v1
//! implementation of layer 2 — the per-request `Origin`-header allow-
//! list check. Layer 1 (`SameSite=Strict` on the session cookie) is
//! enforced by the cookie writer in `routes::v1::auth`. Layer 3
//! (double-submit token) is deferred per SPEC and out of scope here.
//!
//! ## Two consumption shapes
//!
//! * [`CsrfGuard`] — an axum extractor (`FromRequestParts`) wrapping
//!   [`check_origin`]. **This is the primary consumption shape.** Every
//!   state-changing browser-write route (`/api/v1/auth/*`, `hosts`,
//!   `ssh-identities`, `server-profiles`, `terminal-sessions`) takes
//!   `_csrf: CsrfGuard` ahead of the body extractor.
//! * [`check_origin`] — the underlying helper. An internal building
//!   block exposed at `pub(crate)` visibility for any future handler
//!   that needs to call the check imperatively mid-flow (e.g. a route
//!   that conditionally enforces the guard based on request state).
//!   No route calls it directly today.
//!
//! Both shapes share the same policy and the same error code so a
//! handler that switches from one to the other does not change the
//! wire response.
//!
//! ## Policy
//!
//! Matches SPEC.md "CSRF posture":
//!
//! * Missing `Origin` → 403 `csrf_origin_mismatch`.
//! * `Origin` not valid UTF-8 → 403 `csrf_origin_mismatch`.
//! * `Origin` not in `allowed_origins` → 403 `csrf_origin_mismatch`.
//! * Match (exact byte equality) → continue.
//!
//! Empty `allowed_origins` rejects every write — that is the secure
//! default; tests / dev populate the list explicitly.
//!
//! ## Comparison policy
//!
//! Comparison is **case-sensitive byte equality**. Browsers normalize
//! the scheme and host of an `Origin` to lower-case before serialising,
//! so exact-match is sufficient when the configured allow-list is also
//! lower-case. A case-insensitive variant is deferred — handling
//! internationalised hostnames safely (IDN, NFKC, scheme-only-lower)
//! requires care that this slice does not. If a future deployment
//! needs Unicode-host parity the allow-list pre-normalises at boot
//! time, NOT in this hot path.
//!
//! ## Redaction
//!
//! The wire body is the static `forbidden` envelope (see
//! [`ApiError::CsrfOriginMismatch`](crate::error::ApiError::CsrfOriginMismatch)).
//! The wrapped operator-side detail strings here are deliberately
//! classified ("missing Origin header" / "Origin header is not valid
//! UTF-8" / "Origin not in allowed_origins"); they do NOT echo the
//! offered `Origin` value, so a probe header carrying a sentinel
//! string cannot smuggle that string into either the response body OR
//! the operator-side `warn!` line.

use axum::{
    extract::{FromRef, FromRequestParts},
    http::{HeaderMap, header, request::Parts},
};

use crate::AppState;
use crate::error::ApiError;

/// Run the `Origin` allow-list check against a [`HeaderMap`].
///
/// Returns `Ok(())` on a clean match. Every failure path collapses to
/// [`ApiError::CsrfOriginMismatch`] — see the module-level docs for the
/// policy table.
///
/// **Never echoes the offered `Origin` value.** The wrapped error
/// strings are classified-reason only so the operator-side `warn!` line
/// in [`ApiError::IntoResponse`](crate::error::ApiError) cannot smuggle
/// a probe-controlled value into a log line.
pub(crate) fn check_origin(
    headers: &HeaderMap,
    allowed_origins: &[String],
) -> Result<(), ApiError> {
    let Some(value) = headers.get(header::ORIGIN) else {
        return Err(ApiError::CsrfOriginMismatch(
            "missing Origin header".to_owned(),
        ));
    };
    let Ok(origin) = value.to_str() else {
        return Err(ApiError::CsrfOriginMismatch(
            "Origin header is not valid UTF-8".to_owned(),
        ));
    };
    if !allowed_origins.iter().any(|allowed| allowed == origin) {
        return Err(ApiError::CsrfOriginMismatch(
            "Origin not in allowed_origins".to_owned(),
        ));
    }
    Ok(())
}

/// axum extractor wrapper around [`check_origin`].
///
/// Implemented as `FromRequestParts` so it runs **before** any body-
/// consuming extractor in the same handler signature. Handlers that
/// already need a [`HeaderMap`] for unrelated reasons (the auth-routes
/// module reads the `Cookie:` header on logout) can call
/// [`check_origin`] directly instead — both shapes share the same
/// policy.
///
/// Place this extractor first in the handler signature when used:
///
/// ```ignore
/// async fn create_thing(
///     _csrf: CsrfGuard,
///     State(state): State<AppState>,
///     Json(req): Json<CreateThingRequest>,
/// ) -> Result<..., ApiError> { ... }
/// ```
///
/// Putting it after `Json<...>` would still produce the same 403 on a
/// bad `Origin` (axum runs `FromRequestParts` extractors before the
/// single `FromRequest` body extractor regardless of source order),
/// but listing it first keeps the call-site self-documenting.
///
/// **Scope.** Every state-changing browser-write route consumes this
/// extractor (the four `/api/v1/auth/*` writes plus every protected
/// `/api/v1/*` mutation). GET / WebSocket-upgrade routes do not take
/// `CsrfGuard` — they rely on [`AuthenticatedUser`](super::user::AuthenticatedUser)
/// for their auth gate.
#[derive(Debug, Clone, Copy)]
pub struct CsrfGuard;

impl<S> FromRequestParts<S> for CsrfGuard
where
    S: Send + Sync,
    AppState: FromRef<S>,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let app_state = AppState::from_ref(state);
        check_origin(&parts.headers, &app_state.auth_routes.allowed_origins)?;
        Ok(Self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn allowed(origins: &[&str]) -> Vec<String> {
        origins.iter().map(|s| (*s).to_owned()).collect()
    }

    fn headers_with_origin(value: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(header::ORIGIN, HeaderValue::from_str(value).unwrap());
        h
    }

    #[test]
    fn allows_exact_match() {
        let list = allowed(&["https://relay.example.com"]);
        check_origin(&headers_with_origin("https://relay.example.com"), &list).unwrap();
    }

    #[test]
    fn allows_when_origin_is_one_of_many_allowed() {
        let list = allowed(&[
            "https://relay.example.com",
            "https://staging.example.com",
            "tauri://localhost",
        ]);
        check_origin(&headers_with_origin("https://staging.example.com"), &list).unwrap();
        check_origin(&headers_with_origin("tauri://localhost"), &list).unwrap();
    }

    #[test]
    fn rejects_disallowed_origin() {
        let list = allowed(&["https://relay.example.com"]);
        let err =
            check_origin(&headers_with_origin("https://evil.example.com"), &list).unwrap_err();
        assert!(matches!(err, ApiError::CsrfOriginMismatch(_)));
    }

    #[test]
    fn rejects_missing_origin_header() {
        let list = allowed(&["https://relay.example.com"]);
        let err = check_origin(&HeaderMap::new(), &list).unwrap_err();
        assert!(matches!(err, ApiError::CsrfOriginMismatch(_)));
    }

    #[test]
    fn empty_allow_list_rejects_every_origin() {
        let list = allowed(&[]);
        let err =
            check_origin(&headers_with_origin("https://relay.example.com"), &list).unwrap_err();
        assert!(matches!(err, ApiError::CsrfOriginMismatch(_)));
    }

    #[test]
    fn rejects_non_utf8_origin_header() {
        let list = allowed(&["https://relay.example.com"]);
        let mut h = HeaderMap::new();
        h.insert(
            header::ORIGIN,
            HeaderValue::from_bytes(&[0xff, 0xfe, 0xfd]).unwrap(),
        );
        let err = check_origin(&h, &list).unwrap_err();
        assert!(matches!(err, ApiError::CsrfOriginMismatch(_)));
    }

    #[test]
    fn comparison_is_case_sensitive() {
        // Browsers serialise scheme + host as lower-case, so an exact-
        // match check is sufficient; documenting the policy here so a
        // future "make it case-insensitive" change is a deliberate one.
        let list = allowed(&["https://relay.example.com"]);
        let err =
            check_origin(&headers_with_origin("https://Relay.Example.Com"), &list).unwrap_err();
        assert!(matches!(err, ApiError::CsrfOriginMismatch(_)));
    }

    #[test]
    fn rejects_trailing_slash_variant() {
        // The `Origin` header carries scheme+host(+port) only — never a
        // trailing slash. Pinning the policy so a future allow-list
        // entry that accidentally includes one fails closed.
        let list = allowed(&["https://relay.example.com/"]);
        let err =
            check_origin(&headers_with_origin("https://relay.example.com"), &list).unwrap_err();
        assert!(matches!(err, ApiError::CsrfOriginMismatch(_)));
    }

    #[test]
    fn sentinel_origin_value_does_not_appear_in_error_detail() {
        // A probe-controlled `Origin` value MUST NOT be echoed in the
        // wrapped operator-side detail string — that string is what
        // the warn! line in `error.rs::IntoResponse` formats.
        let sentinel = "https://CSRF-SENTINEL-MARKER-XYZZY.example.com";
        let list = allowed(&["https://relay.example.com"]);
        let err = check_origin(&headers_with_origin(sentinel), &list).unwrap_err();
        let ApiError::CsrfOriginMismatch(detail) = err else {
            panic!("expected CsrfOriginMismatch");
        };
        assert!(
            !detail.contains("CSRF-SENTINEL-MARKER-XYZZY"),
            "operator-side detail must not echo the offered Origin: {detail}"
        );
        assert!(
            !detail.contains(sentinel),
            "operator-side detail must not echo the offered Origin: {detail}"
        );
    }

    #[test]
    fn sentinel_bootstrap_token_shaped_origin_does_not_leak() {
        // A pathological `Origin` that looks like a bootstrap token
        // MUST NOT survive into the error detail either — the same
        // redaction guarantee covers any probe-controlled value.
        let token_shaped = "https://AAAA-BOOTSTRAP-TOKEN-MARKER-AAAA.example.com";
        let list = allowed(&["https://relay.example.com"]);
        let err = check_origin(&headers_with_origin(token_shaped), &list).unwrap_err();
        let ApiError::CsrfOriginMismatch(detail) = err else {
            panic!("expected CsrfOriginMismatch");
        };
        assert!(!detail.contains("BOOTSTRAP-TOKEN-MARKER"));
    }
}
