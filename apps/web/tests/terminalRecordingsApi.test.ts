import { describe, expect, it } from "vitest";
import {
  decodeRecordingChunk,
  describeDecodeFailure,
  describeRecordingError,
  getTerminalRecordingChunks,
  getTerminalRecordingMarkers,
  getTerminalRecordingMetadata,
  isSupportedChunk,
  parseRecordingChunk,
  parseRecordingMarker,
  parseRecordingMetadata,
  type TerminalRecordingChunk,
  type TerminalRecordingMarker,
  type TerminalRecordingMetadata,
} from "../src/lib/api/terminalRecordings.js";

/**
 * Sentinels that MUST NEVER appear in:
 *   - parsed DTOs (`JSON.stringify(parsed)` form)
 *   - formatted user-facing error summaries
 *   - any `Error.message` / thrown rejection
 *   - localStorage / sessionStorage writes
 *
 * Each sentinel pins a specific category of leakage:
 *  - `OPERATOR_DETAIL`     — wire-side "internal_error" body extras
 *  - `DATA_B64`            — raw base64 chunk payload
 *  - `RAW_RECORDING`       — payload sentinel decoded from base64
 *  - `PRIVATE_KEY` /
 *    `ENCRYPTED_PRIVATE_KEY` — vault material, must never reach a
 *                              recording surface
 *  - `SESSION_TOKEN` /
 *    `TOKEN_HASH`          — auth material
 *  - `PASSWORD_HASH`       — auth material
 *  - `BOOTSTRAP_TOKEN`     — first-user bootstrap secret
 */
const SENTINEL_OPERATOR = "RELAY_SENTINEL_RECORDING_OPERATOR_DETAIL_9001";
const SENTINEL_DATA_B64 = "U0VOVElORUxfUkVDT1JESU5HX1BBWUxPQURfOTAwMg=="; // "SENTINEL_RECORDING_PAYLOAD_9002"
const SENTINEL_RAW_RECORDING = "SENTINEL_RECORDING_PAYLOAD_9002";
const SENTINEL_PRIVATE_KEY = "RELAY_SENTINEL_PRIVATE_KEY_BYTES_9003";
const SENTINEL_ENCRYPTED_PK = "RELAY_SENTINEL_ENCRYPTED_PRIVATE_KEY_9004";
const SENTINEL_SESSION_TOKEN = "RELAY_SENTINEL_SESSION_TOKEN_9005";
const SENTINEL_TOKEN_HASH = "RELAY_SENTINEL_TOKEN_HASH_9006";
const SENTINEL_PASSWORD_HASH = "RELAY_SENTINEL_PASSWORD_HASH_9007";
const SENTINEL_BOOTSTRAP = "RELAY_SENTINEL_BOOTSTRAP_TOKEN_9008";

const ALL_SENTINELS = [
  SENTINEL_OPERATOR,
  SENTINEL_DATA_B64,
  SENTINEL_RAW_RECORDING,
  SENTINEL_PRIVATE_KEY,
  SENTINEL_ENCRYPTED_PK,
  SENTINEL_SESSION_TOKEN,
  SENTINEL_TOKEN_HASH,
  SENTINEL_PASSWORD_HASH,
  SENTINEL_BOOTSTRAP,
];

const SESSION_ID = "11111111-1111-1111-1111-111111111111";

const META_FIXTURE: TerminalRecordingMetadata = {
  terminal_session_id: SESSION_ID,
  has_recording: true,
  chunk_count: 4,
  marker_count: 2,
  first_seq: 1,
  last_seq: 8,
  first_recorded_at: "2026-05-02T10:00:00Z",
  last_recorded_at: "2026-05-02T10:00:05Z",
};

// "hello" base64
const HELLO_B64 = "aGVsbG8=";
const CHUNK_FIXTURE: TerminalRecordingChunk = {
  seq_start: 1,
  seq_end: 5,
  byte_len: 5,
  data_b64: HELLO_B64,
  encryption: "none",
  compression: "none",
  created_at: "2026-05-02T10:00:00Z",
};

const MARKER_FIXTURE: TerminalRecordingMarker = {
  kind: "started",
  seq: 0,
  payload: { client_kind: "web" },
  created_at: "2026-05-02T10:00:00Z",
};

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

// ---------------------------------------------------------------------------
// Parsers
// ---------------------------------------------------------------------------

