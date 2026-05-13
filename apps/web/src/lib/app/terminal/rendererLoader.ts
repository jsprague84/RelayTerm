/**
 * Production-shell renderer loader.
 *
 * The single seam at which the production terminal workspace picks a
 * renderer for an attach. xterm is the compatibility baseline and is
 * **always** statically imported; the experimental adapters
 * (`@relayterm/terminal-ghostty-web`, `@relayterm/terminal-restty`,
 * `@relayterm/terminal-wterm`) are loaded ONLY via dynamic `import()`
 * AND ONLY when the operator has flipped the experimental-renderer-
 * evaluation gate. Any "selected but not allowed / unknown / load
 * failed" path collapses to xterm with a typed {@link RendererLoadFallback}
 * so the workspace can render the fallback in honest copy.
 *
 * Architectural rules this file is the load-bearing enforcer of:
 *  - The production app shell MUST NOT statically import any
 *    experimental renderer adapter package. The
 *    `apps/web/tests/appShellIsolation.test.ts` rule allows the
 *    experimental package names to appear only inside this file AND
 *    only inside dynamic `import()` expressions.
 *  - The bundle for the default-renderer path stays free of the
 *    experimental adapters' WASM payloads — Vite/Rollup chunk-splits
 *    each dynamic import into its own asset, which the operator never
 *    fetches unless the gate is on AND the matching adapter is picked.
 *  - The loader NEVER takes / surfaces / logs payload bytes (input,
 *    output, paste content, identities, session tokens). Its inputs
 *    are renderer-neutral cosmetic options + a cell grid; its outputs
 *    are a `TerminalRenderer` instance and metadata.
 *
 * The default importer table calls real dynamic `import()`s. Tests
 * inject their own importers via {@link loadRendererWithImporters} so
 * the heavy WASM payloads never run in the Vitest jsdom environment.
 */
import type {
  BaseTerminalRendererOptions,
  TerminalRenderer,
} from "@relayterm/terminal-core";
import { XtermRenderer } from "@relayterm/terminal-xterm";

import {
  DEFAULT_RENDERER_ID,
  isExperimentalRenderer,
  isRendererId,
  type RendererId,
} from "../settings/terminalSettings.js";

/**
 * Diagnostic that survives a fallback. Values are stable strings so the
 * SMOKE runbook can read them off `data-renderer-fallback` without
 * having to ad-hoc-parse copy. None of the values include any payload
 * byte; they are operator-facing taxonomy only.
 *
 * Taxonomy split by failure stage:
 *  - `experimental_gate_off` / `unknown_renderer_id` / `adapter_load_failed`
 *    fire SYNCHRONOUSLY inside this loader's gate + dynamic-import +
 *    constructor paths. The loader emits these as `RendererLoadResult.fallback`
 *    and the caller never sees a thrown error.
 *  - `adapter_mount_failed` fires ASYNCHRONOUSLY at the workspace's
 *    `renderer.mount(target)` call site. The loader cannot emit it
 *    because it does not own the mount target; instead the production
 *    workspace catches the rejection (see
 *    `terminalLaunch.ts::mountRendererSafely`) and writes this value
 *    onto its own `data-renderer-fallback` attribute. Keeping the value
 *    in the shared taxonomy means the SMOKE runbook reads ONE closed
 *    vocabulary across both stages.
 */
export type RendererLoadFallback =
  | "experimental_gate_off"
  | "unknown_renderer_id"
  | "adapter_load_failed"
  | "adapter_mount_failed";

export interface RendererLoadResult {
  renderer: TerminalRenderer;
  /** The id the loader actually mounted. xterm on any fallback path. */
  rendererId: RendererId;
  /** The id the caller asked for. Mirrors the persisted setting. */
  requestedRendererId: RendererId;
  /**
   * Present only when the loader had to fall back. Undefined on the
   * happy path (`rendererId === requestedRendererId`).
   */
  fallback?: RendererLoadFallback;
}

export interface LoadRendererInput {
  id: RendererId;
  experimentalEnabled: boolean;
  options: BaseTerminalRendererOptions;
  /** Initial cell grid; needed by restty + wterm constructors. */
  cols: number;
  rows: number;
}

/**
 * Dynamic-import surface, factored out so the unit tests can swap in a
 * stub-importer table without ever invoking the real
 * `@relayterm/terminal-{ghostty-web,restty,wterm}` modules (which would
 * pull in their WASM payloads). The production caller uses
 * {@link loadRenderer}, which fixes the importer table to the real
 * dynamic imports below.
 */
