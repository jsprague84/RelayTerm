import { describe, expect, it } from "vitest";

/**
 * Static-source pin for the ghostty-web adapter's CSP posture.
 *
 * The adapter is the seam between RelayTerm and upstream `ghostty-web`,
 * which ships its WASM payload BOTH as an inlined
 * `data:application/wasm;base64,…` URL inside `dist/ghostty-web.js`
 * (used by the no-arg `init()` sugar) AND as a sibling asset at the
 * subpath `ghostty-web/ghostty-vt.wasm`. The adapter is required to
 * load via the asset-URL path so RelayTerm's production CSP
 * (`default-src 'self'`, no `connect-src` override) does not block a
 * `data:` fetch at runtime.
 *
 * These tests are STRUCTURAL — they inspect the adapter's *source*
 * rather than running it — so they catch a regression even if the
 * unit-test mocks pass. Any future contributor who reverts to the
 * upstream `init()` path will trip them.
 *
 * The tests deliberately do NOT inspect `node_modules/ghostty-web` —
 * the upstream bundle is allowed to contain the inline data URL; what
 * matters is that the adapter never reaches that path.
 *
 * The sources are imported via Vite's `?raw` suffix (resolved by
 * vitest's Vite layer) so the package can stay free of an
 * `@types/node` devDep.
 */
import indexSrc from "../src/index.ts?raw";
import optionsSrc from "../src/options.ts?raw";
import rendererSrc from "../src/GhosttyWebRenderer.ts?raw";
import wasmUrlSrc from "../src/wasmUrl.ts?raw";

const ADAPTER_SOURCES: Array<{ name: string; text: string }> = [
  { name: "src/index.ts", text: indexSrc },
  { name: "src/options.ts", text: optionsSrc },
  { name: "src/GhosttyWebRenderer.ts", text: rendererSrc },
  { name: "src/wasmUrl.ts", text: wasmUrlSrc },
];

describe("ghostty-web adapter CSP / WASM-asset source posture", () => {
  it("does not embed a `data:application/wasm` literal anywhere in src", () => {
    for (const { name, text } of ADAPTER_SOURCES) {
      // Strip line + block comments so doc strings discussing the
      // upstream data-URL footgun are allowed to mention it. We only
      // care about the executable surface.
      const codeOnly = stripCommentLines(text);
      expect(
        codeOnly,
        `${name} must not embed an executable data:application/wasm reference`,
      ).not.toMatch(/data:application\/wasm/);
    }
  });

  it("imports the upstream WASM as a Vite-resolved `?url` asset", () => {
    expect(wasmUrlSrc).toMatch(
      /from\s+["']ghostty-web\/ghostty-vt\.wasm\?url["']/,
    );
  });

  it("does not call the upstream no-arg `init` sugar", () => {
    for (const { name, text } of ADAPTER_SOURCES) {
      const codeOnly = stripCommentLines(text);
      // The upstream module's only direct route to the inlined data URL
      // is the no-arg `init()` export. Pin that the adapter neither
      // imports nor calls it.
      expect(
        codeOnly,
        `${name} must not import the upstream \`init\` export`,
      ).not.toMatch(
        /import\s*\{[^}]*\binit\b[^}]*\}\s*from\s*["']ghostty-web["']/,
      );
    }
  });
});

/**
 * Strip `//`-style and `/* … *​/` comment regions before searching for
 * forbidden tokens. Doc strings inside the adapter explicitly discuss
 * the inlined data URL as background; the pin is on the *executable*
 * surface only.
 */
function stripCommentLines(text: string): string {
  return text
    .replace(/\/\*[\s\S]*?\*\//g, "")
    .split("\n")
    .map((line) => line.replace(/\/\/.*$/, ""))
    .join("\n");
}