describe("parseRecordingMetadata", () => {
  it("parses a minimal valid metadata body", () => {
    expect(parseRecordingMetadata(META_FIXTURE)).toEqual(META_FIXTURE);
  });

  it("accepts null seq bounds + null timestamps for the empty case", () => {
    const empty: TerminalRecordingMetadata = {
      terminal_session_id: SESSION_ID,
      has_recording: false,
      chunk_count: 0,
      marker_count: 0,
      first_seq: null,
      last_seq: null,
      first_recorded_at: null,
      last_recorded_at: null,
    };
    expect(parseRecordingMetadata(empty)).toEqual(empty);
  });

  it("rejects malformed bodies", () => {
    expect(parseRecordingMetadata(null)).toBeNull();
    expect(parseRecordingMetadata(42)).toBeNull();
    expect(parseRecordingMetadata("nope")).toBeNull();
    expect(
      parseRecordingMetadata({ ...META_FIXTURE, chunk_count: "many" }),
    ).toBeNull();
    expect(
      parseRecordingMetadata({ ...META_FIXTURE, has_recording: "yes" }),
    ).toBeNull();
    expect(
      parseRecordingMetadata({ ...META_FIXTURE, first_seq: 1.5 }),
    ).toBeNull();
  });

  it("does NOT smuggle vault / auth / payload sentinels onto the parsed object", () => {
    const smuggled = {
      ...META_FIXTURE,
      private_key: SENTINEL_PRIVATE_KEY,
      encrypted_private_key: SENTINEL_ENCRYPTED_PK,
      session_token: SENTINEL_SESSION_TOKEN,
      token_hash: SENTINEL_TOKEN_HASH,
      password_hash: SENTINEL_PASSWORD_HASH,
      first_user_bootstrap_token: SENTINEL_BOOTSTRAP,
      operator_detail: SENTINEL_OPERATOR,
      data_b64: SENTINEL_DATA_B64,
    };
    const parsed = parseRecordingMetadata(smuggled);
    expect(parsed).not.toBeNull();
    const obj = parsed as Record<string, unknown>;
    expect(obj.private_key).toBeUndefined();
    expect(obj.encrypted_private_key).toBeUndefined();
    expect(obj.session_token).toBeUndefined();
    expect(obj.token_hash).toBeUndefined();
    expect(obj.password_hash).toBeUndefined();
    expect(obj.first_user_bootstrap_token).toBeUndefined();
    expect(obj.operator_detail).toBeUndefined();
    expect(obj.data_b64).toBeUndefined();
    const stringified = JSON.stringify(parsed);
    for (const s of ALL_SENTINELS) {
      expect(stringified).not.toContain(s);
    }
  });
});

describe("parseRecordingChunk", () => {
  it("parses a valid chunk row", () => {
    expect(parseRecordingChunk(CHUNK_FIXTURE)).toEqual(CHUNK_FIXTURE);
  });

  it("rejects negative byte_len, inverted seq, non-int seq, missing fields", () => {
    expect(parseRecordingChunk({ ...CHUNK_FIXTURE, byte_len: -1 })).toBeNull();
    expect(
      parseRecordingChunk({ ...CHUNK_FIXTURE, seq_start: 5, seq_end: 1 }),
    ).toBeNull();
    expect(parseRecordingChunk({ ...CHUNK_FIXTURE, seq_start: 0 })).toBeNull();
    expect(parseRecordingChunk({ ...CHUNK_FIXTURE, seq_start: 1.5 })).toBeNull();
    expect(parseRecordingChunk({ ...CHUNK_FIXTURE, data_b64: 42 })).toBeNull();
    expect(parseRecordingChunk(null)).toBeNull();
    expect(parseRecordingChunk(undefined)).toBeNull();
    expect(parseRecordingChunk({})).toBeNull();
  });

  it("preserves any encryption / compression string for forward compatibility", () => {
    const future = parseRecordingChunk({
      ...CHUNK_FIXTURE,
      encryption: "recording_v1",
      compression: "zstd",
    });
    expect(future).not.toBeNull();
    expect(future?.encryption).toBe("recording_v1");
    expect(future?.compression).toBe("zstd");
  });

  it("does NOT smuggle vault / auth sentinels onto the parsed chunk", () => {
    const smuggled = {
      ...CHUNK_FIXTURE,
      private_key: SENTINEL_PRIVATE_KEY,
      encrypted_private_key: SENTINEL_ENCRYPTED_PK,
      session_token: SENTINEL_SESSION_TOKEN,
      token_hash: SENTINEL_TOKEN_HASH,
      password_hash: SENTINEL_PASSWORD_HASH,
      first_user_bootstrap_token: SENTINEL_BOOTSTRAP,
      operator_detail: SENTINEL_OPERATOR,
    };
    const parsed = parseRecordingChunk(smuggled);
    expect(parsed).not.toBeNull();
    const stringified = JSON.stringify(parsed);
    expect(stringified).not.toContain(SENTINEL_PRIVATE_KEY);
    expect(stringified).not.toContain(SENTINEL_ENCRYPTED_PK);
    expect(stringified).not.toContain(SENTINEL_SESSION_TOKEN);
    expect(stringified).not.toContain(SENTINEL_TOKEN_HASH);
    expect(stringified).not.toContain(SENTINEL_PASSWORD_HASH);
    expect(stringified).not.toContain(SENTINEL_BOOTSTRAP);
    expect(stringified).not.toContain(SENTINEL_OPERATOR);
  });
});

