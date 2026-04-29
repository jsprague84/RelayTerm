//! Validation helpers for user-supplied strings that cross the backend
//! boundary into the domain model.
//!
//! Each helper returns an owned, normalized form so callers can persist the
//! validated value directly. Validation is intentionally conservative — the
//! goal is to reject obviously malformed input early, not to be a full
//! conformance checker for, e.g., DNS or SSH.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Maximum number of tags accepted on a single domain entity.
pub const MAX_TAGS: usize = 32;

/// Reasons an input value can be rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ValidationError {
    #[error("{field} must not be empty")]
    Empty { field: &'static str },
    #[error("{field} must be at most {max} characters (got {actual})")]
    TooLong {
        field: &'static str,
        max: usize,
        actual: usize,
    },
    #[error("{field} must not contain control characters")]
    ControlChar { field: &'static str },
    #[error("{field} must not contain whitespace")]
    Whitespace { field: &'static str },
    #[error("{field} contains an invalid character: {ch:?}")]
    InvalidChar { field: &'static str, ch: char },
    #[error("{field} must not start or end with whitespace")]
    Surrounding { field: &'static str },
    #[error("{field} must start with a letter or underscore")]
    BadLeadingChar { field: &'static str },
    #[error("{field} must be in range {min}..={max} (got {actual})")]
    OutOfRange {
        field: &'static str,
        min: u32,
        max: u32,
        actual: u32,
    },
    #[error("too many {field}: {actual} > {max}")]
    TooMany {
        field: &'static str,
        max: usize,
        actual: usize,
    },
    #[error("duplicate {field}: {value}")]
    Duplicate { field: &'static str, value: String },
}

/// Validated newtype wrappers around `String`/`u16` so that "I have a thing
/// that has been validated" is encoded in the type signature.
macro_rules! string_newtype {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }

            #[must_use]
            pub fn into_string(self) -> String {
                self.0
            }

            /// Reconstruct a value that was previously validated by this
            /// crate's validators before persistence (e.g. a row written
            /// through the validated boundary and read back from the
            /// database).
            ///
            /// **Trusted-source only.** This is an internal reconstruction
            /// hook for the persistence layer, NOT a general escape hatch
            /// from validation. For untrusted input — anything entering
            /// across the HTTP / WebSocket / IPC boundary, anything from a
            /// config file, anything from the renderer — call the
            /// corresponding `validate_*` function instead.
            #[must_use]
            pub fn from_validated(value: String) -> Self {
                Self(value)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }
    };
}

string_newtype!(HostDisplayName);
string_newtype!(Hostname);
string_newtype!(SshUsername);
string_newtype!(ProfileName);
string_newtype!(Tag);

/// Validated SSH port (1..=65535). Wraps `u16` because port `0` is invalid
/// for an outbound SSH connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SshPort(u16);

impl SshPort {
    pub const DEFAULT: Self = Self(22);

    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }

    /// Reconstruct a port value that was previously validated by
    /// [`validate_ssh_port`] before persistence.
    ///
    /// **Trusted-source only.** This is an internal reconstruction hook for
    /// the persistence layer, NOT a general escape hatch from validation.
    /// For untrusted input call [`validate_ssh_port`] instead.
    #[must_use]
    pub const fn from_validated(port: u16) -> Self {
        Self(port)
    }
}

impl fmt::Display for SshPort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

/// Validate a host display name shown in the UI.
///
/// Rules: 1..=128 chars after trimming, no control characters, no leading or
/// trailing whitespace in the trimmed form.
pub fn validate_host_display_name(input: &str) -> Result<HostDisplayName, ValidationError> {
    const FIELD: &str = "host display name";
    const MAX: usize = 128;

    if input.is_empty() {
        return Err(ValidationError::Empty { field: FIELD });
    }
    if input != input.trim() {
        return Err(ValidationError::Surrounding { field: FIELD });
    }
    if input.chars().count() > MAX {
        return Err(ValidationError::TooLong {
            field: FIELD,
            max: MAX,
            actual: input.chars().count(),
        });
    }
    if input.chars().any(char::is_control) {
        return Err(ValidationError::ControlChar { field: FIELD });
    }
    Ok(HostDisplayName(input.to_owned()))
}

/// Validate a hostname or address (IPv4, IPv6, or DNS name).
///
/// Rules: 1..=253 chars (DNS limit), no whitespace, no control characters,
/// only ASCII alphanumerics, `-`, `.`, `:`, `[`, or `]`.
pub fn validate_hostname(input: &str) -> Result<Hostname, ValidationError> {
    const FIELD: &str = "hostname";
    const MAX: usize = 253;

    if input.is_empty() {
        return Err(ValidationError::Empty { field: FIELD });
    }
    if input.len() > MAX {
        return Err(ValidationError::TooLong {
            field: FIELD,
            max: MAX,
            actual: input.len(),
        });
    }
    for ch in input.chars() {
        if ch.is_whitespace() {
            return Err(ValidationError::Whitespace { field: FIELD });
        }
        if ch.is_control() {
            return Err(ValidationError::ControlChar { field: FIELD });
        }
        let ok = ch.is_ascii_alphanumeric() || matches!(ch, '-' | '.' | ':' | '[' | ']' | '_');
        if !ok {
            return Err(ValidationError::InvalidChar { field: FIELD, ch });
        }
    }
    Ok(Hostname(input.to_owned()))
}

/// Validate an SSH port number (1..=65535).
pub fn validate_ssh_port(port: u32) -> Result<SshPort, ValidationError> {
    const FIELD: &str = "ssh port";
    if !(1..=u32::from(u16::MAX)).contains(&port) {
        return Err(ValidationError::OutOfRange {
            field: FIELD,
            min: 1,
            max: u32::from(u16::MAX),
            actual: port,
        });
    }
    #[allow(clippy::cast_possible_truncation)]
    Ok(SshPort(port as u16))
}

/// Validate an SSH username.
///
/// Rules: 1..=64 chars, must start with ASCII letter or `_`, remaining chars
/// are ASCII alphanumeric, `-`, `_`, or `.`.
pub fn validate_ssh_username(input: &str) -> Result<SshUsername, ValidationError> {
    const FIELD: &str = "ssh username";
    const MAX: usize = 64;

    if input.is_empty() {
        return Err(ValidationError::Empty { field: FIELD });
    }
    if input.len() > MAX {
        return Err(ValidationError::TooLong {
            field: FIELD,
            max: MAX,
            actual: input.len(),
        });
    }
    let mut chars = input.chars();
    let first = chars.next().expect("non-empty by check above");
    if !(first.is_ascii_alphabetic() || first == '_') {
        return Err(ValidationError::BadLeadingChar { field: FIELD });
    }
    for ch in chars {
        let ok = ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.');
        if !ok {
            return Err(ValidationError::InvalidChar { field: FIELD, ch });
        }
    }
    Ok(SshUsername(input.to_owned()))
}

/// Validate a server-profile name shown in the UI and used as a stable label.
///
/// Rules: 1..=64 chars after trimming, no control characters, no leading or
/// trailing whitespace.
pub fn validate_profile_name(input: &str) -> Result<ProfileName, ValidationError> {
    const FIELD: &str = "profile name";
    const MAX: usize = 64;

    if input.is_empty() {
        return Err(ValidationError::Empty { field: FIELD });
    }
    if input != input.trim() {
        return Err(ValidationError::Surrounding { field: FIELD });
    }
    if input.chars().count() > MAX {
        return Err(ValidationError::TooLong {
            field: FIELD,
            max: MAX,
            actual: input.chars().count(),
        });
    }
    if input.chars().any(char::is_control) {
        return Err(ValidationError::ControlChar { field: FIELD });
    }
    Ok(ProfileName(input.to_owned()))
}

/// Validate a single tag.
///
/// Rules: 1..=32 chars, ASCII alphanumeric, `-`, `_`. No whitespace.
pub fn validate_tag(input: &str) -> Result<Tag, ValidationError> {
    const FIELD: &str = "tag";
    const MAX: usize = 32;

    if input.is_empty() {
        return Err(ValidationError::Empty { field: FIELD });
    }
    if input.len() > MAX {
        return Err(ValidationError::TooLong {
            field: FIELD,
            max: MAX,
            actual: input.len(),
        });
    }
    for ch in input.chars() {
        let ok = ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_');
        if !ok {
            return Err(ValidationError::InvalidChar { field: FIELD, ch });
        }
    }
    Ok(Tag(input.to_owned()))
}

/// Validate a list of tags. Rejects empty entries, duplicates, and lists
/// longer than [`MAX_TAGS`].
pub fn validate_tags(inputs: &[&str]) -> Result<Vec<Tag>, ValidationError> {
    if inputs.len() > MAX_TAGS {
        return Err(ValidationError::TooMany {
            field: "tags",
            max: MAX_TAGS,
            actual: inputs.len(),
        });
    }
    let mut out = Vec::with_capacity(inputs.len());
    for raw in inputs {
        let tag = validate_tag(raw)?;
        if out.iter().any(|t: &Tag| t.as_str() == tag.as_str()) {
            return Err(ValidationError::Duplicate {
                field: "tag",
                value: tag.into_string(),
            });
        }
        out.push(tag);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_display_name_accepts_normal_strings() {
        let v = validate_host_display_name("Prod DB (us-east-1)").unwrap();
        assert_eq!(v.as_str(), "Prod DB (us-east-1)");
    }

    #[test]
    fn host_display_name_rejects_empty() {
        let err = validate_host_display_name("").unwrap_err();
        assert!(matches!(err, ValidationError::Empty { .. }));
    }

    #[test]
    fn host_display_name_rejects_surrounding_whitespace() {
        let err = validate_host_display_name(" foo ").unwrap_err();
        assert!(matches!(err, ValidationError::Surrounding { .. }));
    }

    #[test]
    fn host_display_name_rejects_control_chars() {
        let err = validate_host_display_name("foo\nbar").unwrap_err();
        assert!(matches!(err, ValidationError::ControlChar { .. }));
    }

    #[test]
    fn host_display_name_rejects_too_long() {
        let s = "a".repeat(129);
        let err = validate_host_display_name(&s).unwrap_err();
        assert!(matches!(err, ValidationError::TooLong { .. }));
    }

    #[test]
    fn hostname_accepts_dns_name() {
        validate_hostname("db-1.internal.example.com").unwrap();
    }

    #[test]
    fn hostname_accepts_ipv4() {
        validate_hostname("10.0.0.5").unwrap();
    }

    #[test]
    fn hostname_accepts_ipv6_bracketed() {
        validate_hostname("[2001:db8::1]").unwrap();
    }

    #[test]
    fn hostname_rejects_whitespace() {
        let err = validate_hostname("bad host").unwrap_err();
        assert!(matches!(err, ValidationError::Whitespace { .. }));
    }

    #[test]
    fn hostname_rejects_invalid_char() {
        let err = validate_hostname("host;rm-rf").unwrap_err();
        assert!(matches!(err, ValidationError::InvalidChar { .. }));
    }

    #[test]
    fn ssh_port_accepts_22() {
        let p = validate_ssh_port(22).unwrap();
        assert_eq!(p.get(), 22);
    }

    #[test]
    fn ssh_port_rejects_zero() {
        let err = validate_ssh_port(0).unwrap_err();
        assert!(matches!(err, ValidationError::OutOfRange { .. }));
    }

    #[test]
    fn ssh_port_rejects_too_large() {
        let err = validate_ssh_port(70_000).unwrap_err();
        assert!(matches!(err, ValidationError::OutOfRange { .. }));
    }

    #[test]
    fn ssh_username_accepts_normal() {
        validate_ssh_username("deploy").unwrap();
        validate_ssh_username("root").unwrap();
        validate_ssh_username("svc-build_42").unwrap();
        validate_ssh_username("_systemd").unwrap();
    }

    #[test]
    fn ssh_username_rejects_leading_digit() {
        let err = validate_ssh_username("1abc").unwrap_err();
        assert!(matches!(err, ValidationError::BadLeadingChar { .. }));
    }

    #[test]
    fn ssh_username_rejects_invalid_char() {
        let err = validate_ssh_username("user@host").unwrap_err();
        assert!(matches!(err, ValidationError::InvalidChar { .. }));
    }

    #[test]
    fn profile_name_accepts_normal() {
        validate_profile_name("Prod / us-east-1").unwrap();
    }

    #[test]
    fn profile_name_rejects_too_long() {
        let s = "a".repeat(65);
        let err = validate_profile_name(&s).unwrap_err();
        assert!(matches!(err, ValidationError::TooLong { .. }));
    }

    #[test]
    fn tag_accepts_simple() {
        validate_tag("prod").unwrap();
        validate_tag("us-east-1").unwrap();
        validate_tag("k8s_node").unwrap();
    }

    #[test]
    fn tag_rejects_invalid_char() {
        let err = validate_tag("with space").unwrap_err();
        assert!(matches!(err, ValidationError::InvalidChar { .. }));
    }

    #[test]
    fn tags_reject_duplicates() {
        let err = validate_tags(&["prod", "prod"]).unwrap_err();
        assert!(matches!(err, ValidationError::Duplicate { .. }));
    }

    #[test]
    fn tags_reject_too_many() {
        let many: Vec<String> = (0..(MAX_TAGS + 1)).map(|i| format!("t{i}")).collect();
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();
        let err = validate_tags(&refs).unwrap_err();
        assert!(matches!(err, ValidationError::TooMany { .. }));
    }

    #[test]
    fn tags_accept_max() {
        let many: Vec<String> = (0..MAX_TAGS).map(|i| format!("t{i}")).collect();
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();
        let out = validate_tags(&refs).unwrap();
        assert_eq!(out.len(), MAX_TAGS);
    }
}
