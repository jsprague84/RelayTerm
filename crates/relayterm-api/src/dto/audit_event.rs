//! Audit event response DTO.
//!
//! Wire shape for the read-only current-user audit feed. The DTO is
//! deliberately renderer/UI-agnostic — it carries safe public metadata
//! and a structured per-kind summary, NOT the raw payload that lives in
//! `audit_events.payload`.
//!
//! ## Redaction contract (security-critical)
//!
//! [`AuditEventResponse`] MUST NOT carry:
//!
//! - `private_key`, `encrypted_private_key`, PEM bytes, or public-key bytes
//! - terminal I/O, replay frames, peer banners
//! - raw russh, transport, or SQL error text
//! - vault internals (master key bytes, nonces, ciphertext)
//! - `client_info` blobs, `remote_addr`, or user-agent strings
//! - any field of an audit payload that wasn't explicitly allow-listed
//!   by [`AuditPayloadSummary`]
//!
//! Unknown audit kinds collapse to [`AuditPayloadSummary::Generic`] —
//! they intentionally drop the payload rather than echo it. The set is
//! closed by design: when a new audit kind grows a public surface, add
//! a sanitizer arm here AND a redaction-sentinel test.
//!
//! All redaction is enforced by sentinel-string tests in
//! [`crate::routes::v1::audit_events`] and the API integration test
//! crate; the sentinel list mirrors `AUDIT_FORBIDDEN_SUBSTRINGS`.

use chrono::{DateTime, Utc};
use relayterm_core::audit_event::{AuditEvent, AuditEventKind};
use relayterm_core::ids::AuditEventId;
use serde::Serialize;
use serde_json::Value as JsonValue;

/// Wire shape for one audit event in the current-user feed.
///
/// `actor_id` is intentionally omitted: the caller IS the actor, so
/// echoing it back would be redundant AND would invite a future drift
/// where a cross-user row leaks via copy-paste. `remote_addr` is
/// intentionally omitted: the lifecycle paths today never set it, and
/// surfacing client IPs to a normal user route is a separate slice.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct AuditEventResponse {
    pub(crate) id: AuditEventId,
    pub(crate) kind: &'static str,
    pub(crate) recorded_at: DateTime<Utc>,
    pub(crate) summary: AuditPayloadSummary,
}

/// Per-kind sanitized payload summary.
///
/// Each variant is a closed allow-list of fields the API is willing
/// to expose. Anything not listed here is dropped. The variant is
/// chosen by [`AuditEventKind`] in [`AuditEventResponse::from_event`];
/// kinds that don't have an explicit sanitizer fall through to
/// [`Self::Generic`] which carries no payload data at all.
#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum AuditPayloadSummary {
    /// `server_profile_created`, `server_profile_disabled`,
    /// `server_profile_enabled`. Mirrors the public fields of
    /// `routes/v1/server_profiles::write_lifecycle_audit`. Each field
    /// is `Option<String>` because a future payload schema change must
    /// degrade to "field absent" rather than rejecting the event —
    /// the audit feed should never go blank because of one malformed
    /// row.
    ServerProfileLifecycle {
        server_profile_id: Option<String>,
        name: Option<String>,
        host_id: Option<String>,
        ssh_identity_id: Option<String>,
        disabled_at: Option<DateTime<Utc>>,
    },
    /// Catch-all for kinds without an explicit sanitizer. Carries no
    /// payload data — the UI renders a generic "Audit event" line.
    Generic,
}

impl AuditEventResponse {
    /// Build the wire DTO from a domain [`AuditEvent`].
    ///
    /// Field selection here is the redaction backstop. Adding a new
    /// audit kind with a public surface means:
    ///
    /// 1. Add a sanitizer arm in [`AuditPayloadSummary`].
    /// 2. Wire it in the `match` below.
    /// 3. Add a redaction-sentinel test that constructs an `AuditEvent`
    ///    with a payload containing every name in
    ///    `AUDIT_FORBIDDEN_SUBSTRINGS` and asserts the serialised DTO
    ///    contains none of them.
    pub(crate) fn from_event(event: AuditEvent) -> Self {
        let summary = sanitize_payload(event.kind, &event.payload);
        Self {
            id: event.id,
            kind: event.kind.as_str(),
            recorded_at: event.recorded_at,
            summary,
        }
    }
}