export interface RendererImporters {
  /**
   * ghostty-web adapter dynamic importer. Resolves to the module
   * namespace; the loader picks `GhosttyWebRenderer` off it.
   */
  ghosttyWeb: () => Promise<{
    GhosttyWebRenderer: new (
      options: BaseTerminalRendererOptions,
    ) => TerminalRenderer;
  }>;
  restty: () => Promise<{
    ResttyRenderer: new (
      options: BaseTerminalRendererOptions & { cols: number; rows: number },
    ) => TerminalRenderer;
  }>;
  wterm: () => Promise<{
    WtermRenderer: new (
      options: BaseTerminalRendererOptions & { cols: number; rows: number },
    ) => TerminalRenderer;
  }>;
}

/**
 * The real-world dynamic-import table. The string-literal arguments to
 * `import(...)` are the **only** references to the experimental adapter
 * package names allowed inside `apps/web/src/lib/app/**`; the isolation
 * test pins this.
 */
export const DEFAULT_RENDERER_IMPORTERS: RendererImporters = {
  ghosttyWeb: () => import("@relayterm/terminal-ghostty-web"),
  restty: () => import("@relayterm/terminal-restty"),
  wterm: () => import("@relayterm/terminal-wterm"),
};

function makeXterm(input: LoadRendererInput): TerminalRenderer {
  return new XtermRenderer(input.options);
}

/**
 * Pure form of {@link loadRenderer} that takes its importer table as a
 * parameter. Used by tests; the production caller uses
 * {@link loadRenderer} which fixes the table to
 * {@link DEFAULT_RENDERER_IMPORTERS}.
 */
export async function loadRendererWithImporters(
  input: LoadRendererInput,
  importers: RendererImporters,
): Promise<RendererLoadResult> {
  const requested = input.id;

  if (!isRendererId(requested)) {
    return {
      renderer: makeXterm(input),
      rendererId: DEFAULT_RENDERER_ID,
      requestedRendererId: DEFAULT_RENDERER_ID,
      fallback: "unknown_renderer_id",
    };
  }

  if (requested === DEFAULT_RENDERER_ID) {
    return {
      renderer: makeXterm(input),
      rendererId: DEFAULT_RENDERER_ID,
      requestedRendererId: DEFAULT_RENDERER_ID,
    };
  }

  if (!input.experimentalEnabled) {
    return {
      renderer: makeXterm(input),
      rendererId: DEFAULT_RENDERER_ID,
      requestedRendererId: requested,
      fallback: "experimental_gate_off",
    };
  }

  // Experimental + gate on: dynamic import.
  try {
    switch (requested) {
      case "ghostty-web": {
        const mod = await importers.ghosttyWeb();
        return {
          renderer: new mod.GhosttyWebRenderer(input.options),
          rendererId: "ghostty-web",
          requestedRendererId: "ghostty-web",
        };
      }
      case "restty": {
        const mod = await importers.restty();
        return {
          renderer: new mod.ResttyRenderer({
            ...input.options,
            cols: input.cols,
            rows: input.rows,
          }),
          rendererId: "restty",
          requestedRendererId: "restty",
        };
      }
      case "wterm": {
        const mod = await importers.wterm();
        return {
          renderer: new mod.WtermRenderer({
            ...input.options,
            cols: input.cols,
            rows: input.rows,
          }),
          rendererId: "wterm",
          requestedRendererId: "wterm",
        };
      }
      case "xterm":
        // Unreachable: handled by the early `requested === DEFAULT_RENDERER_ID`
        // branch above. The case is here so TypeScript proves the switch is
        // exhaustive against {@link RendererId}.
        return {
          renderer: makeXterm(input),
          rendererId: DEFAULT_RENDERER_ID,
          requestedRendererId: DEFAULT_RENDERER_ID,
        };
    }
  } catch {
    // Adapter import / construction failed — the most likely cause is a
    // WASM init error inside the dynamically-loaded module. We swallow
    // the underlying message deliberately: it can include the import
    // URL or stack which is operator-noise; the static fallback string
    // is the safe signal. xterm is the safe fallback.
    if (isExperimentalRenderer(requested)) {
      return {
        renderer: makeXterm(input),
        rendererId: DEFAULT_RENDERER_ID,
        requestedRendererId: requested,
        fallback: "adapter_load_failed",
      };
    }
    // Defensive — should be unreachable because xterm construction is
    // synchronous and the early branch above handled that path.
    throw new Error("renderer load failed");
  }
}

/**
 * Production entry point. Returns the renderer the operator gets when a
 * new terminal workspace mounts. xterm always wins on any fallback or
 * not-opted-in path.
 */
export function loadRenderer(
  input: LoadRendererInput,
): Promise<RendererLoadResult> {
  return loadRendererWithImporters(input, DEFAULT_RENDERER_IMPORTERS);
}
