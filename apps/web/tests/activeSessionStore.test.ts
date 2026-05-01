import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  ACTIVE_SESSION_STORAGE_KEY,
  PROFILE_LABEL_MAX_LEN,
  SESSION_ID_MAX_LEN,
  activeSessionFromLaunch,
  buildReconnectAttempt,
  clearActiveSession,
  loadActiveSession,
  normalizeActiveSession,
  parseActiveSession,
  saveActiveSession,
  serializeActiveSession,
  shouldOfferReconnect,
  updateActiveSessionSeq,
  type ActiveSessionRecord,
} from "../src/lib/app/terminal/activeSessionStore.js";
import type { ActiveLaunch } from "../src/lib/app/terminal/activeLaunch.js";

class MemoryStorage {
  private map = new Map<string, string>();
  getItem(key: string): string | null {
    return this.map.has(key) ? this.map.get(key)! : null;
  }
  setItem(key: string, value: string): void {
    this.map.set(key, value);
  }
  removeItem(key: string): void {
    this.map.delete(key);
  }
  clear(): void {
    this.map.clear();
  }
  raw(): Map<string, string> {
    return this.map;
  }
}

const STORAGE_HOST = globalThis as { localStorage?: unknown };
let storage: MemoryStorage;
let originalStorage: unknown;

beforeEach(() => {
  storage = new MemoryStorage();
  originalStorage = STORAGE_HOST.localStorage;
  Object.defineProperty(globalThis, "localStorage", {
    value: storage,
    configurable: true,
    writable: true,
  });
});

afterEach(() => {
  if (originalStorage === undefined) {
    delete (globalThis as { localStorage?: unknown }).localStorage;
  } else {
    Object.defineProperty(globalThis, "localStorage", {
      value: originalStorage,
      configurable: true,
      writable: true,
    });
  }
  vi.restoreAllMocks();
});

const SESSION_ID = "11111111-2222-3333-4444-555555555555";
const SAVED_AT = "2026-05-01T12:00:00.000Z";

function validRecord(overrides: Partial<ActiveSessionRecord> = {}): ActiveSessionRecord {
  return {
    session_id: SESSION_ID,
    profile_label: "prod-app",
    cols: 100,
    rows: 30,
    last_seen_seq: 42,
    status_hint: "active",
    saved_at: SAVED_AT,
    ...overrides,
  };
}

