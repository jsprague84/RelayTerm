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
| `[data-testid="host-detail-actions"]`             | Action row inside the host detail panel that contains the Edit / Delete affordances; always rendered (the per-action buttons render conditionally based on submit / confirm state). |
| `[data-testid="host-detail-edit-open"]`           | "Edit host" button on the host detail panel; disabled while any host edit / delete request is in flight. Hidden once the edit form is open for this host. |
| `[data-testid="host-detail-edit-form"]`           | Host edit form container (rendered only while the edit state is `open` for the selected host). |
| `[data-testid="host-detail-edit-display-name"]`   | Display-name input inside the host edit form. |
| `[data-testid="host-detail-edit-hostname"]`       | Hostname input inside the host edit form. |
| `[data-testid="host-detail-edit-port"]`           | Port input inside the host edit form (1..=65535, integer). |
| `[data-testid="host-detail-edit-username"]`       | Default-user input inside the host edit form. |
| `[data-testid="host-detail-edit-save"]`           | "Save" submit button inside the host edit form. |
| `[data-testid="host-detail-edit-cancel"]`         | "Cancel" button inside the host edit form. |
| `[data-testid="host-detail-edit-submitting"]`     | In-flight "Saving…" indicator inside the host detail panel; rendered in place of the form / error render while the PATCH is in flight (mutually exclusive). |
| `[data-testid="host-detail-edit-error"]`          | Host edit error summary (`describeUpdateHostError` — safe formatter only; never echoes wire `message` or transport detail). |
| `[data-testid="host-detail-delete-open"]`         | "Delete host" button on the host detail panel; disabled while any host edit / delete request is in flight. Hidden once the confirm panel is open for this host. |
| `[data-testid="host-detail-delete-confirm"]`      | Host delete confirmation panel (rendered only while delete state is `confirming` for the selected host); carries the warning copy that names the FK-RESTRICT consequence. |
| `[data-testid="host-detail-delete-confirm-input"]`| Host-display-name echo input inside the delete confirm panel; the delete submit only enables on an exact match. |
| `[data-testid="host-detail-delete-confirm-submit"]`| "Delete host" submit button inside the confirm panel (enabled only after exact display-name match). |
| `[data-testid="host-detail-delete-cancel"]`       | "Cancel" button inside the delete confirm panel. |
| `[data-testid="host-detail-delete-submitting"]`   | In-flight "Deleting…" indicator inside the host detail panel; rendered in place of the confirm panel while the DELETE is in flight (mutually exclusive). |
| `[data-testid="host-detail-delete-error"]`        | Host delete error summary (`describeDeleteHostError` — safe formatter only). `409 referenced` maps to "still used by a saved server profile or has trusted host keys — remove the dependent items first." |
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
| `[data-testid="profile-detail-actions"]`          | Action row inside the profile detail panel that contains the Edit / Delete affordances; always rendered (the per-action buttons render conditionally based on submit / confirm state). |
| `[data-testid="profile-detail-edit-open"]`        | "Edit profile" button on the profile detail panel; disabled while any profile edit / delete request is in flight. Hidden once the edit form is open for this profile. |
| `[data-testid="profile-detail-edit-form"]`        | Profile edit form container (rendered only while the edit state is `open` for the selected profile). Builds a delta on submit — saving with no changes surfaces "change at least one field". |
| `[data-testid="profile-detail-edit-name"]`        | Name input inside the profile edit form. |
| `[data-testid="profile-detail-edit-host"]`        | Host `<select>` inside the profile edit form. |
| `[data-testid="profile-detail-edit-identity"]`    | SSH identity `<select>` inside the profile edit form. |
| `[data-testid="profile-detail-edit-username-override"]` | Username-override input inside the profile edit form (blank → host default). |
| `[data-testid="profile-detail-edit-tags"]`        | Tags input inside the profile edit form (comma-separated). |
| `[data-testid="profile-detail-edit-save"]`        | "Save" submit button inside the profile edit form. |
| `[data-testid="profile-detail-edit-cancel"]`      | "Cancel" button inside the profile edit form. |
| `[data-testid="profile-detail-edit-submitting"]`  | In-flight "Saving…" indicator inside the profile detail panel; rendered in place of the form / error render while the PATCH is in flight (mutually exclusive). |
| `[data-testid="profile-detail-edit-error"]`       | Profile edit error summary (`describeUpdateServerProfileError` — safe formatter only). |
| `[data-testid="profile-detail-delete-open"]`      | "Delete profile" button on the profile detail panel; disabled while any profile edit / delete request is in flight. Hidden once the confirm panel is open for this profile. |
| `[data-testid="profile-detail-delete-confirm"]`   | Profile delete confirmation panel (rendered only while delete state is `confirming` for the selected profile); carries the warning copy that explicitly names the disable-instead path when terminal session history exists. |
| `[data-testid="profile-detail-delete-confirm-input"]` | Profile-name echo input inside the delete confirm panel; the delete submit only enables on an exact match. |
| `[data-testid="profile-detail-delete-confirm-submit"]`| "Delete profile" submit button inside the confirm panel (enabled only after exact name match). |
| `[data-testid="profile-detail-delete-cancel"]`    | "Cancel" button inside the delete confirm panel. |
| `[data-testid="profile-detail-delete-submitting"]`| In-flight "Deleting…" indicator inside the profile detail panel; rendered in place of the confirm panel while the DELETE is in flight (mutually exclusive). |
| `[data-testid="profile-detail-delete-error"]`     | Profile delete error summary (`describeDeleteServerProfileError` — safe formatter only). `409 referenced` maps to "it has terminal session history — disable it instead to keep the history while blocking new launches." |
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
| `[data-testid="production-terminal-viewport"]`    | Renderer host element (terminal output renders inside; xterm by default).  |
| `[data-testid="production-terminal-renderer-diagnostic"]` | Renderer diagnostic strip rendered in the workspace footer after the renderer mounts (text body includes `rendererLabel` + experimental/fallback hint). Provides a visible "which renderer am I looking at" cue alongside the `data-renderer` attribute on `production-terminal`. |
| `[data-testid="production-terminal-launch-timing"]` | Launch-timing diagnostic strip rendered in the workspace footer when a launch-timing recorder was supplied by the launch caller (`ServersView.launchProfile`; the saved-session reconnect path omits the recorder so this block is absent on a reconnect from the empty-state Terminal view). Carries one `<dt>` per entry in `LAUNCH_TIMING_EVENT_NAMES` with `data-launch-event` ∈ `launch_started`/`create_session_post_started`/`create_session_post_resolved`/`ws_connect_started`/`ws_open`/`first_server_message`/`first_output`/`attached`/`detach_requested`/`close_requested`/`ws_close`/`error`, `data-launch-event-state` ∈ `observed`/`pending`, and `data-launch-event-ms` set to the relative-monotonic offset (in ms) for observed events (empty for pending). Payload-free by contract — see `apps/web/src/lib/app/terminal/terminalLaunchTiming.ts`'s "Redaction posture" comment. |
| `[data-testid="production-terminal-launch-timing-post-outcome"]` | Inline pill inside the launch-timing strip; appears only after `create_session_post_resolved` lands. Closed vocabulary: `POST ok` / `POST error`. Never carries an HTTP status, a wire `message`, or any URL fragment. |
| `[data-testid="production-terminal-launch-timing-error-kind"]` | Inline pill inside the launch-timing strip; appears only after the first typed client / transport / POST error fires. Closed vocabulary mirroring the recorder's `LaunchTimingErrorKind` union (`create_session_post`/`transport`/`decode`/`unexpected_first_frame`/`send_before_attached`/`send_after_terminal`/`server_error`/`unknown`). Never carries a free-form message. |
| `[data-testid="production-terminal-launch-timing-list"]` | Definition-list container holding the per-event rows. One `<dt>`/`<dd>` pair per name in `LAUNCH_TIMING_EVENT_NAMES`; iterate the children to walk the full snapshot. |
| `data-launch-timing` (attribute on `production-terminal`) | `"available"` when a launch-timing recorder was supplied by the launch caller (a Servers-view launch); `"none"` when no recorder was supplied (saved-session reconnect path) OR pre-mount. Use this to gate any smoke assertion that relies on the timing strip / per-event attributes. |
| `data-launch-timing-create-post-outcome` (attribute on `production-terminal`) | Closed vocabulary `ok`/`error`/`""` (empty before the POST resolves). Mirror of the inline pill above; pull from the section root when you only need the create-POST outcome and don't want to traverse the strip. |
| `data-launch-timing-error-kind` (attribute on `production-terminal`) | Closed vocabulary mirroring `LaunchTimingErrorKind`; empty until the first error fires. Mirror of the inline pill above. |
| `data-launch-timing-ws-open-ms` / `data-launch-timing-ws-close-ms` / `data-launch-timing-first-output-ms` (attributes on `production-terminal`) | Per-event relative-ms shortcuts for the three measurements smokes most commonly read. Empty string until the corresponding event lands. The lifetime_X_then_close verification (see § "Launch timing diagnostics" below) compares `data-launch-timing-ws-open-ms` against `data-launch-timing-ws-close-ms` and the backend nginx access-log timestamp. |
| `data-renderer` (attribute on `production-terminal`) | The renderer id the workspace actually mounted: `xterm`, `ghostty-web`, `restty`, `wterm`, or `unmounted` before the attach resolves. Use this — not visual cues — when proving renderer identity for a smoke row. |
| `data-renderer-experimental` (attribute on `production-terminal`) | `"true"` when the mounted renderer is experimental, `"false"` otherwise (including `unmounted`). |
| `data-renderer-fallback` (attribute on `production-terminal`) | Closed-vocabulary fallback taxonomy: `""` on the happy path, otherwise one of `experimental_gate_off` / `unknown_renderer_id` / `adapter_load_failed` / `adapter_mount_failed`. The first three are produced by `rendererLoader.ts`'s synchronous paths (gate, unknown id, dynamic-import / constructor failure) AND fall back to xterm with `data-renderer="xterm"`. `adapter_mount_failed` is produced by `ProductionTerminal.svelte`'s `mountRendererSafely` call when the renderer's asynchronous `mount(target)` rejects (e.g., CSP-blocked WASM init); the workspace stays `data-renderer="unmounted"` and surfaces the operator-facing copy `Renderer failed to mount. Switch back to xterm in Settings and reopen the terminal.` in `production-terminal-error`. A fallback row in the smoke entry MUST quote this attribute, not the workspace copy. |
| `data-renderer-gate` (attribute on `production-terminal`) | `"on"` when the operator's experimental-renderer-evaluation gate is enabled in Settings, `"off"` otherwise. Independent of which renderer ended up mounted. |
| `data-renderer-input` (attribute on `production-terminal`) | `"marked"` once the workspace has stamped the renderer-neutral input marker on the mounted renderer's keyboard-input element, `"none"` otherwise (renderer not mounted, mount failed, or the renderer does not implement the optional `focusTarget()` method — restty today). A renderer-evaluation smoke checks this is `"marked"` before relying on `[data-relayterm-terminal-input]` for Path A / Path C input. |
| `data-renderer-autofit` (attribute on `production-terminal`) | Closed-vocabulary diagnostic for the renderer-neutral autofit capability ([`docs/renderer-neutral-autofit.md`](../../docs/renderer-neutral-autofit.md)): `"off"` when the operator did not enable autofit in Settings (default for fresh users); `"active"` when autofit is enabled AND the mounted renderer wired it (`TerminalRenderer.autofitActive() === true` — xterm with its `ResizeObserver` + `FitAddon`, or wterm with its `WTerm.autoResize`); `"unsupported"` when autofit is enabled but the renderer no-ops it (`autofitActive()` is `false` / throws / the method is omitted, OR no renderer is mounted — ghostty-web / restty today). A renderer-evaluation matrix row that tests resize/fit MUST quote this attribute as proof of the autofit posture; a `"unsupported"` value is documented adapter behaviour for the experimental renderers without a container-fit path, not a regression. The companion staging resmoke (`docs/wterm-fit-reflow-resmoke`) precondition is `data-renderer-autofit="active"` for the wterm matrix row. |
| `[data-relayterm-terminal-input]` (attribute on a renderer-owned element) | Renderer-neutral marker on the element that actually receives keyboard input — xterm's hidden helper `<textarea>`, ghostty-web's contenteditable host element (which is also `production-terminal-viewport`; the marker is a dedicated attribute so it coexists rather than clobbers the testid), or wterm's hidden keyboard `<textarea>`. This is the single stable selector a smoke focuses + verifies (`document.activeElement`) for renderer-fair Path A / Path C input — see section D "Renderer-fair input". Stamped only after a successful mount; absent on the mount-failure path. |
| `[data-testid="production-terminal-focus"]`       | "Focus terminal" button (moves keyboard focus into the renderer via the renderer-neutral `focus()` method; enabled while live). Clicking it focuses `[data-relayterm-terminal-input]` for every renderer. |
| `[data-testid="production-terminal-fit"]`         | "Fit" button (refits the renderer to its container; the renderer's `onResize` listener drives the wire `resize` frame — the button does NOT call `client.sendResize`). Two new disabled states as of 2026-05-15 (`feat/renderer-neutral-autofit`): disabled with `title="Autofit is keeping the terminal sized to its container."` when `data-renderer-autofit="active"` (the one-shot button is redundant); disabled with `title="Fit is not supported by the current renderer."` when the mounted renderer exposes no `fit()` method (ghostty-web, restty, wterm today). The closed-vocabulary tooltips are pinned by `apps/web/tests/terminalLaunch.test.ts`. |
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
| `[data-testid="sessions-row-view-recording"]`     | Per-row "View recording" button (rendered for `detached` and `closed` rows only — `active` rows route to the live `Open` action, `starting` rows have nothing to replay; opens the read-only recording replay viewer in place of the navigation-selected view; the viewer's metadata gate honestly surfaces "No recording available" if the opened session has no chunk / marker rows). |
| `[data-testid="recording-replay-view"]`           | Read-only recording replay viewer root (carries `data-session-id` and `data-status` ∈ {`idle`,`loading_metadata`,`loading_chunks`,`ready`,`empty`,`error`,`decode_warning`}). Mounted by the AppShell when the operator clicks `sessions-row-view-recording`; replaces the navigation-selected view until cleared via `recording-replay-back` or any nav click. |
| `[data-testid="recording-replay-banner"]`         | Static replay-only banner — pins the contract that the viewer is recorded output, input was not recorded, the live SSH session cannot be resumed from a recording, and backend-restart recovery is not implemented yet. |
| `[data-testid="recording-replay-refresh"]`        | "Reload recording" button — re-fetches metadata, chunks, and markers; disabled while a load is in flight. |
| `[data-testid="recording-replay-back"]`           | "Back to sessions" button — clears `activeReplaySessionId` in the AppShell. |
| `[data-testid="recording-replay-loading"]`        | "Loading recording metadata…" status (rendered while metadata is in flight). |
| `[data-testid="recording-replay-loading-chunks"]` | "Loading recorded output…" status (rendered while chunks are paging in; reports the chunks-written running total — never the chunk bytes). |
| `[data-testid="recording-replay-error"]`          | Recording load error (safe formatter only — function of `kind`+`status`+`code`; never echoes wire `message`, `data_b64`, recording bytes, or vault / auth sentinels). |
| `[data-testid="recording-replay-empty"]`          | "No recording available" empty-state card (rendered when the metadata gate returns `has_recording == false`). |
| `[data-testid="recording-replay-decode-warning"]` | Decode-warning panel (one chunk had unsupported encryption / unsupported compression / invalid base64 / declared-length mismatch; the warning does NOT echo the chunk bytes). |
| `[data-testid="recording-replay-complete"]`       | "Replay complete" status (rendered when every chunk has been streamed into the read-only xterm). |
| `[data-testid="recording-replay-metadata"]`       | Metadata strip (`chunk_count`, `marker_count`, `first_seq`, `last_seq`, `first_recorded_at`, `last_recorded_at` — counts and seq bounds only, never bytes). |
| `[data-testid="recording-replay-viewport"]`       | Read-only xterm viewport — the only surface decoded chunk bytes reach. xterm is constructed with `disableStdin: true` and the viewer does NOT subscribe to `onInput`; keystrokes inside the viewport produce no input. |
| `[data-testid="recording-replay-markers"]`        | Markers strip (`<details>` block; rendered only when at least one marker exists). |
| `[data-testid="recording-replay-marker"]`         | One marker row (`data-marker-kind` ∈ {`started`,`attached`,`detached`,`reattached`,`resized`,`closed`,`replay_gap`}). The payload preview is a truncated JSON snippet of the opaque metadata payload — never PTY bytes by writer contract. |
| `[data-testid="recording-replay-about"]`          | "About replay" panel — pins the load-bearing copy (sensitive content, output-only, keystrokes not sent anywhere, recording bytes not persisted in browser storage). |
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
| `[data-testid="settings-experimental-renderer"]`  | Experimental renderer evaluation card root inside the Settings view. The card and its gate toggle are ALWAYS rendered when the Settings view is open; the warning copy, renderer radio group, and effective-renderer diagnostic only reveal when the gate toggle is flipped on. A smoke check that wants to assert "experimental renderer surface is gated" must check the radio testids (`renderer-option-<id>`) and `settings-experimental-renderer-warning`, NOT this card root. |
| `[data-testid="settings-experimental-renderer-toggle"]` | Gate toggle (checkbox). Off by default. Persists to localStorage as `experimentalRendererEvaluationEnabled`. Turning it OFF resets the persisted `rendererId` back to `xterm` so a stale experimental selection cannot survive a future gate flip. |
| `[data-testid="settings-experimental-renderer-warning"]` | Warning panel rendered ONLY when the gate is on (`role="alert"`). Static copy — does not echo any operator-supplied value. |
| `[data-testid="settings-renderer-selector"]`      | Renderer radio-group fieldset rendered ONLY when the gate is on. |
| `[data-testid="renderer-option-xterm"]`           | xterm baseline radio (default-checked; same selector name as the dev lab — both surfaces share the contract). |
| `[data-testid="renderer-option-ghostty-web"]`     | ghostty-web experimental radio (visible ONLY when the gate is on). |
| `[data-testid="renderer-option-restty"]`          | restty experimental radio (visible ONLY when the gate is on). |
| `[data-testid="renderer-option-wterm"]`           | wterm experimental radio (visible ONLY when the gate is on). |
| `[data-testid="settings-renderer-effective"]`     | Effective-renderer diagnostic strip rendered next to the radio group. Mirrors `effectiveRendererId(draft)` — when an experimental id is selected but the gate is off, surfaces "currently fall back to xterm." Useful for proving the gate logic in a smoke without launching a terminal session. |
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
| `[data-testid="renderer-option-xterm"]`           | xterm baseline radio (default-checked). Same selector also appears in the production Settings view when the experimental gate is on; the surface is disambiguated by the parent root (`xterm-live-terminal-lab` vs `settings-experimental-renderer`). |
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
If `writeText` rejects, fall back to dispatching a synthetic
`ClipboardEvent('paste', { clipboardData: dt, bubbles: true })` against
`document.querySelector('.xterm-helper-textarea')` after focusing it —
xterm subscribes to that element's `paste` event and `evaluatePaste`
runs on the same `onInput` payload regardless of paste source, so the
policy outcomes are identical.

**Bracketed-paste reality.** Once the remote shell turns on bracketed
paste mode (DECSET 2004) — fish, bash with readline, and zsh all do
this on startup — xterm wraps EVERY paste payload it forwards to
`onData` with the bracketed-paste markers `\x1b[200~ ... \x1b[201~`.
The pastePolicy classifies any paste containing those markers as
`confirm` with `reasonCode = "bracketed_paste_markers"`. That priority
sits above `multiline`, `control_chars`, and `large_payload` (see
`decidePaste` in `pastePolicy.ts`), so the panel reason for the
multiline / large / single-line payloads below will be
`bracketed_paste_markers` whenever the shell has bracketed paste on.
This is intentional — a paste against a non-bracketed-paste shell
(say, a raw `cat` redirecting stdin) returns to the `multiline` /
`large_payload` reasons. The smoke verifies the integration shape
(panel renders with the right reason for the wrapped payload, content
NOT surfaced, send/cancel/dismiss all wire correctly), not the
specific reason string for each paste size.

1. **Single-line paste — confirm panel renders, content redacted.**
   - Programmatically dispatch a paste of
     `"echo relayterm-single-line-smoke"` against the xterm helper
     textarea. Use `browser_evaluate`:

     ```js
     async () => {
       const ta = document.querySelector('.xterm-helper-textarea');
       ta.focus();
       const dt = new DataTransfer();
       dt.setData('text/plain', 'echo relayterm-single-line-smoke');
       ta.dispatchEvent(new ClipboardEvent('paste', { clipboardData: dt, bubbles: true, cancelable: true }));
     }
     ```

   - Assert the confirm panel renders with reason
     `bracketed_paste_markers` (the single-line payload is
     bracketed-paste-wrapped by xterm before reaching `evaluatePaste`),
     no blocked panel is present, and the sentinel string does NOT
     appear anywhere in the panel:

     ```js
     () => {
       const panel = document.querySelector('[data-testid="production-terminal-paste-confirm"]');
       const sentinel = 'relayterm-single-line-smoke';
       return {
         present: !!panel,
         reason: panel?.dataset.pasteReason,
         contentLeak:
           (panel?.textContent ?? '').includes(sentinel) ||
           (panel?.innerHTML ?? '').includes(sentinel),
         blockedAbsent: !document.querySelector('[data-testid="production-terminal-paste-blocked"]'),
       };
     }
     ```

     Expected: `present: true`, `reason: "bracketed_paste_markers"`,
     `contentLeak: false`, `blockedAbsent: true`. The heading is the
     static `Paste contains bracketed-paste markers.`, the meta carries
     a 1-line + byte-count line.

2. **Multiline paste — confirm panel renders with metadata only.**
   - Dispatch a paste of `"echo relayterm-multi-a\necho relayterm-multi-b\n"`
     the same way (synthetic ClipboardEvent against
     `.xterm-helper-textarea`).
   - Assert the confirm panel renders, the meta line shows the line
     count + byte length, and neither sentinel (`relayterm-multi-a`,
     `relayterm-multi-b`) appears anywhere in the panel `textContent`
     or `innerHTML`. (The reason will be `bracketed_paste_markers`
     when the remote shell has bracketed paste on; the panel renders
     identically either way.)
   - `browser_click [data-testid="production-terminal-paste-confirm-cancel"]`.
   - Re-snapshot: confirm panel is gone, neither sentinel appears in
     the terminal viewport rows (a confirmed send would render the
     pasted text as terminal echo).

3. **Multiline paste — Send forwards exactly once.**
   - Dispatch the same multiline paste again.
   - `browser_click [data-testid="production-terminal-paste-confirm-send"]`.
   - Press Enter (`browser_press_key Enter`) and observe the viewport:
     the remote shell echoes the pasted text exactly once and the
     confirm panel disappears.

4. **Large paste — confirm panel renders with metadata only.**
   - Dispatch a paste of `'a'.repeat(5000)` (above the 4 KiB confirm
     threshold, below the 64 KiB hard cap).
   - Assert the confirm panel renders, the meta line shows the byte
     length (≥ 5012 bytes once xterm's bracketed-paste markers are
     counted), and a long run of `a`s is NOT in the panel
     (`(panel.textContent ?? '').includes('aaaaaaaaaaaaaaaa')` must
     be `false`).
   - `browser_click [data-testid="production-terminal-paste-confirm-cancel"]`.

5. **Blocked paste — exceeds_hard_cap; Dismiss clears the panel.**
   - Dispatch a paste of `'a'.repeat(65 * 1024)` (above the 64 KiB
     hard cap; the hard cap rule sits ABOVE the bracketed-paste rule
     in `decidePaste`, so the reason here is `exceeds_hard_cap`).
   - Assert the BLOCKED panel renders with
     `data-paste-reason="exceeds_hard_cap"`, the meta line shows the
     dropped byte count, and the long run of `a`s is NOT in the panel.
     Confirm panel must be absent.
   - `browser_click [data-testid="production-terminal-paste-blocked-dismiss"]`.
   - Re-snapshot: blocked panel is gone. Terminal viewport is
     unchanged — nothing was sent to the remote shell.

6. **Blocked paste — nul_byte; same redaction posture.**
   - Dispatch a paste of `'echo relayterm-nul-paste\x00more'` — the
     embedded NUL byte (`\x00`) is what trips the rule, NOT the
     visible "nul" in the sentinel string. The `nul_byte` rule sits
     AHEAD of `bracketed_paste_markers` in `decidePaste`, so the
     reason here is `nul_byte` regardless of xterm wrapping. Assert
     the blocked panel renders with `data-paste-reason="nul_byte"`,
     the `relayterm-nul-paste` sentinel does NOT appear in the panel,
     and the meta line shows the dropped byte count.
   - `browser_click [data-testid="production-terminal-paste-blocked-dismiss"]`.

7. **Lifecycle teardown — pending paste clears on detach / close /
   disconnect / unmount.**
   - Trigger any confirm-risk paste (e.g. multiline) so a panel is
     present.
   - Click `[data-testid="production-terminal-detach"]` (or
     `-close` / `-dispose`). Assert the confirm panel is gone
     immediately. Reconnect (within `DETACHED_TTL_MS`) — the prior
     paste content must NOT auto-render in the viewport.

8. **Logout cleanup — workspace unmounts, no paste content survives.**
   - Trigger any confirm-risk paste so a panel is present.
   - `browser_click [data-testid="auth-sign-out"]`. Assert
     `[data-testid="auth-login-screen"]` re-renders, the production
     terminal element is gone, `document.cookie` no longer carries the
     session cookie, and `localStorage` / `sessionStorage` carry no
     paste sentinels.

9. **Console redaction.**
   - `browser_console_messages level=error all=true`.
   - Expected: no entries containing any paste body sentinel
     (`relayterm-single-line-smoke`, `relayterm-multi-*`,
     `relayterm-nul-paste`, the long `a` runs from steps 4–5). The
     favicon `404`, the initial `/api/v1/auth/me` 401 (pre-login), and
     a Vite WebSocket reconnect note are the only allowed entries.

10. **Audit-events redaction (backend cross-check).** Query the
    `audit_events` table directly and confirm no row carries any paste
    sentinel in `payload`:

    ```sql
    SELECT count(*) FROM audit_events
     WHERE payload::text ILIKE '%relayterm-multi%'
        OR payload::text ILIKE '%relayterm-nul%'
        OR payload::text ILIKE '%relayterm-single-line%';
    ```

    Expected: `0`. Matches the canonical rule in AGENTS.md "Don't put
    paste content in `audit_events.payload`".

If a live SSH target is unavailable, skip this section and re-run
after the backend gains one. The `pastePolicy.test.ts` unit tests pin
the underlying classification rules without a backend.

### B.2. Production recording replay smoke (requires a live backend with `terminal_recording.enabled = true`)

This step verifies the read-only recording replay viewer
(`apps/web/src/lib/app/views/RecordingReplayView.svelte`) end-to-end
against a real recorded session. It is **not** part of the production
prod-build smoke above because it requires:

  - the backend booted with
    `[terminal_recording] enabled = true` and `[terminal_recording.encryption] mode = "disabled"`
    (dev-only — operator accepts plaintext-at-rest),
  - a launchable SSH target (so chunks actually get written),
  - the operator running a brief session and closing it.

Skip if the local backend has no recording / no SSH target available.
The unit tests in `apps/web/tests/terminalRecordingsApi.test.ts` pin
the parser / decode / redaction rules without a backend.

Pre-conditions: at least one `terminal_session` row owned by the
authenticated user with at least one row in
`terminal_recording_chunks`. Easiest: launch a session, run
`echo replayterm-recording-smoke; date`, then click `End session`.

1. **Open the replay viewer from the Sessions list.**
   - `browser_click [data-testid="nav-sessions"]`
   - Expect `[data-testid="sessions-row"]` for the just-closed session
     with `data-status="closed"`.
   - On that row, expect `[data-testid="sessions-row-view-recording"]`
     (the affordance is offered for `detached` and `closed` rows only —
     `active` rows route to the live `Open` action, `starting` rows
     have nothing to replay; the viewer's metadata gate is the
     load-bearing check for whether any chunks actually exist).
   - `browser_click [data-testid="sessions-row-view-recording"]`
   - Expect `[data-testid="recording-replay-view"]` to appear with
     `data-session-id` matching the row's `data-session-id`.

2. **Replay-only banner is present.**
   - `[data-testid="recording-replay-banner"]` exists and the rendered
     text contains `Replay only`, `not connected to a live SSH session`,
     and `Input was not recorded`.

3. **Metadata strip and replay completion.**
   - Expect the viewer transitions through
     `data-status="loading_metadata"` → `loading_chunks` → `ready`
     (or `decode_warning` if a future chunk lands with unsupported
     encryption / compression).
   - `[data-testid="recording-replay-metadata"]` shows non-zero
     `chunk_count` and a numeric `first_seq` / `last_seq`.
   - `[data-testid="recording-replay-complete"]` is visible at the end
     of the load.
   - The xterm viewport `[data-testid="recording-replay-viewport"]`
     visibly contains the recorded output (`replayterm-recording-smoke`,
     the prompt, `date` output, etc.).

4. **Keyboard input is dropped.**
   - Focus the replay viewport and type a string the recorded shell
     never produced (e.g. `replayterm-typing-smoke`).
   - Re-snapshot the viewport text. Expected: the typed string does
     NOT appear in the viewport. xterm is constructed with
     `disableStdin: true` and the viewer never subscribes to
     `onInput`, so keystrokes produce no output and no wire send.
   - `browser_console_messages level=error all=true` shows no errors.

5. **Recording bytes are not in browser storage.**
   - `browser_evaluate`:

     ```js
     () => {
       const sentinel = 'replayterm-recording-smoke';
       const local = JSON.stringify({ ...localStorage });
       const session = JSON.stringify({ ...sessionStorage });
       return {
         localStorageHasSentinel: local.includes(sentinel),
         sessionStorageHasSentinel: session.includes(sentinel),
         localStorageHasDataB64Key: Object.keys(localStorage).some((k) => k.includes('recording') || k.includes('data_b64')),
       };
     }
     ```

   - Expected: every field is `false`. Decoded chunk bytes are streamed
     directly into xterm and are never persisted client-side.

6. **Empty-recording case (optional).**
   - If a session predates `terminal_recording.enabled = true`, click
     its `View recording` button and expect
     `[data-testid="recording-replay-empty"]` ("No recording available").

7. **Back to sessions clears the viewer.**
   - `browser_click [data-testid="recording-replay-back"]`
   - Expect `[data-testid="recording-replay-view"]` to disappear and
     `[data-testid="production-view-sessions"]` to be visible again.

8. **Backend audit / log leakage sweep.**
   - With backend SQL access:

     ```sql
     SELECT count(*) FROM audit_events
      WHERE payload::text ILIKE '%replayterm-recording-smoke%';
     ```

     Expected: `0`. Recording reads MUST write zero audit rows; the
     synthetic recording sentinel MUST NOT appear in any audit row
     (canonical rule: AGENTS.md "Don't put recording bytes in audit
     payloads").
   - The backend `tracing` logs from the same window MUST NOT contain
     the sentinel either. `journalctl` / the operator's log surface is
     the place to grep.

If a live recording is unavailable, skip this section. The unit tests
already pin the parser / decode / redaction rules; the smoke verifies
the integration path against real chunk material.

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

### D. Renderer evaluation smoke (requires staging, requires a throwaway SSH target)

This step exists to give a future operator (human or agent) a single
repeatable procedure for measuring a candidate renderer (xterm baseline
OR one of the experimental adapters `ghostty-web` / `restty` / `wterm`)
against the same evaluation-matrix rows the 2026-05-13 xterm baseline
staging smoke (see
[`docs/deployment/vps-staging-smoke.md`](../../docs/deployment/vps-staging-smoke.md)
§ "2026-05-13 · Xterm production-baseline renderer smoke") established
AND the four rows it deliberately deferred (Unicode / box drawing /
wide chars; copy-paste round-trip; alternate-screen; mouse mode).

The design doc that motivates this section — input-path taxonomy, what
each path proves, command matrix, and recommended fixture commands —
lives in
[`docs/renderer-smoke-harness.md`](../../docs/renderer-smoke-harness.md).
Treat that doc as the source of truth for "why this row uses this
input path"; this section is the runbook that turns it into a smoke
procedure.

#### Purpose

- Establish repeatable manual / Playwright-MCP renderer smoke steps so a
  ghostty-web / restty / wterm candidate can be compared against the
  xterm baseline on the same dimensions, in the same order, against the
  same throwaway target.
- Carry forward the 2026-05-13 baseline's deferred matrix rows (Unicode,
  paste, alternate-screen, mouse) without weakening any
  architecture / redaction / CSRF rule.
- Do **not** infer renderer behavior from visual appearance alone; every
  row records the input path it used so the result can be replayed.

xterm is and remains the production compatibility baseline and the
default renderer per
[`docs/terminal-renderer-evaluation.md`](../../docs/terminal-renderer-evaluation.md).
A run of this smoke is one human-evaluator pass through the matrix; it
is **not** a Gate-2 promotion decision (see "Explicit non-goals"
below).

#### Pre-conditions

- A reachable RelayTerm staging URL (the canonical staging stack is
  `https://relayterm-staging.js-node.cc`; private deployments may
  differ).
- A staging-only smoke user (created via the production sign-in flow on
  the staging stack; **never** a production account). Sign-in posture
  follows section C of this document.
- A throwaway internal-only SSH target named
  `relayterm-staging-<smoke-id>-ssh` attached only to the internal
  Compose network, no host port published. The hermetic-target pattern
  matches the 2026-05-13 baseline smoke entry; do **not** point the
  smoke at a real internal service.
- A throwaway SSH identity (generated backend-side OR imported via the
  base64-sidecar + `atob` inside a single `page.evaluate` pattern
  documented in the 2026-05-13 baseline smoke). The PEM bytes never
  appear in any MCP tool-call payload, Error, log, audit row, or DOM
  string. Public-key bytes are also not recorded in the smoke entry
  (per the redaction rules for `audit_events.payload`; the smoke entry
  mirrors that posture).
- No production credentials. No personal / private SSH keys. No
  reusable passwords.
- Backend and web container images at the digests pinned in the smoke
  entry's "Stack pin" line (record both `sha256:<digest>` values). If
  the digests are stale, refresh the staging stack before starting.
- Surface: desktop browser is the required pass; Tauri desktop and
  Android WebView are optional add-ons per the renderer evaluation
  plan's "Surfaces" list. Each optional surface adds a separate smoke
  entry, not extra rows on the browser entry.

#### Renderer path confirmation

Before running any row, record exactly which renderer is mounted —
visual cues alone (cursor shape, glyph appearance, scrollbar style) are
**not** sufficient.

- **xterm baseline.** Read the `data-renderer` attribute on
  `[data-testid="production-terminal"]` (should be `xterm`) and the
  visible renderer diagnostic at
  `[data-testid="production-terminal-renderer-diagnostic"]` (should
  contain the literal string `xterm baseline`). The
  `data-renderer-experimental` attribute should be `"false"` and
  `data-renderer-fallback` should be the empty string. xterm is the
  default; on a fresh browser the workspace mounts xterm without any
  operator action.
- **Experimental candidates.** Enable the operator gate via the
  Settings view BEFORE launching the smoke session:
  1. `browser_click [data-testid="nav-settings"]`.
  2. Confirm `[data-testid="settings-experimental-renderer"]` is
     present.
  3. `browser_click
     [data-testid="settings-experimental-renderer-toggle"]` to flip
     the gate on; confirm
     `[data-testid="settings-experimental-renderer-warning"]` is now
     rendered (the warning copy is static).
  4. `browser_click [data-testid="renderer-option-<id>"]` for the
     candidate (`ghostty-web` / `restty` / `wterm`).
  5. `browser_click [data-testid="settings-apply"]` to persist the
     selection; confirm `[data-testid="settings-status-saved"]`.
  6. Launch a new terminal session as per the regular smoke flow.
     After the workspace mounts, assert:
     - `data-renderer` on `production-terminal` equals the candidate id.
     - `data-renderer-experimental` is `"true"`.
     - `data-renderer-fallback` is the empty string.
     - `data-renderer-gate` is `"on"`.

  If `data-renderer-fallback` is non-empty
  (`experimental_gate_off` / `unknown_renderer_id` /
  `adapter_load_failed` / `adapter_mount_failed`), the workspace did
  not mount the candidate. The first three fall back to xterm
  (`data-renderer="xterm"`) — record the fallback reason verbatim,
  do not infer candidate behavior, and either remediate (re-enable
  the gate; pick a different candidate; re-check the production
  build) or mark every subsequent row `deferred — renderer fell back
  to xterm`. `adapter_mount_failed` is the asynchronous-mount-failure
  signal added 2026-05-13 after the ghostty-web staging smoke surfaced
  a CSP/WASM wedge; on this value the workspace stays
  `data-renderer="unmounted"` AND surfaces the
  `production-terminal-error` panel with `Renderer failed to mount.
  Switch back to xterm in Settings and reopen the terminal.` Mark
  every subsequent row `deferred — renderer not identified` and
  remediate per the workspace copy (Settings → xterm → relaunch).

- **Cleanup.** At smoke end, re-open Settings, flip the gate OFF, save.
  The toggle's onChange handler also resets the persisted renderer
  back to xterm so a future smoke against this browser starts clean.
  Confirm `data-renderer` on the next session is `xterm` and
  `data-renderer-gate` is `"off"`.

If renderer identity cannot be proven for a given run, mark every row
as `deferred — renderer not identified` and stop.

#### Renderer-fair input (load-bearing)

Before running any Path A / Path C row, focus the terminal through the
renderer-neutral affordance and **verify** focus landed. This step
exists because the renderers disagree on which DOM element receives
keyboard input:

- **xterm** routes keystrokes through a hidden helper `<textarea>` that
  is a child of the viewport element.
- **ghostty-web** makes the viewport element itself `contenteditable`
  and attaches its keydown listener there — its hidden `<textarea>` is
  for IME / composition / paste only.
- **wterm** appends a hidden keyboard `<textarea>` to the host element
  and attaches its keydown listener there; `focusTarget()` reports that
  textarea.
- **restty** is deferred (see "Explicit non-goals") and may differ
  again — its adapter does not implement `focusTarget()` yet.

The 2026-05-14c ghostty-web production-shell smoke could not drive
input past the first keystroke because the runbook had no
renderer-neutral selector for "the element a real keystroke hits" — it
was guessing between the viewport DIV and a per-renderer helper
textarea. The workspace now resolves that: after a successful mount it
stamps the marker attribute `[data-relayterm-terminal-input]` on
whichever element the renderer's `focusTarget()` reports, and reflects
`data-renderer-input="marked"` on `production-terminal`.

**Renderer-fair focus procedure (run once per attach, and again after
any detach / reconnect):**

1. Confirm `production-terminal` carries `data-renderer-input="marked"`.
   If it is `"none"`, the mounted renderer did not expose a stable
   input target (restty today, or a mount failure) — mark every
   Path A / Path C row `deferred — renderer input target unavailable`
   and skip to the redaction sweep.
2. Focus the terminal. Two renderer-neutral ways, both acceptable —
   prefer the button (it routes through the renderer's own `focus()`,
   which is the path a real operator hits):
   - `browser_click [data-testid="production-terminal-focus"]` (the
     "Focus terminal" button — calls `renderer.focus()`, which focuses
     the same element `focusTarget()` reported), or
   - focus `[data-relayterm-terminal-input]` directly via the MCP
     focus / click primitive. This bypasses the renderer's `focus()`
     side effects (e.g. scroll-to-cursor) — fine for driving input,
     but the button is the truer operator path.
   Do **not** click the bare `[data-testid="production-terminal-viewport"]`
   element — for xterm that focuses the host DIV, not the helper
   textarea, and the first keystroke will not reach the renderer.
3. Verify focus landed on the renderer's input element:

   ```js
   () => {
     const target = document.querySelector('[data-relayterm-terminal-input]');
     return {
       hasTarget: !!target,
       focused: target !== null && document.activeElement === target,
     };
   }
   ```

   Expect `{ hasTarget: true, focused: true }`. If `focused` is
   `false`, re-run step 2 once; if it still fails, mark the affected
   rows `deferred — renderer focus could not be verified` rather than
   recording an unreviewable input result.
4. Only now drive Path A keystrokes (`browser_press_key` /
   `browser_type`) or a Path C trusted Ctrl+V. They reach the renderer
   because `document.activeElement` is the verified input element.
5. The marker attribute carries **no payload bytes** — it is a fixed
   boolean marker. Never read the input element's `value` /
   `textContent`; input bytes are observed only as viewport output
   round-trips, exactly as the command matrix specifies.

#### Input-path rules (load-bearing)

Each command-matrix row below labels the input path it requires. The
labels match
[`docs/renderer-smoke-harness.md`](../../docs/renderer-smoke-harness.md)
§ "Input-path taxonomy":

- **Path A (Playwright `keyboard.press` / `keyboard.type` — ASCII).**
  Trusted keyboard events via MCP `browser_press_key` / `browser_type`.
  This is the path the 2026-05-13 baseline used and is what every
  ASCII command in the matrix below uses.
- **Path C (clipboard write + trusted Ctrl+V).** Requires a one-time
  `clipboard-read` / `clipboard-write` permission grant for the test
  browser context (see "Clipboard permission step" below). Drives the
  renderer's real `paste` event handler.
- **Path D (remote-shell-generated output).** Run a non-paste shell
  command via Path A; the **output** bytes are what is under test. Used
  for Unicode / box-drawing / wide-char / emoji output, the
  alternate-screen enter / leave transition, and the
  mode-enable half of mouse-mode probes.
- **Path E (direct WebSocket-client injection).** Backend-side only.
  **Never** counted as a renderer-matrix row. If a future operator
  wants to run a wire / replay regression check, record it as a
  **separate** smoke entry (its own dated block in
  `docs/deployment/vps-staging-smoke.md`); do not fold its results
  into a renderer-evaluation matrix row.
- **Path I (dev-only inject route).** Explicitly rejected per the
  harness plan; not part of this runbook at any tier. If a slice
  proposes such a route, refer it back to
  [`docs/renderer-smoke-harness.md`](../../docs/renderer-smoke-harness.md)
  § "I. Dev-only test-harness route (rejected)" before approving.

#### Clipboard permission step (one-time per smoke)

Paste-row coverage (Path C) requires the browser context to hold
`clipboard-read` and `clipboard-write` permissions. The smoke harness
grants them; the production `apps/web` bundle does not request them at
startup (current behavior — unchanged by this runbook).

1. Inside `browser_evaluate`, call:

   ```js
   () =>
     navigator.permissions
       .query({ name: 'clipboard-write' })
       .then((status) => status.state);
   ```

   - If the result is `"granted"`, proceed to the paste rows.
   - If the result is `"prompt"` or `"denied"` AND the MCP browser
     context exposes a permission-grant primitive, grant
     `["clipboard-read", "clipboard-write"]` scoped to the staging
     origin and re-query.
   - If the MCP browser cannot grant the permission, **defer** every
     Path-C row (paste safe / paste confirm / paste blocked) as
     `deferred — clipboard permission unavailable`. Do **not** fall
     back to a synthetic `ClipboardEvent` dispatch — the harness plan
     rejects that path for renderer-fairness reasons (synthetic events
     carry `isTrusted === false` and may be dropped by some adapters).
     B.1's synthetic-dispatch fallback is a **paste-policy**
     integration check, not a renderer comparison.

2. Once granted, every paste-row payload is constructed inside a
   single `browser_evaluate` from a local fixture string and written
   via `navigator.clipboard.writeText(payload)`. The paste payload
   itself **never** transits an MCP tool-call argument or return
   value: the call returns only "wrote N bytes — `ok`," not the body.

3. Never paste real secrets, real private keys, real passwords, or
   any production-shaped string into the clipboard. Use the sentinel
   strings from the command matrix below.

#### Command matrix

Each row records: input path, fixture command (illustrative — operator
may substitute), the unique ASCII sentinel to look for, and the
expected renderer behavior. Sentinels are the only ASCII recorded in
the smoke entry; non-ASCII bytes (Unicode glyphs, box-drawing chars,
emoji) are treated as opaque per
[`docs/renderer-smoke-harness.md`](../../docs/renderer-smoke-harness.md)
§ "Security / redaction rules".

The sentinel strings below intentionally diverge from the illustrative
sentinels in
[`docs/renderer-smoke-harness.md`](../../docs/renderer-smoke-harness.md)
§ "Command matrix" — the plan calls those examples illustrative and
non-normative. The runbook's strings are chosen so each row's sentinel
is a one-shot unique grep target (and so the redaction-sweep SENTINELS
list below covers every smoke-run echo).

1. **Basic ASCII I/O — Path A.** Run the "Renderer-fair input"
   procedure above first (focus + verify `document.activeElement` is
   `[data-relayterm-terminal-input]`), then type each and confirm
   round-trip:

   ```sh
   echo relayterm-renderer-baseline
   whoami
   pwd
   uname -a
   ```

   Sentinel: `relayterm-renderer-baseline`. Expected: every command's
   stdout renders in the viewport; no garbled bytes; no MCP-side
   `Input.dispatchKeyEvent` errors.

2. **Resize / fit — Path A + viewport handle.**

   ```sh
   stty size
   printf 'cols-test:%*sEND\n' 80 ''
   ```

   Resize the browser viewport (e.g. 1440 × 900 → 1024 × 768) and
   click `[data-testid="production-terminal-fit"]`. Re-run `stty size`
   and verify the new rows / cols values match the renderer's reported
   geometry. Confirm `session_events.resized` rises by exactly one row
   per resize (operator-side DB check; **never** include row payload
   in the smoke entry).

   Renderer-fairness note: `fit()` is an xterm-specific capability —
   the production "Fit" control routes through `safeFit()`, which
   probes for it at runtime. As of 2026-05-15
   (`feat/renderer-neutral-autofit`) the workspace's
   `computeFitButtonState` helper **disables** the Fit button with the
   closed copy `"Fit is not supported by the current renderer."` when
   the mounted renderer exposes no `fit()` method (ghostty-web, restty,
   wterm today). Do **not** click the disabled button; note the
   disabled state + tooltip text as **documented adapter behavior** —
   record it as `works with caveats`, not `fail`. A `fail` is reserved
   for a renderer that *claims* fit/resize and gets it wrong. See
   [`docs/spec/terminal-adapters.md`](../../docs/spec/terminal-adapters.md)
   § "Production-shell evaluation status and resize/fit caveat" and
   [`docs/renderer-neutral-autofit.md`](../../docs/renderer-neutral-autofit.md)
   § 9 for the precedence rules.

   **Autofit precondition (added 2026-05-15,
   `feat/renderer-neutral-autofit`).** The renderer-neutral autofit
   capability ships **off by default**, so a default-Settings smoke
   continues to observe the xterm-only Fit semantics above. For a
   resmoke that wants to prove a renderer reflows under a narrowed
   container (the deferred `docs/wterm-fit-reflow-resmoke` slice in
   particular), the operator must first enable
   `[data-testid="settings-autofit-enabled"]` in Settings, reopen the
   session, AND verify the workspace carries
   `data-renderer-autofit="active"` before doing the
   resize-and-stty-size check. A `data-renderer-autofit="unsupported"`
   value records that the mounted renderer no-ops autofit honestly
   (ghostty-web, restty today) — record as `works with caveats`, not
   `fail`. The Fit button is intentionally disabled with the closed
   copy "Autofit is keeping the terminal sized to its container." when
   `autofitActive()` is true; an enabled Fit button on
   `data-renderer-autofit="active"` would be a regression.

3. **Long output — Path A.**

   ```sh
   seq 1 300
   echo relayterm-after-long-output
   ```

   Sentinel: `relayterm-after-long-output`. Expected: all 300 lines
   render; the post-`seq` echo round-trips cleanly; renderer's
   scrollback contains the burst.

4. **Unicode / CJK output — Path D.**

   ```sh
   printf 'unicode: café Ω λ 🚀\n'
   ```

   Sentinel: `unicode:` prefix only. The glyph bytes are opaque to the
   smoke entry. Expected: characters render at the correct cell width
   (full-width CJK / emoji take two cells; combining accent on `é`
   renders as one glyph or is documented as not supported on this
   renderer). Record honestly — emoji-with-variation-selector glyph
   fallback is a `works with caveats`, not a `regression`.

5. **Box drawing — Path D.**

   ```sh
   printf 'box: ┌─┬─┐\nbox: │a│b│\nbox: └─┴─┘\n'
   ```

   Sentinel: `box:` prefix only. Expected: the three lines align
   column-for-column at the right cell width; no gaps; no spillover
   into adjacent cells.

6. **Wide chars — Path D.**

   ```sh
   printf 'wide: コンニチハ\n'
   ```

   Sentinel: `wide:` prefix only. Expected: each fullwidth katakana
   character occupies two columns; the prompt that follows lands at
   the correct column. If the renderer collapses wide chars to one
   column, record as `regression vs. baseline` for this row.

7. **Paste — safe sentinel (Path C, clipboard required).**

   Build the paste payload as a multi-line block. Constructed in a
   single `browser_evaluate`:

   ```sh
   echo relayterm-paste-1
   echo relayterm-paste-2
   ```

   Run the "Renderer-fair input" procedure above to focus + verify
   `[data-relayterm-terminal-input]`, then dispatch a trusted
   `Control+V`. Expect
   the production paste-safety pipeline to fire: if the remote shell
   has bracketed paste on (fish / bash with readline / zsh do by
   default — see section B.1 "Bracketed-paste reality") the panel
   reason will be `bracketed_paste_markers`; if the shell has it off,
   the reason will be `multiline`. Either way, expect
   `[data-testid="production-terminal-paste-confirm"]` to render with
   `data-paste-reason` set and **no** paste body in the panel text or
   HTML. Record the panel reason + line count + byte length only —
   **never** the body. Click
   `[data-testid="production-terminal-paste-confirm-send"]` to
   complete the round-trip; the two sentinels (`relayterm-paste-1`,
   `relayterm-paste-2`) MUST appear in the viewport after Send. If
   the renderer's own `paste` handler is bypassed (e.g. an experimental
   renderer that does not subscribe to a real `paste` event), record
   as `regression vs. baseline` — paste behavior is a renderer
   correctness property here.

8. **Paste — blocked sentinel (Path C, optional).**

   Construct a paste of three concatenated parts: the ASCII string
   `relayterm-x`, a single NUL byte (`\x00`), and the ASCII string
   `y`. Write it via
   `navigator.clipboard.writeText('relayterm-x' + '\x00' + 'y')`
   inside a single `browser_evaluate` (the literal NUL is what trips
   the `nul_byte` rule; the surrounding ASCII is the redaction
   sentinel). Dispatch the trusted Ctrl+V. Expect
   `[data-testid="production-terminal-paste-blocked"]` with
   `data-paste-reason="nul_byte"`, no `safe` send, and the
   `relayterm-x` sentinel does **not** appear in the viewport. Record
   metadata only — line count + byte length +
   `data-paste-reason` only; **never** the body (same redaction
   posture as the paste-policy integration rows in section B.1 above).

9. **Alternate screen — Path D (minimal probe).**

   ```sh
   tput smcup
   printf 'alt-screen-probe\n'
   sleep 1
   tput rmcup
   ```

   Sentinel: `alt-screen-probe`. Expected: the renderer switches to
   the alternate screen; `alt-screen-probe` renders inside it; on
   `tput rmcup` the cursor returns to the pre-`smcup` cell and the
   prior viewport is restored. The full-screen-app row (`htop` /
   `vim` / `less`) stays partially deferred per the harness plan
   until a target image with the larger tooling set is pinned;
   record those as `deferred — fixture absent`. Do **not** use vim /
   htop as the minimal-probe fixture.

10. **Mouse mode enable (output half only) — Path D.**

    ```sh
    printf '\e[?1000h'
    sleep 1
    printf '\e[?1000l'
    ```

    Expected: the renderer enters mouse-tracking mode (visible
    indicator depends on renderer — e.g. xterm stops auto-scrolling on
    select). Record the mode-input half (clicks translating to wire
    `Input`) as `deferred — fixture absent` until a purpose-built
    click-coordinate fixture lands; this row is the **mode-enable**
    half only per the harness plan.

11. **Detach / reconnect / replay — Path A + production buttons.**

    ```sh
    echo relayterm-before-detach
    ```

    Click `[data-testid="production-terminal-detach"]`. Wait inside
    the configured `DETACHED_LIVE_PTY_TTL` (default 30 s). Click
    `[data-testid="production-terminal-reconnect"]` (or re-enter via
    the Sessions list). Once attached, type:

    ```sh
    echo relayterm-after-reconnect
    ```

    Sentinels: `relayterm-before-detach`, `relayterm-after-reconnect`.
    Expected: the post-reattach echo round-trips; the session UUID is
    the same row in `terminal_sessions`. Renderer-side scrollback
    parity across reattach is a **separate property** the 2026-05-13
    baseline does not claim and this runbook does not assert — record
    visible scrollback state honestly ("renderer remounted; viewport
    empty until new output" is the baseline behavior for xterm).

12. **Narrow / mobile viewport — Path A + viewport handle.**

    Resize the browser viewport to roughly `390 × 844` (iPhone-class
    portrait) and click
    `[data-testid="production-terminal-fit"]`. Type:

    ```sh
    echo relayterm-mobile-width-renderer
    ```

    Sentinel: `relayterm-mobile-width-renderer`. Expected: the
    workspace stays usable at the narrow width and the echo
    round-trips with no MCP / renderer error. Reflow of the prior
    scrollback to the narrower width is **renderer-dependent** — xterm
    reflows via its `fit()` path; a renderer without an xterm-style
    `fit()` (ghostty-web today) keeps its mounted column count and
    clips long lines at the canvas edge rather than rewrapping. Record
    the observed reflow / clip behavior honestly; for a renderer
    without `fit()` the non-reflow is `works with caveats`, not a
    `regression vs. baseline` — see
    [`docs/spec/terminal-adapters.md`](../../docs/spec/terminal-adapters.md)
    § "Production-shell evaluation status and resize/fit caveat".

#### Clipboard permission deferral note

If section "Clipboard permission step" could not grant the permission
on this MCP setup, rows 7 and 8 are marked `deferred — clipboard
permission unavailable`. A `deferred` row is **not** a renderer
regression and is **not** a smoke failure — it is an input-path
limitation. Re-run those rows when the permission becomes grantable
(or when a future operator runs a small scripted Playwright wrapper
per
[`docs/renderer-smoke-harness.md`](../../docs/renderer-smoke-harness.md)
§ "Option A — runbook + permission-grant note").

#### Recording results

For every row, record in the staging-smoke entry:

- **Status.** One of `pass` / `fail` / `deferred`. `fail` requires a
  matching row in the classification table below to attribute the
  failure.
- **Surface.** `browser` / `desktop-tauri` / `android-tauri`. One row
  set per surface; browser is required, the Tauri surfaces are
  optional add-ons that record in their own dated blocks.
- **Renderer.** Which renderer was mounted (`xterm` baseline OR the
  experimental adapter id `ghostty-web` / `restty` / `wterm`). Match
  the path-confirmation step above; never infer from glyphs.
- **Observed rows / cols.** Where the row exercises geometry (resize,
  narrow viewport), record the `stty size` output. Not the renderer's
  internal width — the PTY-reported width.
- **Visual notes.** Brief, factual, no overclaim. "All 300 lines
  rendered; tail visible" is fine. "Looks identical to xterm" without
  a side-by-side is not — record what you actually observed.
- **Screenshots.** Optional. If included, MUST NOT contain paste
  bodies, real host data, real usernames, or any secret. Capture the
  renderer viewport only, not surrounding shell chrome.
- **Input path.** Tag every row with the input path it used (A / C /
  D). A row recorded without an input path tag is unreviewable.

#### Redaction sweep (mandatory final step)

Before closing the smoke entry, run a DOM scan and a console scan and
record zero matches outside the terminal viewport for these
sentinels. The list mirrors the 2026-05-13 baseline; do not soften.

```js
() => {
  const SENTINELS = [
    'private_key_openssh',
    'encrypted_private_key',
    'BEGIN OPENSSH PRIVATE KEY',
    'openssh-key-v1',
    'passphrase',
    'session_token',
    'token_hash',
    'cookie',
    'password',
    'data_b64',
    'REDACT-MARKER',
    'relayterm-paste-1',
    'relayterm-paste-2',
    'relayterm-x',
  ];
  const html = document.documentElement.outerHTML;
  return SENTINELS.map((s) => ({ s, found: html.includes(s) }));
};
```

Expected: zero `found: true` rows for the auth / key / session
sentinels; the paste sentinels are allowed inside the terminal
viewport `[data-testid="production-terminal-viewport"]` only after a
successful Path-C send, and **never** inside a paste-policy panel
(`production-terminal-paste-confirm` / `production-terminal-paste-blocked`).

Add the smoke-run's own ASCII sentinels (`relayterm-renderer-baseline`,
`relayterm-after-long-output`, `alt-screen-probe`,
`relayterm-before-detach`, `relayterm-after-reconnect`,
`relayterm-mobile-width-renderer`) to the SENTINELS list and re-run;
they are allowed inside the terminal viewport only. If the renderer
under test introduced its own row sentinels, append those too.

Run the equivalent sweep against `browser_console_messages
level=error all=true`; only the favicon `404` and the initial
`/api/v1/auth/me` 401 (pre-login) are allowed.

#### Cleanup

- If the smoke profile carries any terminal history rows that contain
  the smoke's ASCII sentinels in audit-visible columns, **disable**
  the profile rather than delete it (per the inventory lifecycle
  policy in [`SPEC.md`](../../SPEC.md) "Inventory lifecycle and
  destructive-action policy"; canonical pattern in
  [`docs/spec/inventory.md`](../../docs/spec/inventory.md)).
- `docker stop` and `docker rm` the throwaway SSH target. The
  container name pattern is `relayterm-staging-<smoke-id>-ssh` so a
  later operator can grep / clean stragglers safely.
- Leave the staging stack running. The smoke does not bring the stack
  up or down on its own.
- **Do not** manually delete rows from `terminal_sessions`,
  `session_events`, `known_host_entries`, or `audit_events`. The
  retention purge primitive
  ([`docs/agent/redaction-rules.md`](../../docs/agent/redaction-rules.md)
  § 12) is the only sanctioned writer to those tables; manual deletes
  bypass audit and are explicitly prohibited.
- Shred any local key material (smoke key generated browser-side
  base64-sidecar pattern), and confirm the host filesystem has no
  copy of the OpenSSH PEM body.

#### Result classification

Every failed row maps to exactly one of these classes. A `fail`
without a class is an unreviewable smoke entry.

- **Renderer issue.** The renderer mis-rendered output it received
  correctly on the wire. Example: wide chars collapsed to one column,
  box-drawing gaps, paste handler did not fire on a trusted Ctrl+V.
  This is the class that motivates a renderer-fairness verdict.
- **Smoke harness limitation.** The MCP / browser environment could
  not exercise the row, but a real user would not hit it. Example:
  clipboard permission could not be granted in this MCP setup.
  Recorded as `deferred — <reason>`, not `fail`.
- **Backend / session issue.** The orchestrator, session, replay
  ring, or detach TTL behaved incorrectly. Example: reconnect outside
  the TTL did not surface `replay_window_lost`; resize did not write
  exactly one `session_events.resized` row. Open a separate slice;
  do **not** roll into a renderer verdict.
- **Staging deploy issue.** The staging stack itself misbehaved
  (image digest mismatch, nginx cache hit on stale `/assets/*`,
  CSRF allowed-origin mismatch). Reference the 2026-05-09 and
  2026-05-11 Encountered Lessons in AGENTS.md before re-classifying.
- **Input permission issue.** Clipboard permission, `isTrusted`,
  IME composition surface, etc. Same posture as "Smoke harness
  limitation" but specifically about the input-path layer per
  [`docs/renderer-smoke-harness.md`](../../docs/renderer-smoke-harness.md)
  § "Input-path taxonomy".
- **Deferred row.** Fixture intentionally not available (e.g. tmux
  for the in-target driver path, a click-coordinate fixture for the
  mouse-input half). Record once per fixture, do not re-classify on
  every run.

#### Mobile smoke methodology (Playwright-first; real-phone narrow)

The 2026-05-16 methodology update added to
[`docs/wterm-mobile-smoke-plan.md`](../../docs/wterm-mobile-smoke-plan.md)
§ 5 ("2026-05-16 methodology update — Playwright-first execution
model, real-phone narrow scope") is the source of truth; this
subsection is its operator-runbook surface. Treat the plan as
authoritative on row-channel mapping and the closed real-phone
list; treat this subsection as the per-row checklist a smoke
operator works through.

The slice that surfaced the methodology was the
`docs/mobile-smoke-methodology-update` docs slice; the dated
entries it sits on top of are the 2026-05-15c surface-2 wterm
smoke and the 2026-05-16 xterm-control resmoke in
[`docs/deployment/vps-staging-smoke.md`](../../docs/deployment/vps-staging-smoke.md).

##### Default channel: Playwright mobile emulation

**Surface-1 narrow-viewport is NOT surface-2 Android Chrome.**
Playwright mobile emulation at an Android-class viewport is the
default execution channel for browser-automatable rows, but it
does not prove anything about real Android touch, soft keyboard,
IME, OS clipboard / paste UI, native selection handles, Android
back gesture, real orientation events, or Chrome tab / session
lifecycle. Those rows need a phone (see "Real-phone narrow
scope" below). The plan calls this out in `docs/wterm-mobile-smoke-plan.md`
§ 4 #1 and § 5; this subsection inherits that distinction.

For every row of `docs/wterm-mobile-smoke-plan.md` § 5 that does
not appear on the real-phone list below, drive the row from a
desktop browser at an Android-class viewport via Playwright (MCP
if a human operator is supervising; a committed runner is still
deliberate-later per
[`docs/renderer-smoke-harness.md`](../../docs/renderer-smoke-harness.md)
§ "Option B").

- Preferred viewport: roughly `1080 × 2340` for parity with the
  Samsung Galaxy S10e the 2026-05-15c / 2026-05-16 entries used,
  OR `390 × 844` (iPhone-class portrait) for parity with the
  2026-05-15 autofit resmoke. Pick one per smoke; record which.
- Focus via the renderer-fair input seam already documented in §
  D → "Renderer-fair input" (`production-terminal-focus` button
  →  `[data-relayterm-terminal-input]` → verify
  `document.activeElement` is the marked input). Never click the
  bare `production-terminal-viewport` element (the 2026-05-14
  ghostty-web lesson).
- Use server-side logs and `data-*` attribute reads as the
  load-bearing evidence (see "Target / log inspection
  checklist" below). Phone screenshots are NOT a substitute and
  are not committed to the repo by default.

##### Real-phone narrow scope

A real Android device is the right surface only for rows
whose evidence depends on a property that desktop emulation
cannot exhibit. Closed list (matches the plan):

- soft-keyboard open / close + `visualViewport` shrink (rows 4
  / 5);
- ASCII input through the OS soft keyboard (row 6, soft-keyboard
  half only);
- modifier-key affordances (row 7) — Android IME-dependent;
- paste flow (row 8) — Android Chrome's clipboard permission
  posture + the OS paste UI;
- copy / select flow (row 9) — native selection handles + the OS
  context menu;
- touch ergonomics on the workspace nav / control row (row 13's
  ergonomics half only);
- orientation change semantics on an active session (row 14);
- Android back gesture / button (row 15);
- Chrome tab / session backgrounding behaviour (also row 15).

Every other row defaults to Playwright emulation. Row 17
(xterm control comparison) mirrors whichever channel its
parent row uses.

##### Operator prompt template (for real-phone rows only)

When a row needs a phone, pre-stage one-instruction +
one-question prompts with a closed-vocabulary response set.
The operator never retypes a multi-line command. Examples
(reproduced from the plan; record the exact wording in the
dated smoke entry):

- "Tap the terminal area. Did the on-screen keyboard rise?
  (yes / no / partially)"
- "Paste this one-line command from clipboard (operator
  receives the literal command pre-staged in their clipboard
  buffer). Did anything appear? (yes — paste-confirm panel /
  yes — direct insertion / no)"
- "Long-press the highlighted text. Did the OS selection
  handles appear? (yes / no / partial)"
- "Press the Android back gesture once. What happened?
  (stayed on the workspace / left the workspace / the session
  disconnected / Chrome navigated away)"
- "Rotate to landscape. Is the input area still visible above
  the keyboard? (yes / no / no keyboard visible)"
- "Switch apps, then come back. Does Chrome show the session
  still connected? (yes / no / shows 'detached (TTL window)'
  / shows a disconnect banner)"

If the operator needs to read back a `data-*` attribute, a
`localStorage` key, or a console message, the row is in the
wrong channel — promote it to Playwright emulation, **OR** drive
the real phone via the USB-DevTools / CDP read channel
described in the next subsection. The 2026-05-15c "uiautomator
cannot read DOM" lesson still holds; what changed in 2026-05-16e
is that Chrome DevTools USB attach is now a verified real-phone
read path that bypasses uiautomator entirely.

##### Real-phone DOM read via USB DevTools (CDP attach)

When a row's evidence genuinely depends on a real-device
property (soft-keyboard `visualViewport` shrink, native
selection handles, OS clipboard / paste UI, real orientation
events, real-Android-Chrome network behaviour) AND also needs
a `data-*` / `localStorage` / `document.activeElement`
read-back, drive the read-back from the workstation via
Chrome DevTools USB attach. Verified on a real Samsung Galaxy
S10e in the 2026-05-16e
[`docs/android-phone-launch-timing-resmoke`](../../docs/deployment/vps-staging-smoke.md#2026-05-16e-docsandroid-phone-launch-timing-resmoke--real-android-chrome-first-launch-reads-the-new-client-side-timing-strip-the-2026-05-15c--2026-05-16-first-launch-transient-is-not-reproduced-on-this-attempt-nginx-records-close-time-re-confirmed-on-the-real-phone-surface)
slice.

```sh
# 1. Verify the device is authorized.
adb devices

# 2. Forward Chrome DevTools to a local port.
adb forward tcp:9222 localabstract:chrome_devtools_remote

# 3. Find the per-page debugger WS URL — FILTER STRICTLY on the
#    RelayTerm host first, never print non-RelayTerm titles.
curl -s http://127.0.0.1:9222/json/list \
  | jq '[.[] | select(.url | startswith("https://relayterm-staging.js-node.cc/")) | {id, url, ws: .webSocketDebuggerUrl}]'
```

A tiny Node script (Node 22+ has global `WebSocket`) connects
to the per-page WS URL and runs `Runtime.enable` +
`Runtime.evaluate { returnByValue: true, awaitPromise: true }`
for each readback — the same evaluation shape Playwright MCP
uses. Reads are pure DOM inspection; do NOT enable the
`Network`, `Fetch`, `Page` (screenshot), or `Tracing` CDP
domains, do NOT call `Page.captureScreenshot`, and do NOT
collect `cookies()` / request headers. The
`relayterm_session` cookie is `HttpOnly` and invisible to JS
anyway (a real-phone CDP `document.cookie` read returns `""`).

**Privacy gotchas (load-bearing).**

- `curl /json/list` without a filter dumps **every** open tab's
  title + URL, including non-RelayTerm tabs the operator was
  browsing. Filter strictly on
  `relayterm-staging.js-node.cc` BEFORE printing or
  forwarding to chat. If a non-RelayTerm title slips through,
  flag it inline, do not commit, do not memorise.
- Phone screenshots, screen recordings, `Page.captureScreenshot`
  CDP calls, and tab-switcher captures are out of scope for
  these rows.
- A single `localStorage.setItem` write to normalise the
  `relayterm.terminal-settings.v2` record before a launch is
  acceptable IF the operator approves it in chat and the
  dated entry records the before/after value. Anything beyond
  that (writing to `relayterm.active-terminal.v1`, clearing
  storage, calling `navigator.serviceWorker.getRegistrations().unregister()`,
  evicting `caches.delete(...)`) goes through the
  cache-bust subsection of "Launch timing diagnostics", not
  here.

##### SSH inbound probe — `netstat -tn`, not `docker logs`

The hermetic throwaway target (`lscr.io/linuxserver/openssh-server`)
writes only its init / boot lines to docker stdout; runtime
sshd activity is in syslog inside the container. The
2026-05-15c "russh never dialed" reading was based on the
incorrect probe; the 2026-05-16 resmoke established the
correct one (cross-link: AGENTS.md 2026-05-16 inline
Encountered Lesson).

```sh
# Authoritative — is there an established TCP connection on 2222
# inside the throwaway?
docker exec <target-container> netstat -tn | grep ':2222 .*ESTABLISHED'

# Authoritative — is sshd-session actually running?
docker exec <target-container> ps -ef | grep 'sshd-session.pam'

# Use only for boot / authorized_keys notes:
docker logs --tail 50 <target-container>
```

A 90-second poll loop of the netstat probe during the launch
window is the cheapest way to time the actual TCP
establishment relative to POST `/api/v1/terminal-sessions`
and WS `GET .../ws`. `net-tools` is present in the linuxserver
image; `ss` / `ip` are not.

##### Target / log inspection checklist (server-side, per row)

Each row that needs a server-side observation pulls from this
fixed set:

- **Backend nginx access log** — POST 201 timing is the
  authoritative POST timestamp. The `GET …/ws HTTP/1.1 101`
  line records the WebSocket-upgrade **close** timestamp, not
  the open timestamp — confirmed by the 2026-05-16b
  Playwright-first investigation (Phase A: POST 16:10:16,
  workspace-driven close at 16:11:32, nginx ws log line at
  16:11:32 = matches close, not open). Treat the 2026-05-15c
  / 2026-05-16 "60–68 s POST→WS gap" measurements as
  "session lifespan from POST to detach", not "POST→WS-open
  delay", until the workspace + backend timing-diagnostics
  slice (`feat/web-terminal-launch-timing-diagnostics`) lands
  an open-time-explicit signal.
- **Postgres `session_events`** — `attached` / `detached` /
  `closed` rows for the session UUID; `payload->>'last_seen_seq'`,
  `payload->>'reason'`. Public metadata only.
- **Postgres `terminal_sessions`** — `status`, `last_seen_at`,
  `closed_at`.
- **Postgres `audit_events`** — lifecycle rows
  (`ssh_identity_created`, `host_key_accepted`,
  `server_profile_created`). Public metadata only per
  [`docs/agent/redaction-rules.md`](../../docs/agent/redaction-rules.md)
  § 1.
- **SSH target `netstat -tn` poll** — the ESTABLISHED
  connection from the backend container to the throwaway on
  port 2222.
- **SSH target `docker logs`** — init / boot /
  `authorized_keys` lines ONLY; never as inbound-traffic
  evidence.
- **Renderer viewport state** — `data-renderer`,
  `data-renderer-experimental`, `data-renderer-fallback`,
  `data-renderer-gate`, `data-renderer-input`,
  `data-renderer-autofit`, `data-phase` on
  `production-terminal`. Closed-vocabulary; no payload.
- **WebSocket binary plane (RTB1)** — NOT inspected from
  Playwright. Wire-level rows belong in their own
  `vps-staging-smoke.md` dated entry per the harness plan's
  Path E discipline.

Record the **timing** of each. The 2026-05-16 resmoke
established that POST→201, WS→101, SSH ESTABLISHED,
`session_events.attached`, `session_events.detached`,
`last_seen_seq`, and renderer `data-phase` are distinct
timeline events; do not collapse any two into "the session
attached" / "the session detached".

##### Launch timing diagnostics (client-side; payload-free)

The production terminal workspace exposes a client-side
launch-timing diagnostic strip (`[data-testid="production-terminal-launch-timing"]`)
seeded by a `LaunchTimingRecorder` constructed at the moment of
the "Launch terminal" click in the Servers view. Each lifecycle
event renders as a `<dt>` carrying `data-launch-event`,
`data-launch-event-state` (`observed` / `pending`), and
`data-launch-event-ms` (relative monotonic offset from
`launch_started`, in ms). The same surface mirrors three of
the most-read events onto attributes on `production-terminal`
itself: `data-launch-timing-ws-open-ms`,
`data-launch-timing-ws-close-ms`, and
`data-launch-timing-first-output-ms`.

**Why this exists.** The 2026-05-16b investigation established
that the staging nginx `access_log` line for the
`GET …/ws → 101` upgrade records the WebSocket-upgrade
**close** timestamp, not the open timestamp. A POST→WS-open
delay can no longer be inferred from the nginx log alone. The
client-side recorder gives every smoke a first-class
"WebSocket open observed by the client" signal that does not
require backend access — and a `ws_close` signal that can be
compared against the nginx line to validate the
close-time interpretation.

**Redaction posture (smoke contract).** The recorder NEVER
captures terminal payload bytes, server `message` strings,
WebSocket URLs, cookies, headers, tokens, or any
`Error.message` text. Errors collapse to a closed-vocabulary
kind. The diagnostic lives entirely in memory; nothing writes
to `localStorage` / `sessionStorage`. The redaction sweep in
this section's "Redaction sweep" subsection MUST verify the
strip's DOM against the sentinel set; a row that quotes a
`data-launch-event-ms` value is acceptable, a row that quotes
text from the strip beyond the closed-vocabulary labels is
not.

**Reading the strip from Playwright MCP.** After the workspace
mounts (`[data-testid="production-terminal"]` present with
`data-phase ∈ {"connecting","attached","replaying","detached","closed"}`):

```js
// browser_evaluate
() => {
  const root = document.querySelector('[data-testid="production-terminal"]');
  const rows = Array.from(
    document.querySelectorAll('[data-testid="production-terminal-launch-timing-list"] [data-launch-event]'),
  ).map((el) => ({
    name: el.getAttribute('data-launch-event'),
    state: el.getAttribute('data-launch-event-state'),
    ms: el.getAttribute('data-launch-event-ms') || null,
  }));
  return {
    available: root?.getAttribute('data-launch-timing'),
    postOutcome: root?.getAttribute('data-launch-timing-create-post-outcome'),
    errorKind: root?.getAttribute('data-launch-timing-error-kind'),
    wsOpenMs: root?.getAttribute('data-launch-timing-ws-open-ms'),
    wsCloseMs: root?.getAttribute('data-launch-timing-ws-close-ms'),
    firstOutputMs: root?.getAttribute('data-launch-timing-first-output-ms'),
    rows,
  };
}
```

Pass criteria for a renderer-evaluation row that uses launch
timing as evidence: `available === "available"`, the
`postOutcome` is `"ok"`, `errorKind` is empty, and `ws_open` /
`first_server_message` / `first_output` are all in the
`observed` state with monotonically non-decreasing ms values.

###### Lifetime_X_then_close verification sub-step (load-bearing)

Before any downstream code change relies on the "nginx
`GET …/ws → 101` line records close time, not open time"
interpretation, run a controlled lifetime_X_then_close
verification using the new client-side timing diagnostic:

1. Open one production terminal session in Playwright MCP
   against staging. Wait for `data-phase="attached"`. Capture
   the `data-launch-timing-ws-open-ms` attribute value AND the
   wall-clock time of the snapshot read (`Date.now()` in the
   same `browser_evaluate` call).
2. Hold the session open with **no operator-side input** for a
   known X seconds. The recommended pinning value is
   `X = 30` seconds — long enough to exceed network jitter
   and short enough to fit inside one Playwright MCP slot.
   The lifetime_X_then_close design REQUIRES X > 5 s so the
   gap is unambiguous against any single-RTT jitter; values
   ≤ 1 s do not differentiate open from close.
3. Click `[data-testid="production-terminal-close"]` (End
   session). Wait for `data-phase="closed"`. Read
   `data-launch-timing-ws-close-ms` AND the wall-clock
   timestamp of the close snapshot.
4. Inspect the backend nginx access log for the
   `GET /api/v1/terminal-sessions/<id>/ws HTTP/1.1 101` line
   for the captured session UUID. Capture its wall-clock
   timestamp.
5. **Expected outcome:** the nginx log timestamp from step 4
   equals the close-snapshot wall-clock from step 3 within
   ~1 second (network + nginx flush jitter); it does NOT equal
   the open-snapshot wall-clock from step 1. The client's
   `ws_close_ms − ws_open_ms` should be ~X × 1000 ms ± jitter.
6. **If the outcome differs:** the close-time interpretation
   from the 2026-05-16b investigation does not hold for this
   nginx config; STOP and re-read the proxy directives before
   any further investigation builds on that assumption.

Record the result as a new dated entry in
`docs/deployment/vps-staging-smoke.md` under the section
`"<date> · lifetime_X_then_close nginx WS log verification"`.
Do NOT promote the launch-timing strip to a renderer-promotion
input until the verification has passed — the diagnostic
itself is useful from day one for cross-renderer comparisons
(every renderer goes through the same launch flow), but the
"lifetime measurement matches nginx close-time" interpretation
is the load-bearing contract a future smoke would need to
quote.

**Update (2026-05-16d — verification ran).** The
`docs/terminal-launch-timing-diagnostics-smoke` slice ran the
six-step verification above against staging end-to-end and
**confirmed the nginx-records-close-time reading** (client
`ws_close` matched the nginx `GET .../ws 101` line to within
~0.15 s; client `ws_open` was ~117 s away from the nginx
line and is NOT what nginx logged). See
[`docs/deployment/vps-staging-smoke.md`](../../docs/deployment/vps-staging-smoke.md)
§ "2026-05-16d · `docs/terminal-launch-timing-diagnostics-
smoke`" for the full evidence table. Two additional
methodology traps the slice surfaced — pin BOTH before
running this verification again:

###### Methodology trap 1 — cache-bust the SPA after web-container recreation

When a staging smoke recreates the relayterm-web container
to pick up new code, the browser frequently still serves a
cached `index.html` that references the OLD bundle hash —
even though `/` returns the fresh HTML on a hard reload from
the workstation `curl`. The first Playwright (or operator)
launch from a stale tab will load the OLD bundle and exhibit
the OLD code's behaviour — including, in this slice's run,
ZERO timing diagnostics on a launch that DID reach the new
container. That superficially looks like the first-launch
detach pattern the next slice is investigating; in the cache
case it is just the cache. BEFORE asserting that a missing
selector / missing diagnostic / detach-at-seq-0 is a real
signal, run ONCE per recreation cycle (inside Playwright MCP
`browser_evaluate`):

```js
// Wrapped as an IIFE so this snippet is portable across both
// Playwright MCP's `browser_evaluate` (which accepts a bare
// arrow function) AND a vanilla `page.evaluate(...)` /
// devtools console paste (which requires a value expression,
// not a top-level statement).
(async () => {
  if ('caches' in window) {
    const keys = await caches.keys();
    await Promise.all(keys.map(k => caches.delete(k)));
  }
  if ('serviceWorker' in navigator) {
    const regs = await navigator.serviceWorker.getRegistrations();
    await Promise.all(regs.map(r => r.unregister()));
  }
  return { clearedCaches: true };
})()
```

THEN close the tab (`browser_close`) AND navigate fresh with
a cache-busting query string (`browser_navigate '…?cachebust=
<ts>'`). Verify the loaded script src matches the freshly-
built bundle hash before treating any assertion as real:

```js
() => Array.from(document.querySelectorAll('script[src]'))
  .map(s => s.getAttribute('src'))
```

The relayterm-staging stack sends `cache-control: public,
immutable, max-age=31536000` on the **bundle** (`/assets/
index-<hash>.js`), which is correct — the hash changes when
the bundle changes, so the immutable bundle cache is exactly
right and should NOT be "fixed". The trap is upstream of
that: the browser tab's already-parsed DOM holds a stale
`index.html` reference to the OLD `<script src="/assets/
index-OLDHASH.js">`. Cache-busting the navigation forces a
re-fetch of `index.html`, which now points at the new bundle
hash; the fresh bundle then loads (and the new bundle's
immutable-cache header takes effect for the next operator).

###### Methodology trap 2 — staging nginx idle-closes WebSocket upgrades at ~60 s

The staging reverse-proxy idle-closes the
`/api/v1/terminal-sessions/<id>/ws` upstream after **~60 s
of no traffic** in either direction (consistent with nginx's
default `proxy_read_timeout 60s` applied to the proxied
WebSocket). On top of that the orchestrator's
detached-live-PTY TTL adds the documented 30 s reconnect
window, after which the session row auto-closes. So:

- A `lifetime_X_then_close` hold of **X > ~60 s with no
  operator input** will trigger an nginx-driven WS close
  before the operator clicks End-session. The `close_requested`
  recorder row will stay `pending` (the click runs after the
  wire is already closed, so the workspace's `closeClicked`
  handler can not fire a wire `Close` frame); `ws_close`
  fires from the transport `close` event. The lifetime
  measurement is still valid (`ws_close − ws_open` is the
  natural lifetime); it just is NOT operator-controlled.
- For an operator-controlled close, pick X ≤ ~50 s — well
  inside the idle window. The 30 s recommended default
  upstream is safe.
- For a lifetime > ~60 s, the test is now measuring "wire
  lifetime under nginx idle-close" rather than "operator
  click → wire close". Either is fine but be explicit in the
  dated entry about which one the row reports.

These are properties of the deployed staging proxy config,
not of RelayTerm code. If a future operator UX needs longer
detached windows, the fix is in the nginx reverse-proxy
config on the location for the WS upgrade path
(`proxy_read_timeout` and/or `proxy_send_timeout`), not in
any RelayTerm crate.

##### Evidence classification — every row tags its evidence

Every dated mobile-smoke entry from this point forward tags
each evidence row with one of (matches the plan exactly):

- **playwright-emulated** — observed from a desktop browser
  at a mobile viewport via Playwright (MCP or scripted). Not
  a real-device claim.
- **real-phone operator** — the operator reported the
  observation from the physical device.
- **CDP-driven on real device** — added 2026-05-17. The JS
  side of the row (the click, the input, the state read)
  runs in the real Android Chrome on a USB-attached real
  device via Chrome DevTools Protocol `Runtime.evaluate`,
  but the trigger is a synthetic `click()` / DOM mutation,
  not a real-touch / OS-keyboard / OS-paste event. Strictly
  stronger than **playwright-emulated** (real Android JS
  engine + real network stack + real WS + real WASM);
  strictly weaker than **real-phone operator** (no
  hit-test, no soft-keyboard interaction, no `pointerdown`
  / `touchstart` chain, no OS clipboard / paste UI, no
  native selection handles, no back-gesture). Use ONLY for
  rows whose primary evidence is network / WS / JS /
  WASM / attach-timing on a real device. Do NOT use for
  any row in the "Real-phone narrow scope" closed list
  above — those rows stay real-phone-operator-only because
  hardware behaviour IS the evidence. Verified
  end-to-end by the 2026-05-17 slice
  `docs/android-phone-launch-timing-multi-run-resmoke`
  (three sequential xterm launches on a Galaxy S10e where
  the operator could not physically tap the off-screen
  Launch button on the 360 × 617 px viewport).
- **server-side inspected** — observation came from backend /
  nginx / Postgres / SSH-target logs or DB rows.
- **inferred** — derived from another row's evidence; cite
  the source row.
- **deferred — &lt;reason&gt;** — not exercised on this
  surface. Reasons match the harness vocabulary (`renderer
  not identified`, `clipboard permission unavailable`,
  `fixture absent`, `blocked by Row 12`, `emulation
  insufficient — needs phone`, `workspace-side investigation
  pending`).

A row recorded without an evidence-class label is
unreviewable — same posture as the existing rule that a row
without an input-path tag is unreviewable.

##### Classification template for a non-pass finding

Mirrors § 5's "Classification template for a finding" in the
plan. When a row produces a non-pass, attribute it to one of:
**renderer-specific**, **mobile workspace / session**,
**transient**, **target / setup issue**, or **inconclusive**
(promote inconclusive rows to `deferred — <reason>` and name
the disambiguating slice).

#### Explicit non-goals

- **No committed Playwright runner.** This runbook does NOT promote
  the renderer smoke to a CI lane. A future slice can do so per
  [`docs/renderer-smoke-harness.md`](../../docs/renderer-smoke-harness.md)
  § "Option B — committed Playwright runner (deliberate-later)".
  Until then this smoke is human-driven against Playwright MCP.
- **No performance benchmark automation.** Memory / CPU notes remain
  free-form human-readable observations per
  [`docs/terminal-renderer-evaluation.md`](../../docs/terminal-renderer-evaluation.md)
  § "Memory / CPU rough observations".
- **No renderer promotion decision.** A single run of this smoke is
  a data point, not a Gate-2 promotion. Promotion follows the gates
  in [`docs/terminal-renderer-evaluation.md`](../../docs/terminal-renderer-evaluation.md)
  § "Promotion criteria"; the soak window and the default-flip slice
  are separate.
- **No backend protocol changes.** The renderer smoke drives the
  existing wire surfaces; the wire protocol stays RelayTerm-shaped
  per [`SPEC.md`](../../SPEC.md) "Architectural invariants" and
  AGENTS.md "Architectural rule (load-bearing)".
- **No tmux / screen persistence work.** Persistence across backend
  restart is a separate roadmap in
  [`docs/persistent-sessions.md`](../../docs/persistent-sessions.md);
  it is independent of renderer evaluation and is not gated on this
  smoke.
- **No direct WebSocket injection counted as renderer input.** Path E
  is backend-only; any wire / replay regression check recorded from a
  WebSocket-client smoke goes into its own dated entry in
  `docs/deployment/vps-staging-smoke.md`, never folded into a
  renderer-evaluation row.

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
