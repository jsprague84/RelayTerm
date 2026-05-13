# SPEC — Inventory & dashboard surface area

> Detailed contracts split out of `SPEC.md` for context efficiency. The
> top-level `SPEC.md` is the index; this file is the long form.
>
> The lifecycle policy itself (delete vs disable, FK / RESTRICT rules,
> audit-event expectations, UI implications) STAYS in `SPEC.md` →
> "Inventory lifecycle and destructive-action policy" because it is
> normative and load-bearing. This document is the per-surface detail
> for the UIs and APIs that consume that policy.

## Contents

- [Production inventory read-only views](#production-inventory-read-only-views)
- [Production read-only inventory detail panels](#production-read-only-inventory-detail-panels)
- [Production inventory client-side search & filters](#production-inventory-client-side-search--filters)
- [Production SSH identity generation UI](#production-ssh-identity-generation-ui)
- [Production host & server-profile creation UI](#production-host--server-profile-creation-ui)
- [Production host-key preflight & trust UI](#production-host-key-preflight--trust-ui)
- [Production SSH auth-check UI](#production-ssh-auth-check-ui)
- [Production dashboard summary](#production-dashboard-summary)
- [Dashboard recent activity](#dashboard-recent-activity)
- [Server profile disable / enable backend (landed)](#server-profile-disable--enable-backend-landed)
- [Server profile lifecycle audit](#server-profile-lifecycle-audit)
- [Current-user audit events read API (landed)](#current-user-audit-events-read-api-landed)
- [Server profile disable / enable UI (landed)](#server-profile-disable--enable-ui-landed)
- [Future implementation order](#future-implementation-order)

---

### Production inventory read-only views

The Servers and Identities views are inventories of `hosts`, `server_profiles`, and `ssh_identities`. They prove the production shell can fetch real backend data through typed, redaction-safe helpers without pulling in the dev lab or any renderer adapter. The Servers view today stays read-only for hosts and server-profiles outside of disable / enable; the Identities view also wires SSH-identity rename + delete on top of the read-only baseline. (Host-key trust, auth-check, SSH identity generation, terminal launch, and the API helpers for host / server-profile edit + delete are now wired — see the per-flow sections below.)

**Scope (load-bearing — this slice).**

1. **Servers view** (`apps/web/src/lib/app/views/ServersView.svelte`) renders two grouped sections: a Hosts list (display name, hostname, port, default username) and a Profiles list (name, linked host summary if resolvable from the fetched hosts, effective username with explicit "(host default)" / "(override)" attribution, tags, and last-connected timestamp). Hosts and profiles are fetched in parallel via `Promise.all`; either failure collapses the whole view to a single safe error summary keyed off the first failed resource.
2. **Identities view** (`apps/web/src/lib/app/views/IdentitiesView.svelte`) renders one row per identity with name, key type, full SHA-256 fingerprint, a one-line public-key preview (`publicKeyPreview` truncates the base64 body to keep tables tight), created-at, last-used-at, and a "Copy public key" button. The button copies ONLY `identity.public_key` — never the fingerprint, never any other field. Clipboard failures collapse to a static `Copy failed` label without echoing origin/permission detail.
3. **Dashboard counts** (`DashboardView.svelte`) shows `hosts` / `profiles` / `identities` / `sessions` cardinality using the same helpers (the dashboard surface is described in full under "Production dashboard summary"). The counts are nice-to-have: any failure collapses to an unobtrusive `—` placeholder so the per-view error surface stays the canonical triage path. No polling.
4. **No secret material is rendered.** `SshIdentity` (TypeScript DTO) does not declare an `encrypted_private_key` or `private_key` field. The runtime parser in `parseSshIdentity` builds the DTO field-by-field, so a backend bug or hostile fixture that includes those keys cannot smuggle them onto the parsed object. `tests/inventoryApi.test.ts` pins this with sentinel strings asserted absent from the parsed object, the serialized JSON, and the formatted preview.
5. **Loading / empty / error states are honest.** Loading states render an unobtrusive "Loading…" placeholder. Empty states explain that inventory rows are created through the backend (the Identities view's "Generate SSH identity" panel, the Servers view's "Create host" / "Create server profile" panels, or the `/api/v1/*` REST surface). Error states render the formatted summary and nothing else (no retry-storm, no auto-reload).
6. **Architecture rule preserved.** The new helpers and views live entirely under `lib/app/` and `lib/api/`; no import touches `lib/dev/` or any renderer adapter. `appShellIsolation.test.ts` continues to pass.

**Redaction posture (load-bearing).**

- The shared error reader (`readErrorEnvelope` in `apiErrors.ts`) extracts ONLY `code` and `message` from the backend's `{ error: { code, message } }` envelope; sibling fields (including any future `operator_detail`) are dropped.
- `describeLoadError` formats the UI summary as a function of `kind` + `status` + `code` only — it never echoes the wire `message` of an HTTP error or the thrown message of a transport error. The typed error object preserves both so programmatic callers can branch, but the formatter is the single point that reaches the UI.
- The helpers do NOT log raw response bodies. `inventoryApi.test.ts` pins `console.log/warn/error` as untouched on success and on transport failure.
- The OpenSSH public-key preview is a pure string operation on the supplied argument; nothing in the helper looks up a private-key field by side channel.

**Wire shapes (mirror of `crates/relayterm-api/src/dto/`).**

- `Host` — `{ id, display_name, hostname, port, default_username, created_at, updated_at }`. The parser rejects ports outside `1..=65535` or non-integer values; unknown extra fields are silently dropped so a future safe addition does not break older clients.
- `ServerProfile` — `{ id, name, host_id, ssh_identity_id, username_override, tags[], created_at, updated_at, last_connected_at }`. Parser rejects non-string tag entries. `resolveProfileLinks(profile, hosts)` produces `{ host, effectiveUsername, inheritedFromHost }` — the join is done on the client; a missing `host_id` is rendered honestly as "host not in your inventory" and `effectiveUsername` falls back to `null` when neither override nor host default is reachable.
- `SshIdentity` — `{ id, name, key_type, public_key, fingerprint_sha256, created_at, last_used_at }`. `key_type` is constrained to the wire-stable `ed25519 | rsa | ecdsa_p256 | ecdsa_p384 | ecdsa_p521` set; unknown algorithm tags collapse to `malformed_response`.

**Future work (explicit out-of-scope for this slice).**

Inline edit / delete UI for hosts and server-profiles (API helpers exist; no view calls them yet); private-key import; route-param detail pages and `get_by_id` round-trips; password bootstrap / `ssh-copy-id`; browser-driven SPA inventory smoke (mutation surface is API-smoked + unit-test-pinned today); durable session-recording UI; real auth UI; mobile/Tauri shell integration. Each is a separate slice. (SSH identity generation, host-key preflight + trust UI, auth-check UI, terminal launch from the production shell, SSH-identity rename + delete, read-only inventory detail panels, and inventory-management backend mutation routes are now wired — see the per-flow sections below.)

### Production read-only inventory detail panels

A click-to-select detail panel sits next to each inventory list (Hosts, Server profiles, SSH identities). The panel is read-only — it surfaces fields the list query already loaded, joined client-side against the other lists already on screen. The panel does NOT introduce edit / delete UI, route-param detail pages, `get_by_id` round-trips, or any new backend surface.

**Scope (load-bearing — this slice).**

1. **Selection model.** Each list row exposes a real `<button>` with a stable `data-testid` (`host-row-select`, `profile-row-select`, `identity-row-select`). Clicking the same row twice closes the panel; selecting a different row swaps the panel content. On the Servers view, host and profile selection are mutually exclusive — only one detail panel is visible at a time. Identity selection is independent of the generation panel; the existing "Generate SSH identity" surface is unchanged. The select button carries `aria-expanded` (the disclosure-widget semantic — the button opens a sibling panel; `aria-pressed` is deliberately not also set so screen readers do not see two contradictory ARIA roles) and renders a visible ring when active. A "Close" button (with `data-testid` `host-detail-close` / `profile-detail-close` / `identity-detail-close`) dismisses the panel.
2. **Host detail panel** (`host-detail-panel`) shows display name, hostname, port, default username, created-at, updated-at, and a short-id of the row. It also renders the count and ordered list of profiles whose `host_id` matches — joined entirely from the already-loaded `view.profiles`; no new fetch. Honest copy explicitly states host details do not prove reachability and connection readiness depends on a server profile + host-key trust + auth-check.
3. **Profile detail panel** (`profile-detail-panel`) shows name, the linked-host summary if resolvable from the loaded hosts (otherwise an honest "host not in your inventory"), the linked-identity summary (id + name + key type + fingerprint, joined from the already-loaded identities — no new fetch; honest "identity not in your inventory — metadata available in the SSH Identities view" otherwise), the effective username with explicit "(host default)" / "(override)" attribution, tags, last-connected, created-at, updated-at, and a short-id. The panel does NOT call `get_by_id`, does NOT fetch the linked identity individually, and does NOT infer host-key trust / auth-check / terminal readiness — those facts live on the per-profile state stores already rendered alongside the row.
4. **Identity detail panel** (`identity-detail-panel`) shows name, key type, full SHA-256 fingerprint, a one-line public-key preview (`publicKeyPreview` — the same helper used in the row), created-at, last-used-at (or "never"), short-id, and a `<pre>`-rendered full OpenSSH public key with a deliberate "Copy public key" button. The full key reaches the DOM through exactly one path — the `<pre>` block — and the copy action's value is the typed `public_key` field; `title=` / `aria-*` tooltips are not used to surface key material. The button label collapses to "Copy failed" without echoing browser detail when the clipboard API is unavailable.
5. **Helpers.** `apps/web/src/lib/app/inventory/inventoryDetails.ts` carries the pure projections: `shortId`, `safeDisplayValue`, `hostProfileCount`, `relatedProfilesForHost`, `identitySummary`, `resolveProfileDetail`, `describeReadinessFromKnownState`, `identityPublicDetail`, `publicKeyCopyValue`. Each helper is field-by-field — the SSH-identity helpers re-assert the redaction discipline established by `parseSshIdentity` (`encrypted_private_key` / `private_key` are not declared on the projection types AND cannot smuggle through onto the returned object even when present on the input). `tests/inventoryDetails.test.ts` pins this with redaction sentinels asserted absent from the returned projections, the JSON-stringified projections, and the deliberate copy value.
6. **Architecture rule preserved.** The new module lives entirely under `lib/app/`; no import touches `lib/dev/` or any renderer adapter. `appShellIsolation.test.ts` continues to pass.

**Honesty rules (load-bearing).**

- The detail panel surfaces ONLY data the list query already returned. No `get_by_id` round-trip is added; a future detail-route slice can replace the panel without changing the helper contract.
- Related-object summaries are joins over the supplied list arrays. An unresolved link is rendered honestly ("host not in your inventory" / "identity not in your inventory — metadata available in the SSH Identities view") — the helpers never synthesise a placeholder host, identity, or fingerprint.
- The profile detail panel does NOT imply host-key trust, auth-check success, or terminal-launch readiness from the existence of the profile or the resolution of its links. The advisory line (`describeReadinessFromKnownState`) explicitly names host-key trust and auth-check as separate, still-required steps; it never claims "ready", "trusted", "verified", or "passed".
- Selection state is local to the view component (no URL / route-param coupling, no global store). A page reload, a refresh, or a navigation event resets the detail panel to closed — the URL never carries an inventory id.

**Redaction posture (load-bearing).**

- `IdentitySummary` and `IdentityPublicDetail` (the projection types) do not declare `encrypted_private_key` or `private_key`. The helpers build them field-by-field from typed `SshIdentity` input, so a backend bug or hostile fixture that smuggled a private-key field onto the input cannot reach the projection or any string the panel renders. Sentinel tests pin this against future regressions.
- The full OpenSSH public key reaches the DOM through exactly one path per panel — the `<pre>` block. `publicKeyPreview` is used in the in-card summary so the full key cannot leak through an incidental hover surface, `title=` attribute, or `aria-*` description.
- The copy action's value is the typed `public_key` field; the helper that yields it (`publicKeyCopyValue`) is pure and cannot read or echo any private-key field, even when one is present on the input. Clipboard failure collapses to a static `Copy failed` label without echoing origin / permission detail.
- No helper logs, throws, or formats raw response bodies. The panels never echo wire `message` fields; advisory copy is composed from the helper's own enum-shaped state.

**Future work (explicit out-of-scope for this slice).**

Route-param detail pages (e.g. `/servers/:id`); full-page detail routes; `get_by_id` round-trips and per-detail backend calls; edit / delete / rename UI for any inventory entity; private-key import; password bootstrap / `ssh-copy-id`; live host-key trust / auth-check status surfaced inside the detail panel beyond what the row already renders; multi-tab workspace with sticky detail; mobile/Tauri-specific detail layout; pagination over the inventory list. (Client-side search and filters are now wired — see "Production inventory client-side search & filters" below.) Each is a separate slice.

### Production inventory client-side search & filters

A usability layer over the existing read-only inventory views. Servers and Identities each gain a small filter toolbar above the list that narrows what is rendered. The filter is in-memory only over already-loaded data — no backend search, no pagination, no URL or local-storage persistence.

**Scope (load-bearing — this slice).**

1. **Pure helpers** (`apps/web/src/lib/app/inventory/inventoryFilters.ts`): `normalizeSearchText(input)` (trims, lowercases, collapses internal whitespace runs to a single space; non-string and empty inputs collapse to `""`), `filterHosts(hosts, query)`, `filterProfiles(profiles, hosts, identities, filters)`, `filterIdentities(identities, filters)`, `collectProfileTags(profiles)` (sorted, deduped, case-insensitive), and `countFilteredResults(visible, total, singular, plural?)` (renders a "Showing X of Y <noun>" string, or a shorter "Y <noun>" form when no filter is active). Helpers are field-by-field and never mutate their inputs; an empty filter returns a NEW shallow copy of the source list so callers can rely on result-array ownership.
2. **Servers view filter toolbar** (`servers-filter-toolbar`) renders three controls: `Search hosts` (input matching display name, hostname, port-as-decimal, default username), `Search profiles` (input matching profile name, tags, username override, effective username, linked-host display name + hostname, linked-identity name + fingerprint + key type), and `Profile tag` (select pre-populated with the unique tags currently in use; auto-resets to "All tags" when the active tag disappears from the loaded inventory). A `Clear filters` button is enabled only while at least one Servers filter is active. The Hosts and Profiles result-count badges (`hosts-count` / `profiles-count`) flip to the "Showing X of Y" form when the corresponding filter is active.
3. **Identities view filter toolbar** (`identities-filter-toolbar`) renders one or two controls: `Search identities` (input matching name, fingerprint, and key type) and a `Key type` select (rendered ONLY when more than one key type appears in the loaded list). A `Clear filters` button is enabled only while at least one identity filter is active. The Identities result-count badge (`identities-count`) flips to the "Showing X of Y" form when the corresponding filter is active.
4. **Empty-filter states.** Hosts list renders `hosts-filter-empty` ("No hosts match this filter."), profiles list renders `profiles-filter-empty` ("No profiles match this filter."), identities list renders `identities-filter-empty` ("No identities match this filter."). These are distinct from the existing zero-rows empty states (`hosts-empty` / `profiles-empty` / `identities-empty`) so the operator can tell "you have nothing" apart from "your filter excluded everything."
5. **Detail-panel coexistence.** Selecting a row that is later filtered out of the list keeps the detail panel open; the panel renders an honest banner (`host-detail-hidden-by-filter` / `profile-detail-hidden-by-filter` / `identity-detail-hidden-by-filter`) telling the operator the row is hidden by the active filter and pointing at the controls that brought it there. Clearing the relevant filter brings the row back into the list without re-selecting it.
6. **Architecture rule preserved.** The new helpers and toolbars live entirely under `lib/app/inventory/` and the two view components; no import touches `lib/dev/` or any renderer adapter. `appShellIsolation.test.ts` continues to pass.

**Redaction posture (load-bearing).**

- The matching haystack for an SSH identity (in `filterIdentities` AND in `filterProfiles` for a profile's linked identity) is built field-by-field from `name`, `fingerprint_sha256`, and `key_type` only. The OpenSSH `public_key` body is deliberately NOT in the haystack — substring matching against a 400-char base64 string is rarely useful and would invite a future preview surface that echoes the matched fragment. Sentinel tests in `tests/inventoryFilters.test.ts` pin that a hostile public-key body cannot drive a match.
- A hostile fixture that smuggles `private_key`, `encrypted_private_key`, `session_output`, or `access_token` onto an SshIdentity input cannot reach the matching haystack — the haystack reads only typed properties. The result array still references the input object (the helpers are pure, not deep-clones), but the helpers do not surface those fields through any computed string. The redaction-sentinel tests pin that a query against any of those sentinel substrings returns the empty array.
- Helpers do NOT log search queries. The search inputs are user-typed UI text; no path here writes them to the console, throws them inside an Error, or echoes them through a wire body.
- The filter toolbars do NOT alter the redaction posture of the existing detail panels — `private_key` / `encrypted_private_key` remain undeclared on the typed DTOs, and the filter helpers do not declare them either.

**Stable selectors.** New `data-testid` hooks: `servers-filter-toolbar`, `servers-host-search`, `servers-profile-search`, `servers-profile-tag-filter`, `servers-clear-filters`, `hosts-filter-empty`, `profiles-filter-empty`, `host-detail-hidden-by-filter`, `profile-detail-hidden-by-filter`; `identities-filter-toolbar`, `identities-search`, `identities-key-type-filter`, `identities-clear-filters`, `identities-filter-empty`, `identity-detail-hidden-by-filter`. The existing `hosts-count` / `profiles-count` / `identities-count` selectors continue to identify the result-count badges (now the count-string is sourced from `countFilteredResults`).

**Future work (explicit out-of-scope for this slice).**

Backend-side search / filtering; pagination over inventory lists; URL query-string state for filters (deep-linking a filtered view); saved / starred filters; saved per-user view preferences; multi-tag AND/OR composition (today the tag select is a single exact match); free-text search over deeper fields (e.g. created-at ranges); regex / fuzzy matching; saving a search as a "smart group". Each is a separate slice.
### Production SSH identity generation UI

The first production-safe write flow on the Identities view: an operator can ask the backend to generate a fresh keypair, see only the public metadata, and copy the OpenSSH public key for manual installation on the target server. No private material is ever rendered, copied, logged, or returned over the wire.

**Scope (load-bearing — this slice).**

1. **"Generate SSH identity" panel** lives on `IdentitiesView.svelte`, opened by a button in the view header. The form has a name input (≤ {`MAX_IDENTITY_NAME_LEN` = 64} characters, no surrounding whitespace, no control characters) and a key-type select bound to `SUPPORTED_GENERATION_KEY_TYPES`. The submit button is disabled while a request is in flight or while the trimmed name is empty. The "Close" button is disabled while submitting so an in-flight request cannot be orphaned.
2. **`createSshIdentity(request, options)`** in `lib/api/sshIdentities.ts` is the single client entry point. It client-side-validates the request (mirrors the backend's `CreateSshIdentityRequest::validate` rules), POSTs `{ name, key_type }` to `/api/v1/ssh-identities`, parses the response with `parseSshIdentity` (which already drops `private_key`/`encrypted_private_key`), and returns a typed `CreateSshIdentityResult`. It does not throw, does not log raw response bodies, and does not echo wire / transport detail through any user-facing string.
3. **Supported key types** are gated by `SUPPORTED_GENERATION_KEY_TYPES` — currently `["ed25519"]`, the deliberate intersection of the wire-stable `SshKeyType` union (which has to decode legacy rows) and what the backend vault can actually generate today. A test pins this against drift.
4. **Success UI** renders name, key type, SHA-256 fingerprint, created-at, the full OpenSSH public key in a `<pre>`, and a "Copy public key" button (re-uses the existing `copyPublicKey` helper). The success card stays visible until the user closes the panel; the new identity is also prepended to the inventory list (or a refresh is triggered if the list was loading/errored).
5. **Error UI** renders one line from `describeCreateSshIdentityError` and nothing else. The summary is a function of `kind` + `status` + `code` (and the validation `reason` enum) only — never the wire `message`, never the transport `Error.message`. A 503 `service_unavailable` is collapsed to a friendly "backend vault is not configured" hint so an operator running without a master key sees an actionable message; every other HTTP error keeps the raw `HTTP <status> <code>` form.
6. **No backend changes.** The existing `POST /api/v1/ssh-identities` route already returns the wire shape the inventory parser consumes; the slice is purely a frontend addition.
7. **Architecture rule preserved.** No import added under `lib/app/` touches `lib/dev/` or any renderer adapter; `appShellIsolation.test.ts` continues to pass.

**Redaction posture (load-bearing).**

- The `SshIdentity` TypeScript DTO does not declare `encrypted_private_key` or `private_key`. `parseSshIdentity` constructs the DTO field-by-field, so a backend bug or hostile fixture that includes those keys on a 201 response cannot smuggle them onto the returned object. Sentinel tests in `tests/inventoryApi.test.ts` pin this for both the parser and the `createSshIdentity` happy path.
- `describeCreateSshIdentityError` is the only formatter that reaches the UI. It never echoes the wire `message` or transport `Error.message`. Sentinel tests pin this against future regressions for `http`, `transport`, and `service_unavailable` kinds.
- The success card surfaces the OpenSSH public key in exactly two places: a `<pre>` for inspection and the "Copy public key" button (which copies `identity.public_key` only). The key never appears in `title=` / `aria-*` tooltips, console output, or any data attribute.
- Generation surface mirrors the existing list/copy redaction discipline: helpers do NOT log raw response bodies; tests pin `console.log/warn/error` as untouched across success, HTTP failure, and transport failure.

**Wire shapes (mirror of `crates/relayterm-api/src/dto/ssh_identity.rs`).**

- Request: `{ name: string, key_type?: "ed25519" }`. The backend accepts the broader `SshKeyType` union as a string but the client gates it to `SUPPORTED_GENERATION_KEY_TYPES` so a UI typo cannot reach the boundary.
- Response (`201 Created`): the same `SshIdentity` shape used by `listSshIdentities` — `{ id, name, key_type, public_key, fingerprint_sha256, created_at, last_used_at }`. No private-key field exists on the wire.

**UX copy (load-bearing).**

- The panel intro states that RelayTerm generated the keypair on the backend, the private key is encrypted at rest with the master key and never reaches the browser, and that copy/install on `~/.ssh/authorized_keys` is currently manual.
- The success card explicitly tells the operator to append the public key to the target server. It does NOT imply the key is already installed, that the identity can already authenticate against any host, or that the private key can be recovered from the UI.
- The footer note carries the remaining future-work list: private-key import, password bootstrap, and `ssh-copy-id` automation are deliberate later slices. (SSH-identity rename and delete have landed — see `apps/web/src/lib/app/views/IdentitiesView.svelte` for the in-place rename + delete affordances; the full route contract is in SPEC.md → "Inventory lifecycle and destructive-action policy".)

**Future work (explicit out-of-scope for this slice).**

Private-key import (BYOK); password bootstrap and `ssh-copy-id` to automate `authorized_keys` install; per-identity audit log surface (the global Settings-view audit feed already surfaces `ssh_identity_deleted` rows); multi-vault key rotation. Each is a separate slice. (Identity rename and delete have landed — `PATCH /api/v1/ssh-identities/:id` is rename-only; `DELETE /api/v1/ssh-identities/:id` refuses `409 ssh_identity referenced` when any owned `server_profiles` row references the identity, and writes a `ssh_identity_deleted` audit BEFORE the DELETE on success.)

### Production host & server-profile creation UI

The next production-safe write flows on the Servers view: an operator can create a `host` (a reachable target definition) and a `server_profile` (a binding of a host to an SSH identity). Both flows are metadata-only — they do NOT trust a host key, do NOT verify SSH authentication, and do NOT confirm the public key is installed on the target.

**Scope (load-bearing — this slice).**

1. **"Create host" panel** lives on `ServersView.svelte`, opened by a button in the view header. The form has `display_name` (≤ 128 chars, no surrounding whitespace, no control chars), `hostname` (≤ 253 chars, no whitespace, no control chars, only ASCII alphanumerics + `-`, `.`, `:`, `[`, `]`, `_`), `port` (integer 1..=65535, defaults to 22), and `default_username` (≤ 64 chars, leading letter/`_`, ASCII alphanumerics + `-`, `_`, `.` thereafter). Submit is disabled while a request is in flight or while any required text field is empty after trim.
2. **"Create server profile" panel** lives on the same view. The form has `name` (≤ 64 chars, no surrounding whitespace, no control chars), a `host` select (from the caller's existing hosts), an `ssh_identity` select (from the caller's existing identities), an optional `username_override` (same shape as host username), and an optional `tags` input parsed from a comma-separated string (≤ 32 tags, each ≤ 32 chars, ASCII alphanumerics + `-`/`_`, no duplicates). The "Create server profile" button is **disabled at the toolbar** when the caller has zero hosts OR zero identities — `canSubmitServerProfile(hostCount, identityCount)` returns a typed reason (`no_hosts | no_identities | no_hosts_or_identities | ok`) and the UI renders an honest empty-state hint without ever opening the form.
3. **`createHost(request, options)` and `createServerProfile(request, options)`** in `lib/api/hosts.ts` and `lib/api/serverProfiles.ts` are the single client entry points. Each client-side-validates the request (mirrors the backend's validators in `crates/relayterm-core/src/validation.rs`), POSTs to the relevant endpoint via the shared `postJsonItem` helper in `apiErrors.ts`, parses the response with the existing `parseHost` / `parseServerProfile`, and returns a typed result. Neither helper throws, logs raw response bodies, or echoes wire / transport detail through any user-facing string.
4. **Success UI** for hosts shows the new display name, `hostname:port`, and default user, with an explicit "Reachability and host-key trust are not verified by this action." disclaimer. Success UI for profiles shows the new name and an explicit "The host key is not yet trusted and SSH authentication has not been verified for this profile." disclaimer. The newly-created row is also prepended to the inventory list (or a refresh is triggered if the list was loading/errored).
5. **Error UI** renders one line from `describeCreateHostError` / `describeCreateServerProfileError` and nothing else. Both formatters stay a function of `kind` + `status` + `code` (and the validation `reason` enum) only — never the wire `message`, never the transport `Error.message`. The server-profile formatter collapses `404 not_found` to a friendly "linked host or SSH identity not found" hint so a stale-reference race shows an actionable message.
6. **No backend changes.** The existing `POST /api/v1/hosts` and `POST /api/v1/server-profiles` routes already accept the wire shapes the new helpers send; the slice is purely a frontend addition.
7. **Architecture rule preserved.** No import added under `lib/app/` touches `lib/dev/` or any renderer adapter; `appShellIsolation.test.ts` continues to pass.

**Redaction posture (load-bearing).**

- Both formatters never echo wire / transport detail. Sentinel-string tests in `tests/createApi.test.ts` pin this for `http`, `transport`, and `validation` kinds across both helpers, including the `404 not_found` collapse on profile create.
- `parseServerProfile` already constructs the DTO field-by-field, so a backend bug or hostile fixture that includes `private_key` / `encrypted_private_key` on a 201 response cannot smuggle them onto the parsed object. A redaction-sentinel test pins this for the `createServerProfile` happy path.
- Helpers do NOT log raw response bodies. Tests pin `console.log/warn/error` as untouched across success, HTTP failure, and transport failure for both `createHost` and `createServerProfile`.

**Wire shapes (mirror of `crates/relayterm-api/src/dto/`).**

- Host create request: `{ display_name, hostname, port?, default_username }`. The validator normalizes `port` to `DEFAULT_SSH_PORT` (22) when omitted before sending so the wire body is always explicit. Response (`201 Created`): the same `Host` shape used by `listHosts`.
- Server-profile create request: `{ name, host_id, ssh_identity_id, username_override?, tags? }`. `username_override` is included on the wire ONLY when non-null and non-empty (matches the existing integration-test shape so the backend's "omitted == null" behavior is exercised). `tags` is always sent (defaulting to `[]`). Response (`201 Created`): the same `ServerProfile` shape used by `listServerProfiles`.

**UX copy (load-bearing).**

- Host panel intro: "A host is a metadata-only target definition" and "No SSH connection is attempted. Host-key trust and auth-check happen per-profile (panels appear under each profile row after creation)."
- Profile panel intro: "A server profile binds a host, a username, and an SSH identity into a single connect target" and "Creating a profile does NOT trust the host key, does NOT verify SSH authentication, and does NOT install the public key on the target server. Run host-key trust and then auth-check on the new profile row after it appears."
- The view header and footer are updated with the same load-bearing claim: creation here does not imply trust or reachability.

**Stable selectors.** New `data-testid` hooks: `servers-create-host-{open,close,panel,form,display-name,hostname,port,username,submit,error,success}` and `servers-create-profile-{open,close,panel,form,name,host,identity,username-override,tags,submit,error,success,blocked}`.

**Future work (explicit out-of-scope for this slice).**

Inline edit / delete UI for hosts and server-profiles in the Servers view (the underlying `updateHost` / `deleteHost` / `updateServerProfile` / `deleteServerProfile` API helpers are landed and unit-tested in `apps/web/src/lib/api/{hosts,serverProfiles}.ts`; the view does not call them yet); password bootstrap / `ssh-copy-id`; route-param detail / `get_by_id` panels; mobile/Tauri shell integration. Each is a separate slice. (Host-key preflight + trust UI and auth-check UI are now wired — see "Production host-key preflight & trust UI" and "Production SSH auth-check UI" below. Inventory-management backend mutation routes for host / server-profile / SSH-identity edit + delete are also landed and API-smoked against staging on 2026-05-12 — see `docs/deployment/vps-staging-smoke.md`.)

### Production host-key preflight & trust UI

The next production-safe security flow on the Servers view: an operator can run `host-key-preflight` for a server profile, see the captured fingerprint and trust classification, and explicitly trust an unknown key by confirming the fingerprint. This is NOT auth-check, NOT terminal launch, and NOT automatic trust-on-first-use.

**Scope (load-bearing — this slice).**

1. **Per-profile "Host key" panel** is rendered inside each profile row on `ServersView.svelte` via the `HostKeyPanel.svelte` component. The panel exposes a "Run host-key preflight" button, a status badge (`Not trusted` / `Trusted` / `Changed`), the captured key type, the captured `SHA256:<base64>` fingerprint (selectable / copyable), and — only for the `unknown` outcome — a fingerprint-confirmation input + "Trust this host key" button. The panel holds local Svelte state ONLY (no global stores, no router, no polling, no auto-retry).
2. **`hostKeyPreflight(profileId, options)` and `trustHostKey(profileId, expectedFingerprint, options)`** in `lib/api/serverProfiles.ts` are the single client entry points. Each parses the response with a field-by-field DTO guard (`parseHostKeyPreflightResponse` / `parseTrustHostKeyResponse`) so a stray `private_key` / `encrypted_private_key` smuggled onto a wire body cannot reach the parsed object. Neither helper throws, logs raw response bodies, or echoes wire / transport detail through any user-facing string.
3. **Trust is NEVER auto-issued.** The "Trust this host key" button is enabled only when ALL of: (a) the most recent preflight returned `unknown`; (b) the captured fingerprint is non-empty AND passes the local `isValidFingerprintShape` shape check; (c) the operator has typed the captured fingerprint into the confirmation input AND it matches the captured value byte-exactly. `trustGateForPreflight(preflight)` is the pure function that decides this; `fingerprintConfirmationMatches(captured, confirmation)` is the pure function that compares the strings (case-significant — base64 is case-significant).
4. **`changed` and `revoked` outcomes refuse trust.** `changed` is a wire status and the UI surfaces it as a non-actionable refusal. `revoked` is NOT a wire status today — the backend collapses revoked-and-reappearing keys to `unknown`, then refuses the trust request with `409 conflict { entity: "host_key" }`. The UI treats `revoked` ONLY as a derived trust-rejection reason, deferred to the trust-error formatter, never as a parsed-status value. The trust-error formatter collapses `409` to a single deliberately conservative message ("the host key changed, was revoked, or no longer matches the fingerprint shown — re-run preflight before trying again") because the wire body cannot distinguish the three sub-cases.
5. **Client-side fingerprint shape check.** `isValidFingerprintShape(fp)` mirrors the backend's `validated_expected_fingerprint` (`crates/relayterm-api/src/dto/preflight.rs`): must start with `SHA256:`, length 8..=128, no whitespace or control characters. The `trustHostKey` helper refuses a malformed fingerprint with `{ kind: "validation", reason: "invalid_fingerprint_shape" }` BEFORE any wire round-trip. Backend remains authoritative.
6. **No backend changes.** The existing `POST /api/v1/server-profiles/:id/host-key-preflight` and `POST /api/v1/server-profiles/:id/trust-host-key` routes already return the wire shapes the new helpers parse; the slice is purely a frontend addition.
7. **Architecture rule preserved.** No import added under `lib/app/` touches `lib/dev/` or any renderer adapter; `appShellIsolation.test.ts` continues to pass.

**Redaction posture (load-bearing).**

- `parseHostKeyPreflightResponse` and `parseTrustHostKeyResponse` build their DTOs field-by-field, so a backend bug or hostile fixture that includes `private_key` / `encrypted_private_key` on a 200 response cannot smuggle them onto the parsed object. Sentinel-string redaction tests in `tests/hostKeyApi.test.ts` pin this for both parsers (the parsed object, `JSON.stringify` of the parsed object).
- `describePreflightError` and `describeTrustHostKeyError` are functions of `kind` + `status` + `code` ONLY. Sentinel-string tests pin that they NEVER echo the wire `message` of an HTTP error or the thrown `Error.message` of a transport failure, across `400`, `401`, `404`, `409`, `502`, `503`, and unknown statuses, plus `transport` and `malformed_response`.
- Helpers do NOT log raw response bodies. Tests pin `console.log/warn/error` as untouched across success, HTTP failure, and transport failure for both `hostKeyPreflight` and `trustHostKey`.
- Host-key fingerprints are public-ish security metadata; they are deliberately rendered. Identity-side material (encrypted blob, decrypted PEM) is never on the wire for either route, never declared on either DTO, and never reachable from the panel.

**Wire shapes (mirror of `crates/relayterm-api/src/dto/preflight.rs`).**

- Preflight request: empty body. Response (`200 OK`): `{ profile_id, host_id, hostname, port, host_key_status: "unknown" | "trusted" | "changed", host_key_type, host_key_fingerprint, message }`.
- Trust request: `{ "expected_fingerprint": "SHA256:<base64>" }`. Response (`200 OK`): `{ known_host_entry_id, host_id, host_key_type, host_key_fingerprint, trusted_at }`.

**UX copy (load-bearing).**

- Preflight disclaimer (`PREFLIGHT_DISCLAIMER`): "Preflight verifies the server's host key during SSH key exchange. It does not authenticate, does not open a terminal, and does not install your public key."
- Trust disclaimer (`TRUST_DISCLAIMER`): "Only trust if the fingerprint matches what you expect for the server. RelayTerm will not overwrite a changed or revoked host key automatically."
- `unknown` description: "Host key was captured during SSH key exchange, but no pinned entry matches it. Verify the fingerprint matches what you expect for this server before trusting it."
- `trusted` description: "Host key matches an active pinned entry. SSH authentication and terminal launch are still future work."
- `changed` description: "Host key differs from the pinned entry for this host. RelayTerm will not overwrite a pinned key automatically. This may indicate server reinstallation, key rotation, or a possible man-in-the-middle."
- The success message after a trust action explicitly disclaims auth and terminal launch: "Host key pinned. … SSH authentication and terminal launch are still future work."

**Stable selectors.** New `data-testid` hooks on `HostKeyPanel.svelte`: `host-key-panel`, `host-key-preflight-button`, `host-key-idle`, `host-key-preflighting`, `host-key-preflight-error`, `host-key-status-badge` (with `data-status` attribute), `host-key-status-description`, `host-key-fingerprint`, `host-key-already-trusted`, `host-key-changed-refused`, `host-key-confirm-input`, `host-key-confirm-mismatch`, `host-key-trust-button`, `host-key-trust-error`, `host-key-trusted-success`. The panel root carries `data-profile-id` for per-row targeting.

**Future work (explicit out-of-scope for this slice).**

Terminal session launch from the production shell; changed-host-key override / re-pin UI; revoked-entry recovery UI; password bootstrap / `ssh-copy-id`; private-key import UI; real auth UI; mobile/Tauri shell integration; backend VT observer. Each is a separate slice. (SSH auth-check UI is now wired — see "Production SSH auth-check UI" below.)
### Production SSH auth-check UI

After a host key has been pinned and trusted (preceding section), an operator can run an SSH auth-check from the production Servers view to confirm the configured `ssh_identity` actually authenticates against the target. This is NOT a terminal launch, NOT a password bootstrap, NOT a private-key import, and NOT a real auth/user-login UI.

**Scope (load-bearing — this slice).**

1. **Per-profile "Auth-check" panel** is rendered inside each profile row on `ServersView.svelte` via the `AuthCheckPanel.svelte` component, immediately below the existing `HostKeyPanel`. The panel exposes a single "Run auth-check" button, a loading indicator, a status badge keyed off the wire status (`Authenticated` / `Auth rejected` / `Host key not trusted` / `Host key changed` / `Connection failed`), a one-line operator-facing description, the `checked_at` timestamp, and — only on `authentication_succeeded` — a static success footnote that explicitly disclaims terminal launch. The panel holds local Svelte state ONLY (no global stores, no router, no polling, no auto-retry).
2. **`authCheckServerProfile(profileId, options)`** in `lib/api/serverProfiles.ts` is the single client entry point. It posts an empty JSON body to `POST /api/v1/server-profiles/:id/auth-check`, parses the response with `parseAuthCheckResponse` (a field-by-field DTO guard), and returns either `{ ok: true, check }` or `{ ok: false, error }`. It does NOT throw, does NOT log raw response bodies, and does NOT echo wire / transport detail through any user-facing string.
3. **Auth-check NEVER opens a PTY, runs a shell, executes a command, persists a terminal session, or installs the public key.** The success copy explicitly disclaims that scope so the operator cannot mistake "credentials work" for "terminal ready". `terminalLaunchWouldBeAllowed(status)` is the single pure helper that names the (currently empty) bridge to a future terminal-launch slice — it returns `true` only on `authentication_succeeded` and is advisory, not a gate.
4. **Trusted host key is a precondition, surfaced as a diagnostic outcome.** The wire `host_key_unknown` and `host_key_changed` statuses arrive as 200-OK typed `status` values, NOT HTTP errors. The UI renders them as "trust the host key first" / "the host key changed; investigate before continuing" — never as an internal error and never as a generic failure. The host-key panel above continues to be the single deliberate trust-issuance surface; auth-check never auto-runs preflight or auto-trusts.
5. **No backend changes.** The existing `POST /api/v1/server-profiles/:id/auth-check` route already returns the wire shape the new helper parses; the slice is purely a frontend addition.
6. **Architecture rule preserved.** No import added under `lib/app/` touches `lib/dev/` or any renderer adapter; `appShellIsolation.test.ts` continues to pass.

**Redaction posture (load-bearing).**

- `parseAuthCheckResponse` builds its DTO field-by-field, so a backend bug or hostile fixture that includes `private_key` / `encrypted_private_key` on a 200 response cannot smuggle them onto the parsed object. Sentinel-string redaction tests in `tests/authCheckApi.test.ts` pin this on the parsed object and on `JSON.stringify` of the parsed object.
- `describeAuthCheckError` is a function of `kind` + `status` + `code` ONLY. Sentinel-string tests pin that it NEVER echoes the wire `message` of an HTTP error or the thrown `Error.message` of a transport failure, across `401`, `404`, `500`, `503`, and unknown statuses, plus `transport` and `malformed_response`.
- The helper does NOT log raw response bodies. Tests pin `console.log/warn/error` as untouched across success, HTTP failure, and transport failure.
- The UI status formatters (`authCheckStatusLabel`, `authCheckStatusDescription`, `authCheckStatusTone`, `terminalLaunchWouldBeAllowed`, `AUTH_CHECK_DISCLAIMER`, `AUTH_CHECK_SUCCESS_FOOTNOTE`) are pure functions of `status` only — no I/O, no Svelte state, no side effects. Tests pin that none of them mention `private_key` / `encrypted_private_key` and that the success copy never implies a PTY, shell, command execution, or terminal/session readiness.

**Wire shape (mirror of `crates/relayterm-api/src/dto/auth_check.rs`).**

- Auth-check request: empty body. Response (`200 OK`): `{ profile_id, host_id, ssh_identity_id, status, message, checked_at }`. `status` ∈ `authentication_succeeded | authentication_failed | host_key_unknown | host_key_changed | connection_failed`. `message` is a static, server-supplied string keyed off `status`; the UI may render it but does not depend on its exact wording (the local `authCheckStatusDescription` helper is the single source of truth for rendered status copy).

**UX copy (load-bearing).**

- Auth-check disclaimer (`AUTH_CHECK_DISCLAIMER`): "Auth-check verifies that the configured SSH identity authenticates against the server. It requires a trusted host key first. It does not open a terminal, does not run commands, and does not install your public key."
- Success footnote (`AUTH_CHECK_SUCCESS_FOOTNOTE`): "Credentials worked for SSH authentication. Terminal launch is still a separate action and is not yet implemented in the production shell."
- `authentication_succeeded` description: explicitly disclaims PTY allocation, command execution, and terminal-launch. Phrasing: "SSH public-key authentication succeeded for the configured username. No PTY was allocated and no command was executed. Terminal launch is a separate, deliberate action."
- `authentication_failed` description: names the wrong-key / wrong-user / `authorized_keys`-not-installed diagnostic without surfacing peer banner detail.
- `host_key_unknown` description: surfaces the trust-host-key precondition explicitly ("Run host-key preflight and trust the captured fingerprint above before re-running auth-check") and never implies authentication was attempted.
- `host_key_changed` description: warns about server reinstallation, key rotation, or man-in-the-middle, and explicitly states auth was not attempted.
- `connection_failed` description: names the SSH-transport-layer cause (refused, timeout, unreachable) without leaking peer detail.
- The host-key panel's `trusted` description and `trusted` success message now point operators to the auth-check panel below: "Run auth-check below to confirm the configured SSH identity authenticates. Terminal launch is still future work."
- The Servers view header and the bottom "future work" footer are updated in lockstep so neither still claims auth-check is future work.

**Stable selectors.** New `data-testid` hooks on `AuthCheckPanel.svelte`: `auth-check-panel` (root, also carries `data-profile-id`), `auth-check-run-button`, `auth-check-idle`, `auth-check-checking`, `auth-check-error`, `auth-check-status-badge` (with `data-status` and `data-tone` attributes), `auth-check-checked-at`, `auth-check-status-description`, `auth-check-success-footnote`.

**Future work (explicit out-of-scope for this slice).**

Terminal session launch from the production shell; changed-host-key override / re-pin UI; revoked-entry recovery UI; password bootstrap / `ssh-copy-id`; private-key import UI; real auth UI; mobile/Tauri shell integration; backend VT observer; auth-check history / audit-log surfacing in the UI. Each is a separate slice.
### Production dashboard summary

The Dashboard view is now a real read-only summary instead of a single health badge. It composes existing API helpers — `checkHealth`, `listHosts`, `listServerProfiles`, `listSshIdentities`, and `listTerminalSessions` — into summary cards, a session-status breakdown, a connection-flow checklist, and a fixed set of internal navigation buttons. No new backend route, no new wire shape, no analytics, no polling.

**Scope (load-bearing — this slice).**

1. **Summary cards** render backend health, hosts count, server-profile count, SSH-identity count, and terminal-session count. Each card load is independent — one failure collapses to the card's `unavailable` state, but it does NOT poison the other cards. Counts that are still loading render as a `—` placeholder; failed loads ALSO render as `—` plus an honest "Unavailable" badge so a zero count is never confused with a failure.
2. **Sessions-by-status breakdown** sums the existing list-endpoint rows by `TerminalSessionStatus` (`active`, `detached`, `starting`, `closed`). The breakdown is reused list data — no new endpoint, no extra round-trip. A list failure collapses the breakdown to a single `Unavailable` line; an empty list renders all-zeros.
3. **Connection-flow checklist** renders seven steps in a stable order:
   1. Generate an SSH identity
   2. Install the public key on the target server
   3. Create a host
   4. Create a server profile
   5. Run host-key preflight and trust the result
   6. Run the auth-check
   7. Launch a terminal

   Steps that the inventory counts can prove (`generate-identity`, `create-host`, `create-profile`, `launch-terminal`) flip to `complete` when their underlying count is `> 0`; otherwise they stay `incomplete`. The remaining three steps — `install-public-key`, `host-key-trust`, `auth-check` — are explicitly `manual`. The dashboard does NOT have per-row state to prove a key was installed, a host key was trusted, or an auth-check passed; the checklist row tells the operator to verify from the per-resource view rather than pretending to know.
4. **Manual refresh only.** A single `Refresh` button drives both the health probe and the four inventory loads in parallel. There is no polling, no auto-refresh, no exponential backoff. The dashboard is a snapshot — operator triage stays on the per-resource views.
5. **Quick-action navigation.** A small fixed table of in-app navigation buttons (Manage servers, Manage SSH identities, Open terminal, View sessions, Configure terminal) routes through the existing AppShell `navigate(id)` helper — pure pushState, no full page reload, no route parameters. The view targets are pinned against `routing.ts` in `dashboardSummary.test.ts` so dashboard CTAs and the production route table cannot drift out of sync.
6. **No backend changes.** No new HTTP route, no new WebSocket frame, no new DTO. The slice is purely a frontend addition on top of the existing inventory + health surface.

**Architecture rule preserved.** The new helper module lives at `apps/web/src/lib/app/dashboard/dashboardSummary.ts`; the view stays at `apps/web/src/lib/app/views/DashboardView.svelte`. No imports from `lib/dev/` and no imports from any `@relayterm/terminal-*` adapter package. `appShellIsolation.test.ts` continues to enforce both bans.

**Redaction posture (load-bearing).**

- The helper consumes already-typed DTOs (`Host`, `ServerProfile`, `SshIdentity`, `TerminalSession`) — never raw wire bodies. The DTO parsers in `lib/api/` already drop `private_key` / `encrypted_private_key` / unknown fields; the dashboard helper cannot reintroduce them because nothing copies fields off `unknown`.
- The dashboard does NOT echo the wire `message` of an HTTP error or the thrown `Error.message` of a transport failure. Per-card failure surfaces as a static `Unavailable` label only.
- The helper does NOT log raw response bodies. The `Refresh` button is the single user-visible signal that a load happened.
- The checklist's manual-row copy is asserted in `dashboardSummary.test.ts` against banned phrases ("host-key trusted", "auth-check passed", "key installed", "ready to launch") so a future copy edit cannot smuggle an implication that the dashboard cannot prove.

**Stable selectors (additions only).** `dashboard-refresh`, `dashboard-summary-cards`, `dashboard-card-{health,hosts,profiles,identities,sessions}`, `dashboard-card-{...}-status`, `dashboard-card-{...}-cta`, `dashboard-count-{hosts,profiles,identities,sessions}`, `dashboard-health-probe`, `dashboard-session-breakdown`, `dashboard-session-status-{active,detached,starting,closed}`, `dashboard-session-loading`, `dashboard-session-unavailable`, `dashboard-sessions-cta`, `dashboard-setup-checklist`, `dashboard-checklist-{generate-identity,install-public-key,create-host,create-profile,host-key-trust,auth-check,launch-terminal}`, `dashboard-checklist-{...}-status`, `dashboard-checklist-{...}-cta`, `dashboard-nav-actions`, `dashboard-nav-{manage-servers,manage-identities,open-terminal,view-sessions,configure-terminal}`. The pre-existing `dashboard-inventory-counts` / `dashboard-counts-refresh` / `dashboard-counts-error` selectors are removed by this slice — the new card grid replaces the legacy three-column inventory tile.

**Checklist limitations (load-bearing — operators read this).**

- A `complete` mark on the count-inferable rows proves only that the corresponding row exists in your inventory. It does NOT prove that the host is reachable, that the SSH identity matches the target, or that the next terminal launch will succeed.
- The `manual` rows do NOT reflect any state the dashboard can observe today. The dashboard cannot tell whether a public key was installed, whether a host-key fingerprint is trusted, or whether the most recent auth-check passed. Future API surfaces may expose that state — at which point the relevant row would graduate from `manual` to count- or flag-inferable; this slice does not anticipate the schema.
- A `launch-terminal` mark of `complete` means a terminal session has been launched at least once. It is NOT a readiness signal for a new launch.

**Future work (explicit out-of-scope for this slice).**

Backend exposure of host-key trust state and last-auth-check outcome on the profile DTO; a checklist that promotes those rows from `manual` to inferable; charts / time-series widgets; admin / cross-user reporting; auto-refresh / polling; mobile-specific dashboard layout; setup-wizard UX (step-by-step flow); terminal launch directly from dashboard; host-key trust / auth-check directly from dashboard; URL-driven dashboard parameters. Each is a separate slice.

### Dashboard recent activity

The Dashboard view also surfaces a compact **Recent activity** section that reuses the existing read-only current-user audit feed (`GET /api/v1/audit-events/recent`). It is a snapshot designed to make the most recent server-profile lifecycle events (and any other current-user audit events) visible from the landing view without forcing the operator into Settings. No new backend route, no new DTO, no admin / cross-user view.

**Scope (load-bearing — this slice).**

1. **Source.** The section reuses `listRecentAuditEvents` from `apps/web/src/lib/api/auditEvents.ts` with `limit: 5`. The frontend never exposes the raw payload JSON; events are rendered through the same `summarizeAuditEvent` helper as the Settings panel. Unknown wire kinds collapse to a generic "Audit event" line.
2. **Bounded count.** The dashboard caps the rendered list at `DASHBOARD_RECENT_ACTIVITY_LIMIT = 5` (pinned in `dashboardSummary.test.ts`). The Settings `RecentActivityPanel` continues to request `limit: 20` — the dashboard intentionally renders fewer rows so it stays a snapshot, not a feed.
3. **Independent failure.** The audit fetch is its own load slot. A 401 / transport blip on the audit feed must NOT poison the inventory cards or the health probe. The section renders one of `loading` / `ready` (with rows or empty-state) / `error` and nothing else.
4. **Manual refresh only.** Two refresh paths exist: (a) the dashboard-wide `Refresh` button drives the health probe, the four inventory loads, AND the audit fetch in parallel; (b) the section's own `Refresh` affordance re-fetches activity alone, leaving the rest of the dashboard untouched. There is no polling, no auto-refresh, no retry storm.
5. **Navigation to Settings.** A `View all →` button uses the existing AppShell `onNavigate(AppViewId)` path to jump to the Settings view, which hosts the fuller `RecentActivityPanel`. The dashboard does NOT introduce route parameters and does NOT trigger a full-page reload. The target is pinned against `routing.ts` so the link cannot silently drift to a placeholder.
6. **No backend changes.** No new route, no new DTO, no new audit kind. The slice is purely a frontend composition on top of the existing audit read API.
7. **No admin / cross-user view.** This section, like the existing Settings panel, is the current-user audit feed only. Cross-user / admin reporting, search, filter, export, and audit-payload detail panes are deliberate later slices.

**Architecture rule preserved.** The helper module is `apps/web/src/lib/app/dashboard/dashboardSummary.ts` (extended with `summarizeRecentActivity`, `activitySectionFromLoad`, `DASHBOARD_RECENT_ACTIVITY_LIMIT`, and `RecentActivitySection` / `RecentActivityLine` types). The view stays at `apps/web/src/lib/app/views/DashboardView.svelte`. No imports from `lib/dev/` and no imports from any `@relayterm/terminal-*` adapter package. `appShellIsolation.test.ts` continues to enforce both bans.

**Redaction posture (load-bearing).**

- The dashboard renders only fields that have already passed through `parseAuditEvent` (which builds the structured `AuditPayloadSummary` field-by-field). Smuggled `private_key` / `encrypted_private_key` / `client_info` / `remote_addr` / `user_agent` / `session_output` / `access_token` keys cannot survive — pinned by sentinel-string tests in `dashboardSummary.test.ts` against the rendered `RecentActivityLine` JSON.
- The dashboard does NOT show actor identifiers (`actor_id`), remote addresses, user-agent strings, or any raw payload JSON. The visible row carries: a safe summary string (kind label + lifecycle profile name when present) and a formatted timestamp.
- Error states use `describeLoadError("audit events", err)` only — the helper never echoes the wire `message` of an HTTP error or the thrown `Error.message` of a transport failure. Pinned with a sentinel string in `activitySectionFromLoad` tests.
- The helper does NOT log raw response bodies. Operator detail belongs in server logs, not the browser console.

**Stable selectors (additions).** `dashboard-recent-activity`, `dashboard-recent-activity-refresh`, `dashboard-recent-activity-view-all`, `dashboard-recent-activity-loading`, `dashboard-recent-activity-error`, `dashboard-recent-activity-empty`, `dashboard-recent-activity-list`, `dashboard-recent-activity-row` (carries `data-kind` set to the wire `AuditEventKind` tag).

**Future work (explicit out-of-scope for this slice).**

Cross-user / admin audit views, audit search / filter / export, audit-payload detail modals, raw-payload expansion, polling / auto-refresh, charts / time-series widgets, audit-by-resource drill-downs, mobile-specific dashboard layout, route-parameter-driven activity filtering, and audit pagination. Each is a separate slice.
### Server profile disable / enable backend (landed)

**Status:** schema, repository, API, and launch / setup-action guards are wired. Audit-event emission is intentionally deferred — see "Audit gap (deferred)" below. Frontend disable / enable UI remains future work and is unchanged today; the production shell still renders inventory read-only.

**Schema.** `server_profiles.disabled_at TIMESTAMPTZ NULL`, no default (migration `20260501000011_server_profiles_disabled_at.sql`). Existing rows are enabled (NULL). Column is **not** indexed in this slice — list filtering by `disabled_at` is not yet a hot path.

**Domain + DTO.** `relayterm_core::server_profile::ServerProfile.disabled_at: Option<DateTime<Utc>>` plus `is_disabled() -> bool`. `ServerProfileResponse.disabled_at` is **always serialised** (`null` when absent) so clients can rely on the field's presence. Frontend `parseServerProfile` accepts a string or `null`, treats a missing field as `null` for forward compatibility, and rejects wrong-shape values to prevent silent drift.

**Endpoints.**

- `POST /api/v1/server-profiles/:id/disable` — stamps `disabled_at = NOW()`. Owner-scoped; foreign / missing ids return a byte-identical 404. Idempotent: a redundant disable returns the existing row unchanged (the original `disabled_at` is preserved — bumping it on a no-op call would be misleading).
- `POST /api/v1/server-profiles/:id/enable` — clears `disabled_at`. Same ownership / idempotency contract.
- Both routes return the updated `ServerProfileResponse` body. Neither route accepts a request body in this slice.

**Failure modes.** `401 unauthorized` when the session cookie is missing or invalid (`AuthenticatedUser` extractor short-circuits). `404 not_found` for a missing or foreign-owned profile (cross-user 404 is byte-identical to a genuine 404). `500 internal_error` for repository / database failures (static body, never echoes SQL).

**Setup-action and launch-time guards.** A profile with `disabled_at IS NOT NULL` refuses these dependent actions with `409 conflict` and the wire message `"server_profile disabled"` (the `code` stays `conflict`):

- `POST /api/v1/terminal-sessions` (launch). The wire `entity` reads `server_profile`; `reason` reads `disabled`. Existing live sessions are unaffected — see "Active session at the moment of profile disable" in the policy section above.
- `POST /api/v1/server-profiles/:id/auth-check`.
- `POST /api/v1/server-profiles/:id/host-key-preflight`.
- `POST /api/v1/server-profiles/:id/trust-host-key`.

Preflight refuses (rather than allowing a read-only probe) so the disabled state is uniformly closed across every dependent action; re-enabling the profile is the explicit return path. The trust route additionally guards against a sneaky bypass where a disabled profile is "re-blessed" without an explicit enable.

**WebSocket attach.** `GET /api/v1/terminal-sessions/:id/ws` does **not** re-check the underlying profile's `disabled_at`. An already-created session row is reachable until it closes via the standard lifecycle paths (operator close, remote shell exit, PTY teardown, TTL expiry). Disable is a launch-time gate, not a runtime kill switch; reapplying it across the live wire would surprise an active operator and serve no security purpose (the SSH transport is already pinned to the credentials in flight).

**Audit emission (landed).** See "Server profile lifecycle audit" below for the full contract. Server profile **create** and the **disable** / **enable** *transitions* each append one row to `audit_events` with public metadata only. The `update` and `delete` routes do not exist yet and therefore do not audit; when they land, they MUST follow the same payload contract and idempotency rules.

**ApiError shape.** `ApiError::Conflict` now carries `entity: &'static str` AND `reason: Option<&'static str>`. The wire envelope still uses `code: "conflict"`; when `reason` is `Some(r)` the message becomes `"{entity} {r}"`. When `reason` is `None` the message keeps the historical `"{entity} conflict"` form so existing clients (and pinned tests for `host_key conflict`, `terminal_session conflict`, etc.) continue to parse byte-identically.

### Server profile lifecycle audit

**Status:** schema, domain, and API emission landed for all five server-profile lifecycle kinds. The kinds emitted today are `server_profile_created` (POST), `server_profile_updated` (PATCH), `server_profile_disabled` / `server_profile_enabled` (POST disable/enable), and `server_profile_deleted` (DELETE). The PATCH and DELETE routes shipped with the inventory-management mutation slice (commit `f1f0691`, staging-smoked 2026-05-12).

**Schema.** Migration `20260501000012_audit_events_lifecycle_kinds.sql` extends the `audit_events_kind_chk` CHECK with `server_profile_disabled` and `server_profile_enabled` (strict superset; no rows invalidated). The matching variants land on `relayterm_core::audit_event::AuditEventKind` with snake_case wire tags pinned by unit tests in `audit_event.rs`.

**Emission points.**

- `POST /api/v1/server-profiles` — on a successful create, appends one `server_profile_created` row.
- `PATCH /api/v1/server-profiles/:id` — on a successful update, appends one `server_profile_updated` row.
- `POST /api/v1/server-profiles/:id/disable` — appends one `server_profile_disabled` row **only on the enabled → disabled transition**. A redundant disable (already-disabled row) returns the existing row unchanged and writes NO audit event.
- `POST /api/v1/server-profiles/:id/enable` — appends one `server_profile_enabled` row **only on the disabled → enabled transition**. A redundant enable returns the existing row unchanged and writes NO audit event.
- `DELETE /api/v1/server-profiles/:id` — appends one `server_profile_deleted` row BEFORE the DELETE so the audit row exists even if the DELETE later fails. Refused 409 attempts (any `terminal_sessions` reference) short-circuit BEFORE the audit append and write NO audit event (matches the AGENTS.md "Things to avoid" idempotent / refused-call rule).
- 401 / 404 paths (cross-user / missing id) write NO audit event. Otherwise the audit log would expose existence by id.

**Payload contract (security-critical).** The JSON object on every emitted row is built field-by-field from a single helper (`write_lifecycle_audit` in `routes/v1/server_profiles.rs`) and contains only public metadata:

```jsonc
{
    "server_profile_id": "<uuid>",
    "name":              "<profile name>",
    "host_id":           "<uuid>",
    "ssh_identity_id":   "<uuid>",
    "disabled_at":       "<rfc3339 timestamp> | null"
}
```

The payload MUST NOT contain: `private_key`, `encrypted_private_key`, plaintext key bytes, public-key bytes, terminal I/O (input keystrokes, output bytes, replay frames), the `client_info` blob from `terminal_session_attachments`, peer banners, raw russh error text, vault internals, or DB error text. Sentinel-string redaction tests in `crates/relayterm-api/tests/api.rs` (the `AUDIT_FORBIDDEN_SUBSTRINGS` helper) pin this on every emission path.

**Failure policy: fail-closed.** If the audit insert fails after the lifecycle row write, the route returns `500 internal_error` to the caller. The wire body is the static `internal error` message; the underlying SQL / driver detail is logged operator-side only and never echoed to the client. The lifecycle row state (the `server_profiles` insert / the `disabled_at` stamp / clear) is already committed by the time the audit insert runs — this matches the partial-success shape documented for `create_session` in AGENTS.md (2026-04-29 lesson). The orphan `server_profiles` row is operator-visible and reconcilable; the audit gap cannot be reconstructed after the fact, so surfacing the failure is preferable to silently dropping it.

**`remote_addr`.** The `audit_events.remote_addr` column is intentionally `NULL` for these rows in this slice. Client IP / user-agent capture across the API surface is its own deferred refactor (see "Out of scope (v1)") — this slice does not introduce a one-off route-level capture path.

**Reasoning.** Lifecycle audit rows are forensic primitives. Their value depends on `(actor, kind, target_id, recorded_at)` being trustworthy and free of secret-shaped fields. The payload deliberately avoids `tags`, `username_override`, host bag-of-fields, and identity public-key bytes — all of which are reachable via standard inventory queries scoped to the `actor_id`. Audit history is not a denormalised inventory snapshot; it is a transition log.

### Current-user audit events read API (landed)

**Status:** read-only `GET /api/v1/audit-events/recent` route plus a small "Recent activity" panel on the production Settings view. This slice is deliberately **not** an admin / cross-user audit viewer; admin tooling, search, filtering, export, retention, and payload-detail expansion remain future work.

**Scope (load-bearing).**

- **Current-user only.** Rows are filtered at the SQL layer by `actor_id = caller` via `AuditEventRepository::recent_for_actor`. There is no `actor_id` query parameter, no admin route, no aggregation surface.
- **NULL-actor exclusion.** Pre-auth events with `actor_id IS NULL` (failed-login attempts, unauthenticated probes) are NOT visible on this route. An admin surface that wants those uses `AuditEventRepository::recent` directly when it lands; this route MUST NOT relax the SQL filter.
- **Limit clamping.** `?limit=N` is clamped to `1..=100`; default is `20`. Out-of-range values are clamped silently rather than 400'd — the limit is a UI hint, not load-bearing input. The clamp is in `routes/v1/audit_events::clamp_limit` with a unit-test table.
- **No raw payload.** Responses go through `AuditEventResponse::from_event` (`crates/relayterm-api/src/dto/audit_event.rs`), which maps each known `AuditEventKind` onto a closed allow-list of safe public fields. Unknown kinds collapse to a generic summary that carries no payload data at all.
- **`actor_id` and `remote_addr` are dropped from the wire.** The caller IS the actor; re-emitting `actor_id` would invite a future drift where a cross-user row leaks via copy-paste. `remote_addr` exposure is a separate slice (client IP / user-agent capture across the API surface).

**Wire shape.** `AuditEventResponse`:

```jsonc
{
    "id":          "<uuid>",
    "kind":        "<snake_case AuditEventKind tag>",
    "recorded_at": "<rfc3339 timestamp>",
    "summary": {
        "kind": "server_profile_lifecycle",
        "server_profile_id": "<uuid> | null",
        "name":              "<string> | null",
        "host_id":           "<uuid> | null",
        "ssh_identity_id":   "<uuid> | null",
        "disabled_at":       "<rfc3339> | null"
    }
}
```

For audit kinds without an explicit sanitizer arm, `summary` collapses to `{ "kind": "generic" }` with no other fields. Per-kind sanitizer arms are added explicitly: each new kind that grows a public surface must (1) extend `AuditPayloadSummary`, (2) wire it in `sanitize_payload`, and (3) add a redaction-sentinel test that constructs an event whose payload contains every name in `AUDIT_FORBIDDEN_SUBSTRINGS` and asserts the serialised DTO contains none of them.

**Redaction contract (security-critical).** The DTO MUST NOT carry `private_key`, `encrypted_private_key`, plaintext PEM bytes, public-key bytes, terminal I/O, replay frames, peer banners, raw russh / transport / SQL error text, vault internals, `client_info` blobs, `remote_addr`, `user_agent`, or any payload field not explicitly allow-listed. Sentinel-string tests at three layers pin this:

1. `crates/relayterm-api/src/dto/audit_event.rs` — sanitizer-level tests serialise the DTO and assert no forbidden substring appears.
2. `crates/relayterm-api/tests/api.rs::audit_events_recent_redacts_secret_shaped_payload_fields` — route-level test that constructs an audit row whose payload smuggles every forbidden name and asserts the response body strips them.
3. `apps/web/tests/auditApi.test.ts` — frontend `parseAuditEvent` drops top-level smuggled fields, falls back to a `generic` summary on unknown summary variants (forward-compatibility for a backend that ships a new sanitizer arm before the frontend updates), and rejects malformed top-level shape.

**Unauthorized.** A request without a valid `relayterm_session` cookie is rejected by the `AuthenticatedUser` extractor before the route runs. Pinned by `audit_events_recent_unauthorized_without_session_cookie`.

**Empty list semantics.** A user with no audit history sees `200 []` (not `404`). Empty is the steady state for a fresh account.

**Frontend surface.** `apps/web/src/lib/api/auditEvents.ts` exposes `listRecentAuditEvents({ limit? })`, `parseAuditEvent`, `describeAuditEventKind`, and `summarizeAuditEvent`. The "Recent activity" panel (`apps/web/src/lib/app/views/RecentActivityPanel.svelte`) renders inside `SettingsView` with explicit loading / empty / error / ready states and a manual `Refresh` button. There is no polling, no auto-retry, and no payload-expansion affordance. Errors collapse through `describeLoadError("audit events", err)` so transport / operator detail cannot leak into the rendered string.

**Stable selectors (additions only).** `settings-recent-activity` (root article), `settings-recent-activity-refresh` (manual refresh button), `settings-recent-activity-loading`, `settings-recent-activity-error`, `settings-recent-activity-empty`, `settings-recent-activity-list` (the `<ul>` once events have loaded), `settings-recent-activity-row` (each `<li>`). Each row also carries a `data-kind` attribute set to the wire `kind` tag for smoke targeting; the value is a public taxonomy label and contains no operator data.

**Out of scope for this slice.** Admin / cross-user audit view, audit search, audit filtering, audit export, retention / sweeper, raw JSON payload expansion, client IP / user-agent capture refactor, payload sanitizers for kinds beyond the server-profile lifecycle trio.

### Server profile disable / enable UI (landed)

**Status:** wired in `apps/web/src/lib/app/views/ServersView.svelte`. The disable / enable surface is the first user-driven destructive-side action in the production shell; the rest of the inventory (hosts, SSH identities, known-hosts) remains read-only with no destructive UI.

**Scope.** Disable an enabled `server_profile` AND re-enable a disabled one. NOT in scope for this slice: delete UI for any inventory entity, host disable/delete UI, SSH identity disable/delete UI, known-host revoke UI, an audit viewer, admin tooling, multi-tab workspace, or any backend behavior change.

**API surface.** `disableServerProfile(profileId)` and `enableServerProfile(profileId)` in `apps/web/src/lib/api/serverProfiles.ts` POST to the existing backend routes. Both reuse `parseServerProfile` so `disabled_at` parsing stays centralised. Errors are formatted via `describeLifecycleError(action, err)` — a function of `kind` + `status` + `code` only; never echoes wire `message` or transport `Error.message`. Redaction-sentinel tests in `tests/profileLifecycle.test.ts` pin that a 200 response carrying a `private_key` / `encrypted_private_key` field cannot reach the parsed `ServerProfile` object.

**List badge + detail panel.** Each row in the Servers profile list emits a `data-profile-disabled` attribute and a small `disabled` badge next to the name when `disabled_at` is non-null. The detail panel renders a `Lifecycle` row carrying an `enabled` / `disabled` badge plus the `disabled_at` timestamp on disabled profiles, AND an inline disabled-profile note that names the gate ("New terminal launches, host-key preflight / trust, and auth-check are blocked. Existing live sessions are unaffected."). Disabled profiles are NOT hidden by default; the existing client-side search and tag filters continue to include them.

**Disable controls.** An enabled profile renders a `Disable profile` button in its row. Clicking opens a confirmation panel that:

- States the gate explicitly (new launches blocked, host-key preflight / trust / auth-check blocked, existing live sessions unaffected).
- Requires the operator to type the profile name verbatim before the disable submit becomes enabled (`disableConfirmationMatches` from `lib/app/inventory/profileLifecycle.ts`). The comparison is strict — case- and whitespace-sensitive — so the confirmation is deliberate but lightweight.
- Carries a `Cancel` button so the operator can back out without firing a request.
- Submits via `disableServerProfile` and replaces the matching row in the in-memory list from the parsed response. No automatic refetch is required; the backend response is the canonical post-disable shape.

The confirmation copy is static and never interpolates profile-specific data, so a hostile profile name cannot reach the rendered paragraph; sentinel-string tests pin this.

**Enable controls.** A disabled profile renders an `Enable profile` button gated only by an explicit click and a static reminder ("Enabling permits setup and launch attempts again. It does NOT prove host-key trust or auth readiness — re-run preflight, trust the host key, and re-run auth-check before launching."). On submit, the row is replaced in-memory from the parsed response and the disabled badge clears.

**Setup-action gating in UI.** While a profile is disabled:

- The `Launch terminal` button is rendered disabled with an honest tooltip and the inline copy switches to "Re-enable this profile to start a new terminal session." This mirrors the backend's `409 conflict { entity: "server_profile", reason: "disabled" }` and prevents the operator from racing into a rejected POST.
- `HostKeyPanel` accepts a `disabled` prop, renders an inline `Profile is disabled. Host-key preflight and trust are blocked until the profile is re-enabled.` notice, AND keeps the preflight button disabled. The same pattern applies to `AuthCheckPanel`.
- These guards are local to the UI and not relied on for security — the backend remains authoritative. The UI mirror exists so a disabled profile never offers an action the backend will refuse.

**Existing live sessions.** Disabling a profile does NOT close, kill, or otherwise touch its existing `terminal_sessions`. The UI copy names this guarantee in the disable confirmation, the row notice, and the detail panel note. The Sessions view continues to render live sessions whose underlying profile has been disabled (see "Sessions view list & per-row state").

**Idempotency.** A redundant disable on an already-disabled row (or enable on an already-enabled row) returns the same row from the backend; the UI replaces it in place and clears the lifecycle state. Concurrent UI clicks are guarded by the per-row `submitting` lifecycle state.

**Errors.** `describeLifecycleError` collapses 404 to `"server profile not found"`, 401 to `"not authenticated"`, transport failures to `"transport error"`, and parse failures to `"malformed response"`. The error banner is dismissable via a per-row `Dismiss` button so the operator can retry from a clean state without reloading.

**Future work this slice does NOT do.** No delete UI, no host or SSH identity lifecycle UI, no known-host revoke UI, no audit viewer, no terminal-session kill on profile disable, no admin tooling. Those remain as separate slices per the policy section above.
### Future implementation order

This is the recommended staged plan. Each item is its own slice; do not bundle. Earlier items unblock later items.

1. **~~Add `disabled_at TIMESTAMPTZ NULL` to `server_profiles`~~ (LANDED).** Migration, domain model, DTO, and frontend parser all carry the field. See "Server profile disable / enable backend (landed)" above. The "third state" guidance still applies: graduate to a `status` text column only if a third state (e.g. `archived`) becomes necessary.
2. **~~Backend route `POST /api/v1/server-profiles/:id/disable` (and paired `:id/enable`)~~ (LANDED).** Idempotent, owner-scoped, `AuthenticatedUser`-gated. ~~Audit-event emission is deferred~~ Audit-event emission landed alongside `server_profile_created`; see "Server profile lifecycle audit" above for the kinds, payload contract, idempotency rules, and fail-closed failure policy.
3. **~~Launch-time guard on `POST /api/v1/terminal-sessions`~~ (LANDED).** Plus parallel guards on `auth-check`, `host-key-preflight`, and `trust-host-key`. Existing live sessions keep running. WebSocket attach is intentionally not gated — disable is a launch-time gate, not a runtime kill switch.
4. **~~Frontend disable / enable UI~~ (LANDED).** Server-profile lifecycle controls live on the Servers view: per-row `Disable profile` / `Enable profile` actions, name-echo confirmation for disable, inline disabled badge on row + detail panel, gated launch / preflight / trust / auth-check affordances, safe error formatter, and redaction-sentinel tests on the API path AND the static confirmation copy. See "Server profile disable / enable UI (landed)" above. An audit viewer remains future work — the backend already emits the rows, but no read surface exists.
5. **~~Backend route `DELETE /api/v1/server-profiles/:id`~~ (LANDED).** Refuses with `409 conflict { entity: "server_profile", reason: "referenced" }` when any `terminal_sessions` row (live OR closed) references the profile. Owner-scoped 404 collapses cross-user existence checks. Writes a `server_profile_deleted` audit row BEFORE the DELETE so the row exists even if the DELETE later fails. The wire envelope uses `reason: "referenced"` rather than a row-count field — counting referencing sessions would be a side-channel into history the caller may not own. The paired `PATCH /api/v1/server-profiles/:id` also landed (`server_profile_updated` audit on success).
6. **~~Backend route `DELETE /api/v1/hosts/:id`~~ (LANDED).** Refuses with `409 conflict { entity: "host", reason: "referenced" }` when **either** an owned `server_profiles` row **or** any `known_host_entries` row references the host — the route-layer `any_dependents_for_user` predicate is a single short-circuit OR across both refs. The schema FK `known_host_entries.host_id ON DELETE CASCADE` is intentionally unreachable from the user-facing surface (refusing the delete preserves pinned-trust history as a deliberate property; see SPEC.md). No audit row is written — the `host_*` kinds are deliberately absent from the schema CHECK constraint and adding them is a separate slice. The paired `PATCH /api/v1/hosts/:id` also landed.
7. **~~Backend route `DELETE /api/v1/ssh-identities/:id`~~ (LANDED).** Refuses with `409 conflict { entity: "ssh_identity", reason: "referenced" }` when any owned `server_profiles` row references the identity. Writes `ssh_identity_deleted` audit BEFORE the DELETE; payload is public metadata only (`id`, `name`, `key_type`, `fingerprint_sha256`, `created_at`) and NEVER includes `encrypted_private_key`, `public_key` bytes, or PEM. The encrypted private-key bytes go away with the row; no separate wipe step is needed. The paired `PATCH /api/v1/ssh-identities/:id` is rename-only — `key_type`, `public_key`, and `encrypted_private_key` are immutable; no `ssh_identity_updated` audit kind exists.
8. **Inline edit / delete UI on the Servers view.** SSH-identity rename + delete are wired in the Identities view today. Host edit / delete and server-profile edit / delete API helpers are landed and unit-test-pinned (`apps/web/tests/inventoryMutationsApi.test.ts`); calling them from `ServersView.svelte` (inline row affordances with name-echo confirmation for destructive actions, mirroring the disable-flow pattern) is the next slice. Browser-driven smoke of the full SPA mutation flows is a follow-up — staging smoke today is API-driven only.
9. **Backend known-host revoke route** (e.g. `POST /api/v1/hosts/:id/known-hosts/:entry_id/revoke`). Stamps `revoked_at`. Writes `host_key_revoked` audit event (kind already exists). Owner-scoped via the host's `owner_id`. Once revoke is wired, host delete can land an admin-only opt-in path that cascades through revoked-but-not-deleted pins; today the user-facing host delete refuses on any pin reference (revoked or not).
10. **`host_*` audit kinds** — paired migration to extend `audit_events_kind_chk` with `host_created` / `host_updated` / `host_deleted`, paired Rust enum variants, paired sanitizer arms in `AuditPayloadSummary`, paired sentinel-string tests. Once the kinds exist, the existing PATCH/DELETE/POST host routes wire emission. Optional `ssh_identity_updated` follows the same pattern.
11. **Stale-row sweepers and admin tooling** — operator surface for `starting` rows that survived a backend restart, orphaned attachments, and very-old closed sessions. Explicit, audit-logged, never silent. Out of scope for v1.
12. **Operator unrevoke and admin hard-delete** of `known_host_entries` / closed `terminal_sessions` — admin-only, audit-logged, deliberately later.

Each step's "definition of done" inherits the standard checklist (tests, sqlx prepare on schema change, audit event reachable, owner-scoping, redaction posture). When the first destructive route lands, append an "Encountered Lessons" entry in AGENTS.md if any non-obvious gotcha emerged (FK ordering, audit-payload surface, dialog redaction).
