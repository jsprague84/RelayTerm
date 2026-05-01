/**
 * Local-only active terminal session record.
 *
 * Scope: a single browser-local pointer at the operator's most recent
 * production terminal session, so that navigating away from the Terminal
 * view (or doing a full-page reload while a session is still alive in
 * the backend's bounded detached-TTL window) does not strand the runtime.
 *
 * NOT a backend settings API. NOT a session list — that lives on the
 * backend and is rendered by `SessionsView`. NOT a multi-tab workspace.
 * NOT a durable recording / replay store. NOT a backend-restart recovery
 * mechanism. NOT cross-device / cross-browser sync. Each is a separate,
 * deliberate slice.
 *
 * Architecture rules (load-bearing):
 *  - Persisted shape stores ONLY safe public metadata: the session id,
 *    the originating server profile id, an operator-facing label, the
 *    cell-grid dims, an optional resume bookmark (`last_seen_seq`), an
 *    optional cached status hint, and a `saved_at` timestamp. It MUST
 *    NOT carry secrets, terminal input, terminal output, replay frames,
 *    public/private keys, `encrypted_private_key`, host fingerprints,
 *    peer banners, or session_event payloads. The redaction sentinel
 *    test in `tests/activeSessionStore.test.ts` pins this against
 *    future "be helpful and stash extras" regressions.
 *  - Parse failures (missing key, malformed JSON, wrong schema, hostile
 *    fixture) MUST collapse to `null` silently. Unknown / extra fields
 *    are dropped on parse so a stray `private_key` cannot smuggle onto
 *    the parsed object.
 *  - Save failures (storage unavailable, quota exceeded) MUST NOT
 *    throw. The caller gets a boolean; the user-facing surface stays
 *    silent — losing the local pointer is annoying, not an error to
 *    log.
 *  - The reconnect attempt is gated by an explicit user action. The
 *    helpers here NEVER auto-connect; the empty-state Terminal view
 *    pulls the saved record on mount and offers an explicit button.
 */
import { CELL_GRID_MAX, CELL_GRID_MIN } from "../../terminal/cellGrid.js";
import type { TerminalSessionStatus } from "../../api/terminalSessions.js";
import type { ActiveLaunch } from "./activeLaunch.js";

/** localStorage key. Bump the suffix on a breaking schema change. */
export const ACTIVE_SESSION_STORAGE_KEY = "relayterm.active-terminal.v1";

/**
 * Maximum length of a session id. The backend's UUID format is 36
 * characters; the bound here is generous to absorb future id formats
 * (e.g. base32) but keeps the persisted record small. A length-bound is
 * also our defence against a corrupted entry inflating localStorage.
 */
export const SESSION_ID_MAX_LEN = 128;

/** Maximum length of the operator-facing profile label. */
export const PROFILE_LABEL_MAX_LEN = 256;

/** Maximum length of the saved-at ISO timestamp. */
export const SAVED_AT_MAX_LEN = 64;

const ALL_STATUSES: readonly TerminalSessionStatus[] = [
  "starting",
  "active",
  "detached",
  "closed",
] as const;

/**
 * Persisted shape (v1). Adding a field is a breaking change relative to
 * existing localStorage entries — bump the storage key and migrate from
 * the v1 read path. The struct uses snake_case to match the wire DTO
 * naming so a copy/paste through devtools stays familiar.
 */
export interface ActiveSessionRecord {
  /** Backend `terminal_session.id`. The single load-bearing field. */
  session_id: string;
  /** Operator-facing label, usually the originating profile name. */
  profile_label?: string;
  /** Cell-grid columns to use on reconnect. */
  cols?: number;
  /** Cell-grid rows to use on reconnect. */
  rows?: number;
  /**
   * Highest output `seq` the previous attachment observed. Used as the
   * replay bookmark on reconnect. Only persisted as a non-negative
   * integer; only consumed for the wire `attach` request when strictly
   * positive.
   */
  last_seen_seq?: number;
  /**
   * Cached lifecycle hint from the last save. NOT authoritative — the
   * backend is the source of truth on reconnect; this hint exists only
   * so the empty-state UI can render the right copy without a fetch.
   */
  status_hint?: TerminalSessionStatus;
  /**
   * ISO 8601 timestamp the record was written. Used for future age-out;
   * not currently consumed by the UI. Stored as a string (not a
   * `number`) so the JSON form is human-readable in devtools.
   */
  saved_at: string;
}

// ---------------------------------------------------------------------------
// Field-level validators
// ---------------------------------------------------------------------------
//
// Every parser branch picks its field directly out of the input record
// and validates it explicitly — no spread, no merge. A hostile fixture
// can therefore not smuggle an extra key onto the parsed object.

function isNonEmptyBoundedString(value: unknown, maxLen: number): value is string {
  return typeof value === "string" && value.length > 0 && value.length <= maxLen;
}

function isCellGridDim(value: unknown): value is number {
  return (
    typeof value === "number" &&
    Number.isInteger(value) &&
    value >= CELL_GRID_MIN &&
    value <= CELL_GRID_MAX
  );
}