describe("parseRecordingMarker", () => {
  it("parses each known marker kind", () => {
    for (const kind of [
      "started",
      "attached",
      "detached",
      "reattached",
      "resized",
      "closed",
      "replay_gap",
    ] as const) {
      const seq = kind === "started" ? 0 : 5;
      const m = parseRecordingMarker({ ...MARKER_FIXTURE, kind, seq });
      expect(m?.kind).toBe(kind);
    }
  });

  it("rejects unknown kinds and negative seqs", () => {
    expect(
      parseRecordingMarker({ ...MARKER_FIXTURE, kind: "exploded" }),
    ).toBeNull();
    expect(parseRecordingMarker({ ...MARKER_FIXTURE, seq: -1 })).toBeNull();
    expect(parseRecordingMarker({ ...MARKER_FIXTURE, seq: 1.5 })).toBeNull();
    expect(parseRecordingMarker(null)).toBeNull();
  });

  it("preserves an arbitrary opaque payload as `unknown`", () => {
    const m = parseRecordingMarker({
      ...MARKER_FIXTURE,
      kind: "resized",
      seq: 3,
      payload: { cols: 80, rows: 24 },
    });
    expect(m?.payload).toEqual({ cols: 80, rows: 24 });
  });

  it("treats a missing payload as null (not undefined)", () => {
    const noPayload = { kind: "closed", seq: 9, created_at: MARKER_FIXTURE.created_at };
    const m = parseRecordingMarker(noPayload);
    expect(m).not.toBeNull();
    expect(m?.payload).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// Decode + support guard
// ---------------------------------------------------------------------------

describe("isSupportedChunk", () => {
  it("returns true only for none/none", () => {
    expect(isSupportedChunk(CHUNK_FIXTURE)).toBe(true);
    expect(isSupportedChunk({ ...CHUNK_FIXTURE, encryption: "recording_v1" })).toBe(false);
    expect(isSupportedChunk({ ...CHUNK_FIXTURE, compression: "zstd" })).toBe(false);
  });
});

describe("decodeRecordingChunk", () => {
  it("decodes a valid chunk to bytes matching byte_len", () => {
    const result = decodeRecordingChunk(CHUNK_FIXTURE);
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(Array.from(result.bytes)).toEqual([0x68, 0x65, 0x6c, 0x6c, 0x6f]);
      expect(result.bytes.byteLength).toBe(CHUNK_FIXTURE.byte_len);
    }
  });

  it("rejects unsupported encryption", () => {
    const result = decodeRecordingChunk({
      ...CHUNK_FIXTURE,
      encryption: "recording_v1",
    });
    expect(result).toEqual({ ok: false, reason: "unsupported_encryption" });
  });

  it("rejects unsupported compression", () => {
    const result = decodeRecordingChunk({
      ...CHUNK_FIXTURE,
      compression: "zstd",
    });
    expect(result).toEqual({ ok: false, reason: "unsupported_compression" });
  });

  it("rejects malformed base64", () => {
    const result = decodeRecordingChunk({
      ...CHUNK_FIXTURE,
      data_b64: "***not-base64***",
    });
    expect(result).toEqual({ ok: false, reason: "invalid_base64" });
  });

  it("rejects a length mismatch", () => {
    const result = decodeRecordingChunk({
      ...CHUNK_FIXTURE,
      byte_len: 4, // declared length wrong on purpose
    });
    expect(result).toEqual({ ok: false, reason: "byte_len_mismatch" });
  });
});

// ---------------------------------------------------------------------------
// Wire helpers
// ---------------------------------------------------------------------------

describe("getTerminalRecordingMetadata", () => {
  it("uses credentials include and the canonical path-encoded endpoint", async () => {
    let captured: { url: string; init: RequestInit | undefined } | null = null;
    const fetchImpl = (async (
      input: string | URL | Request,
      init?: RequestInit,
    ) => {
      captured = { url: String(input), init };
      return jsonResponse(200, META_FIXTURE);
    }) as unknown as typeof fetch;

    await getTerminalRecordingMetadata("a/b c", { fetchImpl });
    expect(captured).not.toBeNull();
    expect(captured!.url).toBe(
      "/api/v1/terminal-sessions/a%2Fb%20c/recording/metadata",
    );
    expect(captured!.init?.credentials).toBe("include");
  });

  it("returns parsed metadata on a 2xx body", async () => {
    const fetchImpl = (async () =>
      jsonResponse(200, META_FIXTURE)) as unknown as typeof fetch;
    const result = await getTerminalRecordingMetadata(SESSION_ID, { fetchImpl });
    expect(result).toEqual({ ok: true, data: META_FIXTURE });
  });

  it("collapses 404 to a typed http error and never leaks operator detail", async () => {
    const fetchImpl = (async () =>
      jsonResponse(404, {
        error: {
          code: "not_found",
          message: "not found",
          operator_detail: SENTINEL_OPERATOR,
        },
      })) as unknown as typeof fetch;
    const result = await getTerminalRecordingMetadata(SESSION_ID, { fetchImpl });
    expect(result.ok).toBe(false);
    if (!result.ok && result.error.kind === "http") {
      expect(result.error.status).toBe(404);
      expect(result.error.code).toBe("not_found");
      expect(JSON.stringify(result.error)).not.toContain(SENTINEL_OPERATOR);
    } else {
      expect.fail("expected http error");
    }
  });

  it("returns malformed_response when the body cannot be parsed", async () => {
    const fetchImpl = (async () =>
      jsonResponse(200, { id: "wrong-shape" })) as unknown as typeof fetch;
    const result = await getTerminalRecordingMetadata(SESSION_ID, { fetchImpl });
    expect(result).toEqual({ ok: false, error: { kind: "malformed_response" } });
  });

  it("returns transport on fetch throw", async () => {
    const fetchImpl = (async () => {
      throw new Error(`boom ${SENTINEL_OPERATOR}`);
    }) as unknown as typeof fetch;
    const result = await getTerminalRecordingMetadata(SESSION_ID, { fetchImpl });
    expect(result.ok).toBe(false);
    if (!result.ok && result.error.kind === "transport") {
      // Programmatic callers may branch on `message`; the formatter is
      // the surface that strips it for the UI.
      expect(result.error.message).toContain("boom");
    } else {
      expect.fail("expected transport error");
    }
  });
});

describe("getTerminalRecordingChunks", () => {
  it("uses credentials include and path-encodes the session id", async () => {
    let captured: { url: string; init: RequestInit | undefined } | null = null;
    const fetchImpl = (async (
      input: string | URL | Request,
      init?: RequestInit,
    ) => {
      captured = { url: String(input), init };
      return jsonResponse(200, [CHUNK_FIXTURE]);
    }) as unknown as typeof fetch;

    await getTerminalRecordingChunks("a/b c", { fetchImpl });
    expect(captured!.url).toBe(
      "/api/v1/terminal-sessions/a%2Fb%20c/recording/chunks",
    );
    expect(captured!.init?.credentials).toBe("include");
  });

  it("includes from_seq and limit when provided", async () => {
    let captured: string | null = null;
    const fetchImpl = (async (input: string | URL | Request) => {
      captured = String(input);
      return jsonResponse(200, []);
    }) as unknown as typeof fetch;
    await getTerminalRecordingChunks(SESSION_ID, {
      fromSeq: 7,
      limit: 32,
      fetchImpl,
    });
    expect(captured).toBe(
      `/api/v1/terminal-sessions/${SESSION_ID}/recording/chunks?from_seq=7&limit=32`,
    );
  });

  it("ignores invalid query params (negative / non-int)", async () => {
    let captured: string | null = null;
    const fetchImpl = (async (input: string | URL | Request) => {
      captured = String(input);
      return jsonResponse(200, []);
    }) as unknown as typeof fetch;
    await getTerminalRecordingChunks(SESSION_ID, {
      fromSeq: -1,
      limit: 0,
      fetchImpl,
    });
    expect(captured).toBe(
      `/api/v1/terminal-sessions/${SESSION_ID}/recording/chunks`,
    );
  });

  it("returns the parsed chunk list", async () => {
    const fetchImpl = (async () =>
      jsonResponse(200, [CHUNK_FIXTURE])) as unknown as typeof fetch;
    const result = await getTerminalRecordingChunks(SESSION_ID, { fetchImpl });
    expect(result).toEqual({ ok: true, data: [CHUNK_FIXTURE] });
  });

  it("rejects a partially malformed list as malformed_response", async () => {
    const fetchImpl = (async () =>
      jsonResponse(200, [
        CHUNK_FIXTURE,
        { ...CHUNK_FIXTURE, byte_len: -1 },
      ])) as unknown as typeof fetch;
    const result = await getTerminalRecordingChunks(SESSION_ID, { fetchImpl });
    expect(result).toEqual({ ok: false, error: { kind: "malformed_response" } });
  });
});

describe("getTerminalRecordingMarkers", () => {
  it("uses credentials include and path-encodes the session id", async () => {
    let captured: { url: string; init: RequestInit | undefined } | null = null;
    const fetchImpl = (async (
      input: string | URL | Request,
      init?: RequestInit,
    ) => {
      captured = { url: String(input), init };
      return jsonResponse(200, [MARKER_FIXTURE]);
    }) as unknown as typeof fetch;

    await getTerminalRecordingMarkers("a/b c", { fetchImpl });
    expect(captured!.url).toBe(
      "/api/v1/terminal-sessions/a%2Fb%20c/recording/markers",
    );
    expect(captured!.init?.credentials).toBe("include");
  });

  it("returns the parsed marker list and accepts the started kind at seq=0", async () => {
    const fetchImpl = (async () =>
      jsonResponse(200, [MARKER_FIXTURE])) as unknown as typeof fetch;
    const result = await getTerminalRecordingMarkers(SESSION_ID, { fetchImpl });
    expect(result).toEqual({ ok: true, data: [MARKER_FIXTURE] });
  });
});

// ---------------------------------------------------------------------------
// Error formatter redaction
// ---------------------------------------------------------------------------

describe("describeRecordingError", () => {
  it("never echoes the wire `message` field of an http error", () => {
    for (const status of [400, 401, 403, 404, 409, 500, 502]) {
      const summary = describeRecordingError({
        kind: "http",
        status,
        code: "internal_error",
        message: ALL_SENTINELS.join(" "),
      });
      for (const s of ALL_SENTINELS) {
        expect(summary).not.toContain(s);
      }
    }
  });

  it("never echoes the thrown message of a transport error", () => {
    const summary = describeRecordingError({
      kind: "transport",
      message: `request to https://example.com ${ALL_SENTINELS.join(" ")}`,
    });
    expect(summary).not.toContain("https://");
    for (const s of ALL_SENTINELS) {
      expect(summary).not.toContain(s);
    }
  });

  it("formats well-known categories with stable copy", () => {
    expect(
      describeRecordingError({
        kind: "http",
        status: 404,
        code: "not_found",
        message: "not found",
      }),
    ).toBe("Recording is not available for this session.");
    expect(
      describeRecordingError({
        kind: "http",
        status: 401,
        code: "unauthorized",
        message: "unauthorized",
      }),
    ).toBe("Could not load recording: not authenticated.");
    expect(describeRecordingError({ kind: "transport", message: "x" })).toBe(
      "Could not load recording: backend unavailable.",
    );
    expect(describeRecordingError({ kind: "malformed_response" })).toBe(
      "Could not load recording: malformed response.",
    );
  });

  it("never echoes data_b64 or raw recording bytes via the http message field", () => {
    const summary = describeRecordingError({
      kind: "http",
      status: 500,
      code: "internal_error",
      message: `${SENTINEL_DATA_B64} ${SENTINEL_RAW_RECORDING}`,
    });
    expect(summary).not.toContain(SENTINEL_DATA_B64);
    expect(summary).not.toContain(SENTINEL_RAW_RECORDING);
  });
});

describe("describeDecodeFailure", () => {
  it("formats every reason without echoing chunk bytes", () => {
    expect(describeDecodeFailure("unsupported_encryption")).toContain(
      "encryption",
    );
    expect(describeDecodeFailure("unsupported_compression")).toContain(
      "compression",
    );
    expect(describeDecodeFailure("invalid_base64")).toContain("malformed");
    expect(describeDecodeFailure("byte_len_mismatch")).toContain("length");
    for (const reason of [
      "unsupported_encryption",
      "unsupported_compression",
      "invalid_base64",
      "byte_len_mismatch",
    ] as const) {
      const summary = describeDecodeFailure(reason);
      for (const s of ALL_SENTINELS) {
        expect(summary).not.toContain(s);
      }
    }
  });
});

// ---------------------------------------------------------------------------
// Browser-storage redaction (load-bearing)
// ---------------------------------------------------------------------------

describe("recording helpers do not write to browser storage", () => {
  it("getTerminalRecordingMetadata does not touch local/sessionStorage", async () => {
    const localCalls: Array<[string, string]> = [];
    const sessionCalls: Array<[string, string]> = [];
    const guarded = withSpyStorage(localCalls, sessionCalls, async () => {
      const fetchImpl = (async () =>
        jsonResponse(200, META_FIXTURE)) as unknown as typeof fetch;
      await getTerminalRecordingMetadata(SESSION_ID, { fetchImpl });
    });
    await guarded;
    expect(localCalls).toEqual([]);
    expect(sessionCalls).toEqual([]);
  });

  it("getTerminalRecordingChunks does not touch local/sessionStorage", async () => {
    const localCalls: Array<[string, string]> = [];
    const sessionCalls: Array<[string, string]> = [];
    await withSpyStorage(localCalls, sessionCalls, async () => {
      const fetchImpl = (async () =>
        jsonResponse(200, [CHUNK_FIXTURE])) as unknown as typeof fetch;
      const result = await getTerminalRecordingChunks(SESSION_ID, { fetchImpl });
      expect(result.ok).toBe(true);
    });
    expect(localCalls).toEqual([]);
    expect(sessionCalls).toEqual([]);
  });

  it("decodeRecordingChunk does not touch local/sessionStorage and never throws", async () => {
    const localCalls: Array<[string, string]> = [];
    const sessionCalls: Array<[string, string]> = [];
    await withSpyStorage(localCalls, sessionCalls, async () => {
      // Both supported and unsupported branches.
      decodeRecordingChunk(CHUNK_FIXTURE);
      decodeRecordingChunk({
        ...CHUNK_FIXTURE,
        encryption: "recording_v1",
      });
    });
    expect(localCalls).toEqual([]);
    expect(sessionCalls).toEqual([]);
  });
});

/**
 * Run `body` with a Proxy-based spy installed over local/sessionStorage
 * `setItem`. Every call is recorded as a `[key, value]` tuple so the
 * tests can assert nothing was written. The spies tear down even when
 * `body` throws.
 *
 * Storage may not exist in some test environments; when that's the
 * case the function executes `body` with no spy.
 */
async function withSpyStorage(
  localCalls: Array<[string, string]>,
  sessionCalls: Array<[string, string]>,
  body: () => Promise<void>,
): Promise<void> {
  const local = (globalThis as { localStorage?: Storage }).localStorage;
  const session = (globalThis as { sessionStorage?: Storage }).sessionStorage;
  const originalLocalSet =
    typeof local?.setItem === "function" ? local.setItem.bind(local) : null;
  const originalSessionSet =
    typeof session?.setItem === "function"
      ? session.setItem.bind(session)
      : null;
  if (local && originalLocalSet) {
    local.setItem = (k: string, v: string) => {
      localCalls.push([k, v]);
      originalLocalSet(k, v);
    };
  }
  if (session && originalSessionSet) {
    session.setItem = (k: string, v: string) => {
      sessionCalls.push([k, v]);
      originalSessionSet(k, v);
    };
  }
  try {
    await body();
  } finally {
    if (local && originalLocalSet) local.setItem = originalLocalSet;
    if (session && originalSessionSet) session.setItem = originalSessionSet;
  }
}
