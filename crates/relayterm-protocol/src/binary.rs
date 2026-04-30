//! Binary frame envelope for the RelayTerm terminal data plane.
//!
//! ## Why a separate frame format
//!
//! The hot terminal data path — PTY output and keystroke input — moves
//! many small frames per second. JSON + base64 is fine for the control
//! plane (attach/detach/resize/replay control, errors, lifecycle), but
//! it inflates payload by ~33% and forces a base64 round trip on every
//! byte. Binary WebSocket frames carry raw bytes losslessly.
//!
//! The control plane stays JSON. Only `Output` (PTY → client) and
//! `Input` (client → PTY) move to binary. Distinguishing the two on the
//! wire is the role of the [`BinaryFrameKind`] byte.
//!
//! ## Wire format
//!
//! All multi-byte integers are big-endian. The fixed 20-byte header is:
//!
//! | offset | size | field                                                      |
//! |-------:|-----:|------------------------------------------------------------|
//! |     0  |   4  | magic `b"RTB1"` (0x52 0x54 0x42 0x31)                      |
//! |     4  |   1  | [`BinaryFrameKind`] byte                                   |
//! |     5  |   1  | flags (reserved, MUST be `0` in v1; readers ignore unknown bits) |
//! |     6  |   2  | reserved (MUST be `0`; readers ignore)                     |
//! |     8  |   8  | `seq` u64 (Output: orchestrator-stamped; Input: `0`)       |
//! |    16  |   4  | `payload_len` u32                                          |
//! |    20  |   N  | payload bytes                                              |
//!
//! Total wire size is `HEADER_LEN + payload_len`. Payloads are capped at
//! [`MAX_PAYLOAD_LEN`] (1 MiB) — readers MUST reject larger frames
//! BEFORE allocating a payload buffer, so a malicious peer cannot OOM
//! a process by claiming a huge length.
//!
//! ## Versioning
//!
//! The magic carries a `1` suffix. Future revisions append a new magic
//! (`b"RTB2"` etc.) and may run side-by-side; readers MUST reject any
//! magic they don't recognise.
//!
//! ## Logging and redaction
//!
//! Like [`crate::ClientMsg::Input`], [`BinaryFrame`] implements `Debug`
//! manually so the payload bytes never reach a tracing log or panic
//! backtrace. Only `kind`, `seq`, and `payload_len` are surfaced.

use std::fmt;

/// Magic bytes that prefix every binary frame in the v1 envelope.
pub const BINARY_MAGIC_V1: [u8; 4] = *b"RTB1";

/// Length of the fixed binary frame header (magic + kind + flags + reserved
/// + seq + payload_len). The payload follows immediately after.
pub const BINARY_HEADER_LEN: usize = 20;

/// Hard cap on a single binary frame's payload bytes. 1 MiB is comfortably
/// larger than any legitimate keystroke or PTY output chunk we expect on
/// a healthy session, while keeping a hostile peer from forcing a huge
/// allocation by claiming a `payload_len` of e.g. `u32::MAX`. Readers
/// MUST reject larger claimed lengths BEFORE allocating.
pub const MAX_PAYLOAD_LEN: usize = 1024 * 1024;

/// Kind tag distinguishing binary terminal data frames.
///
/// New variants append; never renumber. Readers MUST treat any unknown
/// byte as [`BinaryFrameDecodeError::UnknownKind`] and drop the frame
/// without echoing the payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum BinaryFrameKind {
    /// PTY output bytes from the remote shell to the renderer. The `seq`
    /// is the orchestrator-stamped monotonic counter (replay frames
    /// re-use the original `seq`).
    Output = 0x01,
    /// Renderer keystrokes / paste / etc. forwarded to the live PTY's
    /// stdin. `seq` is `0` and ignored on receive — input is not
    /// sequenced; the live SSH stream is the only ordering.
    Input = 0x02,
}

impl BinaryFrameKind {
    /// Decode a kind byte. Returns `None` for any byte that does not
    /// correspond to a known variant; the caller maps that to
    /// [`BinaryFrameDecodeError::UnknownKind`].
    #[must_use]
    pub const fn from_u8(byte: u8) -> Option<Self> {
        match byte {
            0x01 => Some(Self::Output),
            0x02 => Some(Self::Input),
            _ => None,
        }
    }