function isNonNegativeInteger(value: unknown): value is number {
  return (
    typeof value === "number" &&
    Number.isInteger(value) &&
    value >= 0 &&
    Number.isFinite(value)
  );
}

function isStatusHint(value: unknown): value is TerminalSessionStatus {
  return (
    typeof value === "string" &&
    (ALL_STATUSES as readonly string[]).includes(value)
  );
}

const CONTROL_CHAR_RE = new RegExp(
  `[${String.fromCharCode(0)}-${String.fromCharCode(0x1f)}${String.fromCharCode(0x7f)}]`,
  "g",
);

function sanitizeLabel(value: string): string {
  // Strip control chars (including newlines / tabs); the label is for
  // a single-line UI surface. Keep quotes / spaces / unicode as-is.
  const cleaned = value.replace(CONTROL_CHAR_RE, "").trim();
  if (cleaned.length > PROFILE_LABEL_MAX_LEN) {
    return cleaned.slice(0, PROFILE_LABEL_MAX_LEN);
  }
  return cleaned;
}

/**
 * Parse an arbitrary value (typically `JSON.parse(localStorage)` output)
 * into an {@link ActiveSessionRecord}. Returns `null` if the input is not
 * an object, if the required `session_id` is missing or malformed, or if
 * `saved_at` is missing — both fields are load-bearing.
 *
 * Optional fields that fail validation are silently dropped from the
 * parsed record rather than causing a whole-record rejection. The
 * rationale: a corrupted `last_seen_seq` (e.g. `"abc"`) should not lock
 * the operator out of an otherwise-recoverable session — the parsed
 * record just won't carry a resume bookmark.
 *
 * The function NEVER throws and NEVER reads keys other than the eight
 * documented fields — a hostile entry that injects extra keys cannot
 * smuggle anything onto the parsed object.
 */
export function parseActiveSession(input: unknown): ActiveSessionRecord | null {
  if (input === null || typeof input !== "object" || Array.isArray(input)) {
    return null;
  }
  const raw = input as Record<string, unknown>;

  const sessionId = raw["session_id"];
  if (!isNonEmptyBoundedString(sessionId, SESSION_ID_MAX_LEN)) return null;

  const savedAt = raw["saved_at"];
  if (!isNonEmptyBoundedString(savedAt, SAVED_AT_MAX_LEN)) return null;

  const record: ActiveSessionRecord = {
    session_id: sessionId,
    saved_at: savedAt,
  };

  const label = raw["profile_label"];
  if (typeof label === "string") {
    // Sanitize first (strip control chars, trim, clip to max len) so an
    // overlong / dirty label is recovered rather than dropped — only a
    // wrong type drops the field entirely.
    const cleaned = sanitizeLabel(label);
    if (cleaned.length > 0) record.profile_label = cleaned;
  }

  const cols = raw["cols"];
  if (isCellGridDim(cols)) record.cols = cols;

  const rows = raw["rows"];
  if (isCellGridDim(rows)) record.rows = rows;

  const seq = raw["last_seen_seq"];
  if (isNonNegativeInteger(seq)) record.last_seen_seq = seq;

  const hint = raw["status_hint"];
  if (isStatusHint(hint)) record.status_hint = hint;

  return record;
}

/**
 * Re-project a record through {@link parseActiveSession} so the persisted
 * form is the canonical normalized shape. A draft built by a caller (or
 * a hostile cast) cannot smuggle extra fields through this serializer:
 * the parser explicitly drops anything outside the eight documented keys.
 *
 * Returns `null` if the draft cannot be normalized (e.g. missing
 * `session_id`).
 */
export function normalizeActiveSession(
  draft: ActiveSessionRecord,
): ActiveSessionRecord | null {
  return parseActiveSession(draft);
}

/**
 * Serialize for localStorage. Always emits the canonical normalized
 * shape so the redaction sentinel test can pin the JSON the storage
 * layer actually sees. Returns `null` if the draft cannot be normalized
 * (the caller should not attempt to store it).
 */
export function serializeActiveSession(
  record: ActiveSessionRecord,
): string | null {
  const normalized = normalizeActiveSession(record);
  if (normalized === null) return null;
  return JSON.stringify(normalized);
}

interface StorageLike {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

function storage(): StorageLike | null {
  try {
    if (typeof globalThis === "undefined") return null;
    const store = (globalThis as { localStorage?: StorageLike }).localStorage;
    return store ?? null;
  } catch {
    return null;
  }
}

/**
 * Read the saved active session record from localStorage. Any failure —
 * missing key, unavailable storage, JSON parse error, schema mismatch —
 * collapses to `null` silently. Errors are NEVER logged or surfaced;
 * the only signal a caller gets is "no record", which is exactly the
 * right thing for a local convenience pointer.
 */
export function loadActiveSession(): ActiveSessionRecord | null {
  const store = storage();
  if (!store) return null;
  let raw: string | null;
  try {
    raw = store.getItem(ACTIVE_SESSION_STORAGE_KEY);
  } catch {
    return null;
  }
  if (raw === null) return null;
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return null;
  }
  return parseActiveSession(parsed);
}

