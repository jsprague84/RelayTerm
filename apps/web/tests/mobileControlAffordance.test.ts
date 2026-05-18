/**
 * Mobile portrait touch-target affordance (feat/mobile-shell-usability-polish).
 *
 * The production shell ships compact action buttons (`px-3 py-1 text-xs`)
 * at the desktop sizing baseline. On a 390 × 844 Android-class portrait
 * viewport — the v1 cutline's mobile target per
 * `docs/v1-production-readiness.md` §10 + `apps/web/e2e/SMOKE.md` § D —
 * those buttons compute to ~24-26px tall, well below the
 * WCAG-/Apple-/Material-recommended ~36-44px minimum. The polish slice
 * bumps the mobile sizing without touching the desktop layout:
 *
 *   `min-h-9 ... py-1.5 ... sm:min-h-0 sm:py-1`
 *
 * That keeps every existing `data-testid` and the desktop classes
 * byte-identical while ensuring the affordance is reachable with a
 * fingertip at narrow widths.
 *
 * This file is a static text-scan harness (same style as
 * `mobileNav.test.ts` and `mobileIdentifierInputs.test.ts`) — no jsdom,
 * no Svelte mount, no rendered HTML inspection. The goal is to pin the
 * `min-h-9` token on the named critical-action buttons so a careless
 * refactor that drops it on a mobile-shipping action fails the suite
 * before staging.
 *
 * Scope rule: only the touch-target tokens are pinned. The exact
 * padding / colour / border classes are NOT asserted — those are visual
 * polish and a deliberate restyle should not need a test edit.
 */

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";
import { describe, expect, it } from "vitest";

const __dirname = dirname(fileURLToPath(import.meta.url));

const SERVERS_VIEW_PATH = resolve(
  __dirname,
  "../src/lib/app/views/ServersView.svelte",
);
const SESSIONS_VIEW_PATH = resolve(
  __dirname,
  "../src/lib/app/views/SessionsView.svelte",
);
const PRODUCTION_TERMINAL_PATH = resolve(
  __dirname,
  "../src/lib/app/terminal/ProductionTerminal.svelte",
);
const APP_SHELL_PATH = resolve(__dirname, "../src/lib/app/AppShell.svelte");

const SERVERS_VIEW = readFileSync(SERVERS_VIEW_PATH, "utf8");
const SESSIONS_VIEW = readFileSync(SESSIONS_VIEW_PATH, "utf8");
const PRODUCTION_TERMINAL = readFileSync(PRODUCTION_TERMINAL_PATH, "utf8");
const APP_SHELL = readFileSync(APP_SHELL_PATH, "utf8");

/**
 * Extract the `class="..."` attribute string of a single `<button ...>`
 * element matching the given `data-testid`. A naive single-regex span
 * between `<button` and the first `>` does NOT work here — many of the
 * buttons in ServersView carry handler attributes whose values contain
 * `=>` arrow-function tokens, which terminate a `[^>]*?` span
 * prematurely. Walking the source in two steps avoids that:
 *
 *   1. Locate `data-testid="<testId>"` (unique within the file).
 *   2. Scan backwards from there to the nearest `<button\b`.
 *   3. Scan forwards from the button opening to find the FIRST
 *      `class="..."` attribute — Svelte sources put `class=` ahead of
 *      `data-testid=` on every button in this codebase, so the first
 *      `class=` after `<button` is the button's own (not a nested
 *      child's, which would only appear AFTER the button's opening
 *      `>`).
 *
 * Returns the class value (the string between the quotes). Throws with
 * descriptive context so a formatting change surfaces as a loud failure
 * rather than a silent pass.
 */
function extractButtonClass(source: string, testId: string): string {
  const testidMarker = `data-testid="${testId}"`;
  const testidIndex = source.indexOf(testidMarker);
  if (testidIndex === -1) {
    throw new Error(`No element with data-testid="${testId}" found`);
  }
  const buttonOpenIndex = source.lastIndexOf("<button", testidIndex);
  if (buttonOpenIndex === -1) {
    throw new Error(
      `Found data-testid="${testId}" but no preceding <button opening`,
    );
  }
  const classMatch = source
    .slice(buttonOpenIndex, testidIndex)
    .match(/\bclass="([^"]*)"/);
  if (!classMatch) {
    throw new Error(
      `<button data-testid="${testId}"> has no class= attribute before the testid`,
    );
  }
  return classMatch[1];
}

