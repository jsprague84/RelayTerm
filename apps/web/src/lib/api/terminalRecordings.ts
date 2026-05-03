/**
 * Frontend helpers for the durable terminal recording read API.
 *
 * Wire surfaces (read-only, owner-scoped, no audit writes — see
 * `crates/relayterm-api/src/routes/v1/terminal_recordings.rs`):
 *  - `GET /api/v1/terminal-sessions/:id/recording/metadata`
 *  - `GET /api/v1/terminal-sessions/:id/recording/chunks?from_seq=&limit=`
 *  - `GET /api/v1/terminal-sessions/:id/recording/markers?from_seq=&limit=`
 *
 * Redaction posture (load-bearing — mirrors AGENTS.md "Things to avoid"
 * for chunk payload bytes):
 *  - {@link TerminalRecordingChunk.data_b64} carries base64 PTY OUTPUT
 *    bytes. Base64 is a wire shape, NOT a redaction layer. The bytes
 *    MUST NEVER be logged, written to localStorage / sessionStorage,
 *    surfaced through the formatted error helper, or appear in any
 *    `Error.message` thrown by these helpers. The
 *    {@link describeRecordingError} formatter is a function of `kind` +
 *    `status` + `code` ONLY; it never echoes the wire `message` of an
 *    HTTP error or the thrown `Error.message` of a transport failure.
 *  - {@link decodeRecordingChunk} is the single legitimate consumer of
 *    `data_b64`. It returns the decoded bytes for the replay viewer to
 *    hand directly to xterm. Callers MUST NOT stash the bytes in any
 *    persistent surface.
 *  - The helpers do NOT log raw response bodies. A future revision that
 *    adds tracing must keep the same rule — body fields can later carry
 *    encrypted-but-still-recoverable payload material.
 *
 * Authentication:
 *  - Every request uses `credentials: "include"` so the browser ships
 *    the session cookie (the auth gate is the canonical surface; the
 *    inventory helpers default to same-origin which is equivalent for
 *    same-origin builds, but the explicit setting future-proofs against
 *    a host-mismatch deployment).
 *  - The helpers do NOT authenticate themselves; the AppShell's auth
 *    gate is the single concern, and a 401 surfaces as a typed HTTP
 *    error here.
 *
 * Out of scope for this module:
 *  - Decryption / decompression: today both fields are `"none"` on the
 *    wire (writer contract). {@link parseRecordingChunk} accepts any
 *    string for forward compatibility; the consumer guard
 *    {@link isSupportedChunk} returns `false` when either field is
 *    something we cannot replay yet, so the viewer can show an
 *    explicit "unsupported" message instead of corrupting the screen.
 *  - Marker rendering: the helper exposes the raw JSON `payload` as
 *    `unknown`; the viewer treats it as metadata only and never as
 *    terminal bytes (writer contract: marker payloads are
 *    field-by-field, never byte material).
 */

import {
  readErrorEnvelope,
  type LoadOptions,
  type WireError,
} from "./apiErrors.js";

// ---------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------

/**
 * Aggregate metadata for a session's durable recording. Mirrors the
 * backend `TerminalRecordingMetadataResponse` (see
 * `crates/relayterm-api/src/dto/terminal_recording.rs`).
 *
 * Counts and seq bounds only — NEVER chunk bytes, NEVER marker bytes.
 * `has_recording` is `true` iff the session has at least one chunk OR
 * marker row, i.e. the same boolean the viewer gates the "load chunks"
 * pass on.
 */
export interface TerminalRecordingMetadata {
  terminal_session_id: string;
  has_recording: boolean;
  chunk_count: number;
  marker_count: number;
  /** Lowest `seq_start` across chunks; `null` when no chunks exist. */
  first_seq: number | null;
  /** Highest `seq_end` across chunks; `null` when no chunks exist. */
  last_seq: number | null;
  /** Earliest `created_at` across chunk OR marker rows; ISO 8601. */
  first_recorded_at: string | null;
  /** Latest `created_at` across chunk OR marker rows; ISO 8601. */
  last_recorded_at: string | null;
}

