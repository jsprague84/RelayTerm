/**
 * Phase 4 wiring tests for `HostKeyPanel.svelte` — the host-key replace
 * affordance + modal.
 *
 * We do NOT mount the Svelte component here: the existing test harness
 * is vitest-on-Node only (no jsdom, no svelte testing-library, no
 * vite-plugin-svelte in the test transform). The strongest practical
 * coverage is therefore:
 *
 *  1. **Static template scan** — read the `.svelte` source as text and
 *     assert that all wire-bearing testids are present, all required
 *     copy strings appear, the wire-forbidden words ("Force trust",
 *     "Override", "Ignore") never appear, and no sentinel literal
 *     (`private_key`, `encrypted_private_key`, `password`, `cookie`,
 *     `session_token`, `token_hash`) ever lands in the static template.
 *  2. **Composition tests** that exercise the gate / decision helpers
 *     in the SAME shape the panel composes them, so a future change
 *     that breaks the visibility / submit-disabled rules trips a unit
 *     test without needing a full component harness.
 *
 * The pure helpers themselves (`replaceGateForPreflight`,
 * `decideReplaceSubmit`, `synthesizePostReplacePreflight`,
 * `replaceConfirmationMatches`, `reasonCodeIsValid`,
 * `replacementReasonOptions`) are exhaustively unit-tested in
 * `replaceHostKeyApi.test.ts`. This file complements that with the
 * panel-level wiring invariants.
 */

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";
import { describe, expect, it } from "vitest";
import {
  decideReplaceSubmit,
  replaceGateForPreflight,
  replacementReasonOptions,
  type ReplaceSubmitDecision,
} from "../src/lib/app/hostKeyTrustState.js";
import type { HostKeyPreflightResponse } from "../src/lib/api/serverProfiles.js";

const __dirname = dirname(fileURLToPath(import.meta.url));
const PANEL_SOURCE_PATH = resolve(
  __dirname,
  "../src/lib/app/views/HostKeyPanel.svelte",
);

const PANEL_SOURCE = readFileSync(PANEL_SOURCE_PATH, "utf8");

const OLD_FP = "SHA256:abcdefGHIJKLmnopqrstuvwxyz0123456789ABCDEFGHJ";
const NEW_FP = "SHA256:zyxwvuTSRQPOnmlkjihgfedcba9876543210ZYXWVUT12";

const PREFLIGHT_BASE: HostKeyPreflightResponse = {
  profile_id: "11111111-1111-1111-1111-111111111111",
  host_id: "22222222-2222-2222-2222-222222222222",
  hostname: "edge-1.example.internal",
  port: 22,
  host_key_status: "unknown",
  host_key_type: "ed25519",
  host_key_fingerprint: NEW_FP,
  active_pin_fingerprint: null,
  message: "host key not yet pinned; KEX-stage probe only",
};

const PREFLIGHT_CHANGED: HostKeyPreflightResponse = {
  ...PREFLIGHT_BASE,
  host_key_status: "changed",
  active_pin_fingerprint: OLD_FP,
};

// ---------------------------------------------------------------------------
// Static template scan — testids, copy, forbidden words, sentinel redaction
// ---------------------------------------------------------------------------