    /// Wire byte for this kind.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }
}

/// Decoded binary frame. The payload is held inline; the codec does not
/// borrow from the input slice so the caller is free to drop the source
/// buffer immediately after [`decode_binary_frame`] returns.
#[derive(Clone, PartialEq, Eq)]
pub struct BinaryFrame {
    pub kind: BinaryFrameKind,
    /// Sequence number stamped by the orchestrator (Output) or 0 (Input).
    pub seq: u64,
    pub payload: Vec<u8>,
}

impl fmt::Debug for BinaryFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Mirror the `ClientMsg::Input` redaction guard: payload bytes
        // never reach a tracing log or panic backtrace through the
        // automatic `Debug` impl. A future code path that formats a
        // frame for diagnostics gets length+kind+seq, no bytes.
        f.debug_struct("BinaryFrame")
            .field("kind", &self.kind)
            .field("seq", &self.seq)
            .field("payload_len", &self.payload.len())
            .field("payload", &"<redacted terminal bytes>")
            .finish()
    }
}

/// Failure modes from [`decode_binary_frame`]. Each variant is a short,
/// public classifier; the offending bytes are NEVER carried in the
/// error so a handler that surfaces it through tracing cannot leak
/// terminal data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryFrameDecodeError {
    /// Frame was shorter than [`BINARY_HEADER_LEN`] — header truncated.
    TruncatedHeader,
    /// Magic bytes did not match [`BINARY_MAGIC_V1`].
    BadMagic,
    /// Reserved fields (flags byte / reserved u16) were non-zero.
    /// Required `0` in v1; readers reject so a v2 sender that re-uses
    /// the byte for new semantics can't be silently misinterpreted.
    NonZeroReserved,
    /// Header parsed but `payload_len` exceeds [`MAX_PAYLOAD_LEN`]. The
    /// reader rejects BEFORE allocating, so an attacker cannot OOM a
    /// process by stamping `u32::MAX` here.
    PayloadTooLarge,
    /// Header's claimed `payload_len` does not match the bytes
    /// available in the buffer (frame truncated or trailing bytes).
    LengthMismatch,
    /// Kind byte was outside the known [`BinaryFrameKind`] set.
    UnknownKind,
}

impl fmt::Display for BinaryFrameDecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::TruncatedHeader => "truncated header",
            Self::BadMagic => "bad magic",
            Self::NonZeroReserved => "reserved bytes must be zero",
            Self::PayloadTooLarge => "payload exceeds maximum length",
            Self::LengthMismatch => "payload length mismatch",
            Self::UnknownKind => "unknown frame kind",
        };
        f.write_str(s)
    }
}

impl std::error::Error for BinaryFrameDecodeError {}

/// Encode a binary frame into a freshly allocated buffer suitable for a
/// `WebSocket::send_binary` call. The encoder rejects payloads larger
/// than [`MAX_PAYLOAD_LEN`] with [`BinaryFrameDecodeError::PayloadTooLarge`]
/// — emitting a frame the peer would itself reject is a logic bug we
/// surface at the call site rather than putting on the wire.
///
/// # Errors
///
/// Returns [`BinaryFrameDecodeError::PayloadTooLarge`] when the payload
/// is over the cap.
pub fn encode_binary_frame(
    kind: BinaryFrameKind,
    seq: u64,
    payload: &[u8],
) -> Result<Vec<u8>, BinaryFrameDecodeError> {
    if payload.len() > MAX_PAYLOAD_LEN {
        return Err(BinaryFrameDecodeError::PayloadTooLarge);
    }
    let mut buf = Vec::with_capacity(BINARY_HEADER_LEN + payload.len());
    buf.extend_from_slice(&BINARY_MAGIC_V1);
    buf.push(kind.as_u8());
    buf.push(0); // flags
    buf.extend_from_slice(&[0u8, 0u8]); // reserved
    buf.extend_from_slice(&seq.to_be_bytes());
    // Cast is safe: bounded by MAX_PAYLOAD_LEN above, well under u32::MAX.
    let len = u32::try_from(payload.len()).expect("payload len fits in u32 by cap");
    buf.extend_from_slice(&len.to_be_bytes());
    buf.extend_from_slice(payload);
    Ok(buf)
}

