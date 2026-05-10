import { describe, it, expect } from "vitest";
import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";

/**
 * Production app shell components must NOT statically import from
 * `lib/dev/`. Dev components are pulled in only via the dev-only
 * branch in `App.svelte`, which `import.meta.env.DEV` lets Vite
 * tree-shake out of the production bundle. A stray import from a
 * shell file would tie the production bundle to dev code.
 *
 * Implementation note: this is a deliberately conservative raw-text
 * scan, not an AST or import-resolver check. It will flag the same
 * banned strings inside comments or template literals — that is
 * intentional. The shell surface is small enough that the false-
 * positive rate is zero today, and a regex catch is robust against
 * Svelte component imports, dynamic `import()`, and plain `.ts`
 * files all in one rule.
 */
const APP_DIR = new URL("../src/lib/app/", import.meta.url).pathname;

function* walk(dir: string): Generator<string> {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = join(dir, entry.name);
    if (entry.isDirectory()) {
      yield* walk(full);
    } else if (
      entry.isFile() &&
      (entry.name.endsWith(".svelte") || entry.name.endsWith(".ts"))
    ) {
      yield full;
    }
  }
}

describe("production app shell isolation", () => {
  it("does not import from lib/dev", () => {
    const offenders: string[] = [];
    for (const file of walk(APP_DIR)) {
      const text = readFileSync(file, "utf8");
      if (/from\s+["'][^"']*\/dev\//.test(text)) {
        offenders.push(file);
      }
    }
    expect(offenders).toEqual([]);
  });

  it("does not import an experimental renderer adapter package", () => {
    // Experimental renderer packages stay dev-lab-only. xterm is the
    // production baseline (per AGENTS.md "Adding a new terminal
    // renderer adapter") so `@relayterm/terminal-xterm` is allowed in
    // the production shell; ghostty-web, restty, and wterm are NOT.
    // `@relayterm/terminal-core` is renderer-neutral by construction
    // and is also fine.
    const experimentalImport =
      /@relayterm\/terminal-(ghostty-web|restty|wterm)/;
    const offenders: string[] = [];
    for (const file of walk(APP_DIR)) {
      const text = readFileSync(file, "utf8");
      if (experimentalImport.test(text)) {
        offenders.push(file);
      }
    }
    expect(offenders).toEqual([]);
  });

  it("wraps <TerminalView> in a key block on activeLaunch.sessionId", () => {
    // TerminalView's `let saved = $state(loadActiveSession())` is captured
    // at first mount and never re-read while the component stays alive.
    // After AppShell's `handleSessionClosed` runs `clearActiveSession()`
    // and sets `activeLaunch = null`, TerminalView's `{:else}` empty
    // state still renders the stale saved pointer if the component was
    // not unmounted/remounted across the launch transition. The wrapper
    //
    //   {#key activeLaunch?.sessionId ?? "empty"}
    //     <TerminalView ... />
    //   {/key}
    //
    // forces a fresh mount on every launch transition (non-null → null
    // on wire-close, null → some-id on launch, id → different-id on
    // reconnect-from-Sessions), so `saved` always reflects current
    // localStorage. A regression that drops this wrapper would re-open
    // the "End session → Reconnect → connection error" UX bug surfaced
    // in the 2026-05-09 staging smoke; pin the wrapper here so the
    // regression trips this test instead.
    const appShellPath = new URL(
      "../src/lib/app/AppShell.svelte",
      import.meta.url,
    ).pathname;
    const text = readFileSync(appShellPath, "utf8");
    // Two assertions so the test is tolerant of comment / whitespace
    // reformatting between the `{#key}` line and the `<TerminalView>`
    // tag (e.g. an inline comment being moved into the body of the
    // key block) without losing its grip on the structural property
    // being pinned.
    const keyOpen =
      /\{#key\s+activeLaunch\?\.sessionId\s*\?\?\s*"empty"\s*\}/;
    expect(keyOpen.test(text)).toBe(true);
    // The key block immediately containing <TerminalView ... />: scan
    // up to ~500 chars after the matched opener (covers a reasonable
    // amount of inline comment / whitespace) for the tag.
    const keyBody = new RegExp(
      String.raw`\{#key\s+activeLaunch\?\.sessionId\s*\?\?\s*"empty"\s*\}[\s\S]{0,500}<TerminalView\b`,
    );
    expect(keyBody.test(text)).toBe(true);
  });
});
