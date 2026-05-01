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
});
