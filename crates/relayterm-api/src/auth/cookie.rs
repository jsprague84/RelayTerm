//! Cookie parsing helpers shared between the `/api/v1/auth/*` routes
//! and the [`AuthenticatedUser`](super::user::AuthenticatedUser)
//! extractor.
//!
//! Hand-rolled rather than via `axum-extra` to avoid a workspace
//! dependency and to keep redaction posture local: the token never
//! reaches a typed wrapper — the caller owns the `&str` borrow for the
//! lifetime of the request.
//!
//! ## Match policy
//!
//! Matching is **exact** on the cookie name. A cookie called
//! `relayterm_session_other` or `fake_relayterm_session` MUST NOT be
//! returned for [`SESSION_COOKIE_NAME`] — those would let a sibling
//! application on the same domain mint a fake session. The unit tests
//! pin this behaviour under "prefix" / "suffix" confusion fixtures.

use axum::http::{HeaderMap, header};

/// Browser session cookie name. Stable wire identifier — clients (the
/// Tauri shells, future API integration tests) MUST NOT depend on this
/// being changeable per environment. Pinned at SPEC.md "Session model →
/// Cookie configuration".
pub(crate) const SESSION_COOKIE_NAME: &str = "relayterm_session";

/// Pull the session token value out of the `Cookie:` header.
///
/// Returns `None` when:
/// * the header is missing,
/// * the header is not valid UTF-8,
/// * no pair in the header matches [`SESSION_COOKIE_NAME`] exactly,
/// * the matching pair has an empty value (treated as absent so the
///   downstream validator returns the SAME 401 it returns for a
///   stranger token — a probe must not learn that the cookie was
///   present-but-empty).
///
/// Never echoes the cookie value back to the caller; the redacted
/// `Debug` impls on [`SessionToken`](relayterm_auth::SessionToken) and
/// [`SessionTokenHash`](relayterm_auth::SessionTokenHash) are the
/// downstream backstop.
pub(crate) fn extract_session_cookie(headers: &HeaderMap) -> Option<&str> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    for pair in raw.split(';') {
        let pair = pair.trim();
        let Some(eq) = pair.find('=') else {
            continue;
        };
        let (name, rest) = pair.split_at(eq);
        // `rest` includes the leading '='; skip it.
        let value = &rest[1..];
        // Exact-match on the cookie name. A `relayterm_session_other`
        // or `fake_relayterm_session` pair does NOT win this loop —
        // the `name == SESSION_COOKIE_NAME` comparison rejects both.
        if name == SESSION_COOKIE_NAME {
            if value.is_empty() {
                return None;
            }
            return Some(value);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn header_with_cookie(value: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(header::COOKIE, HeaderValue::from_str(value).unwrap());
        h
    }

    #[test]
    fn finds_single_session_cookie() {
        let h = header_with_cookie("relayterm_session=abc-def");
        assert_eq!(extract_session_cookie(&h), Some("abc-def"));
    }

    #[test]
    fn finds_session_cookie_among_many() {
        let h = header_with_cookie("foo=bar; relayterm_session=abc-def; baz=qux");
        assert_eq!(extract_session_cookie(&h), Some("abc-def"));
    }

    #[test]
    fn returns_none_when_cookie_header_absent() {
        assert_eq!(extract_session_cookie(&HeaderMap::new()), None);
    }

    #[test]
    fn returns_none_when_session_cookie_absent() {
        let h = header_with_cookie("foo=bar; baz=qux");
        assert_eq!(extract_session_cookie(&h), None);
    }

    #[test]
    fn returns_none_when_value_empty() {
        // An empty value behaves the same as a missing cookie — the
        // downstream validator would 401 on it anyway, and collapsing
        // the two paths keeps the wire response identical.
        let h = header_with_cookie("relayterm_session=");
        assert_eq!(extract_session_cookie(&h), None);
    }

    #[test]
    fn returns_none_for_non_utf8_cookie_header() {
        let mut h = HeaderMap::new();
        h.insert(
            header::COOKIE,
            HeaderValue::from_bytes(&[0xff, 0xfe, 0xfd]).unwrap(),
        );
        assert_eq!(extract_session_cookie(&h), None);
    }

    #[test]
    fn ignores_pair_without_equals_sign() {
        // A bare token without an `=` is not a valid cookie pair; the
        // parser MUST skip it without panicking and continue scanning.
        let h = header_with_cookie("garbage; relayterm_session=ok-token");
        assert_eq!(extract_session_cookie(&h), Some("ok-token"));
    }

    #[test]
    fn rejects_prefix_named_cookie() {
        // `relayterm_session_other` shares a prefix with the real
        // session cookie name — an exact-match parser MUST NOT return
        // its value. A naive `starts_with` parser would let a sibling
        // app on the same domain mint a fake session.
        let h = header_with_cookie("relayterm_session_other=evil-prefix-token");
        assert_eq!(extract_session_cookie(&h), None);
    }

    #[test]
    fn rejects_suffix_named_cookie() {
        // `fake_relayterm_session` shares a suffix with the real
        // session cookie name — an exact-match parser MUST NOT return
        // its value either.
        let h = header_with_cookie("fake_relayterm_session=evil-suffix-token");
        assert_eq!(extract_session_cookie(&h), None);
    }

    #[test]
    fn prefers_first_session_cookie_when_duplicated() {
        // Header with two `relayterm_session=` pairs (a malformed
        // header, but possible from a misbehaving client). The parser
        // returns the first match — pinning the behaviour so a future
        // refactor can't silently flip to "last wins" and let a probe
        // override a real session.
        let h = header_with_cookie("relayterm_session=first; relayterm_session=second");
        assert_eq!(extract_session_cookie(&h), Some("first"));
    }

    #[test]
    fn tolerates_extra_whitespace_around_pairs() {
        let h = header_with_cookie("foo=bar ;   relayterm_session=spaced  ; baz=qux");
        assert_eq!(extract_session_cookie(&h), Some("spaced"));
    }
}
