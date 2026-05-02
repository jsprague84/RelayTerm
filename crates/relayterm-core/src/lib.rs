//! Shared domain types for RelayTerm.
//!
//! This crate is the canonical location for ID newtypes, value types, and
//! pure domain records used across the workspace. It deliberately avoids
//! transport-layer or persistence-layer dependencies (no Axum, no SQLx) so
//! it can be pulled into any other crate without creating cycles.

pub mod audit_event;
pub mod host;
pub mod ids;
pub mod known_host;
pub mod password_credential;
pub mod repository;
pub mod server_profile;
pub mod session_event;
pub mod ssh_identity;
pub mod terminal_recording;
pub mod terminal_session;
pub mod user;
pub mod user_session;
pub mod validation;

pub use audit_event::{AuditEvent, AuditEventKind};
pub use host::Host;
pub use ids::{
    AuditEventId, HostId, KnownHostEntryId, ServerProfileId, SessionEventId, SshIdentityId,
    TerminalRecordingChunkId, TerminalRecordingMarkerId, TerminalSessionAttachmentId,
    TerminalSessionId, UserId, UserSessionId,
};
pub use known_host::KnownHostEntry;
pub use password_credential::PasswordCredential;
pub use repository::{
    AuditEventRepository, CreateAuditEvent, CreateHost, CreateKnownHostEntry,
    CreatePasswordCredential, CreateServerProfile, CreateSessionEvent, CreateSshIdentity,
    CreateTerminalRecordingChunk, CreateTerminalRecordingMarker, CreateTerminalSession,
    CreateTerminalSessionAttachment, CreateUser, CreateUserSession, HostRepository,
    KnownHostEntryRepository, PasswordCredentialRepository, RepositoryError,
    ServerProfileRepository, SessionEventRepository, SshIdentityRepository,
    TerminalRecordingRepository, TerminalSessionRepository, UserRepository, UserSessionRepository,
};
pub use server_profile::ServerProfile;
pub use session_event::{SessionEvent, SessionEventKind};
pub use ssh_identity::{SshIdentity, SshKeyType};
pub use terminal_recording::{
    TerminalRecordingChunk, TerminalRecordingCompression, TerminalRecordingMarker,
    TerminalRecordingMarkerKind, TerminalRecordingPayloadEncryption,
};
pub use terminal_session::{TerminalSession, TerminalSessionAttachment, TerminalSessionStatus};
pub use user::User;
pub use user_session::UserSession;
pub use validation::{
    HostDisplayName, Hostname, ProfileName, SshPort, SshUsername, Tag, ValidationError,
    validate_host_display_name, validate_hostname, validate_profile_name, validate_ssh_port,
    validate_ssh_username, validate_tag, validate_tags,
};

use serde::{Deserialize, Serialize};

/// Identifier for a long-lived SSH session orchestrated by the backend.
///
/// This is an alias for [`TerminalSessionId`] to keep the wire protocol
/// terminology stable while the domain layer uses the more specific name.
pub type SessionId = TerminalSessionId;

/// Sequence number for terminal output events.
///
/// Monotonic per session; the orchestrator assigns the value, clients echo
/// it back on reconnect to request replay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SeqNo(pub u64);

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not implemented yet")]
    NotImplemented,
    #[error(transparent)]
    Validation(#[from] ValidationError),
}

pub type Result<T> = std::result::Result<T, Error>;