/**
 * One persisted recording chunk on the read API. Mirrors the backend
 * `TerminalRecordingChunkResponse`.
 *
 * `data_b64` is the ONLY surface that carries chunk bytes; the
 * {@link decodeRecordingChunk} helper is the only legitimate consumer.
 * Today both `encryption` and `compression` are `"none"` on the wire
 * (writer contract); the parser accepts any string so a future
 * `recording_v1` / `zstd` rev does not crash older clients, and the
 * consumer guard {@link isSupportedChunk} screens before decode.
 */
export interface TerminalRecordingChunk {
  seq_start: number;
  seq_end: number;
  byte_len: number;
  /** base64 (RFC-4648 standard alphabet) — see module docs. */
  data_b64: string;
  encryption: string;
  compression: string;
  created_at: string;
}

/** The marker `kind` enum tags mirrored from
 * `relayterm_core::terminal_recording::TerminalRecordingMarkerKind`.
 * The parser accepts only these tags; an unknown tag rejects the row
 * (viewer surfaces a malformed-response error). */
export type TerminalRecordingMarkerKind =
  | "started"
  | "attached"
  | "detached"
  | "reattached"
  | "resized"
  | "closed"
  | "replay_gap";

const MARKER_KINDS: ReadonlySet<TerminalRecordingMarkerKind> = new Set([
  "started",
  "attached",
  "detached",
  "reattached",
  "resized",
  "closed",
  "replay_gap",
]);

/**
 * One persisted recording marker on the read API. Mirrors the backend
 * `TerminalRecordingMarkerResponse`.
 *
 * `payload` is opaque JSON metadata — counts, dimensions, reason codes.
 * NEVER PTY bytes by writer contract; the viewer renders it as metadata
 * (key/value display) only, never as terminal output.
 */
export interface TerminalRecordingMarker {
  kind: TerminalRecordingMarkerKind;
  seq: number;
  payload: unknown;
  created_at: string;
}

// ---------------------------------------------------------------------------
// Parsers
// ---------------------------------------------------------------------------

/**
 * Parse a metadata wire body field-by-field. Unknown extra fields are
 * dropped silently (forward compatibility). Returns `null` if any
 * required field is missing or has the wrong shape.
 */
export function parseRecordingMetadata(
  raw: unknown,
): TerminalRecordingMetadata | null {
  if (!raw || typeof raw !== "object") return null;
  const r = raw as Record<string, unknown>;
  if (
    typeof r.terminal_session_id !== "string" ||
    typeof r.has_recording !== "boolean" ||
    !isFiniteInt(r.chunk_count) ||
    !isFiniteInt(r.marker_count)
  ) {
    return null;
  }
  const firstSeq = parseNullableInt(r.first_seq);
  if (firstSeq === undefined) return null;
  const lastSeq = parseNullableInt(r.last_seq);
  if (lastSeq === undefined) return null;
  const firstAt = parseNullableString(r.first_recorded_at);
  if (firstAt === undefined) return null;
  const lastAt = parseNullableString(r.last_recorded_at);
  if (lastAt === undefined) return null;
  return {
    terminal_session_id: r.terminal_session_id,
    has_recording: r.has_recording,
    chunk_count: r.chunk_count,
    marker_count: r.marker_count,
    first_seq: firstSeq,
    last_seq: lastSeq,
    first_recorded_at: firstAt,
    last_recorded_at: lastAt,
  };
}

/**
 * Parse a chunk wire row field-by-field. Built so an unknown sibling
 * field on the wire body (a future addition or a smuggled extra) cannot
 * smuggle onto the parsed object — only the named fields below are
 * copied through.
 */
export function parseRecordingChunk(
  raw: unknown,
): TerminalRecordingChunk | null {
  if (!raw || typeof raw !== "object") return null;
  const r = raw as Record<string, unknown>;
  if (
    !isFiniteInt(r.seq_start) ||
    !isFiniteInt(r.seq_end) ||
    !isFiniteInt(r.byte_len) ||
    typeof r.data_b64 !== "string" ||
    typeof r.encryption !== "string" ||
    typeof r.compression !== "string" ||
    typeof r.created_at !== "string"
  ) {
    return null;
  }
  if (r.byte_len < 0) return null;
  if (r.seq_start < 1 || r.seq_end < r.seq_start) return null;
  return {
    seq_start: r.seq_start,
    seq_end: r.seq_end,
    byte_len: r.byte_len,
    data_b64: r.data_b64,
    encryption: r.encryption,
    compression: r.compression,
    created_at: r.created_at,
  };
}