describe("parseActiveSession", () => {
  it("returns null for non-objects", () => {
    expect(parseActiveSession(null)).toBeNull();
    expect(parseActiveSession("nope")).toBeNull();
    expect(parseActiveSession(42)).toBeNull();
    expect(parseActiveSession([])).toBeNull();
    expect(parseActiveSession(undefined)).toBeNull();
  });

  it("returns null when session_id is missing", () => {
    expect(parseActiveSession({ saved_at: SAVED_AT })).toBeNull();
  });

  it("returns null when session_id is empty", () => {
    expect(
      parseActiveSession({ session_id: "", saved_at: SAVED_AT }),
    ).toBeNull();
  });

  it("returns null when session_id is oversized", () => {
    const big = "x".repeat(SESSION_ID_MAX_LEN + 1);
    expect(
      parseActiveSession({ session_id: big, saved_at: SAVED_AT }),
    ).toBeNull();
  });

  it("returns null when session_id is the wrong type", () => {
    expect(
      parseActiveSession({ session_id: 42, saved_at: SAVED_AT }),
    ).toBeNull();
  });

  it("returns null when saved_at is missing", () => {
    expect(parseActiveSession({ session_id: SESSION_ID })).toBeNull();
  });

  it("accepts a fully-valid record", () => {
    const record = validRecord();
    expect(parseActiveSession(record)).toEqual(record);
  });

  it("ignores unknown / extra fields (no smuggling)", () => {
    const parsed = parseActiveSession({
      session_id: SESSION_ID,
      saved_at: SAVED_AT,
      // hostile fixture: a stray secret cannot reach the parsed object
      private_key: "-----BEGIN OPENSSH PRIVATE KEY-----",
      encrypted_private_key: "ENCRYPTED-DO-NOT-PERSIST",
      session_output: "RAW PTY OUTPUT",
      access_token: "BEARER abc.def.ghi",
      replay_buffer: ["frame1", "frame2"],
      __proto__: { stolen: true },
    });
    expect(parsed).not.toBeNull();
    const keys = Object.keys(parsed!);
    expect(keys).not.toContain("private_key");
    expect(keys).not.toContain("encrypted_private_key");
    expect(keys).not.toContain("session_output");
    expect(keys).not.toContain("access_token");
    expect(keys).not.toContain("replay_buffer");
    expect((parsed as Record<string, unknown>).private_key).toBeUndefined();
    expect(
      (parsed as Record<string, unknown>).encrypted_private_key,
    ).toBeUndefined();
  });

  it("drops malformed optional fields silently rather than rejecting the record", () => {
    const parsed = parseActiveSession({
      session_id: SESSION_ID,
      saved_at: SAVED_AT,
      cols: "eighty", // wrong type
      rows: -5, // out of range
      last_seen_seq: -1, // negative
      status_hint: "ghost", // unknown status
      profile_label: 42, // wrong type
    });
    expect(parsed).not.toBeNull();
    expect(parsed!.cols).toBeUndefined();
    expect(parsed!.rows).toBeUndefined();
    expect(parsed!.last_seen_seq).toBeUndefined();
    expect(parsed!.status_hint).toBeUndefined();
    expect(parsed!.profile_label).toBeUndefined();
  });

  it("rejects out-of-range cell-grid dims", () => {
    const parsed = parseActiveSession({
      session_id: SESSION_ID,
      saved_at: SAVED_AT,
      cols: 9999,
      rows: 0,
    });
    expect(parsed!.cols).toBeUndefined();
    expect(parsed!.rows).toBeUndefined();
  });

  it("strips control characters from profile_label", () => {
    const dirty = `prod${String.fromCharCode(0x00)}\napp${String.fromCharCode(0x7f)}`;
    const parsed = parseActiveSession({
      session_id: SESSION_ID,
      saved_at: SAVED_AT,
      profile_label: dirty,
    });
    expect(parsed!.profile_label).toBe("prodapp");
  });

  it("clips overlong profile_label", () => {
    const big = "x".repeat(PROFILE_LABEL_MAX_LEN + 50);
    const parsed = parseActiveSession({
      session_id: SESSION_ID,
      saved_at: SAVED_AT,
      profile_label: big,
    });
    expect(parsed!.profile_label!.length).toBe(PROFILE_LABEL_MAX_LEN);
  });

  it("accepts last_seen_seq of 0 (valid bookmark, no replay request)", () => {
    const parsed = parseActiveSession({
      session_id: SESSION_ID,
      saved_at: SAVED_AT,
      last_seen_seq: 0,
    });
    expect(parsed!.last_seen_seq).toBe(0);
  });
});

describe("normalizeActiveSession + serializeActiveSession redaction", () => {
  it("normalizes round-trip", () => {
    const draft = validRecord();
    expect(normalizeActiveSession(draft)).toEqual(draft);
  });

  it("never echoes a smuggled secret in the JSON form", () => {
    // hostile cast: simulate an upstream layer assigning extra fields
    const draft = {
      ...validRecord(),
      private_key: "PRIVATE-DO-NOT-PERSIST",
      encrypted_private_key: "ENCRYPTED-DO-NOT-PERSIST",
      session_output: "RAW PTY OUTPUT",
      access_token: "BEARER ABC",
      replay_buffer: ["frame1", "frame2"],
      ssh_pem: "-----BEGIN OPENSSH PRIVATE KEY-----",
    } as ActiveSessionRecord;
    const json = serializeActiveSession(draft);
    expect(json).not.toBeNull();
    expect(json).not.toContain("PRIVATE-DO-NOT-PERSIST");
    expect(json).not.toContain("ENCRYPTED-DO-NOT-PERSIST");
    expect(json).not.toContain("RAW PTY OUTPUT");
    expect(json).not.toContain("BEARER ABC");
    expect(json).not.toContain("BEGIN OPENSSH");
    expect(json).not.toContain("frame1");
    expect(json).not.toMatch(/private_key/);
    expect(json).not.toMatch(/encrypted_private_key/);
    expect(json).not.toMatch(/session_output/);
    expect(json).not.toMatch(/access_token/);
    expect(json).not.toMatch(/replay_buffer/);
    expect(json).not.toMatch(/ssh_pem/);
  });

  it("returns null for an unnormalisable draft", () => {
    expect(
      serializeActiveSession({
        session_id: "",
        saved_at: SAVED_AT,
      } as ActiveSessionRecord),
    ).toBeNull();
  });
});

