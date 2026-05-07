# SPEC — Production web app shell

> Detailed contracts split out of `SPEC.md` for context efficiency. The
> top-level `SPEC.md` is the index; this file is the long form.
>
> Inventory views, dashboard, and per-feature UI live in
> [`docs/spec/inventory.md`](inventory.md). Terminal-specific UI lives
> in [`docs/spec/terminal.md`](terminal.md). This file covers the shell
> chrome (sidebar, topbar, navigation, view routing) only.

## Contents

- [Production web app shell](#production-web-app-shell)
- [URL-driven production view routing](#url-driven-production-view-routing)

---

### Production web app shell

The production-facing web app has a real shell now. The shell is layout, navigation, and dev/prod gating only — it is not the production terminal workspace, not real CRUD UI, and not real auth UI. Each of those is a deliberate later slice.

**Scope (load-bearing — this slice).**

1. The shell renders in production (`vite build` / preview) AND in development (`vite dev`).
2. Navigation is a small local view-state model — no router. The discriminator (`AppViewId`) is `dashboard | terminal | sessions | servers | identities | settings`.
3. Each non-dashboard view is a placeholder. Placeholder copy is honest: "not implemented yet", "future work", and a short bullet list of what currently exists on the backend. **Placeholders MUST NOT show fake data, mock secret values, or a `private_key` / `encrypted_private_key` field.** The SSH-identities placeholder explicitly does not surface secrets.
4. Dev-lab tools (`TerminalProtocolLab`, `DevTerminalWorkbench`, the per-renderer lab and renderer diagnostics) stay dev-only. They are reachable only via the "Developer tools" section of the shell, which is gated by `import.meta.env.DEV` AND a `devTools` snippet passed from `App.svelte`. Vite's dead-code elimination drops the dev branch — and the dev-lab imports it pulls in — from the production bundle.
5. The dashboard exposes a one-shot backend health probe (`GET /healthz`) via `lib/api/health.ts`. The probe does NOT poll, does NOT retry, and does NOT surface transport-error detail. Failure collapses to `down`; the underlying error is dropped on the floor (liveness probe, not diagnostic).

**Architecture rule.** Production shell components (`lib/app/`) MUST NOT import anything from `lib/dev/`. The renderer-adapter rule was relaxed once the production terminal workspace landed: `@relayterm/terminal-core` (renderer-neutral) and `@relayterm/terminal-xterm` (the production baseline + its CSS side-effect entry) are allowed in the production shell; the experimental adapters `@relayterm/terminal-{ghostty-web,restty,wterm}` remain banned. This is enforced by `appShellIsolation.test.ts`.

**Package layout.**

```
apps/web/src/lib/app/
├─ AppShell.svelte         # composes sidebar + topbar + view + (dev) tools
├─ SidebarNav.svelte
├─ TopBar.svelte
├─ StatusBadge.svelte
├─ navigation.ts           # NAV_ITEMS, AppViewId, DEFAULT_VIEW, findNavItem
├─ routing.ts              # URL <-> AppViewId helpers (viewForPath, pathForView, ...)
└─ views/
   ├─ DashboardView.svelte    # backend health probe
   ├─ TerminalView.svelte     # placeholder
   ├─ SessionsView.svelte     # placeholder
   ├─ ServersView.svelte      # placeholder
   ├─ IdentitiesView.svelte   # placeholder, no secrets
   ├─ SettingsView.svelte     # placeholder
   └─ PlaceholderView.svelte  # shared layout for non-functional views
apps/web/src/lib/api/
├─ apiErrors.ts                # shared LoadError, fetchJsonList, readErrorEnvelope, describeLoadError
├─ health.ts                   # checkHealth() helper
├─ hosts.ts                    # listHosts() + parseHost()
├─ serverProfiles.ts           # listServerProfiles() + parseServerProfile() + resolveProfileLinks()
└─ sshIdentities.ts            # listSshIdentities() + parseSshIdentity() + publicKeyPreview() + createSshIdentity()
```

**Future work (explicit out-of-scope for this slice).**

Production terminal workspace; production renderer selector; renderer-preference persistence; server / profile / identity CRUD UI; real auth UI (passkey enrollment, session list); mobile/Tauri shell integration; password bootstrap; private-key import; durable session-recording UI. URL routing for the top-level production views is now wired — see "URL-driven production view routing" below for the foundation slice; route parameters, deep-linking, auth routes, and nested routes remain future work. Each is a separate slice.
### URL-driven production view routing

Replaces the purely local view-state model with stable URLs for every production view. No routing library — the shell mirrors `selected` to `window.location.pathname` via `history.pushState` and listens for `popstate`. Foundation slice; route params, nested routes, deep-link launch, and auth routes remain future work.

**Scope (load-bearing — this slice).**

1. **Stable path per production view.** The production-shell route table:

   | Path | View |
   |---|---|
   | `/` | Dashboard (canonical landing alias) |
   | `/dashboard` | Dashboard |
   | `/terminal` | Terminal workspace |
   | `/sessions` | Terminal Sessions |
   | `/servers` | Server profiles |
   | `/identities` | SSH identities |
   | `/settings` | Settings |

2. **Pure helper module** — `apps/web/src/lib/app/routing.ts` exports `viewForPath`, `pathForView`, `normalizeAppPath`, `isKnownAppPath`, and the `AppRoutePath` union. All functions are pure and `window`-free; no helper throws on user-supplied input.
3. **Browser back/forward** — `AppShell.svelte` listens for `popstate` and updates `selected` from `window.location.pathname`. Nav clicks call `history.pushState` so back/forward step through in-app history without a full page reload. Cross-view transitions (`Launch terminal` from Servers, `Open` from Sessions, `Back to servers` from the Terminal view) route through the same `navigate(id)` helper so the URL stays in sync.
4. **Initial mount canonicalization.** On first paint, an unknown pathname (`/whatever`, `/servers/abc`, `/dashboard/extra`) collapses to the default view AND `replaceState`s the canonical path in place — the unknown URL never enters history. Known paths (including `/`) are left untouched.
5. **Dev tools have no route.** The dev-tools toggle and lab live under the same route as the surrounding view; they are not URL-addressable and remain gated by `import.meta.env.DEV` plus the `devTools` snippet from `App.svelte`.

**No secrets in URLs (load-bearing).**

- Terminal session ids, identity ids, profile ids, host ids, fingerprints, and any other backend-issued identifier MUST NOT appear in the URL in this slice. The active-launch hand-off continues to flow through shell-local state, NOT the URL.
- The router has no concept of route parameters. A path that *contains* a session-id-shaped segment (e.g. `/terminal/01HZK...`) collapses to the dashboard fallback — `viewForPath` rejects it and `normalizeAppPath` returns `null`. Sentinel test pin: `routing.test.ts` "redaction posture".
- `normalizeAppPath` strips a trailing `?query` or `#hash` before matching but never echoes their content; nothing the helper returns retains query parameters.

**Deployment requirement (load-bearing).**

The production host MUST serve `index.html` for every app route (`/`, `/dashboard`, `/terminal`, `/sessions`, `/servers`, `/identities`, `/settings`). Vite's dev server already does this; a static deployment without an SPA fallback will 404 on direct loads of any non-root route. Documented here so the deploy slice configures it explicitly.

**Architecture rule preserved.** The new helper lives in `lib/app/`; no imports from `lib/dev/`, no imports from any `@relayterm/terminal-*` adapter package. `appShellIsolation.test.ts` continues to enforce both bans.

**Future work (explicit out-of-scope for this slice).**

Route parameters / detail pages (`/servers/:id`, `/identities/:id`); auth routes (`/login`, passkey enrollment, session list); deep-link terminal-session launch; route-based data preloading; nested routes; multi-tab workspace; URL-driven renderer selection; URL-driven settings deep-links; shareable URLs that include any backend identifier. Each is a separate slice.
