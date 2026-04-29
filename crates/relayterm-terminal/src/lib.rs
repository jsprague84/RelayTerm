//! Session orchestrator.
//!
//! Owns the per-session sequence counter and the replay ring buffer, and is
//! responsible for serving reconnects from `(session_id, last_seen_seq)`.
//!
//! Implementation is deferred. This file declares the shape so callers can
//! compile without binding to internals yet.
