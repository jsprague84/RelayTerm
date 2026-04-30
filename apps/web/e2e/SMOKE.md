# Dev renderer + production shell smoke — manual Playwright MCP procedure

This document captures the browser-level smoke verification for the
**dev-only renderer lab** AND the **production app shell**. It is
intentionally a manual procedure driven by the Playwright MCP server,
not a committed `@playwright/test` suite — the operator (human or agent)
drives a real Chromium against the Vite dev/preview servers and asserts
a small set of stable selectors.

## Why no committed Playwright runner

- Playwright lives globally as an MCP server; pulling
  `@playwright/test` into `apps/web` as a devDep would add browsers,
  config, and a CI surface that isn't paying for itself yet.
- The dev lab is intentionally dev-only — the production terminal UI is
  out of scope, so a heavyweight e2e harness would mostly verify that
  the lab is gated correctly, which a 30-second manual run already does.
- Stable `data-testid` hooks live on the dev lab so this procedure (and
  any future committed runner) targets the same selectors.

## Stable selectors

The dev lab and the production shell expose these `data-testid` hooks.
Treat them as the contract this smoke depends on; if you rename one,
update this file in the same change.

| Selector                                          | Surface                                                       |
|---------------------------------------------------|---------------------------------------------------------------|
| `[data-testid="app-shell-main"]`                  | Production shell main pane (visible in dev AND prod).         |
| `[data-testid="top-bar-title"]`                   | Shell top bar title (mirrors selected nav item).              |
| `[data-testid="nav-dashboard"]`                   | Sidebar nav button — Dashboard (default-selected).            |
| `[data-testid="nav-terminal"]`                    | Sidebar nav button — Terminal workspace placeholder.          |
| `[data-testid="nav-sessions"]`                    | Sidebar nav button — Sessions placeholder.                    |
| `[data-testid="nav-servers"]`                     | Sidebar nav button — Server profiles placeholder.             |
| `[data-testid="nav-identities"]`                  | Sidebar nav button — SSH identities placeholder.              |
| `[data-testid="nav-settings"]`                    | Sidebar nav button — Settings placeholder.                    |
| `[data-testid="production-view-dashboard"]`       | Dashboard view (selected by default).                         |
| `[data-testid="production-view-servers"]`         | Servers view (read-only inventory of hosts + profiles).       |
| `[data-testid="production-view-identities"]`      | Identities view (read-only public-key list).                  |
| `[data-testid="dashboard-inventory-counts"]`      | Dashboard inventory counts card (hosts/profiles/identities).  |
| `[data-testid="dev-mode-badge"]`                  | "dev build" badge in top bar (only visible under `vite dev`). |
| `[data-testid="nav-devtools-toggle"]`             | Sidebar dev-tools toggle (only visible under `vite dev`).     |
| `[data-testid="dev-tools-panel"]`                 | Dev tools panel rendered when toggle is open (dev only).      |
| `[data-testid="dev-terminal-workbench"]`          | Dev workbench root (only visible under `vite dev`).           |
| `[data-testid="xterm-live-terminal-lab"]`         | Live terminal lab root (renderer host + diagnostics).         |
| `[data-testid="renderer-selector"]`               | Radio group containing the four renderer options.             |
| `[data-testid="renderer-option-xterm"]`           | xterm baseline radio (default-checked).                       |
| `[data-testid="renderer-option-ghostty-web"]`     | ghostty-web experimental radio.                               |
| `[data-testid="renderer-option-restty"]`          | restty experimental radio.                                    |
| `[data-testid="renderer-option-wterm"]`           | wterm experimental radio.                                     |
| `[data-testid="renderer-diagnostics"]`            | Diagnostics panel (counters + selected renderer).             |
| `[data-testid="lab-event-log"]`                   | Event log container (info/in/out/error rows).                 |

Renderer-switching contract: clicking a renderer radio while idle (no
session attached) records the choice and pushes a single info line to
the event log:

```
[info] renderer set to <label> (idle)
```

The diagnostics panel's first `dd` cell mirrors the operator's choice
(`xterm baseline`, `ghostty-web experimental`, `restty experimental`,
`wterm experimental`).

## Procedure

The procedure has two halves: a **dev** smoke (Vite dev server) and a
**production** smoke (Vite preview of the built bundle). Each half uses
the same MCP browser tools.

### A. Dev smoke

1. Start the Vite dev server from the repo root:

   ```sh
   pnpm --filter @relayterm/web dev
   ```

   Wait for `Local: http://localhost:5173/`.

2. Drive Playwright MCP:

   ```text
   browser_navigate http://localhost:5173/
   ```