describe("loadActiveSession", () => {
  it("returns null when the key is missing", () => {
    expect(loadActiveSession()).toBeNull();
  });

  it("returns null when the JSON is malformed", () => {
    storage.setItem(ACTIVE_SESSION_STORAGE_KEY, "{not-json");
    expect(loadActiveSession()).toBeNull();
  });

  it("returns null when the entry is the wrong shape", () => {
    storage.setItem(ACTIVE_SESSION_STORAGE_KEY, JSON.stringify(42));
    expect(loadActiveSession()).toBeNull();
  });

  it("returns null when session_id is missing", () => {
    storage.setItem(
      ACTIVE_SESSION_STORAGE_KEY,
      JSON.stringify({ saved_at: SAVED_AT }),
    );
    expect(loadActiveSession()).toBeNull();
  });

  it("returns null when session_id is invalid", () => {
    storage.setItem(
      ACTIVE_SESSION_STORAGE_KEY,
      JSON.stringify({ session_id: "", saved_at: SAVED_AT }),
    );
    expect(loadActiveSession()).toBeNull();
  });

  it("ignores unknown fields on load (no smuggling)", () => {
    storage.setItem(
      ACTIVE_SESSION_STORAGE_KEY,
      JSON.stringify({
        session_id: SESSION_ID,
        saved_at: SAVED_AT,
        cols: 100,
        rows: 30,
        private_key: "PRIV",
        encrypted_private_key: "ENC",
        session_output: "should never persist",
        access_token: "BEARER ABC",
        replay_buffer: ["frame1"],
      }),
    );
    const loaded = loadActiveSession();
    expect(loaded).not.toBeNull();
    const keys = Object.keys(loaded!).sort();
    expect(keys).toEqual(["cols", "rows", "saved_at", "session_id"].sort());
    expect(keys).not.toContain("private_key");
    expect(keys).not.toContain("encrypted_private_key");
    expect(keys).not.toContain("session_output");
    expect(keys).not.toContain("access_token");
    expect(keys).not.toContain("replay_buffer");
  });

  it("does not log on parse failure", () => {
    const log = vi.spyOn(console, "log").mockImplementation(() => {});
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const err = vi.spyOn(console, "error").mockImplementation(() => {});
    storage.setItem(ACTIVE_SESSION_STORAGE_KEY, "{not-json");
    loadActiveSession();
    expect(log).not.toHaveBeenCalled();
    expect(warn).not.toHaveBeenCalled();
    expect(err).not.toHaveBeenCalled();
  });

  it("collapses to null when localStorage is unavailable", () => {
    Object.defineProperty(globalThis, "localStorage", {
      value: undefined,
      configurable: true,
      writable: true,
    });
    expect(loadActiveSession()).toBeNull();
  });
});

