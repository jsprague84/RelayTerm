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
- The smoke covers the dev renderer lab AND the production app shell
  (including the production terminal launch surface) to a depth a short
  manual procedure can verify against stable selectors. A heavyweight
  e2e harness is not paying for itself yet.
- Stable `data-testid` hooks live on both the dev lab and the production
  shell so this procedure (and any future committed runner) targets the
  same selectors.

## Stable selectors

The dev lab and the production shell expose these `data-testid` hooks.
Treat them as the contract this smoke depends on; if you rename one,
update this file in the same change.

| Selector                                          | Surface                                                       |
|---------------------------------------------------|---------------------------------------------------------------|
| `[data-testid="auth-loading"]`                    | Auth-gate loading splash (shown while `getCurrentUser()` is in flight at app start; replaced by either `auth-login-screen`, `auth-bootstrap-screen`, `auth-error-screen`, or the production shell). |
| `[data-testid="auth-error-screen"]`               | Auth-gate error surface (rendered on transport / 5xx / malformed `getCurrentUser()` outcomes; carries an explicit `auth-error-retry` button — the SPA does NOT auto-retry). |
| `[data-testid="auth-error-message"]`              | One-line operator-facing message inside `auth-error-screen` (safe formatter only — never echoes wire `message` or transport detail). |
| `[data-testid="auth-error-retry"]`                | Retry button inside `auth-error-screen` (re-runs `getCurrentUser()`). |
| `[data-testid="auth-login-screen"]`               | Sign-in screen root (rendered when `getCurrentUser()` returns 401 OR after sign-out). |
| `[data-testid="auth-login-heading"]`              | Static "Sign in to RelayTerm" heading (does NOT reveal whether the offered email belongs to a known account). |
| `[data-testid="auth-login-form"]`                 | Sign-in form root.                                            |
| `[data-testid="auth-login-email"]`                | Sign-in email input.                                          |
| `[data-testid="auth-login-password"]`             | Sign-in password input.                                       |
| `[data-testid="auth-login-submit"]`               | Sign-in submit button.                                        |
| `[data-testid="auth-login-form-error"]`           | Client-side validation error inside the sign-in form (safe formatter only — function of the reason enum). |
| `[data-testid="auth-login-error"]`                | Wire-side sign-in error inside the sign-in form (safe formatter only — collapses 401 to "invalid credentials" without revealing whether the offered email is known). |
| `[data-testid="auth-login-bootstrap-link"]`       | "First-time setup" link inside the sign-in screen (switches the unauthenticated screen to `auth-bootstrap-screen`). |
| `[data-testid="auth-bootstrap-screen"]`           | First-time setup screen root (creates the first user via `POST /api/v1/auth/bootstrap`; does NOT mint a session). |
| `[data-testid="auth-bootstrap-heading"]`          | Static "First-time setup" heading.                            |
| `[data-testid="auth-bootstrap-form"]`             | First-time setup form root.                                   |
| `[data-testid="auth-bootstrap-token"]`            | Bootstrap-token input (`<input type="password">`; never logged, never echoed, never persisted to local storage). |
| `[data-testid="auth-bootstrap-email"]`            | First-time setup email input.                                 |
| `[data-testid="auth-bootstrap-display-name"]`     | First-time setup display-name input.                          |
| `[data-testid="auth-bootstrap-password"]`         | First-time setup password input.                              |
| `[data-testid="auth-bootstrap-password-confirm"]` | First-time setup password confirmation input (frontend-only typo guard; the backend does not see the confirmation field). |
| `[data-testid="auth-bootstrap-submit"]`           | First-time setup submit button.                               |
| `[data-testid="auth-bootstrap-form-error"]`       | Client-side validation error inside the bootstrap form (safe formatter only — function of the reason enum; never echoes the offered token / password). |
| `[data-testid="auth-bootstrap-error"]`            | Wire-side bootstrap error inside the bootstrap form (safe formatter only — never echoes the wire `message`, transport detail, or any request input). |
| `[data-testid="auth-bootstrap-cancel"]`           | "Back to sign in" link inside the bootstrap form.             |
| `[data-testid="auth-bootstrap-success"]`          | Bootstrap success card ("Account created. Please sign in."; bootstrap does NOT auto-login). |
| `[data-testid="auth-bootstrap-back-to-login"]`    | "Back to sign in" button inside the bootstrap success card.   |
| `[data-testid="auth-current-user"]`               | Top-bar display-name label (visible at sm breakpoint and up; rendered only when an authenticated user is present). |
| `[data-testid="auth-sign-out"]`                   | Top-bar sign-out button (POSTs `/api/v1/auth/logout` AND clears local active-terminal state regardless of the wire outcome; the gate flips to `auth-login-screen` on completion). |
| `[data-testid="app-shell-main"]`                  | Production shell main pane (visible in dev AND prod).         |
| `[data-testid="top-bar-title"]`                   | Shell top bar title (mirrors selected nav item).              |
| `[data-testid="nav-dashboard"]`                   | Sidebar nav button — Dashboard (default-selected).            |
| `[data-testid="nav-terminal"]`                    | Sidebar nav button — Terminal workspace placeholder.          |
| `[data-testid="nav-sessions"]`                    | Sidebar nav button — Terminal sessions list/status view.      |
| `[data-testid="nav-servers"]`                     | Sidebar nav button — Server profiles placeholder.             |
| `[data-testid="nav-identities"]`                  | Sidebar nav button — SSH identities placeholder.              |
| `[data-testid="nav-settings"]`                    | Sidebar nav button — Local terminal preferences view.         |
| `[data-testid="production-view-dashboard"]`       | Dashboard view (selected by default).                         |
| `[data-testid="production-view-servers"]`         | Servers view (inventory of hosts + profiles, with create panels). |
| `[data-testid="servers-create-host-open"]`        | "Create host" button on the Servers view.                     |
| `[data-testid="servers-create-profile-open"]`     | "Create server profile" button on the Servers view (disabled when there are no hosts OR no SSH identities). |
| `[data-testid="servers-create-host-panel"]`       | Create-host panel container (visible after open).             |
| `[data-testid="servers-create-profile-panel"]`    | Create-server-profile panel container (visible after open).   |
| `[data-testid="host-key-panel"]`                  | Per-profile host-key preflight + trust panel (one per profile row; carries `data-profile-id`). |
| `[data-testid="host-key-preflight-button"]`       | "Run host-key preflight" / "Re-run preflight" button inside the panel. |
| `[data-testid="host-key-status-badge"]`           | Status badge after preflight; carries `data-status` (`unknown`/`trusted`/`changed`). |
| `[data-testid="host-key-fingerprint"]`            | Captured `SHA256:<base64>` fingerprint (selectable / copyable). |
| `[data-testid="host-key-confirm-input"]`          | Fingerprint-confirmation input (visible only when status is `unknown`). |
| `[data-testid="host-key-trust-button"]`           | "Trust this host key" button (enabled only after exact fingerprint confirmation). |
| `[data-testid="host-key-preflight-error"]`        | Preflight error summary (safe formatter only).                |
| `[data-testid="host-key-trust-error"]`            | Trust error summary (safe formatter only; collapses 409 to a conservative re-run-preflight message). |
| `[data-testid="host-key-trusted-success"]`        | Trust success card (rendered after a successful trust action). |
| `[data-testid="auth-check-panel"]`                | Per-profile SSH auth-check panel (one per profile row; carries `data-profile-id`; rendered immediately below the host-key panel). |
| `[data-testid="auth-check-run-button"]`           | "Run auth-check" / "Re-run auth-check" button inside the panel. |
| `[data-testid="auth-check-status-badge"]`         | Status badge after auth-check; carries `data-status` (`authentication_succeeded`/`authentication_failed`/`host_key_unknown`/`host_key_changed`/`connection_failed`) and `data-tone` (`ok`/`warn`/`blocked`/`error`). |
| `[data-testid="auth-check-status-description"]`   | One-line operator-facing description keyed off `status`. |
| `[data-testid="auth-check-success-footnote"]`     | Static success footnote (only rendered on `authentication_succeeded`; explicitly disclaims terminal launch). |
| `[data-testid="auth-check-error"]`                | Auth-check error summary (safe formatter only; never echoes wire `message` or transport detail). |
| `[data-testid="servers-filter-toolbar"]`          | Servers view filter toolbar above the Hosts/Profiles sections (in-memory client-side search + filters; no backend search; no pagination; no URL/local-storage persistence). |
| `[data-testid="servers-host-search"]`             | Hosts free-text search input (matches display name, hostname, port-as-decimal, default username). |
| `[data-testid="servers-profile-search"]`          | Profiles free-text search input (matches profile name, tags, username override, effective username, linked-host display + hostname, linked-identity name + fingerprint + key type — never the OpenSSH public key body). |
| `[data-testid="servers-profile-tag-filter"]`      | Profile tag select; pre-populated with the unique tags currently in use; auto-resets to "All tags" when the active tag disappears from the loaded inventory. |
| `[data-testid="servers-clear-filters"]`           | "Clear filters" button on the Servers view; enabled only while at least one Servers filter is active. |
| `[data-testid="hosts-count"]`                     | Hosts result-count badge; flips to "Showing X of Y hosts" form when the host search is active. |
| `[data-testid="hosts-filter-empty"]`              | Hosts empty-filter state ("No hosts match this filter."); distinct from `hosts-empty` (zero rows loaded). |
| `[data-testid="profiles-count"]`                  | Profiles result-count badge; flips to "Showing X of Y profiles" form when a profile filter is active. |
| `[data-testid="profiles-filter-empty"]`           | Profiles empty-filter state ("No profiles match this filter."); distinct from `profiles-empty` (zero rows loaded). |
| `[data-testid="host-row-select"]`                 | Per-host selectable button on the Servers view (opens the host detail panel; toggles closed when re-clicked; carries `aria-expanded`). |
| `[data-testid="host-detail-panel"]`               | Host detail panel container (read-only; fields, related-profiles list, honesty note). |
| `[data-testid="host-detail-close"]`               | Close button inside the host detail panel.                    |
| `[data-testid="host-detail-display-name"]`        | Host display-name field inside the detail panel.              |
| `[data-testid="host-detail-hostname"]`            | Host hostname field inside the detail panel.                  |
| `[data-testid="host-detail-port"]`                | Host port field inside the detail panel.                      |
| `[data-testid="host-detail-username"]`            | Host default-user field inside the detail panel.              |
| `[data-testid="host-detail-created-at"]`          | Host created-at field inside the detail panel.                |
| `[data-testid="host-detail-updated-at"]`          | Host updated-at field inside the detail panel.                |
| `[data-testid="host-detail-id"]`                  | Truncated host id (UUID prefix) inside the detail panel.      |
| `[data-testid="host-detail-profile-count"]`       | Count of profiles whose `host_id` matches the selected host (joined client-side from already-loaded profiles; not a fresh fetch). |
| `[data-testid="host-detail-profiles-list"]`       | Related-profile summary list inside the host detail panel.    |
| `[data-testid="host-detail-profiles-empty"]`      | Empty-state line inside the host detail panel when no profiles reference the host. |
| `[data-testid="host-detail-honesty"]`             | Static honesty note: host details do not prove reachability. |
| `[data-testid="host-detail-hidden-by-filter"]`    | Host detail panel banner rendered when the selected host is currently hidden by the Servers filter (the panel stays open; the banner names the active filter to clear). |
| `[data-testid="profile-row-select"]`              | Per-profile selectable button on the Servers view (opens the profile detail panel; toggles closed when re-clicked; carries `aria-expanded`). |
| `[data-testid="profile-detail-panel"]`            | Server-profile detail panel container (read-only; fields, linked-host + linked-identity summaries, readiness advisory). |
| `[data-testid="profile-detail-close"]`            | Close button inside the profile detail panel.                 |
| `[data-testid="profile-detail-name"]`             | Profile name field inside the detail panel.                   |
| `[data-testid="profile-detail-host"]`             | Profile host summary field inside the detail panel (renders an honest "host not in your inventory" line when the link cannot be resolved against the loaded hosts). |
| `[data-testid="profile-detail-host-missing"]`     | Inline notice rendered inside `profile-detail-host` when the host link is unresolved. |
| `[data-testid="profile-detail-username"]`         | Profile effective-username field inside the detail panel (carries explicit "(host default)" / "(override)" attribution). |
| `[data-testid="profile-detail-identity"]`         | Profile linked-identity summary inside the detail panel (id + name + key type + fingerprint joined client-side; honest "metadata available in the SSH Identities view" when unresolved). |
| `[data-testid="profile-detail-tags"]`             | Profile tags list inside the detail panel (only rendered when tags are non-empty). |
| `[data-testid="profile-detail-last-connected"]`   | Profile last-connected field inside the detail panel.         |
| `[data-testid="profile-detail-created-at"]`       | Profile created-at field inside the detail panel.             |
| `[data-testid="profile-detail-updated-at"]`       | Profile updated-at field inside the detail panel.             |
| `[data-testid="profile-detail-id"]`               | Truncated profile id (UUID prefix) inside the detail panel.   |
| `[data-testid="profile-detail-readiness"]`        | Advisory readiness line inside the profile detail panel; never claims "ready", "trusted", "verified", or "passed" — names host-key trust + auth-check as still-required steps. |
| `[data-testid="profile-detail-hidden-by-filter"]` | Profile detail panel banner rendered when the selected profile is currently hidden by the Servers filter. |
| `[data-testid="profile-launch-terminal"]`         | Per-profile "Launch terminal" button on the Servers view (creates a session and navigates to the Terminal workspace). The button label stays "Launch terminal" (or "Launching…" while submitting); when the row's profile is disabled the button is rendered disabled and a sibling hint reads "Re-enable this profile to start a new terminal session." |
| `[data-testid="profile-launch-error"]`            | Per-row launch error summary (safe formatter only — never echoes wire `message` or transport detail). |
| `[data-testid="profile-launch-error-dismiss"]`    | Dismiss button inside `profile-launch-error`.                 |
| `[data-testid="profile-lifecycle-badge"]`         | Inline `disabled` badge next to a profile's name in the row (rendered only when the profile is disabled; carries `data-lifecycle="disabled"`). |
| `[data-testid="profile-disabled-notice"]`         | Per-row inline note describing the disabled gate (rendered only when the profile is disabled; names "Existing live sessions are unaffected"). |
| `[data-testid="profile-lifecycle-controls"]`      | Per-row lifecycle action area (always rendered; switches between disable / confirm / enable controls based on lifecycle state). |
| `[data-testid="profile-disable-open"]`            | "Disable profile" button on an enabled profile's row (opens the confirmation panel). Replaced at runtime by `profile-disable-submitting` while a disable request is in flight. |
| `[data-testid="profile-disable-submitting"]`      | In-flight "Disabling…" button rendered while a disable request is in flight (mutually exclusive with `profile-disable-open` / the confirmation panel). |
| `[data-testid="profile-disable-confirm-copy"]`    | Static copy paragraph inside the confirmation panel (names the gate; never interpolates profile fields). |
| `[data-testid="profile-disable-confirm-input"]`   | Profile-name echo input inside the confirmation panel; the disable submit only enables on an exact match. |
| `[data-testid="profile-disable-confirm-mismatch"]` | Inline mismatch notice rendered when the typed value is non-empty but does not match the profile name. |
| `[data-testid="profile-disable-submit"]`          | "Disable profile" submit button inside the confirmation panel (enabled only after exact name match). |
| `[data-testid="profile-disable-cancel"]`          | Cancel button inside the confirmation panel. |
| `[data-testid="profile-enable-copy"]`             | Static copy paragraph next to the enable button on a disabled profile's row (names what enabling does NOT prove). |
| `[data-testid="profile-enable-submit"]`           | "Enable profile" button on a disabled profile's row (flips to "Enabling…" while in flight). |
| `[data-testid="profile-lifecycle-error"]`         | Lifecycle action error summary (safe formatter only — never echoes wire `message` or transport detail). |
| `[data-testid="profile-lifecycle-error-dismiss"]` | Dismiss button inside `profile-lifecycle-error`. |
| `[data-testid="host-key-profile-disabled"]`       | Inline "profile disabled" notice inside `host-key-panel` (rendered only when the profile is disabled). The preflight button is also rendered disabled. |
| `[data-testid="auth-check-profile-disabled"]`     | Inline "profile disabled" notice inside `auth-check-panel` (rendered only when the profile is disabled). The auth-check button is also rendered disabled. |
| `[data-testid="profile-detail-lifecycle"]`        | Server-profile detail panel `Lifecycle` row (always rendered; carries either the enabled or disabled badge). |
| `[data-testid="profile-detail-lifecycle-badge"]`  | Inline lifecycle badge inside the detail panel (`enabled` / `disabled`). |
| `[data-testid="profile-detail-disabled-note"]`    | Inline disabled-profile note inside the detail panel (rendered only when the profile is disabled). |
| `[data-testid="production-view-terminal"]`        | Terminal workspace empty state (rendered when there is no active launch). |
| `[data-testid="production-terminal"]`             | Production terminal workspace root (one per active session; carries `data-session-id` and `data-phase`; `data-phase` ∈ `idle`/`creating`/`connecting`/`attached`/`replaying`/`detached`/`closed`/`error`). |
| `[data-testid="production-terminal-phase"]`       | Workspace phase **label** rendered to the operator (`idle`/`creating session…`/`connecting…`/`live`/`replaying`/`detached (TTL window)`/`closed`/`error`); the label string is a function of the canonical `data-phase` value above. |
| `[data-testid="production-terminal-detach"]`      | "Detach" button (sends wire `Detach`; PTY enters TTL window).  |
| `[data-testid="production-terminal-close"]`       | "End session" button (sends wire `Close`; PTY ends immediately). |
| `[data-testid="production-terminal-reconnect"]`   | "Reconnect" button (re-attaches with `last_seen_seq`; disabled until the bookmark is positive). |
| `[data-testid="production-terminal-dispose"]`     | "Disconnect" button (tears down the local client + renderer without touching the session row). |
| `[data-testid="production-terminal-back"]`        | "Back to servers" button (clears the active launch and returns to the Servers view). |
| `[data-testid="production-terminal-ttl-hint"]`    | Detach TTL hint banner (visible only in the `detached` phase, before explicit close). |
| `[data-testid="production-terminal-closed"]`      | Closed-state hint banner.                                     |
| `[data-testid="production-terminal-error"]`       | Workspace error summary (safe formatter only — never echoes wire `message` or transport detail). |
| `[data-testid="production-terminal-viewport"]`    | xterm renderer host element (terminal output renders inside).  |
| `[data-testid="production-terminal-focus"]`       | "Focus terminal" button (moves keyboard focus into the renderer; enabled while live). |
| `[data-testid="production-terminal-fit"]`         | "Fit" button (refits the renderer to its container; the renderer's `onResize` listener drives the wire `resize` frame — the button does NOT call `client.sendResize`). |
| `[data-testid="production-terminal-clear"]`       | "Clear local viewport" button (renderer-only; never sends a wire frame, never mutates backend replay buffer, never asks the remote shell to run `clear`). |
| `[data-testid="production-terminal-settings-note"]` | Inline workspace note: "Appearance settings apply to new terminal sessions." (sourced from `TERMINAL_UX_COPY`). |
| `[data-testid="production-terminal-copy-paste-note"]` | Inline workspace note: browser-shortcut copy/paste guidance with bracketed-paste / OSC 52 flagged as future work (sourced from `TERMINAL_UX_COPY`). |
| `[data-testid="production-terminal-paste-confirm"]` | Paste-confirm panel (rendered only when `evaluatePaste` returned a `confirm` decision for a paste-candidate input; carries `data-paste-reason` ∈ `multiline`/`large_payload`/`control_chars`/`bracketed_paste_markers`). The full paste content is NEVER displayed; the panel renders metadata (line count, byte length) and a static disclaimer only. |
| `[data-testid="production-terminal-paste-confirm-heading"]` | Static heading inside the confirm panel (sourced from `describePasteDecision(reasonCode)`). |
| `[data-testid="production-terminal-paste-confirm-meta"]` | Metadata line inside the confirm panel ("X line(s), Y byte(s)"). |
| `[data-testid="production-terminal-paste-confirm-send"]` | "Send paste" button inside the confirm panel (snapshots the closure-scoped pending paste text, clears it, then calls `client.sendInput` exactly once). |
| `[data-testid="production-terminal-paste-confirm-cancel"]` | "Cancel" button inside the confirm panel (clears the closure-scoped pending paste text without sending). |
| `[data-testid="production-terminal-paste-blocked"]` | Paste-blocked panel (rendered only when `evaluatePaste` returned a `blocked` decision; carries `data-paste-reason` ∈ `nul_byte`/`exceeds_hard_cap`). The blocked content is dropped before the panel renders; only metadata reaches the DOM. |
| `[data-testid="production-terminal-paste-blocked-heading"]` | Static heading inside the blocked panel (sourced from `describePasteDecision(reasonCode)`). |
| `[data-testid="production-terminal-paste-blocked-meta"]` | Metadata line inside the blocked panel ("Y byte(s) dropped. Nothing was sent to the remote shell."). |
| `[data-testid="production-terminal-paste-blocked-dismiss"]` | "Dismiss" button inside the blocked panel. |
| `[data-testid="terminal-empty-settings-note"]`    | Empty-state Terminal view inline copy mirroring `production-terminal-settings-note`. |
| `[data-testid="terminal-empty-copy-paste-note"]`  | Empty-state Terminal view inline copy mirroring `production-terminal-copy-paste-note`. |
| `[data-testid="terminal-empty-saved"]`            | Empty-state "Reconnect last session" affordance card (rendered only when the local active-session pointer carries a record AND the mount-time backend validation has not classified it as stale; carries `data-saved-session-id` and `data-validation` (`idle` / `checking` / `reconnectable` / `uncertain`)). |
| `[data-testid="terminal-empty-saved-stale"]`      | Empty-state stale notice (rendered when the mount-time backend validation reports the saved session is closed or 404; replaces the affordance card and carries `data-saved-session-id`; the local pointer is dropped). |
| `[data-testid="terminal-empty-saved-checking"]`   | Inline "Checking saved session against the backend…" line inside the affordance card while the validation request is in flight; the Reconnect button is disabled during this state. |
| `[data-testid="terminal-empty-saved-uncertain"]`  | Inline cautious message inside the affordance card when the validation pass returned an uncertain outcome (transport blip / surprising HTTP / malformed / `starting`); the Reconnect button stays enabled because the failure may be transient and the local pointer is preserved. |
| `[data-testid="terminal-empty-reconnect-last"]`   | "Reconnect last session" button inside the saved-affordance card (explicit user action; never auto-fires; disabled while the mount-time validation is in flight). |
| `[data-testid="terminal-empty-forget-last"]`      | "Forget saved session" button inside the saved-affordance card (drops the local pointer without attempting reconnect). |
| `[data-testid="production-view-sessions"]`        | Terminal sessions list/status view root.                      |
| `[data-testid="sessions-refresh-button"]`         | Sessions view explicit refresh button.                        |
| `[data-testid="sessions-refresh-note"]`           | Static honesty note next to the Refresh button: "Refresh re-fetches the current backend state. There is no auto-refresh or live update yet — closed sessions cannot be recovered from this view." |
| `[data-testid="sessions-loading"]`                | Sessions view loading state.                                  |
| `[data-testid="sessions-error"]`                  | Sessions view list-load error summary (safe formatter only).  |
| `[data-testid="sessions-empty"]`                  | Sessions view empty state (rendered when the list is empty).  |
| `[data-testid="sessions-list"]`                   | Sessions list container (one row per `terminal_session`).     |
| `[data-testid="sessions-row"]`                    | One row in the sessions list (carries `data-session-id` and `data-status`). |
| `[data-testid="sessions-row-status"]`             | Per-row status badge; carries `data-status` (`starting`/`active`/`detached`/`closed`). |
| `[data-testid="sessions-row-description"]`        | Per-row honest one-line status description (no overpromise). |
| `[data-testid="sessions-row-ttl-hint"]`           | Per-row TTL disclaimer (visible only on `detached` rows).     |
| `[data-testid="sessions-row-reconnect"]`          | Per-row "Open" / "Reconnect" button (disabled for `closed`/`starting` rows, while the row is verifying, or when already attached; the label flips to "Verifying…" during the brief pre-handoff backend verify). |
| `[data-testid="sessions-row-close"]`              | Per-row "Close" button (disabled for `closed` rows).          |
| `[data-testid="sessions-row-close-error"]`        | Per-row close-error summary (safe formatter only — never echoes wire `message` or transport detail). |
| `[data-testid="sessions-row-open-error"]`         | Per-row open-error summary (rendered when the pre-handoff backend verify reports the row is stale or still `starting`; safe formatter only — never echoes wire `message` or transport detail; dismissable). |
| `[data-testid="production-view-identities"]`      | Identities view (public-key list + generate panel).           |
| `[data-testid="identities-refresh-button"]`       | Refresh button on the Identities view.                        |
| `[data-testid="identities-filter-toolbar"]`       | Identities view filter toolbar above the list (in-memory client-side search + filters; no backend search; no pagination; no URL/local-storage persistence). |
| `[data-testid="identities-search"]`               | Identities free-text search input (matches name, fingerprint, key type — never the OpenSSH public key body). |
| `[data-testid="identities-key-type-filter"]`      | Identities key-type select; rendered ONLY when more than one key type appears in the loaded list. |
| `[data-testid="identities-clear-filters"]`        | "Clear filters" button on the Identities view; enabled only while at least one identity filter is active. |
| `[data-testid="identities-count"]`                | Identities result-count badge; flips to "Showing X of Y identities" form when an identity filter is active. |
| `[data-testid="identities-filter-empty"]`         | Identities empty-filter state ("No identities match this filter."); distinct from `identities-empty` (zero rows loaded). |
| `[data-testid="identities-generate-open"]`        | "Generate SSH identity" button (opens the generate panel).    |
| `[data-testid="identities-generate-panel"]`       | Generate panel container (visible after open).                |
| `[data-testid="identities-generate-form"]`        | Generate panel form root.                                     |
| `[data-testid="identities-generate-name"]`        | Generate panel name input.                                    |
| `[data-testid="identities-generate-key-type"]`    | Generate panel key-type select (today: ed25519 only).         |
| `[data-testid="identities-generate-submit"]`      | Generate panel submit button.                                 |
| `[data-testid="identities-generate-close"]`       | Generate panel close button.                                  |
| `[data-testid="identities-generate-error"]`       | Generate panel error summary (safe formatter only).           |
| `[data-testid="identities-generate-success"]`     | Generate panel success card (public-key + copy action).       |
| `[data-testid="identity-row-select"]`             | Per-identity selectable button on the Identities view ("View details" / "Hide details"; opens the identity detail panel; toggles closed when re-clicked; carries `aria-expanded`). |
| `[data-testid="identity-detail-panel"]`           | SSH identity detail panel container (read-only; fields, full public key in a `<pre>`, copy action, honesty note). |
| `[data-testid="identity-detail-close"]`           | Close button inside the identity detail panel.                |
| `[data-testid="identity-detail-name"]`            | Identity name field inside the detail panel.                  |
| `[data-testid="identity-detail-key-type"]`        | Identity key-type field inside the detail panel.              |
| `[data-testid="identity-detail-fingerprint"]`     | Identity SHA-256 fingerprint field inside the detail panel.   |
| `[data-testid="identity-detail-public-key-preview"]` | One-line truncated public-key preview inside the detail panel (uses the same helper as the row preview; the full key reaches the DOM via `identity-detail-public-key` only). |
| `[data-testid="identity-detail-created-at"]`      | Identity created-at field inside the detail panel.            |
| `[data-testid="identity-detail-last-used-at"]`    | Identity last-used-at field inside the detail panel ("never" when null). |
| `[data-testid="identity-detail-id"]`              | Truncated identity id (UUID prefix) inside the detail panel.  |
| `[data-testid="identity-detail-public-key"]`      | Full OpenSSH public key rendered in a `<pre>` block inside the detail panel — the single deliberate path for the full key on this surface. |
| `[data-testid="identity-detail-copy-public-key"]` | "Copy public key" button inside the detail panel (copies `identity.public_key` only — never any private material; failure collapses to a static `Copy failed` label). |
| `[data-testid="identity-detail-honesty"]`         | Static honesty note: private key never reaches the browser; no UI exists to export, recover, or reveal private material. |
| `[data-testid="identity-detail-hidden-by-filter"]` | Identity detail panel banner rendered when the selected identity is currently hidden by the Identities filter. |
| `[data-testid="dashboard-refresh"]`               | Dashboard manual-refresh button (drives both health probe and inventory loads in parallel; no polling). |
| `[data-testid="dashboard-summary-cards"]`         | Dashboard summary card grid (health + hosts/profiles/identities/sessions counts). |
| `[data-testid="dashboard-card-health"]`           | Dashboard backend-health card (one-shot `/healthz` probe + per-card "Check now" button). |
| `[data-testid="dashboard-card-hosts"]`            | Dashboard hosts count card.                                   |
| `[data-testid="dashboard-card-profiles"]`         | Dashboard server-profiles count card.                         |
| `[data-testid="dashboard-card-identities"]`       | Dashboard SSH-identities count card.                          |
| `[data-testid="dashboard-card-sessions"]`         | Dashboard terminal-sessions count card.                       |
| `[data-testid="dashboard-session-breakdown"]`     | Dashboard sessions-by-status card (active/detached/starting/closed). |
| `[data-testid="dashboard-setup-checklist"]`       | Dashboard connection-flow checklist (count-inferable + manual rows). |
| `[data-testid="dashboard-nav-actions"]`           | Dashboard quick-action navigation buttons (Manage servers / Manage SSH identities / Open terminal / View sessions / Configure terminal). |
| `[data-testid="dashboard-recent-activity"]`       | Dashboard recent-activity card root (current-user audit feed snapshot, capped at 5 rows; not an admin view). |
| `[data-testid="dashboard-recent-activity-refresh"]` | Per-section refresh button (re-fetches the audit feed only; no polling). |
| `[data-testid="dashboard-recent-activity-view-all"]` | "View all" button inside the recent-activity card; routes through the AppShell `onNavigate("settings")` path to the Settings view, which hosts the fuller `RecentActivityPanel` (rendered as a `<button>`, not an anchor). |
| `[data-testid="dashboard-recent-activity-loading"]` | Recent-activity loading state (pre-fetch placeholder). |
| `[data-testid="dashboard-recent-activity-error"]` | Recent-activity error summary (safe formatter only — never echoes wire `message` or transport detail). |
| `[data-testid="dashboard-recent-activity-empty"]` | Recent-activity empty state ("No audit events yet."). |
| `[data-testid="dashboard-recent-activity-list"]`  | Recent-activity list container (one row per event, capped at 5). |
| `[data-testid="dashboard-recent-activity-row"]`   | One row inside the recent-activity list (carries `data-kind` set to the wire `AuditEventKind` tag). |
| `[data-testid="production-view-settings"]`        | Settings view root (local terminal preferences).              |
| `[data-testid="settings-terminal-appearance"]`    | Terminal appearance card (font / cursor / scrollback / theme controls). |
| `[data-testid="settings-font-family"]`            | Font-family text input.                                       |
| `[data-testid="settings-font-size"]`              | Font-size numeric input (clamped 8–32).                       |
| `[data-testid="settings-line-height"]`            | Line-height numeric input (clamped 0.8–2.5).                  |
| `[data-testid="settings-scrollback-lines"]`       | Scrollback-lines numeric input (clamped 0–100,000).           |
| `[data-testid="settings-cursor-style"]`           | Cursor-style select (`block` / `underline` / `bar`).          |
| `[data-testid="settings-cursor-blink"]`           | Cursor-blink checkbox.                                        |
| `[data-testid="settings-theme-preset"]`           | Theme-preset select (curated set; xterm baseline maps).       |
| `[data-testid="settings-preview"]`                | Live preview card showing sample shell output with the selected theme/font. |
| `[data-testid="settings-apply"]`                  | "Save changes" button (persists to localStorage; applies on next session). |
| `[data-testid="settings-reset"]`                  | "Reset to defaults" button (restores defaults and persists them). |
| `[data-testid="settings-status-saved"]`           | Save-success status text (rendered after a successful save / reset). |
| `[data-testid="settings-status-failed"]`          | Save-failure status text (rendered when localStorage write throws). |
| `[data-testid="settings-apply-note"]`             | Settings view inline copy mirroring `production-terminal-settings-note` (sourced from `TERMINAL_UX_COPY`). |
| `[data-testid="settings-copy-paste-note"]`        | Settings view inline copy mirroring `production-terminal-copy-paste-note` (sourced from `TERMINAL_UX_COPY`). |
| `[data-testid="settings-recent-activity"]`        | Recent-audit panel root inside the Settings view (current-user audit feed; read-only; not an admin view). |
| `[data-testid="settings-recent-activity-refresh"]` | Manual refresh button inside the recent-audit panel (no auto-refresh, no polling). |
| `[data-testid="settings-recent-activity-loading"]` | Recent-audit loading state. |
| `[data-testid="settings-recent-activity-error"]`  | Recent-audit error summary (safe formatter only — never echoes wire `message` or transport detail). |
| `[data-testid="settings-recent-activity-empty"]`  | Recent-audit empty state ("No audit events yet."). |
| `[data-testid="settings-recent-activity-list"]`   | Recent-audit list container (one row per event). |
| `[data-testid="settings-recent-activity-row"]`    | One row in the recent-audit list (carries `data-kind` set to the wire `AuditEventKind` tag). |
| `[data-testid="settings-auth-sessions"]`          | Settings session-management panel root (current-user browser sessions; read-only metadata + revoke actions; never displays the cookie token, the token hash, or `remote_addr` / `user_agent` — backend does not expose those yet). |
| `[data-testid="settings-auth-sessions-refresh"]`  | Manual refresh button inside the sessions panel (no auto-refresh, no polling). |
| `[data-testid="settings-auth-sessions-revoke-all"]` | "Revoke all other sessions" button (POSTs `/api/v1/auth/sessions/revoke-all-except-current`; confirms before mutating; disabled when no other active sessions). |
| `[data-testid="settings-auth-sessions-loading"]`  | Sessions panel loading state. |
| `[data-testid="settings-auth-sessions-error"]`    | Sessions panel list-error summary (safe formatter only — never echoes wire `message` or transport detail). |
| `[data-testid="settings-auth-sessions-action-error"]` | Per-action error summary (safe formatter only). |
| `[data-testid="settings-auth-sessions-success"]`  | Per-action success summary (e.g. "Revoked N other sessions."). |
| `[data-testid="settings-auth-sessions-empty"]`    | Sessions empty state ("No sessions found."). |
| `[data-testid="settings-auth-sessions-list"]`     | Sessions list container (one row per session). |
| `[data-testid="settings-auth-sessions-row"]`      | One row in the sessions list (carries `data-current` ∈ {`true`,`false`} and `data-status` ∈ {`active`,`expired`,`revoked`}). |
| `[data-testid="settings-auth-sessions-row-id"]`   | Short-id label for the row (truncated UUID; never the cookie token or token hash). |
| `[data-testid="settings-auth-sessions-current-badge"]` | "Current" badge — present iff this row is the session that authenticated the request. |
| `[data-testid="settings-auth-sessions-status-badge"]` | Status badge ("Active" / "Expired" / "Revoked"). |
| `[data-testid="settings-auth-sessions-revoke"]`   | Per-row Revoke button for a non-current active session (POSTs `/api/v1/auth/sessions/:id/revoke`; confirms before mutating). |
| `[data-testid="settings-auth-sessions-revoke-current"]` | Per-row "Sign out this browser" button for the current active session (revokes + runs local sign-out cleanup; the gate flips to `auth-login-screen` afterwards). |
| `[data-testid="settings-auth-sessions-future-note"]` | Footer note explicitly disclaiming `remote_addr` / `user_agent` / device-name / password-reset / passkeys / admin views as deferred work. |
| `[data-testid="settings-password-panel"]`         | Settings password-change panel root (current-user only; rotates the password after verifying the current one; revokes every OTHER session and keeps the current cookie valid). |
| `[data-testid="settings-password-current"]`       | Current-password input (`<input type="password">`; `autocomplete="current-password"`; never logged, never echoed). |
| `[data-testid="settings-password-new"]`           | New-password input (`<input type="password">`; `autocomplete="new-password"`; client-side length floor mirrors the backend `PASSWORD_MIN_LEN` / `PASSWORD_MAX_LEN`). |
| `[data-testid="settings-password-confirm"]`       | Confirmation input for the new password (frontend-only typo guard; the backend does not see the confirmation field). |
| `[data-testid="settings-password-submit"]`        | "Update password" submit button (POSTs `/api/v1/auth/change-password`; disabled while the request is in flight; copy switches to "Updating…"). |
| `[data-testid="settings-password-status-success"]` | Success status text (renders the safe formatter only — e.g. "Password updated. N other sessions were signed out."; never echoes wire `message`, `code`, or any password input). |
| `[data-testid="settings-password-status-failure"]` | Failure status text (safe formatter only; collapses 401 to a generic "current password is incorrect or your session has ended" string; clears every password field on the failure path). |
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

5. URL routing — assert each top-level nav click drives `pushState`
   (no full page reload, URL mirrors the selected view) and back/forward
   step through in-app history:

   - `browser_navigate http://localhost:5173/servers`
   - `browser_evaluate (() => ({ path: window.location.pathname, view: document.querySelector('[data-testid="app-shell-main"]')?.dataset.view }))`
   - Expected: `{ path: "/servers", view: "servers" }`.
   - `browser_click [data-testid="nav-identities"]`
   - Re-run the snippet. Expected: `{ path: "/identities", view: "identities" }`.
   - `browser_navigate_back`
   - Re-run the snippet. Expected: `{ path: "/servers", view: "servers" }`.
   - `browser_navigate http://localhost:5173/nope`
   - Re-run the snippet. Expected: `{ path: "/dashboard", view: "dashboard" }`
     (unknown paths replaceState-canonicalize to the dashboard).

5a. Recent-audit panel — navigate to the Settings view and confirm the
    current-user audit feed surface is reachable in the dev bundle. The
    panel issues one `GET /api/v1/audit-events/recent` on mount; without
    a live backend the request fails and the panel renders the error
    state. Both the loading-and-then-error path AND the
    loading-and-then-empty / -ready path are valid prod-bundle states —
    the smoke only asserts the panel root + Refresh button are present:

    - `browser_click [data-testid="nav-settings"]`
    - `browser_evaluate`:

      ```js
      () => {
        const has = (sel) => !!document.querySelector(sel);
        return {
          settingsView: has('[data-testid="production-view-settings"]'),
          recentAudit: has('[data-testid="settings-recent-activity"]'),
          recentAuditRefresh: has(
            '[data-testid="settings-recent-activity-refresh"]',
          ),
        };
      }
      ```

      Expected: every field `true`. Whether the panel currently shows
      `loading`, `error`, `empty`, or `list` depends on whether the
      backend is up — the smoke does NOT assert a specific state.

6. For each of `ghostty-web`, `restty`, `wterm`, `xterm` (in that
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

7. `browser_console_messages level=error all=true`. The only allowed
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

4. Navigate to the Servers view and assert the create panels render.
   Hosts and profile creation are production-safe write flows; this
   step does NOT submit the forms (no live backend is assumed by the
   smoke), only verifies they are reachable in the prod bundle:

   - `browser_click [data-testid="nav-servers"]`
   - `browser_evaluate`:

     ```js
     () => {
       const has = (sel) => !!document.querySelector(sel);
       return {
         serversView: has('[data-testid="production-view-servers"]'),
         createHostOpen: has('[data-testid="servers-create-host-open"]'),
         createProfileOpen: has(
           '[data-testid="servers-create-profile-open"]',
         ),
         // Panels are not opened yet — the open buttons are present but
         // the panel containers should be absent until clicked.
         createHostPanelClosed: !has(
           '[data-testid="servers-create-host-panel"]',
         ),
         createProfilePanelClosed: !has(
           '[data-testid="servers-create-profile-panel"]',
         ),
       };
     }
     ```

     Expected: every field `true`.

   - Click the "Create host" button and verify the panel renders:
     - `browser_click [data-testid="servers-create-host-open"]`
     - Assert `has('[data-testid="servers-create-host-panel"]')` is
       `true` and `has('[data-testid="servers-create-host-form"]')` is
       `true`.
     - Click the panel's close button to leave the page tidy:
       `browser_click [data-testid="servers-create-host-close"]`.

   The create-server-profile button may be disabled when the dev
   inventory is empty; in that case `servers-create-profile-blocked`
   carries the honest empty-state hint. This is the documented contract
   — do not mark it a regression.

5. Verify the production terminal launch surface is reachable in the
   prod bundle (no live backend is assumed; this step does NOT click
   "Launch terminal" because that would issue a real `POST` against
   `/api/v1/terminal-sessions`):

   - With the Servers view still selected, `browser_evaluate`:

     ```js
     () => {
       const has = (sel) => !!document.querySelector(sel);
       const launchButtons = document.querySelectorAll(
         '[data-testid="profile-launch-terminal"]',
       );
       return {
         // The button is per-row; if the dev inventory has no profiles
         // the button is absent and the "no profiles yet" empty state
         // renders instead. Both are valid prod-bundle states.
         launchButtonAbsentOrPresent:
           launchButtons.length === 0 || launchButtons.length >= 1,
         // The terminal workspace is not visible until a launch
         // succeeds; the empty-state placeholder lives behind the
         // Terminal nav item.
         workspaceEmptyState: false, // populated below
       };
     }
     ```

   - `browser_click [data-testid="nav-terminal"]`
   - Assert the empty Terminal view renders and the production
     workspace is NOT yet mounted:

     ```js
     () => {
       const has = (sel) => !!document.querySelector(sel);
       return {
         emptyState: has('[data-testid="production-view-terminal"]'),
         workspaceAbsent: !has('[data-testid="production-terminal"]'),
       };
     }
     ```

     Expected: both `true`. The workspace selectors
     (`production-terminal-*`) only become reachable after a successful
     launch from the Servers view; verifying the post-launch surface
     requires a live backend and is out of scope for this smoke.

6. URL routing — production-build parity check. The deployment must
   serve `index.html` for every app route; this step asserts the
   preview server's SPA fallback covers the route table:

   - `browser_navigate http://localhost:4173/sessions`
   - `browser_evaluate (() => ({ path: window.location.pathname, view: document.querySelector('[data-testid="app-shell-main"]')?.dataset.view }))`
   - Expected: `{ path: "/sessions", view: "sessions" }`.
   - `browser_navigate http://localhost:4173/settings`
   - Re-run the snippet. Expected: `{ path: "/settings", view: "settings" }`.
   - `browser_navigate http://localhost:4173/nope`
   - Re-run the snippet. Expected: `{ path: "/dashboard", view: "dashboard" }`
     (unknown paths replaceState-canonicalize to the dashboard).
   - `browser_click [data-testid="nav-servers"]` then `browser_navigate_back`
   - Re-run the snippet. Expected: `{ path: "/dashboard", view: "dashboard" }`.

   If any direct load returns a 404 / blank page, the deploy host is
   missing its SPA fallback — see SPEC.md "URL-driven production view
   routing" for the requirement.

7. `browser_console_messages level=error all=true`. As above, the
   favicon `404` is the only allowed error.

### B.1. Production terminal paste safety smoke (requires a live backend with a launchable session)

This step verifies the paste-safety policy in
`apps/web/src/lib/app/terminal/pastePolicy.ts` (the unit tests pin the
policy itself; this smoke verifies the integration in
`ProductionTerminal.svelte`). It is **not** part of the production
prod-build smoke above because it requires a launched terminal session,
which in turn requires a live backend AND a trusted host-key + working
auth identity. Skip if the local backend has no SSH target available.

Pre-conditions: a launched terminal session is mounted under
`[data-testid="production-terminal"]`, `data-phase="attached"`, the
viewport has focus.

Note on clipboard access: the snippets below use `navigator.clipboard.writeText`
from `browser_evaluate`. Many browser configurations (including some
Playwright MCP setups) gate clipboard-write behind a permission grant.
If `writeText` rejects, fall back to typing directly into the viewport
via `browser_type` against the `[data-testid="production-terminal-viewport"]`
element — `evaluatePaste` runs on the same `onInput` payload regardless
of paste source, so the policy outcomes are identical.

1. **Single-line safe paste — no panel.**
   - Programmatically dispatch a paste of `"echo hello"` against the
     viewport (or use `browser_press_key` to type it; the goal is to
     verify the safe path forwards silently). Use `browser_evaluate`:

     ```js
     async () => {
       const term = document.querySelector(
         '[data-testid="production-terminal-viewport"]',
       );
       term?.focus();
       await navigator.clipboard.writeText("echo hello");
       // Trigger xterm's clipboard read via Ctrl+Shift+V using the
       // browser_press_key tool below.
     }
     ```

   - `browser_press_key Control+Shift+V` (or use `browser_type` on the
     viewport for a deterministic alternative).
   - Assert the confirm and blocked panels are NOT present:

     ```js
     () => {
       const has = (sel) => !!document.querySelector(sel);
       return {
         confirmAbsent: !has('[data-testid="production-terminal-paste-confirm"]'),
         blockedAbsent: !has('[data-testid="production-terminal-paste-blocked"]'),
       };
     }
     ```

     Expected: both `true`. The remote shell received `echo hello`
     (visible as terminal output the next frame).

2. **Multiline confirm — content NOT displayed; cancel does not send.**
   - Write a multi-line string to the clipboard:
     `await navigator.clipboard.writeText("echo first\necho second\n")`.
   - `browser_press_key Control+Shift+V`.
   - Assert the confirm panel renders with `data-paste-reason="multiline"`
     AND that the panel text does NOT contain the paste body:

     ```js
     () => {
       const panel = document.querySelector(
         '[data-testid="production-terminal-paste-confirm"]',
       );
       const heading = document.querySelector(
         '[data-testid="production-terminal-paste-confirm-heading"]',
       );
       const meta = document.querySelector(
         '[data-testid="production-terminal-paste-confirm-meta"]',
       );
       const sentinel = "echo first";
       return {
         present: !!panel,
         reason: panel?.dataset.pasteReason,
         headingText: heading?.textContent ?? "",
         metaText: meta?.textContent ?? "",
         contentLeak:
           (heading?.textContent ?? "").includes(sentinel) ||
           (meta?.textContent ?? "").includes(sentinel) ||
           (panel?.textContent ?? "").includes(sentinel),
       };
     }
     ```

     Expected: `present: true`, `reason: "multiline"`, `contentLeak:
     false`. The heading is the static `Multiline paste detected.`
     string; the meta carries `2 lines, ` plus a byte count; the body
     `echo first` / `echo second` is NOT present anywhere in the panel.

   - `browser_click [data-testid="production-terminal-paste-confirm-cancel"]`.
   - Re-snapshot: confirm panel is gone, no `echo first` text appeared
     in the terminal viewport (it would render as terminal output if
     sent — observe the viewport before and after Cancel). Expected:
     panel absent, viewport unchanged.

3. **Multiline confirm — Send paste forwards exactly the original.**
   - Trigger the same multiline paste again.
   - `browser_click [data-testid="production-terminal-paste-confirm-send"]`.
   - Observe the terminal viewport: the remote shell should receive the
     full multiline payload exactly once (visible as `echo first`,
     newline, `echo second`, newline). The confirm panel disappears.

4. **Blocked paste — drops content; Dismiss clears the panel.**
   - Construct a paste candidate that exceeds the hard cap (64 KiB):
     `await navigator.clipboard.writeText("a".repeat(65 * 1024))`.
   - `browser_press_key Control+Shift+V`.
   - Assert the blocked panel renders with
     `data-paste-reason="exceeds_hard_cap"` and the dropped byte count
     in its meta line. The body `aaaa...` content is NOT in the panel
     (only the size metadata).
   - `browser_click [data-testid="production-terminal-paste-blocked-dismiss"]`.
   - Re-snapshot: blocked panel is gone. Terminal viewport is
     unchanged — nothing was sent to the remote shell.

5. **Console redaction.**
   - `browser_console_messages level=error all=true`.
   - Expected: no entries containing the paste body sentinels (`echo
     first`, `echo second`, the 65 KiB filler character runs). The
     favicon `404` (if present) is the only allowed entry.

If a live SSH target is unavailable, skip this section and re-run
after the backend gains one. The `pastePolicy.test.ts` unit tests pin
the underlying classification rules without a backend.

### C. Auth flow smoke (browser, requires a live backend)

This half exists because the SPA's auth surfaces (AuthGate, LoginView,
BootstrapView, TopBar sign-out, Settings password panel, Settings session
panel, Settings recent-activity panel, the protected app shell after
login) do not render meaningfully without a live backend. The dev /
production smokes above intentionally accept either a live backend OR an
"error / empty" placeholder; this half assumes the backend is up AND
configured (`docs/production-auth.md` § 2 envelope, OR a dev-mode boot
with `RELAYTERM_AUTH__FIRST_USER_BOOTSTRAP_TOKEN` set).

The wire-side of these flows is covered in detail by
`docs/auth-smoke.md` — this section verifies the SPA renders the right
selectors at each gate transition, AND that the gate-flip logic works
end-to-end. Run this only after the backend smoke has passed.

Use a fresh browser profile (no prior cookies for the origin) so the
AuthGate starts on `auth-loading` → `auth-login-screen` rather than
straight into the production shell.

1. **AuthGate loading splash.**
   - `browser_navigate http://localhost:5173/` (dev) or the deployed
     origin (production).
   - Within the first frame the SPA mounts `AuthGate.svelte` and issues
     `GET /api/v1/auth/me`. Race conditions matter here — capture both
     the loading state AND the resolved gate state in a single snapshot:

     ```js
     () => {
       const has = (sel) => !!document.querySelector(sel);
       return {
         loadingPresent: has('[data-testid="auth-loading"]'),
         loginPresent: has('[data-testid="auth-login-screen"]'),
         shellPresent: has('[data-testid="app-shell-main"]'),
         errorPresent: has('[data-testid="auth-error-screen"]'),
       };
     }
     ```

     Expected (with a live backend AND no cookie): exactly one of
     `loadingPresent` / `loginPresent` is `true` — the loading splash is
     short-lived and may have already resolved by the time the snapshot
     runs. `errorPresent` is `false`. `shellPresent` is `false`.

2. **First-time setup gate (only on a fresh database).**
   - With `user_passwords` empty AND `auth.first_user_bootstrap_token`
     configured, the login screen shows
     `[data-testid="auth-login-bootstrap-link"]`. Click it.
   - Assert `[data-testid="auth-bootstrap-screen"]` renders:

     ```js
     () => {
       const has = (sel) => !!document.querySelector(sel);
       return {
         bootstrapScreen: has('[data-testid="auth-bootstrap-screen"]'),
         tokenField: has('[data-testid="auth-bootstrap-token"]'),
         emailField: has('[data-testid="auth-bootstrap-email"]'),
         displayNameField: has('[data-testid="auth-bootstrap-display-name"]'),
         passwordField: has('[data-testid="auth-bootstrap-password"]'),
         confirmField: has('[data-testid="auth-bootstrap-password-confirm"]'),
         submit: has('[data-testid="auth-bootstrap-submit"]'),
         backLink: has('[data-testid="auth-bootstrap-cancel"]'),
       };
     }
     ```

     Expected: every field `true`.
   - Fill in the bootstrap form (test bootstrap token + a real email +
     display name + a ≥ 12-char password, twice). Submit. Assert
     `[data-testid="auth-bootstrap-success"]` renders. Bootstrap does
     NOT auto-login; click `[data-testid="auth-bootstrap-back-to-login"]`
     to return to the sign-in screen.
   - **If the database already has a first user, skip this step.** The
     bootstrap screen still renders if you click the link, but the
     submit returns `409 already_bootstrapped` and
     `[data-testid="auth-bootstrap-error"]` carries the safe-formatter
     copy. That is the documented contract.

3. **Login form happy path.**
   - From the sign-in screen, fill the email + password fields:

     ```text
     browser_fill_form
       [data-testid="auth-login-email"] = operator@example.com
       [data-testid="auth-login-password"] = <password>
     browser_click [data-testid="auth-login-submit"]
     ```

   - On success the gate flips to the production shell:

     ```js
     () => {
       const has = (sel) => !!document.querySelector(sel);
       const userLabel = document
         .querySelector('[data-testid="auth-current-user"]')
         ?.textContent?.trim();
       return {
         shellPresent: has('[data-testid="app-shell-main"]'),
         loginGone: !has('[data-testid="auth-login-screen"]'),
         signOutPresent: has('[data-testid="auth-sign-out"]'),
         currentUserLabel: userLabel ?? null,
       };
     }
     ```

     Expected: `shellPresent`, `loginGone`, `signOutPresent` all `true`;
     `currentUserLabel` matches the operator's display name.

4. **Login form failure path.**
   - Sign out (step 8 below, or use a fresh browser profile) and try a
     deliberately wrong password.
   - Assert `[data-testid="auth-login-error"]` renders the safe-formatter
     copy. The text MUST be a stable function of the failure category —
     it must NOT echo the wire `message`, the offered email, or any
     transport detail. The login heading
     (`[data-testid="auth-login-heading"]`) does NOT change to reveal
     whether the email is known.
   - The email field is left populated so the operator can correct the
     entry. The password field IS auto-cleared on failure so a retry
     starts from a fresh field — the LoginView wipes `password` after
     `loginApi` returns a non-`ok` result. Confirm with
     `browser_evaluate` that the `[data-testid="auth-login-password"]`
     input value is `""`.

5. **Reload preserves session.**
   - After a successful sign-in (step 3), reload the page.
   - The AuthGate flashes `auth-loading` again, issues `GET /auth/me`,
     and resolves directly to the production shell — no password prompt.

6. **Settings password panel.**
   - `browser_click [data-testid="nav-settings"]`.
   - Confirm the panel renders with all three password fields and the
     submit button:

     ```js
     () => {
       const has = (sel) => !!document.querySelector(sel);
       return {
         panel: has('[data-testid="settings-password-panel"]'),
         current: has('[data-testid="settings-password-current"]'),
         newField: has('[data-testid="settings-password-new"]'),
         confirm: has('[data-testid="settings-password-confirm"]'),
         submit: has('[data-testid="settings-password-submit"]'),
       };
     }
     ```

     Expected: every field `true`.
   - Fill in current + new + confirmation (≥ 12 chars; new ≠ current)
     and submit.
   - On success, `[data-testid="settings-password-status-success"]`
     renders the safe-formatter string (e.g. `Password updated. N other
     sessions were signed out.`). **Every password field is wiped** —
     verify by reading the input values via `browser_evaluate`:

     ```js
     () => {
       const get = (sel) => document.querySelector(sel)?.value ?? null;
       return {
         current: get('[data-testid="settings-password-current"]'),
         newField: get('[data-testid="settings-password-new"]'),
         confirm: get('[data-testid="settings-password-confirm"]'),
       };
     }
     ```

     Expected: every field is `""`.
   - On failure (wrong current), `[data-testid="settings-password-status-failure"]`
     renders the safe-formatter copy and the password fields are wiped
     too — the panel does not preserve the offered current password.
   - The current cookie stays valid: `[data-testid="auth-current-user"]`
     in the top bar still shows the operator's display name; reloading
     the page does not bounce to the login screen.

7. **Settings session-management panel.**
   - With Settings still selected, scroll to
     `[data-testid="settings-auth-sessions"]`.
   - Confirm the panel renders the row list and that the current row
     carries the right markers:

     ```js
     () => {
       const has = (sel) => !!document.querySelector(sel);
       const rows = Array.from(
         document.querySelectorAll('[data-testid="settings-auth-sessions-row"]'),
       );
       const currentRow = rows.find(
         (r) => r.dataset.current === "true",
       );
       return {
         panel: has('[data-testid="settings-auth-sessions"]'),
         refresh: has('[data-testid="settings-auth-sessions-refresh"]'),
         futureNote: has('[data-testid="settings-auth-sessions-future-note"]'),
         rowCount: rows.length,
         currentRowStatus: currentRow?.dataset.status ?? null,
         currentRowHasBadge: !!currentRow?.querySelector(
           '[data-testid="settings-auth-sessions-current-badge"]',
         ),
         currentRowRevokeButton: !!currentRow?.querySelector(
           '[data-testid="settings-auth-sessions-revoke-current"]',
         ),
       };
     }
     ```

     Expected: `panel`, `refresh`, `futureNote`,
     `currentRowHasBadge`, `currentRowRevokeButton` all `true`.
     `rowCount >= 1`. `currentRowStatus === "active"`.
   - Optional (only if there is more than one active session): click a
     non-current row's `[data-testid="settings-auth-sessions-revoke"]`
     button, confirm the dialog, and assert
     `[data-testid="settings-auth-sessions-success"]` renders. Confirm
     the row's `data-status` flips to `revoked` after the next refresh.
   - DO NOT click `[data-testid="settings-auth-sessions-revoke-all"]`
     unless you want to invalidate every other session for the
     operator — the action is real.
   - Hard rule: the panel must NOT display any of the following — cookie
     token, token hash, `remote_addr`, `user_agent`, device-name. The
     `data-testid="settings-auth-sessions-future-note"` line is the
     honest disclaimer that pins this contract.

8. **Recent-activity panel (current-user audit feed).**
   - Still on the Settings view, scroll to
     `[data-testid="settings-recent-activity"]`.
   - Confirm the panel renders the list state (assuming the smoke has
     been generating events: `first_user_created`, `login_succeeded`,
     `password_changed`, `session_revoked`, …):

     ```js
     () => {
       const has = (sel) => !!document.querySelector(sel);
       const rows = Array.from(
         document.querySelectorAll('[data-testid="settings-recent-activity-row"]'),
       );
       return {
         panel: has('[data-testid="settings-recent-activity"]'),
         refresh: has('[data-testid="settings-recent-activity-refresh"]'),
         rowCount: rows.length,
         kinds: rows.map((r) => r.dataset.kind),
       };
     }
     ```

     Expected: `panel`, `refresh` both `true`. `rowCount >= 1`.
     `kinds` contains a public-taxonomy label per row (`login_succeeded`,
     `password_changed`, etc.) — the data-attribute is a STABLE wire
     enum, not free-form text.
   - The panel renders empty / error / loading states equivalently — see
     the Settings dev smoke in §A step 5a. This step assumes the loaded
     state because the smoke has been generating events.
   - Hard rule: NO row's text contains the cookie token, token hash,
     plaintext / hashed password, bootstrap token, raw DB error text,
     terminal I/O, or peer banner. NULL-actor rows (failed-login probes)
     are filtered out by the route — they do NOT appear here.
   - Optional: navigate to the Dashboard via
     `[data-testid="nav-dashboard"]` and confirm the parallel surface
     (`[data-testid="dashboard-recent-activity"]`,
     `[data-testid="dashboard-recent-activity-row"]`) renders the same
     event taxonomy capped at 5 rows.

9. **TopBar sign-out.**
   - From any view, click `[data-testid="auth-sign-out"]`.
   - The SPA POSTs `/api/v1/auth/logout`, drops the local active-launch
     state, and flips the gate back to `auth-login-screen` regardless of
     the wire outcome (a transport failure during sign-out still flips
     the gate locally — the server-side cleanup happens on the next
     login).
   - Assert:

     ```js
     () => {
       const has = (sel) => !!document.querySelector(sel);
       return {
         loginPresent: has('[data-testid="auth-login-screen"]'),
         shellGone: !has('[data-testid="app-shell-main"]'),
         currentUserGone: !has('[data-testid="auth-current-user"]'),
       };
     }
     ```

     Expected: every field `true`.
   - Reloading the page does not bounce back into the shell — the cookie
     is cleared.

10. **Console errors.** As with sections A and B,
    `browser_console_messages level=error all=true` should report the
    favicon `404` only.

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