3. Assert the production shell renders AND the dev surfaces are
   reachable via the dev-tools toggle. Use `browser_evaluate` with this
   snippet:

   ```js
   () => {
     const has = (sel) => !!document.querySelector(sel);
     return {
       shell: has('[data-testid="app-shell-main"]'),
       dashboard: has('[data-testid="production-view-dashboard"]'),
       devModeBadge: has('[data-testid="dev-mode-badge"]'),
       devToolsToggle: has('[data-testid="nav-devtools-toggle"]'),
       devToolsPanel: has('[data-testid="dev-tools-panel"]'),
       navItems: [
         "dashboard",
         "terminal",
         "sessions",
         "servers",
         "identities",
         "settings",
       ].every((id) => has(`[data-testid="nav-${id}"]`)),
     };
   }
   ```

   Expected: `shell`, `dashboard`, `devModeBadge`, `devToolsToggle`,
   `navItems` all `true`. `devToolsPanel` is `false` (the panel only
   renders after the toggle is clicked).

4. Open the dev-tools panel and assert the renderer lab is reachable:

   - `browser_click [data-testid="nav-devtools-toggle"]`
   - Re-run the snippet from step 3 and confirm `devToolsPanel: true`.
   - Run a follow-up snippet to confirm the lab surfaces:

     ```js
     () => {
       const has = (sel) => !!document.querySelector(sel);
       return {
         workbench: has('[data-testid="dev-terminal-workbench"]'),
         lab: has('[data-testid="xterm-live-terminal-lab"]'),
         selector: has('[data-testid="renderer-selector"]'),
         diagnostics: has('[data-testid="renderer-diagnostics"]'),
         options: ["xterm", "ghostty-web", "restty", "wterm"].map((id) => ({
           id,
           present: has(`[data-testid="renderer-option-${id}"]`),
           checked:
             document.querySelector(`[data-testid="renderer-option-${id}"]`)
               ?.checked ?? null,
         })),
       };
     }
     ```

   Expected: `workbench`, `lab`, `selector`, `diagnostics` all `true`;
   every renderer option is `present: true`; `xterm` is the only one
   with `checked: true`.

5. For each of `ghostty-web`, `restty`, `wterm`, `xterm` (in that
   order):

   - `browser_click [data-testid="renderer-option-<id>"]`
   - `browser_evaluate` and confirm the diagnostics panel cell shows
     the matching label (`<id> experimental` or `xterm baseline`) and
     the event log's last line matches
     `[info] renderer set to <label> (idle)`.

   The last click is **deliberately** `xterm` so the lab is left on
   the default option at procedure end. If a future renderer is
   appended to this list (per AGENTS.md task pattern step 9), keep
   `xterm` as the final click — confirm `renderer-option-xterm` is
   checked before closing the browser.

6. `browser_console_messages level=error all=true`. The only allowed
   error is the favicon `404` (`GET /favicon.ico 404`) — anything else
   fails the smoke.

### B. Production smoke

1. Stop the dev server. Build and preview:

   ```sh
   pnpm --filter @relayterm/web build
   pnpm --filter @relayterm/web preview --port 4173
   ```

   Wait for `Local: http://localhost:4173/`.

2. Drive Playwright MCP:

   ```text
   browser_navigate http://localhost:4173/
   ```

3. Assert the production shell renders AND every dev-only surface is
   absent (no dev-tools toggle, no dev-mode badge, no renderer lab):

   ```js
   () => {
     const has = (sel) => !!document.querySelector(sel);
     return {
       shell: has('[data-testid="app-shell-main"]'),
       dashboard: has('[data-testid="production-view-dashboard"]'),
       devModeBadge: has('[data-testid="dev-mode-badge"]'),
       devToolsToggle: has('[data-testid="nav-devtools-toggle"]'),
       devToolsPanel: has('[data-testid="dev-tools-panel"]'),
       workbench: has('[data-testid="dev-terminal-workbench"]'),
       lab: has('[data-testid="xterm-live-terminal-lab"]'),
       selector: has('[data-testid="renderer-selector"]'),
       diagnostics: has('[data-testid="renderer-diagnostics"]'),
       rendererOptionsAbsent: [
         "xterm",
         "ghostty-web",
         "restty",
         "wterm",
       ].every((id) => !has(`[data-testid="renderer-option-${id}"]`)),
       navItems: [
         "dashboard",
         "terminal",
         "sessions",
         "servers",
         "identities",
         "settings",
       ].every((id) => has(`[data-testid="nav-${id}"]`)),
     };
   }
   ```

   Expected: `shell`, `dashboard`, `navItems` all `true`. `devModeBadge`,
   `devToolsToggle`, `devToolsPanel`, `workbench`, `lab`, `selector`,
   `diagnostics` all `false`. `rendererOptionsAbsent` is `true`.

4. `browser_console_messages level=error all=true`. As above, the
   favicon `404` is the only allowed error.

## What this smoke does NOT cover

- A real SSH end-to-end browser test (no PTY bytes flow; no backend is
  required).
- Renderer-specific WASM/WebGPU/DOM behavior (`mount()` is never
  exercised because no session is attached).
- Mount/dispose timing or any benchmark claim — the diagnostics panel
  exposes counters, but they remain at zero through this smoke.
- Mobile / Tauri shell.
- Visual regression.
- Persistent renderer preference.

These are intentionally out of scope. If you need any of them, write a
new procedure rather than overloading this one.