describe("saveActiveSession + clearActiveSession", () => {
  it("round-trips through normalize", () => {
    const out = validRecord();
    expect(saveActiveSession(out)).toBe(true);
    expect(loadActiveSession()).toEqual(out);
  });

  it("returns false when storage throws", () => {
    const failing = new MemoryStorage();
    failing.setItem = () => {
      throw new Error("quota");
    };
    Object.defineProperty(globalThis, "localStorage", {
      value: failing,
      configurable: true,
      writable: true,
    });
    expect(saveActiveSession(validRecord())).toBe(false);
  });

  it("returns false when the draft is unnormalisable", () => {
    expect(
      saveActiveSession({
        session_id: "",
        saved_at: SAVED_AT,
      } as ActiveSessionRecord),
    ).toBe(false);
  });

  it("clearActiveSession removes the entry so the next load returns null", () => {
    saveActiveSession(validRecord());
    expect(loadActiveSession()).not.toBeNull();
    clearActiveSession();
    expect(loadActiveSession()).toBeNull();
  });

  it("clearActiveSession is a no-op when storage is unavailable (no throw)", () => {
    Object.defineProperty(globalThis, "localStorage", {
      value: undefined,
      configurable: true,
      writable: true,
    });
    expect(() => clearActiveSession()).not.toThrow();
  });

  it("never persists a smuggled secret in the stored value", () => {
    const draft = {
      ...validRecord(),
      private_key: "PRIVATE-DO-NOT-PERSIST",
      encrypted_private_key: "ENCRYPTED-DO-NOT-PERSIST",
      session_output: "RAW PTY OUTPUT",
      access_token: "BEARER abc",
    } as ActiveSessionRecord;
    saveActiveSession(draft);
    const stored = storage.raw().get(ACTIVE_SESSION_STORAGE_KEY);
    expect(stored).toBeDefined();
    expect(stored).not.toContain("PRIVATE-DO-NOT-PERSIST");
    expect(stored).not.toContain("ENCRYPTED-DO-NOT-PERSIST");
    expect(stored).not.toContain("RAW PTY OUTPUT");
    expect(stored).not.toContain("BEARER abc");
    expect(stored).not.toMatch(/private_key/);
  });
});

describe("updateActiveSessionSeq", () => {
  it("returns false when no record is saved", () => {
    expect(updateActiveSessionSeq(SESSION_ID, 5)).toBe(false);
  });

  it("returns false when the saved id does not match", () => {
    saveActiveSession(validRecord());
    expect(updateActiveSessionSeq("different-id", 5)).toBe(false);
    expect(loadActiveSession()!.last_seen_seq).toBe(42);
  });

  it("returns false for invalid seq values", () => {
    saveActiveSession(validRecord());
    expect(updateActiveSessionSeq(SESSION_ID, -1)).toBe(false);
    expect(updateActiveSessionSeq(SESSION_ID, 1.5)).toBe(false);
    expect(updateActiveSessionSeq(SESSION_ID, Number.NaN)).toBe(false);
  });

  it("updates only the seq + saved_at fields, leaving others intact", () => {
    saveActiveSession(validRecord());
    expect(updateActiveSessionSeq(SESSION_ID, 100)).toBe(true);
    const reloaded = loadActiveSession();
    expect(reloaded!.last_seen_seq).toBe(100);
    expect(reloaded!.session_id).toBe(SESSION_ID);
    expect(reloaded!.profile_label).toBe("prod-app");
    expect(reloaded!.cols).toBe(100);
    expect(reloaded!.rows).toBe(30);
    // saved_at refreshed
    expect(reloaded!.saved_at).not.toBe(SAVED_AT);
  });
});

