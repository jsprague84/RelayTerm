# SPEC area docs

> Per-surface detail split out of `SPEC.md` for context efficiency.
> `SPEC.md` is the index and holds the load-bearing invariants, data
> model, behavior contracts, inventory lifecycle policy, integration
> points, out-of-scope list, and open questions. This directory holds
> the long-form contract for each surface area.

| Area | What's in it |
|---|---|
| [`terminal.md`](terminal.md) | Renderer-independent terminal/session/workspace behavior: terminal-session lifecycle, WebSocket attach/detach, wire protocol + binary envelope, frontend `terminal-core` client, live SSH PTY bridge, output sequence + replay buffer, production terminal launch UI, sessions list/status, settings/viewport/paste safety, active-terminal local recovery, status refresh. Renderer adapter packages are summarized here and detailed in `terminal-adapters.md`. |
| [`terminal-adapters.md`](terminal-adapters.md) | Concrete renderer adapter contracts for the four packages under `packages/terminal-<name>/`: `terminal-xterm` (production baseline), `terminal-ghostty-web` / `terminal-restty` / `terminal-wterm` (experimental, dev-only). Per adapter: package layout, adapter contract, renderer-neutrality re-affirmation, dev-lab diagnostic UI, production-bundle tree-shaking behavior, and explicit future-work scope. |
| [`auth.md`](auth.md) | Credential creation, host-key preflight + trust, authenticated SSH credential check, production authentication architecture (mode model, sessions, CSRF, login throttle, audit kinds, frontend auth UI plan, security tests, implementation order). Operator side lives in [`../production-auth.md`](../production-auth.md) and [`../auth-smoke.md`](../auth-smoke.md). |
| [`inventory.md`](inventory.md) | Production inventory views (hosts, identities, server-profiles), detail panels, client-side search/filters, identity generation UI, host & profile creation UI, host-key preflight/trust UI, auth-check UI, dashboard summary + recent activity feed, server-profile disable/enable backend + audit + UI. The lifecycle policy itself stays in `SPEC.md`. |
| [`recording.md`](recording.md) | Load-bearing invariants for durable recording. The full design lives in [`../terminal-recording.md`](../terminal-recording.md). |
| [`web-shell.md`](web-shell.md) | Shell chrome (sidebar, topbar, navigation), URL-driven view routing. |

## How to update these

1. Land the implementation slice.
2. Update the relevant area doc here.
3. Update the corresponding 1–3 sentence summary in `SPEC.md`.
4. If the change is a contract change (not just an implementation
   detail), update `docs/agent/context-split-map.md` so future agents
   can trace why and where.