describe("HostKeyPanel.svelte static template scan", () => {
  it("exposes the wire-bearing testids the SPA / future tests rely on", () => {
    const required = [
      // Existing trust / preflight testids — must NOT have regressed.
      "host-key-panel",
      "host-key-preflight-button",
      "host-key-fingerprint",
      "host-key-status-badge",
      "host-key-changed-refused",
      "host-key-trust-button",
      "host-key-confirm-input",
      // Phase 4 replace testids.
      "host-key-replace-button",
      "host-key-replace-modal",
      "host-key-replace-old-fingerprint",
      "host-key-replace-new-fingerprint",
      "host-key-replace-reason-select",
      "host-key-replace-confirm-input",
      "host-key-replace-confirm-mismatch",
      "host-key-replace-submit",
      "host-key-replace-cancel",
      "host-key-replace-error",
      "host-key-replaced-success",
    ];
    for (const id of required) {
      expect(PANEL_SOURCE, `missing testid="${id}"`).toContain(
        `data-testid="${id}"`,
      );
    }
  });

  it("renders the operator-facing copy required by the design", () => {
    const required = [
      // Modal title + lede + disclaimer.
      "Replace trusted host key",
      "RelayTerm will not silently overwrite a pinned host key",
      "After replacement, run auth-check",
      // Button labels.
      "Replace trusted host key…",
      "Replace pin",
      "Replacing…",
      "Cancel",
      // Reason picker prompt.
      "Select a reason…",
      // Typed-confirmation gate copy.
      "Type REPLACE to confirm",
      "Type the literal word REPLACE in uppercase",
      // Success state copy.
      "Host key replaced",
    ];
    for (const text of required) {
      expect(PANEL_SOURCE, `missing required copy: ${text}`).toContain(text);
    }
  });

  it("never uses words that imply silent overwrite or TOFU bypass", () => {
    // The design forbids these words on this surface — they imply a
    // bypass posture that the route deliberately refuses to provide.
    const forbidden = [
      "Force trust",
      "force trust",
      "Override",
      "override",
      "Ignore warning",
      "Ignore the",
      "ignore warning",
      "Disable check",
      "disable check",
      "auto-trust",
    ];
    for (const word of forbidden) {
      expect(PANEL_SOURCE, `forbidden word leaked into template: ${word}`)
        .not.toContain(word);
    }
  });

  it("never echoes secret-shaped field names in the static template", () => {
    // Panel template MUST NOT mention any private-key / session /
    // password / cookie / token field name. The replace API helper
    // already redacts these from parsed responses; this is the second
    // line guarding against accidental rendering of a wire field.
    const forbiddenSubstrings = [
      "private_key",
      "encrypted_private_key",
      "password",
      "cookie",
      "session_token",
      "token_hash",
    ];
    for (const sentinel of forbiddenSubstrings) {
      expect(
        PANEL_SOURCE,
        `panel template references forbidden field: ${sentinel}`,
      ).not.toContain(sentinel);
    }
  });

  it("iterates the reason picker from the canonical option list", () => {
    // Reason labels are interpolated via `{opt.label}` — the literal
    // strings are NOT in the .svelte source. What we CAN pin is the
    // {#each REPLACE_REASON_OPTIONS …} loop and the binding to
    // `replaceReasonCode`, which together prove the picker is sourced
    // from the `replacementReasonOptions()` array. Labels are unit-
    // tested in `replaceHostKeyApi.test.ts`.
    expect(PANEL_SOURCE).toContain("REPLACE_REASON_OPTIONS");
    expect(PANEL_SOURCE).toContain("bind:value={replaceReasonCode}");
    expect(PANEL_SOURCE).toMatch(/\{#each\s+REPLACE_REASON_OPTIONS\b/);
    // Sanity: the canonical list still has the four reasons.
    const options = replacementReasonOptions();
    expect(options).toHaveLength(4);
  });

  it("wires the modal as an aria dialog (keyboard-friendly)", () => {
    // Not a focus-trap (the existing modal pattern doesn't trap), but
    // the dialog role + aria-modal lets screen readers announce the
    // modal context. The brief allowed `role="dialog"` here.
    expect(PANEL_SOURCE).toContain('role="dialog"');
    expect(PANEL_SOURCE).toContain('aria-modal="true"');
    expect(PANEL_SOURCE).toContain('aria-labelledby="host-key-replace-title"');
  });
});

// ---------------------------------------------------------------------------
// Visibility composition — the panel mirrors `replaceGateForPreflight`
// ---------------------------------------------------------------------------

describe("HostKeyPanel replace-button visibility composition", () => {
  it("hides the replace button for unknown / trusted / loading-shaped preflights", () => {
    // The panel tests `replacementSummary !== null`; that summary is
    // only built when `host_key_status === "changed"` AND the active
    // pin is well-shaped — i.e. the same condition as
    // `replaceGateForPreflight(...).kind === "ok"`.
    const cases: HostKeyPreflightResponse[] = [
      { ...PREFLIGHT_BASE, host_key_status: "unknown" },
      {
        ...PREFLIGHT_BASE,
        host_key_status: "trusted",
        active_pin_fingerprint: OLD_FP,
      },
      // Changed but no active pin known (e.g. stale server build that
      // doesn't ship `active_pin_fingerprint`): button must stay hidden.
      {
        ...PREFLIGHT_BASE,
        host_key_status: "changed",
        active_pin_fingerprint: null,
      },
      // Changed with malformed active fingerprint shape: hidden.
      {
        ...PREFLIGHT_BASE,
        host_key_status: "changed",
        active_pin_fingerprint: "not-a-fingerprint",
      },
    ];
    for (const preflight of cases) {
      const gate = replaceGateForPreflight(
        preflight,
        preflight.active_pin_fingerprint,
      );
      expect(gate.kind).not.toBe("ok");
    }
  });

  it("shows the replace button only when status=changed AND active pin is well-shaped", () => {
    const gate = replaceGateForPreflight(
      PREFLIGHT_CHANGED,
      PREFLIGHT_CHANGED.active_pin_fingerprint,
    );
    expect(gate).toEqual({
      kind: "ok",
      old_fingerprint: OLD_FP,
      new_fingerprint: NEW_FP,
    });
  });

  it("button visibility never diverges from submit-readiness gate", () => {
    // Pinned by the panel's `replacementSummary` derivation: it MUST
    // return non-null IFF `replaceGateForPreflight(...).kind === "ok"`.
    // Otherwise a malformed `active_pin_fingerprint` (e.g. wire shape
    // drift or older server build sending garbage) could surface a
    // visible-but-permanently-disabled button — violating the R6
    // "invisible, not just disabled" invariant. Walk a representative
    // matrix of refusal shapes and confirm `decideReplaceSubmit`
    // returns a refusal in EXACTLY the same cases the gate refuses.
    const matrix: Array<{ name: string; preflight: HostKeyPreflightResponse }> = [
      {
        name: "unknown",
        preflight: { ...PREFLIGHT_BASE, host_key_status: "unknown" },
      },
      {
        name: "trusted",
        preflight: {
          ...PREFLIGHT_BASE,
          host_key_status: "trusted",
          active_pin_fingerprint: OLD_FP,
        },
      },
      {
        name: "changed-without-active-pin",
        preflight: {
          ...PREFLIGHT_BASE,
          host_key_status: "changed",
          active_pin_fingerprint: null,
        },
      },
      {
        name: "changed-with-malformed-old-fingerprint",
        preflight: {
          ...PREFLIGHT_BASE,
          host_key_status: "changed",
          active_pin_fingerprint: "not-a-fingerprint",
        },
      },
      {
        name: "changed-with-malformed-new-fingerprint",
        preflight: {
          ...PREFLIGHT_CHANGED,
          host_key_fingerprint: "garbage",
        },
      },
    ];
    for (const { name, preflight } of matrix) {
      const gate = replaceGateForPreflight(
        preflight,
        preflight.active_pin_fingerprint,
      );
      const ready = decideReplaceSubmit(
        preflight,
        "server_reinstalled",
        "REPLACE",
      );
      expect(gate.kind, `${name}: gate ok mismatch`).not.toBe("ok");
      expect(ready.kind, `${name}: decideReplaceSubmit ready mismatch`).toBe(
        "blocked",
      );
    }
  });
});

// ---------------------------------------------------------------------------
// Modal submit-disabled composition — the panel mirrors `decideReplaceSubmit`
// ---------------------------------------------------------------------------

describe("HostKeyPanel replace-submit composition", () => {
  it("disables submit until reason is picked AND typed REPLACE matches AND gate is ok", () => {
    // Reason missing → blocked.
    expect(
      decideReplaceSubmit(PREFLIGHT_CHANGED, null, "REPLACE")
        .kind,
    ).toBe("blocked");
    // Confirm input empty → blocked.
    expect(
      decideReplaceSubmit(PREFLIGHT_CHANGED, "server_reinstalled", "")
        .kind,
    ).toBe("blocked");
    // Confirm input wrong-case → blocked.
    expect(
      decideReplaceSubmit(
        PREFLIGHT_CHANGED,
        "server_reinstalled",
        "replace",
      ).kind,
    ).toBe("blocked");
    // Both reason picked AND typed REPLACE matches → ready.
    const ready = decideReplaceSubmit(
      PREFLIGHT_CHANGED,
      "server_reinstalled",
      "REPLACE",
    );
    expect(ready.kind).toBe("ready");
    if (ready.kind === "ready") {
      expect(ready.request).toEqual({
        expected_old_fingerprint: OLD_FP,
        expected_new_fingerprint: NEW_FP,
        reason_code: "server_reinstalled",
      });
    }
  });

  it("never builds a request when the preflight is not in changed status", () => {
    // This proves the request body is NEVER fabricated from a
    // non-changed preflight, even if the operator filled in reason +
    // typed REPLACE — defence-in-depth against a future component edit
    // that accidentally calls `replaceHostKey` from the unknown branch.
    const decision: ReplaceSubmitDecision = decideReplaceSubmit(
      { ...PREFLIGHT_CHANGED, host_key_status: "unknown" },
      "server_reinstalled",
      "REPLACE",
    );
    expect(decision).toEqual({
      kind: "blocked",
      reason: "not_changed_status",
    });
  });
});