/**
 * Critical action buttons that MUST be reachable with a fingertip on
 * Android portrait. The list is intentionally a hard-coded array — a
 * future edit that adds a new high-traffic action must update this list
 * explicitly, so the decision lands in a deliberate test diff.
 */
const MOBILE_TOUCH_TARGETS: Array<{
  source: string;
  testId: string;
  /** Documents WHY this button is on the mobile-reachable list. */
  why: string;
}> = [
  // --- Production terminal workspace control row ---
  {
    source: PRODUCTION_TERMINAL,
    testId: "production-terminal-focus",
    why: "Renderer focus pull — first tap after the terminal mounts.",
  },
  {
    source: PRODUCTION_TERMINAL,
    testId: "production-terminal-fit",
    why: "Renderer-fair fit; smoke row 12 explicitly taps this at 390 × 844.",
  },
  {
    source: PRODUCTION_TERMINAL,
    testId: "production-terminal-clear",
    why: "Local viewport clear; commonly tapped during a smoke walk.",
  },
  {
    source: PRODUCTION_TERMINAL,
    testId: "production-terminal-detach",
    why: "Soft disconnect into the detached-TTL window.",
  },
  {
    source: PRODUCTION_TERMINAL,
    testId: "production-terminal-close",
    why: "Hard close — destructive; needs a clear, finger-sized affordance.",
  },
  {
    source: PRODUCTION_TERMINAL,
    testId: "production-terminal-reconnect",
    why: "Operator-initiated re-attach with replay; high-traffic on mobile WS dropouts.",
  },
  {
    source: PRODUCTION_TERMINAL,
    testId: "production-terminal-dispose",
    why: "Local client + renderer teardown.",
  },
  {
    source: PRODUCTION_TERMINAL,
    testId: "production-terminal-back",
    why: "Exit back to the servers view; the only navigation affordance inside the workspace.",
  },

  // --- Sessions list per-row actions ---
  {
    source: SESSIONS_VIEW,
    testId: "sessions-row-reconnect",
    why: "Reconnect / Open — primary action on a session row.",
  },
  {
    source: SESSIONS_VIEW,
    testId: "sessions-row-close",
    why: "Close — destructive per-row action; mobile target.",
  },
  {
    source: SESSIONS_VIEW,
    testId: "sessions-row-view-recording",
    why:
      "Opens the recording replay viewer for a detached/closed row — the " +
      "third per-row action and shares the same flex-wrap row as the other two.",
  },

  // --- Servers view: host detail edit / delete ---
  {
    source: SERVERS_VIEW,
    testId: "host-detail-edit-open",
    why: "Inventory edit affordance (B1 row A staging smoke pattern).",
  },
  {
    source: SERVERS_VIEW,
    testId: "host-detail-delete-open",
    why: "Inventory destructive affordance (B1 row B staging smoke pattern).",
  },
  {
    source: SERVERS_VIEW,
    testId: "host-detail-delete-confirm-submit",
    why: "Typed-name destructive confirm; explicit operator click.",
  },
  {
    source: SERVERS_VIEW,
    testId: "host-detail-delete-cancel",
    why: "Cancel paired with destructive confirm; equally important to reach.",
  },
  {
    source: SERVERS_VIEW,
    testId: "host-detail-edit-save",
    why: "Inventory edit save; round-tripped against the staging smoke.",
  },
  {
    source: SERVERS_VIEW,
    testId: "host-detail-edit-cancel",
    why: "Cancel pair for the inventory edit form.",
  },

  // --- Servers view: profile detail edit / delete ---
  {
    source: SERVERS_VIEW,
    testId: "profile-detail-edit-open",
    why: "Inventory edit affordance (B1 row D staging smoke pattern).",
  },
  {
    source: SERVERS_VIEW,
    testId: "profile-detail-delete-open",
    why: "Inventory destructive affordance (B1 row E staging smoke pattern).",
  },
  {
    source: SERVERS_VIEW,
    testId: "profile-detail-delete-confirm-submit",
    why: "Typed-name destructive confirm on the server profile.",
  },
  {
    source: SERVERS_VIEW,
    testId: "profile-detail-delete-cancel",
    why: "Cancel pair for the profile delete confirmation.",
  },
  {
    source: SERVERS_VIEW,
    testId: "profile-detail-edit-save",
    why: "Profile edit save; matches host-detail-edit-save's reachability.",
  },
  {
    source: SERVERS_VIEW,
    testId: "profile-detail-edit-cancel",
    why: "Cancel pair for the profile edit form.",
  },

  // --- Servers view: profile-row launch and lifecycle controls ---
  {
    source: SERVERS_VIEW,
    testId: "profile-launch-terminal",
    why: "Primary CTA per profile row — drives the mobile launch flow.",
  },
  {
    source: SERVERS_VIEW,
    testId: "profile-disable-open",
    why: "Lifecycle entry point — also routes the 'still-referenced' delete refusal.",
  },
  {
    source: SERVERS_VIEW,
    testId: "profile-disable-submit",
    why: "Typed-name destructive confirm (disable, not delete).",
  },
  {
    source: SERVERS_VIEW,
    testId: "profile-disable-cancel",
    why: "Cancel pair for the disable confirmation.",
  },
  {
    source: SERVERS_VIEW,
    testId: "profile-disable-submitting",
    why:
      "Spinner-only swap-in for profile-disable-submit while the request is " +
      "in flight. Pinned so a regression that strips the mobile sizing on " +
      "the loading state cannot land silently (the operator sees this on " +
      "every disable submit before the request resolves).",
  },
  {
    source: SERVERS_VIEW,
    testId: "profile-enable-submit",
    why: "Re-enable affordance after a disable; one-tap recovery flow.",
  },
];