/**
 * Persist an active session record to localStorage. Returns `true` on
 * success, `false` on any failure (storage unavailable, quota exceeded,
 * unnormalisable draft). The caller surfaces the boolean as a non-fatal
 * signal — losing the local pointer is recoverable; the saved-record
 * affordance simply will not appear next time.
 */
export function saveActiveSession(record: ActiveSessionRecord): boolean {
  const store = storage();
  if (!store) return false;
  const json = serializeActiveSession(record);
  if (json === null) return false;
  try {
    store.setItem(ACTIVE_SESSION_STORAGE_KEY, json);
    return true;
  } catch {
    return false;
  }
}

/**
 * Update only the `last_seen_seq` of the existing record without
 * disturbing the rest. Returns `true` on success. If no record is saved,
 * if the saved record's `session_id` does not match `expectedSessionId`,
 * or if `seq` is not a non-negative integer, the call is a silent no-op
 * and returns `false`. The session-id guard prevents a stale background
 * write from a previous session id from clobbering a fresh launch.
 */
export function updateActiveSessionSeq(
  expectedSessionId: string,
  seq: number,
): boolean {
  if (!isNonNegativeInteger(seq)) return false;
  const existing = loadActiveSession();
  if (existing === null) return false;
  if (existing.session_id !== expectedSessionId) return false;
  return saveActiveSession({
    ...existing,
    last_seen_seq: seq,
    saved_at: new Date().toISOString(),
  });
}

/** Remove the saved record. */
export function clearActiveSession(): void {
  const store = storage();
  if (!store) return;
  try {
    store.removeItem(ACTIVE_SESSION_STORAGE_KEY);
  } catch {
    // Same swallow rationale as `saveActiveSession`.
  }
}

/**
 * Build a record from an {@link ActiveLaunch}. Used by `AppShell` on
 * launch / reconnect. Pulls out only the safe public fields — the
 * launch type itself does not carry secrets, but a future widening
 * could; this helper is the boundary that keeps the saved record honest.
 */
export function activeSessionFromLaunch(
  launch: ActiveLaunch,
  opts: { statusHint?: TerminalSessionStatus } = {},
): ActiveSessionRecord {
  const record: ActiveSessionRecord = {
    session_id: launch.sessionId,
    cols: launch.cols,
    rows: launch.rows,
    saved_at: new Date().toISOString(),
  };
  if (typeof launch.profileLabel === "string" && launch.profileLabel.length > 0) {
    const cleaned = sanitizeLabel(launch.profileLabel);
    if (cleaned.length > 0) record.profile_label = cleaned;
  }
  if (
    typeof launch.lastSeenSeq === "number" &&
    Number.isInteger(launch.lastSeenSeq) &&
    launch.lastSeenSeq >= 0
  ) {
    record.last_seen_seq = launch.lastSeenSeq;
  }
  if (opts.statusHint !== undefined) {
    record.status_hint = opts.statusHint;
  }
  return record;
}

/**
 * Whether the empty-state Terminal view should offer the saved-record
 * reconnect affordance.
 *
 * Returns `true` when there IS a saved record AND the active launch
 * (if any) is for a different session id — the "already attached to
 * the same session" guard prevents a footgun that would tear down the
 * live workspace and rebuild it from the saved pointer.
 *
 * Today the empty-state branch only renders when `currentSessionId` is
 * `null` (the only path with no `ProductionTerminal` mounted), so the
 * id-equality guard is forward-compatible defence. The function is
 * exposed so a vitest can pin the contract independently of the Svelte
 * runtime — see `tests/activeSessionStore.test.ts`.
 */
export function shouldOfferReconnect(
  record: ActiveSessionRecord | null,
  currentSessionId: string | null,
): boolean {
  if (record === null) return false;
  if (currentSessionId !== null && currentSessionId === record.session_id) {
    return false;
  }
  return true;
}

/**
 * Build a reconnect-attempt {@link ActiveLaunch} from a saved record.
 *
 * Contract: `lastSeenSeq` is included on the returned launch ONLY when
 * the saved value is a strictly positive integer. Zero, missing, and
 * any malformed value collapse to "no resume bookmark" — the wire
 * `attach` then skips the replay request and the operator gets a fresh
 * attach. Cell-grid dims fall back to the standard 80×24 if the saved
 * record omitted them.
 */
export function buildReconnectAttempt(record: ActiveSessionRecord): ActiveLaunch {
  const launch: ActiveLaunch = {
    sessionId: record.session_id,
    cols: record.cols ?? 80,
    rows: record.rows ?? 24,
  };
  if (record.profile_label !== undefined && record.profile_label.length > 0) {
    launch.profileLabel = record.profile_label;
  }
  if (
    typeof record.last_seen_seq === "number" &&
    Number.isInteger(record.last_seen_seq) &&
    record.last_seen_seq > 0
  ) {
    launch.lastSeenSeq = record.last_seen_seq;
  }
  return launch;
}