/**
 * Parse a marker wire row field-by-field. The `payload` field is
 * preserved as opaque JSON (`unknown`) — the viewer treats it as
 * metadata only.
 */
export function parseRecordingMarker(
  raw: unknown,
): TerminalRecordingMarker | null {
  if (!raw || typeof raw !== "object") return null;
  const r = raw as Record<string, unknown>;
  if (
    typeof r.kind !== "string" ||
    !MARKER_KINDS.has(r.kind as TerminalRecordingMarkerKind) ||
    !isFiniteInt(r.seq) ||
    typeof r.created_at !== "string"
  ) {
    return null;
  }
  if (r.seq < 0) return null;
  // `payload` is `unknown` on purpose. The marker contract is metadata
  // only, but we do not whitelist a shape here — an arbitrary JSON
  // value is preserved verbatim because the viewer renders it through
  // a metadata-only key/value display, never as terminal output.
  return {
    kind: r.kind as TerminalRecordingMarkerKind,
    seq: r.seq,
    payload: r.payload ?? null,
    created_at: r.created_at,
  };
}

function isFiniteInt(value: unknown): value is number {
  return typeof value === "number" && Number.isFinite(value) && Number.isInteger(value);
}

function parseNullableInt(value: unknown): number | null | undefined {
  if (value === null || value === undefined) return null;
  if (isFiniteInt(value)) return value;
  return undefined;
}

function parseNullableString(value: unknown): string | null | undefined {
  if (value === null || value === undefined) return null;
  if (typeof value === "string") return value;
  return undefined;
}

// ---------------------------------------------------------------------------
// Chunk decode + support guard
// ---------------------------------------------------------------------------

/**
 * `true` iff the viewer can replay this chunk's payload directly. Today
 * the writer only emits `encryption: "none"` + `compression: "none"`,
 * but a future rev may roll out new schemes. The viewer screens with
 * this guard and shows an explicit unsupported message instead of
 * decoding a payload it cannot interpret.
 */
export function isSupportedChunk(chunk: TerminalRecordingChunk): boolean {
  return chunk.encryption === "none" && chunk.compression === "none";
}

export type DecodeChunkOutcome =
  | { ok: true; bytes: Uint8Array }
  | {
      ok: false;
      reason:
        | "unsupported_encryption"
        | "unsupported_compression"
        | "invalid_base64"
        | "byte_len_mismatch";
    };

/**
 * Decode a chunk's `data_b64` to bytes for the replay renderer.
 *
 * Strict validation:
 *  1. The chunk's `encryption` / `compression` must be `"none"`. A
 *     future scheme rolls in behind a feature flag with its own
 *     decoder; until then we refuse to hand "encrypted" bytes to xterm.
 *  2. Base64 decode must succeed (atob throws on a malformed string).
 *  3. The decoded byte length MUST match the chunk's `byte_len`. The
 *     backend pins this with a SQL CHECK constraint on the writer side;
 *     we re-check on the read side as a defence-in-depth backstop.
 *
 * The function does NOT throw and does NOT log. The decoded bytes are
 * returned for the caller to hand directly to xterm — they MUST NOT be
 * stashed in any persistent surface.
 */
export function decodeRecordingChunk(
  chunk: TerminalRecordingChunk,
): DecodeChunkOutcome {
  if (chunk.encryption !== "none") {
    return { ok: false, reason: "unsupported_encryption" };
  }
  if (chunk.compression !== "none") {
    return { ok: false, reason: "unsupported_compression" };
  }
  let bytes: Uint8Array;
  try {
    bytes = decodeBase64(chunk.data_b64);
  } catch {
    return { ok: false, reason: "invalid_base64" };
  }
  if (bytes.byteLength !== chunk.byte_len) {
    return { ok: false, reason: "byte_len_mismatch" };
  }
  return { ok: true, bytes };
}