fn sanitize_payload(kind: AuditEventKind, payload: &JsonValue) -> AuditPayloadSummary {
    match kind {
        AuditEventKind::ServerProfileCreated
        | AuditEventKind::ServerProfileDisabled
        | AuditEventKind::ServerProfileEnabled => AuditPayloadSummary::ServerProfileLifecycle {
            server_profile_id: copy_string_field(payload, "server_profile_id"),
            name: copy_string_field(payload, "name"),
            host_id: copy_string_field(payload, "host_id"),
            ssh_identity_id: copy_string_field(payload, "ssh_identity_id"),
            disabled_at: copy_timestamp_field(payload, "disabled_at"),
        },
        // Every other kind drops the payload. The set is closed —
        // adding a sanitizer is an explicit, reviewed change.
        _ => AuditPayloadSummary::Generic,
    }
}

/// Field-by-field copy of a string payload field. Anything that isn't
/// a JSON string (including `null`, numbers, objects) drops to `None`
/// rather than coercing — coercion is how a future schema change can
/// silently leak structure.
fn copy_string_field(payload: &JsonValue, field: &str) -> Option<String> {
    payload
        .get(field)
        .and_then(JsonValue::as_str)
        .map(str::to_owned)
}

/// Field-by-field copy of a string-shaped timestamp. The wire-side
/// audit payload writes `disabled_at` as a JSON string (RFC3339),
/// matching how the row is serialised everywhere else; we re-parse
/// rather than echo to keep the wire shape strongly typed.
fn copy_timestamp_field(payload: &JsonValue, field: &str) -> Option<DateTime<Utc>> {
    let raw = payload.get(field).and_then(JsonValue::as_str)?;
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use relayterm_core::ids::{AuditEventId, UserId};
    use serde_json::json;

    /// Mirrors the `AUDIT_FORBIDDEN_SUBSTRINGS` list in the API test
    /// crate. Sanitizer-level coverage so a single-file unit test
    /// catches a regression before the integration test runs.
    const FORBIDDEN: &[&str] = &[
        "encrypted_private_key",
        "private_key",
        "BEGIN OPENSSH PRIVATE KEY",
        "client_info",
        "remote_addr",
        "user_agent",
    ];

    fn event(kind: AuditEventKind, payload: JsonValue) -> AuditEvent {
        AuditEvent {
            id: AuditEventId::new(),
            actor_id: Some(UserId::new()),
            kind,
            payload,
            remote_addr: Some("10.0.0.1".to_owned()),
            recorded_at: Utc::now(),
        }
    }

    #[test]
    fn server_profile_created_summary_carries_only_allow_listed_fields() {
        let id = uuid::Uuid::new_v4();
        let host = uuid::Uuid::new_v4();
        let ident = uuid::Uuid::new_v4();
        let payload = json!({
            "server_profile_id": id,
            "name": "prod-bastion",
            "host_id": host,
            "ssh_identity_id": ident,
            "disabled_at": null,
            // These must NOT survive into the DTO.
            "encrypted_private_key": "BEGIN OPENSSH PRIVATE KEY...",
            "private_key": "PEM bytes",
            "client_info": "Mozilla/5.0",
            "remote_addr": "10.0.0.1",
            "user_agent": "evil",
        });
        let dto =
            AuditEventResponse::from_event(event(AuditEventKind::ServerProfileCreated, payload));
        assert_eq!(dto.kind, "server_profile_created");
        let AuditPayloadSummary::ServerProfileLifecycle {
            server_profile_id,
            name,
            host_id,
            ssh_identity_id,
            disabled_at,
        } = &dto.summary
        else {
            panic!("expected lifecycle summary, got {:?}", dto.summary);
        };
        assert_eq!(server_profile_id.as_deref(), Some(id.to_string().as_str()));
        assert_eq!(name.as_deref(), Some("prod-bastion"));
        assert_eq!(host_id.as_deref(), Some(host.to_string().as_str()));
        assert_eq!(ssh_identity_id.as_deref(), Some(ident.to_string().as_str()));
        assert!(disabled_at.is_none());

        let raw = serde_json::to_string(&dto).unwrap();
        for forbidden in FORBIDDEN {
            assert!(
                !raw.contains(forbidden),
                "DTO must not contain `{forbidden}`: {raw}",
            );
        }
    }

    #[test]
    fn server_profile_disabled_summary_carries_disabled_at_timestamp() {
        let payload = json!({
            "server_profile_id": uuid::Uuid::new_v4(),
            "name": "prod-bastion",
            "host_id": uuid::Uuid::new_v4(),
            "ssh_identity_id": uuid::Uuid::new_v4(),
            "disabled_at": "2026-05-01T12:34:56Z",
        });
        let dto =
            AuditEventResponse::from_event(event(AuditEventKind::ServerProfileDisabled, payload));
        let AuditPayloadSummary::ServerProfileLifecycle { disabled_at, .. } = &dto.summary else {
            panic!("expected lifecycle summary");
        };
        assert!(disabled_at.is_some());
    }

    #[test]
    fn unknown_kind_collapses_to_generic_summary() {
        // `Other` is the kind any unrecognised tag round-trips through;
        // its sanitizer arm must be Generic so payload data never leaks
        // for kinds we haven't explicitly allow-listed.
        let payload = json!({
            "private_key": "PEM bytes",
            "encrypted_private_key": "BEGIN OPENSSH PRIVATE KEY...",
            "raw_error": "russh internal: foo",
        });
        let dto = AuditEventResponse::from_event(event(AuditEventKind::Other, payload));
        assert!(matches!(dto.summary, AuditPayloadSummary::Generic));

        let raw = serde_json::to_string(&dto).unwrap();
        for forbidden in FORBIDDEN {
            assert!(
                !raw.contains(forbidden),
                "Generic summary must not echo payload (`{forbidden}` found): {raw}",
            );
        }
    }

    #[test]
    fn login_kinds_collapse_to_generic_summary() {
        // login_succeeded / login_failed belong to a future auth slice;
        // until then they must NOT echo their payload.
        for kind in [
            AuditEventKind::LoginSucceeded,
            AuditEventKind::LoginFailed,
            AuditEventKind::LogoutSucceeded,
            AuditEventKind::KeyVaultAccess,
            AuditEventKind::KeyVaultDecryptFailed,
            AuditEventKind::HostKeyAccepted,
            AuditEventKind::HostKeyMismatch,
            AuditEventKind::HostKeyRevoked,
            AuditEventKind::ServerProfileUpdated,
            AuditEventKind::ServerProfileDeleted,
            AuditEventKind::SshIdentityCreated,
            AuditEventKind::SshIdentityDeleted,
            AuditEventKind::SessionOpened,
            AuditEventKind::SessionClosed,
        ] {
            let dto = AuditEventResponse::from_event(event(
                kind,
                json!({
                    "method": "password",
                    "private_key": "leak",
                    "remote_addr": "evil",
                }),
            ));
            assert!(
                matches!(dto.summary, AuditPayloadSummary::Generic),
                "{kind:?} should default to Generic summary",
            );
            let raw = serde_json::to_string(&dto).unwrap();
            for forbidden in FORBIDDEN {
                assert!(
                    !raw.contains(forbidden),
                    "{kind:?} must not echo `{forbidden}`: {raw}",
                );
            }
        }
    }

    #[test]
    fn malformed_payload_degrades_to_field_absent() {
        // Wrong types in the payload must drop to None rather than
        // coercing — coercion is how a schema-shaped attack would
        // smuggle non-string content into a string field.
        let payload = json!({
            "server_profile_id": 42,
            "name": { "nested": "object" },
            "host_id": null,
            "ssh_identity_id": ["a", "b"],
            "disabled_at": "not-a-timestamp",
        });
        let dto =
            AuditEventResponse::from_event(event(AuditEventKind::ServerProfileCreated, payload));
        let AuditPayloadSummary::ServerProfileLifecycle {
            server_profile_id,
            name,
            host_id,
            ssh_identity_id,
            disabled_at,
        } = &dto.summary
        else {
            panic!("expected lifecycle summary");
        };
        assert!(server_profile_id.is_none());
        assert!(name.is_none());
        assert!(host_id.is_none());
        assert!(ssh_identity_id.is_none());
        assert!(disabled_at.is_none());
    }

    #[test]
    fn dto_omits_actor_id_and_remote_addr() {
        // The actor IS the caller. Re-emitting it would invite a future
        // drift where a cross-user row leaks. `remote_addr` is omitted
        // because the lifecycle paths today never set it AND because
        // exposing client IPs to a normal user route is a separate slice.
        let dto = AuditEventResponse::from_event(event(
            AuditEventKind::ServerProfileCreated,
            json!({
                "server_profile_id": uuid::Uuid::new_v4(),
                "name": "x",
            }),
        ));
        let raw = serde_json::to_string(&dto).unwrap();
        assert!(!raw.contains("actor_id"));
        assert!(!raw.contains("remote_addr"));
    }
}