describe("activeSessionFromLaunch", () => {
  it("copies safe fields only", () => {
    const launch: ActiveLaunch = {
      sessionId: SESSION_ID,
      cols: 120,
      rows: 40,
      profileLabel: "prod-app",
      lastSeenSeq: 7,
    };
    const record = activeSessionFromLaunch(launch);
    expect(record.session_id).toBe(SESSION_ID);
    expect(record.cols).toBe(120);
    expect(record.rows).toBe(40);
    expect(record.profile_label).toBe("prod-app");
    expect(record.last_seen_seq).toBe(7);
    expect(typeof record.saved_at).toBe("string");
    expect(record.saved_at.length).toBeGreaterThan(0);
  });

  it("does not copy a smuggled extra key", () => {
    const launch = {
      sessionId: SESSION_ID,
      cols: 80,
      rows: 24,
      profileLabel: "prod-app",
      // hostile cast: a stray field cannot smuggle
      private_key: "PRIVATE-DO-NOT-PERSIST",
      encrypted_private_key: "ENC",
      session_output: "RAW",
    } as unknown as ActiveLaunch;
    const record = activeSessionFromLaunch(launch);
    const keys = Object.keys(record);
    expect(keys).not.toContain("private_key");
    expect(keys).not.toContain("encrypted_private_key");
    expect(keys).not.toContain("session_output");
    const json = JSON.stringify(record);
    expect(json).not.toContain("PRIVATE-DO-NOT-PERSIST");
    expect(json).not.toContain("ENC");
  });

  it("omits last_seen_seq when not provided", () => {
    const launch: ActiveLaunch = {
      sessionId: SESSION_ID,
      cols: 80,
      rows: 24,
    };
    const record = activeSessionFromLaunch(launch);
    expect(record.last_seen_seq).toBeUndefined();
  });

  it("accepts an optional status hint", () => {
    const launch: ActiveLaunch = {
      sessionId: SESSION_ID,
      cols: 80,
      rows: 24,
    };
    const record = activeSessionFromLaunch(launch, { statusHint: "active" });
    expect(record.status_hint).toBe("active");
  });
});

describe("shouldOfferReconnect", () => {
  it("returns false when there is no saved record", () => {
    expect(shouldOfferReconnect(null, null)).toBe(false);
    expect(shouldOfferReconnect(null, SESSION_ID)).toBe(false);
  });

  it("returns true when a saved record exists and no session is active", () => {
    expect(shouldOfferReconnect(validRecord(), null)).toBe(true);
  });

  it("returns false when the active session id matches the saved id (footgun guard)", () => {
    expect(shouldOfferReconnect(validRecord(), SESSION_ID)).toBe(false);
  });

  it("returns true when the active session id differs from the saved id", () => {
    expect(shouldOfferReconnect(validRecord(), "different-id")).toBe(true);
  });
});

describe("buildReconnectAttempt", () => {
  it("returns a launch shape with cols/rows", () => {
    const record = validRecord();
    const launch = buildReconnectAttempt(record);
    expect(launch.sessionId).toBe(SESSION_ID);
    expect(launch.cols).toBe(100);
    expect(launch.rows).toBe(30);
    expect(launch.profileLabel).toBe("prod-app");
  });

  it("falls back to 80x24 when dims are missing", () => {
    const launch = buildReconnectAttempt({
      session_id: SESSION_ID,
      saved_at: SAVED_AT,
    });
    expect(launch.cols).toBe(80);
    expect(launch.rows).toBe(24);
  });

  it("includes lastSeenSeq only when strictly positive", () => {
    expect(
      buildReconnectAttempt({ ...validRecord(), last_seen_seq: 5 }).lastSeenSeq,
    ).toBe(5);
    expect(
      buildReconnectAttempt({ ...validRecord(), last_seen_seq: 0 }).lastSeenSeq,
    ).toBeUndefined();
    expect(
      buildReconnectAttempt({
        ...validRecord(),
        last_seen_seq: undefined,
      }).lastSeenSeq,
    ).toBeUndefined();
  });

  it("omits profileLabel when missing", () => {
    const launch = buildReconnectAttempt({
      session_id: SESSION_ID,
      saved_at: SAVED_AT,
    });
    expect(launch.profileLabel).toBeUndefined();
  });

  it("does not leak any extra keys onto the launch shape", () => {
    const launch = buildReconnectAttempt(validRecord());
    const keys = Object.keys(launch).sort();
    // Sessions reconnect contract: launch is the four documented fields.
    expect(keys).toEqual(
      ["cols", "lastSeenSeq", "profileLabel", "rows", "sessionId"].sort(),
    );
  });
});