/**
 * Decode a base64 string using the standard alphabet (RFC-4648).
 *
 * Same behaviour as `relayterm_protocol::output_data_decode` /
 * `crates/relayterm-protocol`. Errors when the input contains a
 * character outside `A-Za-z0-9+/=`.
 */
function decodeBase64(input: string): Uint8Array {
  // `atob` is the same alphabet the wire uses. We deliberately avoid a
  // hand-rolled decoder so a future engineer sees a single canonical
  // implementation. The `length` math is exact because we constructed
  // each byte from a single 8-bit code unit.
  const bin = globalThis.atob(input);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i += 1) {
    out[i] = bin.charCodeAt(i) & 0xff;
  }
  return out;
}

// ---------------------------------------------------------------------------
// Wire helpers
// ---------------------------------------------------------------------------

export type RecordingApiError = WireError;

export type RecordingResult<T> =
  | { ok: true; data: T }
  | { ok: false; error: RecordingApiError };

export interface RecordingFetchOptions extends LoadOptions {
  /** Replaceable for tests. Defaults to the canonical endpoint. */
  endpoint?: string;
}

export interface ListRecordingChunksOptions extends RecordingFetchOptions {
  /** Inclusive lower bound on `seq_start`. Defaults to backend `1`. */
  fromSeq?: number;
  /** Page size; backend clamps to `1..=1024`. */
  limit?: number;
}

export interface ListRecordingMarkersOptions extends RecordingFetchOptions {
  /** Inclusive lower bound on `seq`. Defaults to backend `0`. */
  fromSeq?: number;
  /** Page size; backend clamps to `1..=1024`. */
  limit?: number;
}

const DEFAULT_RECORDING_BASE = "/api/v1/terminal-sessions";

function metadataEndpoint(sessionId: string): string {
  return `${DEFAULT_RECORDING_BASE}/${encodeURIComponent(sessionId)}/recording/metadata`;
}

function chunksEndpoint(sessionId: string, opts: ListRecordingChunksOptions): string {
  const base = `${DEFAULT_RECORDING_BASE}/${encodeURIComponent(sessionId)}/recording/chunks`;
  return appendQuery(base, opts.fromSeq, opts.limit);
}

function markersEndpoint(sessionId: string, opts: ListRecordingMarkersOptions): string {
  const base = `${DEFAULT_RECORDING_BASE}/${encodeURIComponent(sessionId)}/recording/markers`;
  return appendQuery(base, opts.fromSeq, opts.limit);
}

function appendQuery(base: string, fromSeq?: number, limit?: number): string {
  const parts: string[] = [];
  if (typeof fromSeq === "number" && Number.isInteger(fromSeq) && fromSeq >= 0) {
    parts.push(`from_seq=${encodeURIComponent(String(fromSeq))}`);
  }
  if (typeof limit === "number" && Number.isInteger(limit) && limit > 0) {
    parts.push(`limit=${encodeURIComponent(String(limit))}`);
  }
  return parts.length === 0 ? base : `${base}?${parts.join("&")}`;
}

async function getRecordingJson<T>(
  endpoint: string,
  parse: (raw: unknown) => T | null,
  options: LoadOptions,
): Promise<RecordingResult<T>> {
  const fetchImpl = options.fetchImpl ?? globalThis.fetch;
  if (typeof fetchImpl !== "function") {
    return {
      ok: false,
      error: { kind: "transport", message: "fetch unavailable" },
    };
  }

  let response: Response;
  try {
    response = await fetchImpl(endpoint, {
      headers: { accept: "application/json" },
      credentials: "include",
    });
  } catch (err) {
    return {
      ok: false,
      error: {
        kind: "transport",
        message: err instanceof Error ? err.message : "unknown",
      },
    };
  }

  if (!response.ok) {
    const { code, message } = await readErrorEnvelope(response);
    return {
      ok: false,
      error: { kind: "http", status: response.status, code, message },
    };
  }

  let body: unknown;
  try {
    body = await response.json();
  } catch {
    return { ok: false, error: { kind: "malformed_response" } };
  }
  const parsed = parse(body);
  if (parsed === null) {
    return { ok: false, error: { kind: "malformed_response" } };
  }
  return { ok: true, data: parsed };
}