describe("mobile portrait touch-target affordance", () => {
  for (const { source, testId, why } of MOBILE_TOUCH_TARGETS) {
    it(`${testId} carries the mobile min-height token (${why})`, () => {
      const klass = extractButtonClass(source, testId);
      // `min-h-9` (= 2.25rem = 36px) is the load-bearing token. The
      // `sm:min-h-0` override is also pinned so the desktop layout
      // stays compact (otherwise this rule would silently inflate every
      // button across the SPA).
      expect(klass).toContain("min-h-9");
      expect(klass).toContain("sm:min-h-0");
    });
  }
});

describe("AppShell main padding respects narrow viewports", () => {
  it("AppShell main uses tighter horizontal padding below sm:", () => {
    // Below 640px the main column previously used `px-6` (= 24px each
    // side, 48px lost on a 360-390px phone). The polish slice tightens
    // to `px-3 py-4` on mobile and restores `sm:px-6 sm:py-6` at
    // tablet+ widths. Pinning the responsive classes here means a
    // refactor that drops the mobile override fails before staging.
    expect(APP_SHELL).toMatch(
      /class="flex-1 overflow-y-auto px-3 py-4 sm:px-6 sm:py-6"/,
    );
  });
});

describe("host / profile detail definition lists stack on mobile", () => {
  // The `[\s\S]{0,400}` cap on the gap between the dl opening tag and
  // the first dd's testid is generous on purpose: the actual observed
  // distance today is ~80–120 chars (a dt block + the dd opening). The
  // 400-char ceiling gives a future dt-block expansion (e.g. adding a
  // tooltip-bearing `<span>`) headroom without a test edit, while still
  // catching a real regression that moves the dl opening tag away from
  // its first dd entirely.

  it("host-detail dl avoids max-content overflow at narrow widths", () => {
    // The `grid-cols-[max-content_1fr]` layout overflows when the host
    // hostname or timestamp values are long. The mobile-first rule
    // stacks dt/dd in a flex column; `sm:` restores the grid. Match the
    // dl that wraps `host-detail-display-name`.
    expect(SERVERS_VIEW).toMatch(
      /<dl class="flex flex-col gap-1 text-sm sm:grid sm:grid-cols-\[max-content_1fr\] sm:gap-x-4 sm:gap-y-2">[\s\S]{0,400}data-testid="host-detail-display-name"/,
    );
  });

  it("profile-detail dl avoids max-content overflow at narrow widths", () => {
    // Same rationale — the profile detail dl is the second `dl` in
    // ServersView that uses the same grid template. Match on the
    // `profile-detail-name` testid that follows the dl opening tag.
    expect(SERVERS_VIEW).toMatch(
      /<dl class="flex flex-col gap-1 text-sm sm:grid sm:grid-cols-\[max-content_1fr\] sm:gap-x-4 sm:gap-y-2">[\s\S]{0,400}data-testid="profile-detail-name"/,
    );
  });
});
