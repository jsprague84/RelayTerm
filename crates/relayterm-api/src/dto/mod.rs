//! Request / response DTOs.
//!
//! DTOs are deliberately separate from the domain types in `relayterm-core`
//! and from the row types in `relayterm-db`. They exist to:
//!
//! 1. Validate untrusted input at the HTTP boundary using the helpers in
//!    `relayterm_core::validation` before any value is handed to the
//!    repository.
//! 2. Filter the wire shape — no internal-only fields (e.g. `owner_id`),
//!    and **never** the encrypted private key on `SshIdentity`.

pub(crate) mod host;
pub(crate) mod server_profile;
pub(crate) mod ssh_identity;
