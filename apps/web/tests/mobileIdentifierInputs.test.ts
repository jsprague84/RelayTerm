/**
 * Mobile-keyboard hardening for exact-string identifier inputs.
 *
 * Android (and some iOS Safari) keyboards autocapitalize and/or
 * autocorrect normal `<input type="text">` fields. That is wrong for
 * exact-string technical fields — SSH usernames, hostnames, profile
 * names typed as operator labels — because the wire value must match
 * an external string byte-for-byte. The 2026-05-09 Android host-key
 * replacement smoke surfaced this in production: the username `smoke`
 * was silently changed to `Smoke`, the auth-check then failed, and
 * recovery required a one-row Postgres UPDATE.
 *
 * The fix is to set, on each exact-string identifier input:
 *   - autocapitalize="none"
 *   - autocorrect="off"
 *   - spellcheck="false"
 *   - inputmode="text"
 *
 * This file scans the production-shell `.svelte` source files as text
 * (the test harness is vitest-on-Node, no jsdom / Svelte component
 * mount — see `hostKeyPanelReplace.test.ts` for the same pattern) and
 * pins the attribute set on each input that is known to feed a value
 * that must match exactly on the server side. The list is deliberately
 * narrow: free-form prose fields (host display name, profile tags) are
 * NOT required to disable spellcheck/autocorrect because spellcheck is
 * genuinely useful there.
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
const IDENTITIES_VIEW_PATH = resolve(
  __dirname,
  "../src/lib/app/views/IdentitiesView.svelte",
);

const SERVERS_VIEW = readFileSync(SERVERS_VIEW_PATH, "utf8");
const IDENTITIES_VIEW = readFileSync(IDENTITIES_VIEW_PATH, "utf8");

/**
 * Extract the attribute block of a single `<input ... />` element
 * matching the given `data-testid`. The .svelte sources here use a
 * single canonical formatting style for these inputs (one attribute
 * per line, opening `<input` on its own line, closing `/>` on its
 * own line), so a regex span between `<input` and the FIRST `/>` that
 * also contains the testid is enough — no AST needed. Returns the
 * matched element text, or throws with a descriptive message so a
 * future formatting change fails loudly instead of silently passing.
 */
function extractInputByTestId(source: string, testId: string): string {
  // Non-greedy match between `<input` and the first `/>`; we then
  // verify the testid we expect is inside that span.
  const pattern = new RegExp(
    String.raw`<input\b[^>]*?data-testid="${testId}"[^>]*?/>`,
    "s",
  );
  const match = source.match(pattern);
  if (!match) {
    throw new Error(`No <input data-testid="${testId}" /> found`);
  }
  return match[0];
}

/**
 * Inputs that carry an exact-string technical value (host username,
 * hostname, server-profile username override, server-profile / SSH
 * identity operator label). These MUST disable mobile-keyboard
 * autocapitalize, autocorrect, and spellcheck, and explicitly set
 * `inputmode="text"`.
 *
 * The list is intentionally a hard-coded array, not derived from the
 * file, so a future edit that adds a new technical input is forced
 * to update this list explicitly — making the decision visible in the
 * test diff alongside the component change.
 */
const TECHNICAL_IDENTIFIER_INPUTS: Array<{
  source: string;
  testId: string;
  /** Documents WHY this field is exact-string technical. */
  why: string;
}> = [
  {
    source: SERVERS_VIEW,
    testId: "servers-create-host-username",
    why:
      "SSH default username; the 2026-05-09 Android smoke regression " +
      "field — Android changed `smoke` to `Smoke`, breaking auth.",
  },
  {
    source: SERVERS_VIEW,
    testId: "servers-create-host-hostname",
    why: "DNS hostname / IP literal; must match server side byte-for-byte.",
  },
  {
    source: SERVERS_VIEW,
    testId: "servers-create-profile-username-override",
    why:
      "SSH username override for the profile; same exact-match wire " +
      "semantics as the host default username.",
  },
  {
    source: SERVERS_VIEW,
    testId: "servers-create-profile-name",
    why:
      "Server-profile operator label; operators round-trip these as " +
      "search keys and CLI references, so mobile-altered casing breaks " +
      "operator memory.",
  },
  {
    source: IDENTITIES_VIEW,
    testId: "identities-generate-name",
    why:
      "SSH identity operator label; same operator-label rationale as " +
      "the server-profile name.",
  },
];

describe("mobile-keyboard hardening for exact-string identifier inputs", () => {
  for (const { source, testId, why } of TECHNICAL_IDENTIFIER_INPUTS) {
    it(`${testId} disables mobile autocapitalize/autocorrect (${why})`, () => {
      const element = extractInputByTestId(source, testId);
      // `autocapitalize="none"` is the regression-pinning attribute —
      // it's what was missing when Android changed `smoke` to `Smoke`.
      // The other three round out the hardening: `autocorrect="off"`
      // (Safari + WebKitGTK), `spellcheck="false"` (the cross-engine
      // disable), `inputmode="text"` (explicit hint, defaults to text
      // but stated so a future numeric/email keyboard change in this
      // file is an obvious test diff).
      expect(element).toContain('autocapitalize="none"');
      expect(element).toContain('autocorrect="off"');
      expect(element).toContain('spellcheck="false"');
      expect(element).toContain('inputmode="text"');
    });
  }

  it("the 2026-05-09 Android smoke regression field is the host default username", () => {
    // Belt-and-suspenders: even if a future refactor renames the test
    // id, the user-facing label "Default username" must still be the
    // field that received `autocapitalize="none"`. Pin the *label* to
    // the *attribute* by scanning for the label block immediately
    // followed (within ~500 chars) by an input with the hardening set.
    const labelToInput = new RegExp(
      String.raw`Default username[\s\S]{0,500}<input[^>]*?autocapitalize="none"[^>]*?/>`,
      "s",
    );
    expect(
      labelToInput.test(SERVERS_VIEW),
      "The 'Default username' label MUST be followed by an input with " +
        'autocapitalize="none" — this is the field that broke the ' +
        "2026-05-09 Android host-key replacement smoke.",
    ).toBe(true);
  });
});

describe("prose-style inputs are not unnecessarily degraded", () => {
  // Free-form / prose fields where mobile spellcheck and autocorrect
  // are genuinely useful. The brief said: "Do not add these attributes
  // to free-form description/note fields where spellcheck/autocorrect
  // would be useful." Pin that absence here so a careless sweep
  // doesn't disable spellcheck on every input in the file.
  const PROSE_INPUTS = [
    {
      testId: "servers-create-host-display-name",
      why:
        "Human-readable display label (e.g. 'Bastion (us-east-1)') — " +
        "not used as an exact wire value.",
    },
    {
      testId: "servers-create-profile-tags",
      why:
        "Comma-separated free-text tag list; parser already normalises " +
        "tags so casing affordances on the keyboard are not load-bearing.",
    },
  ] as const;

  for (const { testId, why } of PROSE_INPUTS) {
    it(`${testId} keeps mobile-keyboard affordances (${why})`, () => {
      const element = extractInputByTestId(SERVERS_VIEW, testId);
      // These prose inputs intentionally do NOT carry `autocapitalize`
      // or `autocorrect` overrides. If a future commit adds them, this
      // assertion fails loudly so the decision lands in a deliberate
      // test diff and can be debated, not in a silent sweep.
      expect(element).not.toContain('autocapitalize="none"');
      expect(element).not.toContain('autocorrect="off"');
    });
  }
});