/// Decode a binary frame from an opaque byte slice (typically a
/// `Message::Binary` payload from the WebSocket transport).
///
/// The decoder NEVER includes the offending bytes in its error; the
/// caller maps a failure to a static, classifier-only response.
///
/// # Errors
///
/// See [`BinaryFrameDecodeError`].
pub fn decode_binary_frame(buf: &[u8]) -> Result<BinaryFrame, BinaryFrameDecodeError> {
    if buf.len() < BINARY_HEADER_LEN {
        return Err(BinaryFrameDecodeError::TruncatedHeader);
    }
    let magic = &buf[0..4];
    if magic != BINARY_MAGIC_V1 {
        return Err(BinaryFrameDecodeError::BadMagic);
    }
    let kind_byte = buf[4];
    let flags = buf[5];
    let reserved = u16::from_be_bytes([buf[6], buf[7]]);
    if flags != 0 || reserved != 0 {
        return Err(BinaryFrameDecodeError::NonZeroReserved);
    }
    let seq = u64::from_be_bytes([
        buf[8], buf[9], buf[10], buf[11], buf[12], buf[13], buf[14], buf[15],
    ]);
    let payload_len = u32::from_be_bytes([buf[16], buf[17], buf[18], buf[19]]) as usize;
    if payload_len > MAX_PAYLOAD_LEN {
        return Err(BinaryFrameDecodeError::PayloadTooLarge);
    }
    if buf.len() != BINARY_HEADER_LEN + payload_len {
        return Err(BinaryFrameDecodeError::LengthMismatch);
    }
    let kind = BinaryFrameKind::from_u8(kind_byte).ok_or(BinaryFrameDecodeError::UnknownKind)?;
    let payload = buf[BINARY_HEADER_LEN..].to_vec();
    Ok(BinaryFrame { kind, seq, payload })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_round_trip_preserves_seq_and_payload() {
        let payload: Vec<u8> = (0..=255u8).collect();
        let encoded = encode_binary_frame(BinaryFrameKind::Output, 42, &payload).unwrap();
        let decoded = decode_binary_frame(&encoded).unwrap();
        assert_eq!(decoded.kind, BinaryFrameKind::Output);
        assert_eq!(decoded.seq, 42);
        assert_eq!(decoded.payload, payload);
        // Length sanity: the wire frame is exactly header + payload.
        assert_eq!(encoded.len(), BINARY_HEADER_LEN + payload.len());
    }

    #[test]
    fn input_round_trip_with_zero_seq() {
        let payload = b"ls -la\n".to_vec();
        let encoded = encode_binary_frame(BinaryFrameKind::Input, 0, &payload).unwrap();
        let decoded = decode_binary_frame(&encoded).unwrap();
        assert_eq!(decoded.kind, BinaryFrameKind::Input);
        assert_eq!(decoded.seq, 0);
        assert_eq!(decoded.payload, payload);
    }

    #[test]
    fn empty_payload_round_trip() {
        let encoded = encode_binary_frame(BinaryFrameKind::Input, 0, b"").unwrap();
        let decoded = decode_binary_frame(&encoded).unwrap();
        assert_eq!(decoded.payload, Vec::<u8>::new());
        assert_eq!(encoded.len(), BINARY_HEADER_LEN);
    }

    #[test]
    fn malformed_magic_is_rejected() {
        let mut encoded = encode_binary_frame(BinaryFrameKind::Output, 1, b"x").unwrap();
        encoded[0] = b'X';
        assert_eq!(
            decode_binary_frame(&encoded),
            Err(BinaryFrameDecodeError::BadMagic),
        );
    }

    #[test]
    fn unknown_kind_is_rejected_safely() {
        let mut encoded = encode_binary_frame(BinaryFrameKind::Output, 1, b"x").unwrap();
        encoded[4] = 0xFF;
        let err = decode_binary_frame(&encoded).unwrap_err();
        assert_eq!(err, BinaryFrameDecodeError::UnknownKind);
    }

    #[test]
    fn truncated_header_is_rejected() {
        for trunc_len in 0..BINARY_HEADER_LEN {
            let buf = vec![0u8; trunc_len];
            assert_eq!(
                decode_binary_frame(&buf),
                Err(BinaryFrameDecodeError::TruncatedHeader),
                "len={trunc_len} must produce TruncatedHeader",
            );
        }
    }

    #[test]
    fn length_mismatch_is_rejected() {
        let encoded = encode_binary_frame(BinaryFrameKind::Output, 1, b"hello").unwrap();
        // Drop a byte from the payload — declared len won't match.
        let truncated = &encoded[..encoded.len() - 1];
        assert_eq!(
            decode_binary_frame(truncated),
            Err(BinaryFrameDecodeError::LengthMismatch),
        );
        // Append a trailing byte — still a mismatch.
        let mut overlong = encoded.clone();
        overlong.push(0);
        assert_eq!(
            decode_binary_frame(&overlong),
            Err(BinaryFrameDecodeError::LengthMismatch),
        );
    }

    #[test]
    fn oversized_claimed_length_is_rejected_without_allocating() {
        // Build a header that CLAIMS u32::MAX bytes of payload. The
        // decoder must reject on the length check BEFORE attempting
        // any payload-sized allocation.
        let mut buf = Vec::with_capacity(BINARY_HEADER_LEN);
        buf.extend_from_slice(&BINARY_MAGIC_V1);
        buf.push(BinaryFrameKind::Output.as_u8());
        buf.push(0);
        buf.extend_from_slice(&[0u8, 0u8]);
        buf.extend_from_slice(&0u64.to_be_bytes());
        buf.extend_from_slice(&u32::MAX.to_be_bytes());
        // Note: NO payload appended.
        assert_eq!(
            decode_binary_frame(&buf),
            Err(BinaryFrameDecodeError::PayloadTooLarge),
        );
    }

    #[test]
    fn encoder_refuses_oversized_payload() {
        let huge = vec![0u8; MAX_PAYLOAD_LEN + 1];
        assert_eq!(
            encode_binary_frame(BinaryFrameKind::Output, 1, &huge),
            Err(BinaryFrameDecodeError::PayloadTooLarge),
        );
    }

    #[test]
    fn nonzero_flags_or_reserved_is_rejected() {
        // Flag byte at offset 5.
        let mut encoded = encode_binary_frame(BinaryFrameKind::Output, 1, b"x").unwrap();
        encoded[5] = 0b0000_0001;
        assert_eq!(
            decode_binary_frame(&encoded),
            Err(BinaryFrameDecodeError::NonZeroReserved),
        );
        // Reserved u16 at offset 6..8.
        let mut encoded = encode_binary_frame(BinaryFrameKind::Output, 1, b"x").unwrap();
        encoded[7] = 0x01;
        assert_eq!(
            decode_binary_frame(&encoded),
            Err(BinaryFrameDecodeError::NonZeroReserved),
        );
    }

    #[test]
    fn debug_does_not_leak_payload() {
        let sentinel = b"REDACT-MARKER-BINARY-7C42";
        let frame = BinaryFrame {
            kind: BinaryFrameKind::Output,
            seq: 99,
            payload: sentinel.to_vec(),
        };
        let debug = format!("{frame:?}");
        let s = std::str::from_utf8(sentinel).unwrap();
        assert!(
            !debug.contains(s),
            "Debug output for BinaryFrame must NOT contain payload, got: {debug}",
        );
        assert!(
            debug.contains("payload_len"),
            "Debug should still surface the length: {debug}",
        );
    }

    #[test]
    fn maximum_payload_round_trip() {
        // Edge case: exactly MAX_PAYLOAD_LEN must round-trip without
        // tripping the oversize check on either side.
        let payload = vec![0xAAu8; MAX_PAYLOAD_LEN];
        let encoded = encode_binary_frame(BinaryFrameKind::Output, 1, &payload).unwrap();
        let decoded = decode_binary_frame(&encoded).unwrap();
        assert_eq!(decoded.payload.len(), MAX_PAYLOAD_LEN);
    }
}
