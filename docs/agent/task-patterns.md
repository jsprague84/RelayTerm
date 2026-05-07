# Task patterns — long form

> The **index** of recurring task patterns lives in `AGENTS.md` →
> "Task patterns". When the index says **(see this file)**, the full
> step-by-step procedure is here.

---

## 1. Adding a new terminal renderer adapter

Mirrors the shape established by `@relayterm/terminal-xterm`
(baseline), `@relayterm/terminal-ghostty-web`, `@relayterm/terminal-restty`,
and `@relayterm/terminal-wterm`. The architectural rule:
`terminal-core` stays renderer-agnostic, and the backend protocol
stays renderer-neutral — never reshape either to accommodate a
renderer.

### Steps

1. Scaffold `packages/terminal-<name>/` (package name
   `@relayterm/terminal-<name>`); implement `TerminalRenderer` from
   `@relayterm/terminal-core`.
2. Keep exports minimal and renderer-neutral. Extend
   `BaseTerminalRendererOptions` from `@relayterm/terminal-core`
   (which carries the shared `fontFamily` / `fontSize` / `lineHeight` /
   `cursorStyle` / `cursorBlink` / `scrollbackLines` / `theme` shape,
   `RendererTheme`, `RendererThemeAnsi`, and `RendererCursorStyle`) —
   DO NOT redefine these neutral types in the adapter. Renderer-specific
   knobs go behind a local `<renderer>Only` escape hatch on the options
   object — never on the `TerminalRenderer` surface, never on
   `BaseTerminalRendererOptions`.
3. Do NOT add the renderer's runtime as a dep of `terminal-core`. Only
   the adapter package depends on the underlying lib.
4. Add adapter unit tests (vitest). Mock the underlying terminal when
   WASM/WebGPU/DOM/jsdom is awkward — see `terminal-ghostty-web`,
   `terminal-restty`, and `terminal-wterm` tests for the mock pattern.
5. Add redaction tests covering input, output, log, and error paths.
   Raw terminal bytes/strings must never appear in console, logs, or
   thrown error messages.
6. Wire the package into `apps/web` ONLY for the dev lab: register an
   id/label in `apps/web/src/lib/dev/rendererDiagnostics.ts` and add
   creation/switching to `apps/web/src/lib/dev/XtermLiveTerminalLab.svelte`.
   Do not promote experimental renderers into the main app surface.
7. Update the Stack table in `AGENTS.md` with the package, version
   pin, and any API caveats (UTF-8 decode requirements, async init,
   asset/WASM wiring, bundle size, tree-shaking flags). Update the
   relevant `docs/spec/terminal.md` renderer-adapter section with
   adapter limitations and tree-shaking notes.
8. Verify the production bundle: confirm the new package is
   tree-shaken out of any non-dev build (`sideEffects: false` on the
   adapter, no top-level imports from app code).
9. Add a `data-testid="renderer-option-<id>"` attribute to the new
   radio in `XtermLiveTerminalLab.svelte` and extend the smoke
   selectors in `apps/web/e2e/SMOKE.md`. Re-run the manual Playwright
   MCP smoke (dev + production halves) so the new option is in the
   verified set. The smoke is intentionally manual; if it ever needs
   to be a committed runner, that is its own slice.

### Recurring rules for renderer work

- `xterm` is the compatibility baseline and the default. Don't change
  the default without an explicit ask.
- Experimental renderers must be labeled experimental in UI,
  diagnostics, and docs.
- Renderer diagnostics in `rendererDiagnostics.ts` are metadata only —
  not formal benchmarks. Don't present them as perf claims.
- Renderer-specific APIs must not leak into `TerminalRenderer` or
  `terminal-core`.
- The backend protocol does not change to accommodate a renderer. If a
  renderer needs new data, it transforms what's already on the wire.
- Raw terminal input/output (keystrokes, PTY bytes, decoded strings)
  must never be logged or surfaced outside the terminal viewport.

---

## 2. Adding a production app-shell view

The production shell lives under `apps/web/src/lib/app/`. Production
components MUST NOT import from `lib/dev/` or any experimental
renderer adapter package
(`@relayterm/terminal-{ghostty-web,restty,wterm}`); the production
terminal workspace uses `@relayterm/terminal-core` +
`@relayterm/terminal-xterm` (the baseline) only, and the experimental
adapters stay dev-lab-only.

### Steps

1. Extend `AppViewId` and `NAV_ITEMS` in `lib/app/navigation.ts` (id,
   label, description).
2. Add a `*View.svelte` under `lib/app/views/` — placeholders should
   compose `PlaceholderView.svelte` with honest copy ("not implemented
   yet", a short bullet list of what currently exists, and a
   `futureWork` note).
3. Wire the new id into the `{#if}` chain in `AppShell.svelte`.
4. Extend the navigation tests in `tests/navigation.test.ts`.

Do NOT show fake data, mock secret values, or any `private_key` /
`encrypted_private_key` field. Update `apps/web/e2e/SMOKE.md` if a new
stable selector should be in the verified set, and update the relevant
section in `docs/spec/web-shell.md` (and the SPEC.md index summary if
the contract changes).

---

## 3. Fetching backend data from a production view

Use the typed helpers in `apps/web/src/lib/api/` and the shared error
envelope from `apiErrors.ts`.

### Steps

1. Add a `parseX(raw: unknown): X | null` runtime guard in the resource
   module — construct the DTO field-by-field so unknown extra fields
   are dropped silently and a stray `private_key` /
   `encrypted_private_key` cannot smuggle onto the parsed object.
2. Call `fetchJsonList(endpoint, parseX)` so transport, HTTP, and
   parse failures collapse to a single typed `LoadError`.
3. Format UI strings via `describeLoadError(label, err)` — NEVER echo
   the wire `message` of an HTTP error or the thrown `Error.message`
   of a transport failure in any string that reaches the DOM.
4. Render explicit loading / empty / error / ready states (no
   auto-retry storms, no polling unless explicitly scoped).
5. For SSH-identity-shaped data, do NOT declare
   `encrypted_private_key` / `private_key` on the TypeScript interface
   AND add sentinel-string redaction tests asserting absence in the
   parsed object, in `JSON.stringify` of the parsed object, and in any
   formatted preview / copy string.

---

## 4. Adding a new backend WebSocket message type

Define in the protocol module owned by `relayterm-protocol` (and
mirrored on the web side under `lib/ws/`). Wire-stable variants
append, never renumber. JSON shapes for the control plane; binary
`RTB1` envelope only for the hot terminal data path. Tests in
`crates/relayterm-api/tests/api.rs` are the executable contract.

(This pattern is short by design — the load-bearing details live in
`docs/spec/terminal.md` → "Terminal WebSocket attach/detach contract"
and "Terminal data plane: binary envelope".)