async function getRecordingList<T>(
  endpoint: string,
  parseItem: (raw: unknown) => T | null,
  options: LoadOptions,
): Promise<RecordingResult<T[]>> {
  return getRecordingJson<T[]>(endpoint, (raw) => {
    if (!Array.isArray(raw)) return null;
    const out: T[] = [];
    for (const item of raw) {
      const parsed = parseItem(item);
      if (parsed === null) return null;
      out.push(parsed);
    }
    return out;
  }, options);
}

/**
 * GET aggregate recording metadata for the given session id.
 *
 * Backend semantics (mirrored, not re-implemented here):
 *  - Owner-scoped: a foreign-owned id and an unknown id collapse to the
 *    same `404 not_found`. The wire body never differentiates.
 *  - An owned session with no chunks AND no markers returns `200` with
 *    `has_recording: false` and zero counts — this is a normal state,
 *    not an error. The viewer gates "load chunks" on `has_recording`.
 */
export async function getTerminalRecordingMetadata(
  sessionId: string,
  options: RecordingFetchOptions = {},
): Promise<RecordingResult<TerminalRecordingMetadata>> {
  const endpoint = options.endpoint ?? metadataEndpoint(sessionId);
  return getRecordingJson(endpoint, parseRecordingMetadata, options);
}

/**
 * GET a page of recording chunks. `fromSeq` is the inclusive lower
 * bound on `seq_start`; `limit` is the page size (backend clamps to
 * `1..=1024`).
 */
export async function getTerminalRecordingChunks(
  sessionId: string,
  options: ListRecordingChunksOptions = {},
): Promise<RecordingResult<TerminalRecordingChunk[]>> {
  const endpoint = options.endpoint ?? chunksEndpoint(sessionId, options);
  return getRecordingList(endpoint, parseRecordingChunk, options);
}

/**
 * GET a page of recording markers. `fromSeq` is the inclusive lower
 * bound on `seq`; `limit` is the page size.
 */
export async function getTerminalRecordingMarkers(
  sessionId: string,
  options: ListRecordingMarkersOptions = {},
): Promise<RecordingResult<TerminalRecordingMarker[]>> {
  const endpoint = options.endpoint ?? markersEndpoint(sessionId, options);
  return getRecordingList(endpoint, parseRecordingMarker, options);
}

// ---------------------------------------------------------------------------
// Error formatting
// ---------------------------------------------------------------------------

/**
 * Format a recording API error as a one-line UI string. Stays a
 * function of `kind` + `status` + `code` ONLY — never echoes the wire
 * `message` field, the thrown `Error.message` of a transport failure,
 * `data_b64` content, or any chunk/marker payload material.
 */
export function describeRecordingError(err: RecordingApiError): string {
  switch (err.kind) {
    case "http":
      if (err.status === 404 && err.code === "not_found") {
        return "Recording is not available for this session.";
      }
      if (err.status === 401) {
        return "Could not load recording: not authenticated.";
      }
      return `Could not load recording: HTTP ${err.status} ${err.code}`;
    case "transport":
      return "Could not load recording: backend unavailable.";
    case "malformed_response":
      return "Could not load recording: malformed response.";
  }
}

/**
 * Human-facing summary for a chunk decode failure. Same redaction
 * posture as {@link describeRecordingError}: the chunk's `data_b64` is
 * NEVER echoed; the summary depends only on the discriminant.
 */
export function describeDecodeFailure(
  reason: Exclude<DecodeChunkOutcome, { ok: true }>["reason"],
): string {
  switch (reason) {
    case "unsupported_encryption":
      return "This recording chunk uses an encryption scheme this client does not support.";
    case "unsupported_compression":
      return "This recording chunk uses a compression scheme this client does not support.";
    case "invalid_base64":
      return "Recording chunk payload is malformed and cannot be replayed.";
    case "byte_len_mismatch":
      return "Recording chunk length did not match the declared size.";
  }
}
