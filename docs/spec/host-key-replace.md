# Host-key replace (revoke-and-replace) — design

> Status: **Phase 1 + Phase 2 + Phase 3 + Phase 4 implemented;**
> Phase 5 staging smoke remains deferred.
>
> Phase 1 landed as `feat/known-host-revoke-metadata`:
> - Migration `20260510000022_known_host_entries_revoke_metadata.sql`
>   adds `revoked_by`, `revoked_reason_code`, `replaced_by_id`, plus
>   the `known_host_entries_revoked_reason_chk` and
>   `known_host_entries_revoked_columns_set_together` CHECK
>   constraints.
> - `KnownHostRevocationReason` enum
>   (`crates/relayterm-core/src/known_host.rs`) and the repository
>   inputs `ReplaceActivePin` / `ReplacedKnownHostEntries`
>   (`crates/relayterm-core/src/repository.rs`).
> - `KnownHostEntryRepository::replace_active_pin` trait method +
>   `PgKnownHostEntryRepository::replace_active_pin` impl
>   (`crates/relayterm-db/src/repositories/known_host_entry.rs`),
>   transactional (`SELECT … FOR UPDATE` → INSERT new → UPDATE old).
> - Repository tests in `crates/relayterm-db/tests/repositories.rs`
>   cover happy path, fingerprint-mismatch, no-active-pin,
>   already-revoked old pin, previously-revoked new fingerprint,
>   all four reason codes, scoping, and both CHECK constraints.
>
> Phase 2 landed as `feat/replace-host-key-route`:
> - Route `POST /api/v1/server-profiles/:id/replace-host-key`
>   (`crates/relayterm-api/src/routes/v1/server_profiles.rs::replace_host_key`)
>   wired with the canonical `_csrf: CsrfGuard` → `AuthenticatedUser`
>   → `Path` → `Json` extractor order. Owner-scoped resolve via the
>   shared `resolve_owned_profile` helper, so foreign / missing
>   profiles collapse to byte-identical 404. Disabled profiles refuse
>   with the standard `server_profile disabled` 409 (mirrors the
>   trust / preflight / auth-check / launch guards).
> - Request / response DTOs `ReplaceHostKeyRequest` /
>   `ReplaceHostKeyResponse` and the validator
>   `ReplaceHostKeyRequest::validated()`
>   (`crates/relayterm-api/src/dto/preflight.rs`) enforce the
>   `SHA256:<base64>` fingerprint shape and the four-tag reason-code
>   accept-list AT THE BOUNDARY before any DB / network work.
> - **Atomicity decision: option (a) per § R7.** The repository's
>   `replace_active_pin` now emits the paired `host_key_revoked` +
>   `host_key_accepted` audit rows inside the same transaction as
>   the row mutations. An audit-insert failure ROLLBACKs the row
>   writes; replace + audit land together or neither does. This
>   mirrors `TerminalRecordingRepository::purge_for_retention`
>   exactly. The `ReplaceActivePin` input shape did NOT need to grow:
>   the repository assembles both audit payloads field-by-field from
>   the primitives already on the input (`host_id`, `revoked_by`,
>   `reason_code`, the resulting `revoked_old` / `trusted_new` rows).
> - **Audit payload (host-anchored, public-only).** Each row carries
>   `actor_id = revoked_by` and a payload of `{ host_id,
>   known_host_entry_id, replacement_known_host_entry_id,
>   old_fingerprint, new_fingerprint, key_type, reason_code }`. The
>   two payloads cross-link via `replacement_known_host_entry_id`
>   (each row's `known_host_entry_id` is the row it is "about"; the
>   `replacement_*` field is the counterparty). NO public-key bytes,
>   NO hostnames / ports / banners, NO operator-supplied free text,
>   NO russh / DB error text. Sentinel-tested against
>   `AUDIT_FORBIDDEN_SUBSTRINGS` on every replace path.
> - **Race-safety / preflight re-check.** The route runs a fresh
>   `state.preflight.preflight(...)` inside the handler and asserts
>   `captured_unchanged` / `captured_mismatch` / `captured_revoked`
>   BEFORE calling the repository. The repository then
>   `SELECT … FOR UPDATE`s the active row inside the open
>   transaction — two concurrent replaces collapse to "exactly one
>   succeeds" with no double-revoke and no double-trust.
> - **Wire failure modes** (per § R4):
>   - `400 invalid_input`: malformed fingerprint shape OR reason code
>     outside the four-tag accept-list.
>   - `401 unauthorized`: missing / invalid session cookie.
>   - `403 csrf_origin_mismatch`: shared CsrfGuard fires before any
>     DB / body work (pinned by
>     `replace_host_key_returns_403_with_bad_origin_before_body_parse`).
>   - `404 not_found`: missing OR foreign-owned profile, byte-
>     identical body / status.
>   - `409 server_profile disabled`.
>   - `409 host_key active_pin_mismatch`: the active row's
>     fingerprint does not match `expected_old_fingerprint` (or no
>     active pin exists — collapsed by the repository's
>     `FOR UPDATE` select per design).
>   - `409 host_key captured_unchanged`: probe matched the active
>     pin (no-op).
>   - `409 host_key captured_mismatch`: probe captured a different
>     fingerprint than `expected_new_fingerprint`.
>   - `409 host_key captured_revoked`: a revoked row already exists
>     for the captured fingerprint.
>   - `409 host_key new_fingerprint_already_active`: forward-compat
>     for a future code path that races a duplicate trust through
>     this primitive (currently impossible because of the
>     `captured_*` checks above).
>   - `502 bad_gateway`: probe failure (static `"bad gateway"` body
>     so peer banners / topology never leak).
>   - `503 service_unavailable`: vault disabled.
> - **Test coverage** (`crates/relayterm-api/tests/api.rs`,
>   `replace_host_key_*`): happy path with paired-audit assertion +
>   sentinel scan; each conflict reason; 400 on each malformed shape;
>   401 / 403 / 404; 502 / 503; profile-disabled; the trust route
>   still refuses `changed` keys (TOFU posture pin); foreign-owned
>   404 byte-identical to a genuine 404; `assert_no_replace_audit`
>   helper proves NO `host_key_*` audit row lands on any refused
>   replace. Repository tests
>   (`crates/relayterm-db/tests/repositories.rs`,
>   `replace_active_pin_*`) gain an audit-shape assertion and an
>   atomic-rollback assertion (forced via an FK violation on a
>   phantom `revoked_by`).
> - **Pre-existing test-fixture fix:** Phase 1's `revoke_entry` test
>   helper in `crates/relayterm-api/tests/api.rs` only set
>   `revoked_at`, which violates the
>   `known_host_entries_revoked_columns_set_together` CHECK added in
>   the same migration. The helper now writes the full triple
>   (`revoked_at`, `revoked_by`, `revoked_reason_code = 'operator_other'`)
>   and the previously-broken
>   `trust_host_key_refuses_to_re_trust_a_revoked_fingerprint` /
>   `preflight_treats_revoked_match_as_unknown` /
>   `auth_check_blocks_when_host_key_revoked` /
>   `terminal_session_create_blocks_revoked_pin` tests are restored.
>   This was masked because the affected tests are gated on the
>   `postgres-tests` feature.
>
> Phase 3 landed as `feat/replace-host-key-api-helpers`:
> - **API helper.** `replaceHostKey(profileId, request, options?)` on
>   `apps/web/src/lib/api/serverProfiles.ts` issues
>   `POST /api/v1/server-profiles/:id/replace-host-key` with the profile
>   id URL-encoded into the path. Local validators run BEFORE any wire
>   round-trip — both fingerprints go through `isValidFingerprintShape`
>   and `reason_code` goes through `isHostKeyReplacementReasonCode`,
>   collapsing each refusal to a typed `validation` error
>   (`invalid_old_fingerprint_shape`, `invalid_new_fingerprint_shape`,
>   `invalid_reason_code`). The helper does not throw, does not log,
>   and does not echo wire / transport detail through any user-facing
>   string.
> - **Types.** `HostKeyReplacementReasonCode` (closed set of four wire
>   tags), `ReplaceHostKeyRequest`, `ReplaceHostKeyResponse`,
>   `ReplaceHostKeyError` (validation / http / transport /
>   malformed_response), `ReplaceHostKeyConflictReason` (closed set of
>   six 409 discriminators including the `profile_disabled` case).
> - **Response parser.** `parseReplaceHostKeyResponse(raw)` builds the
>   DTO field-by-field; a stray `private_key` /
>   `encrypted_private_key` / `password` / `cookie` / `session_token` /
>   `token_hash` smuggled onto the wire body cannot reach the returned
>   object. Pinned by sentinel tests in
>   `apps/web/tests/replaceHostKeyApi.test.ts`.
> - **Conflict-reason classifier.** `classifyReplaceConflictMessage`
>   (private to the API helper) maps the wire `message` of a 409
>   envelope (`"host_key {reason}"` / `"server_profile disabled"`)
>   against a closed accept-list and surfaces a typed `reason` field
>   on the `http` error variant. Unknown tags collapse to `null` so
>   the formatter never echoes an unrecognised wire string.
> - **Error formatter.** `describeReplaceHostKeyError(err)` is a function
>   of `kind` + `status` + `code` + the derived `reason` discriminator
>   ONLY — never echoes the wire `message` of an HTTP error or the
>   thrown `Error.message` of a transport failure. Each of the six
>   recognised 409 reasons renders distinct, deliberate operator copy
>   ("re-run preflight", "host key did not actually change",
>   "previously revoked", etc.).
> - **Pure UI helpers** in `apps/web/src/lib/app/hostKeyTrustState.ts`:
>   - `reasonCodeIsValid(value)` (re-exports the API-layer guard).
>   - `replacementReasonOptions()` returns a fresh array of
>     `{ code, label }` for the modal's reason picker.
>   - `replaceConfirmationMatches(input)` is byte-exact and case-
>     sensitive (`"REPLACE"` only — refuses `"replace"`, `" REPLACE "`,
>     `"REPLACE\n"`).
>   - `replaceGateForPreflight(preflight, activePinFingerprint)` returns
>     a `ReplaceGate` whose `ok` variant carries both `old_fingerprint`
>     and `new_fingerprint` so the modal and the request body share one
>     derivation point. Refusal variants:
>     `not_changed_status` / `missing_active_pin` /
>     `invalid_old_fingerprint_shape` / `invalid_new_fingerprint_shape`.
> - **Test coverage.** 40 vitest cases in
>   `apps/web/tests/replaceHostKeyApi.test.ts` covering: every reason
>   tag (`reasonCodeIsValid` + `replacementReasonOptions`); every
>   `replaceConfirmationMatches` boundary (case, whitespace, partial,
>   empty); every `replaceGateForPreflight` branch (changed/unknown/
>   trusted, missing active pin, malformed old/new fingerprint, ok with
>   both fingerprints); response parser shape + sentinel redaction;
>   API helper request encoding (URL-encoding, headers, body shape);
>   pre-wire validator refusals (no fetch dispatched on bad shape /
>   bad reason); 409 wire-message classification across all six
>   recognised reasons; unknown-reason fallback to `reason=null`;
>   400 / 401 / 403 / 404 / 502 / 503 envelopes; transport
>   `console.*` silence; malformed_response collapse; formatter
>   distinct-copy invariant; sentinel scan against `private_key`,
>   `encrypted_private_key`, `password`, `cookie`, `session_token`,
>   `token_hash`. Existing host-key trust/preflight tests
>   (`apps/web/tests/hostKeyApi.test.ts`) still pass.
> - **No UI wiring lands here.** No `HostKeyPanel.svelte` button, no
>   modal, no fetch from any view — Phase 4 picks those up. The Phase
>   3 helpers are pure / vitest-only by design (rollout table § "5
>   small, separately-mergeable PRs").
>
> Phase 4 landed as `feat/replace-host-key-ui`:
> - **Backend enabler.** `HostKeyPreflightResponse` gained one optional
>   field `active_pin_fingerprint: Option<String>`
>   (`crates/relayterm-api/src/dto/preflight.rs`). The preflight handler
>   populates it ONLY when status is `changed`, deriving the value from
>   the `known` list it already loads — same key type as the captured
>   key, non-revoked, already trusted, fingerprint differs from the
>   captured one (`crates/relayterm-api/src/routes/v1/server_profiles.rs::host_key_preflight`).
>   The field is `null` for `unknown` and `trusted` outcomes. This is
>   the smallest possible response addition; no new route, no schema
>   change, no repository change. Existing clients that omit the field
>   continue to work — the SPA's parser collapses missing-or-null to
>   `null` and falls back to `replaceGate.kind = missing_active_pin`.
>   The redaction posture is preserved: only the public SHA-256
>   fingerprint string crosses the wire — no public-key bytes, no row
>   id, no audit data.
> - **DTO + parser.** `HostKeyPreflightResponse.active_pin_fingerprint:
>   string | null` on the SPA side
>   (`apps/web/src/lib/api/serverProfiles.ts`). The
>   `parseHostKeyPreflightResponse` helper builds the field-by-field
>   DTO so a stray `private_key` / `encrypted_private_key` smuggled
>   onto the wire cannot reach the returned object. Pinned by the
>   parser tests in `apps/web/tests/hostKeyApi.test.ts` (back-compat:
>   missing field collapses to null; rejection: non-string non-null
>   types).
> - **New pure helpers** in `apps/web/src/lib/app/hostKeyTrustState.ts`:
>   - `decideReplaceSubmit(preflight, reasonCode, confirmInput)` — the
>     single submit-time decision: combines `replaceGateForPreflight`,
>     `reasonCodeIsValid`, and `replaceConfirmationMatches`. Returns
>     `{ kind: "ready", request }` when every gate passes, with the
>     wire request body assembled from the preflight's
>     `active_pin_fingerprint` + captured fingerprint + selected reason.
>     Otherwise `{ kind: "blocked", reason }` naming the gate that
>     refused. The component never builds a partially-validated
>     request.
>   - `synthesizePostReplacePreflight(preflight, replacement)` —
>     derives a `host_key_status: "trusted"` preflight from the
>     original + the successful replace response so the panel advances
>     the badge / fingerprint area to the new pin without an extra
>     round-trip. Builds the result field-by-field; sentinel-tested
>     against `private_key` / `encrypted_private_key` / `cookie` /
>     `session_token` / `token_hash`.
> - **Panel wiring** in `apps/web/src/lib/app/views/HostKeyPanel.svelte`:
>   - "Replace trusted host key…" button rendered ONLY under the
>     `changed_refused` branch AND only when the preflight's
>     `active_pin_fingerprint` is well-shaped. Invisible (not just
>     disabled) for `unknown`, `trusted`, and changed-without-active-pin
>     outcomes — gated by `replacementSummary !== null` which mirrors
>     `replaceGateForPreflight(...).kind === "ok"`.
>   - Modal (inline `role="dialog"` block) shows hostname:port,
>     `Revoking` old fingerprint, `New` fingerprint with key-type
>     label, reason picker (`<select>` bound to
>     `replacementReasonOptions()`), `Type REPLACE to confirm` text
>     input, `Replace pin` submit, `Cancel` button, optional error
>     band rendered through `describeReplaceHostKeyError`.
>   - Submit posts via `replaceHostKey(profileId, request)`. On 2xx,
>     `panelState` advances to `replaced` with the synthesized post-
>     replace preflight (status=trusted, fingerprint=trusted, key_type
>     from response). On non-2xx, the modal stays open with the typed
>     error summary; the operator can fix the form / re-run preflight.
>     The TOFU posture is preserved: there is no "Force trust" /
>     "Override" / "Ignore" affordance — those words are explicitly
>     forbidden in the panel template (pinned by the static-template
>     scan in `tests/hostKeyPanelReplace.test.ts`).
>   - The normal Trust button continues to refuse `changed` keys.
>     The replace affordance is the ONLY operator-sanctioned recovery
>     path from a `changed` outcome.
> - **Tests.**
>   - `apps/web/tests/hostKeyApi.test.ts` — parser round-trip + back-
>     compat + invalid-type rejection for `active_pin_fingerprint`.
>   - `apps/web/tests/replaceHostKeyApi.test.ts` — exhaustive coverage
>     of `decideReplaceSubmit` (every refusal reason; ready-path
>     request shape) and `synthesizePostReplacePreflight` (shape +
>     sentinel scan against pollution from a hostile replacement
>     response).
>   - `apps/web/tests/hostKeyPanelReplace.test.ts` — static-template
>     scan: every wire-bearing testid is present
>     (`host-key-replace-button`, `host-key-replace-modal`,
>     `host-key-replace-old-fingerprint`,
>     `host-key-replace-new-fingerprint`,
>     `host-key-replace-reason-select`,
>     `host-key-replace-confirm-input`,
>     `host-key-replace-confirm-mismatch`,
>     `host-key-replace-submit`, `host-key-replace-cancel`,
>     `host-key-replace-error`, `host-key-replaced-success`); required
>     copy strings appear; forbidden words (`Force trust`, `Override`,
>     `Ignore warning`, `Disable check`, `auto-trust`) never appear;
>     no sentinel field name (`private_key`, `encrypted_private_key`,
>     `password`, `cookie`, `session_token`, `token_hash`) lands in
>     the static template; the picker iterates the canonical option
>     list; the modal carries `role="dialog"` + `aria-modal` +
>     `aria-labelledby`. Composition tests mirror the panel's
>     visibility / submit-disabled rules without needing a full
>     component harness.
>   - Backend integration tests in `crates/relayterm-api/tests/api.rs`:
>     `preflight_changed_when_pinned_fingerprint_differs` now asserts
>     the `active_pin_fingerprint` echoes the active pin's
>     fingerprint; `preflight_unknown_when_no_known_host_entries` /
>     `preflight_trusted_when_pinned_entry_matches` pin the field as
>     `null` on those code paths.
> - **No new top-level dependency.** No new tracing, no new test
>   harness, no new fetch wrapper. The component composes existing
>   helpers + the existing `replaceHostKey` API helper.
> - **What this PR does NOT do** (deliberately): no staging smoke
>   (Phase 5); no SSH CA / host-certificate trust; no admin or bulk
>   replace; no known-host-entries listing endpoint; no `Tauri`
>   shell changes; no schema migration; no repository change.
>
> This doc proposes an explicit, auditable operator flow to revoke an
> active pinned host key and trust a new one in its place — without
> weakening the TOFU posture defined in
> [`auth.md`](auth.md) → "Host-key preflight + known-host trust contract"
> and the inventory lifecycle policy in
> [`SPEC.md`](../../SPEC.md) → "Inventory lifecycle and destructive-action
> policy".
>
> **Stance:** "revoke and replace", not "overwrite". The vocabulary on
> the wire, in the UI, and in audit payloads is *replace trusted host
> key* / *revoke old pin* / *insert new trusted pin*. Words like
> "force trust", "overwrite key", "ignore warning", or "disable check"
> never appear on this surface.

## Why this exists

Today the trust route refuses to overwrite an active pin: a `changed`
host key produces `409 conflict { entity: "host_key" }` with no
operator-side recovery path. That refusal is correct — auto-overwrite
on a fingerprint change is the canonical TOFU bypass — but it leaves
the operator without a sanctioned way to recover from the legitimate
shapes of a fingerprint change:

- A staging / lab target is recreated (the recurring shape that
  surfaced this gap on the VPS staging smoke; see
  [`docs/deployment/vps-staging-smoke.md`](../deployment/vps-staging-smoke.md)
  → "Operator-initiated TOFU re-pin / revoke-and-replace").
- A server is reinstalled or rebuilt and its host key changes.
- An operator deliberately rotates the server's host key.

Without this flow, the supported workaround is "create a new host +
new server profile", which clutters inventory and gives the operator
no audit-grade record of the intent. The clandestine workaround would
be direct DB deletion of the offending `known_host_entries` row — a
TOFU bypass that bypasses audit and lifecycle policy entirely.

The replace flow gives the operator a single deliberate action with:

1. An explicit confirmation that the fingerprint changed.
2. A required reason code drawn from a fixed enum.
3. A typed-`REPLACE` confirmation gate.
4. Two append-only audit rows (`host_key_revoked` for the old pin and
   `host_key_accepted` for the new one), each carrying public metadata
   only and cross-linked to the other.

## Non-goals (explicit)

These are deliberately out of scope for this surface. They are listed
here so a future change cannot quietly broaden the route.

- **No automatic / silent host-key replacement.** Every replace requires
  an explicit operator action. No "auto-trust if changed" mode.
- **No bulk replace.** One server profile per call. A bulk operator
  surface is admin tooling, not user UX.
- **No admin cross-user replace.** Owner-scoped only. Cross-user
  existence collapses to byte-identical 404 (matches the rest of the
  v1 route surface).
- **No SSH CA / host-certificate trust.** Host certificates are a
  separate, future trust model; this surface stays single-host-key.
- **No hard deletion of `known_host_entries`.** Old rows are revoked,
  never dropped. Hard delete remains admin-only future work
  ([`SPEC.md`](../../SPEC.md) → "Known-host revocation policy").
- **No "disable host-key verification" toggle.** There is no global
  or per-profile bypass. The replace route IS the only way to move
  forward from a `changed` outcome.
- **No "unrevoke" or "restore previous pin"** convenience UX. Once
  revoked, a `(host_id, fingerprint)` cannot be re-trusted through
  the user surface. Recovery from a mistakenly-revoked entry is the
  separate, deliberate operator unrevoke slice tracked in
  [`SPEC.md`](../../SPEC.md) → "Known-host revocation policy".
- **No backend auth / session changes.** No CSRF / Origin guard
  changes. No login-throttle changes. No terminal-manager changes.
  No Tauri shell changes.
- **No changes to the existing `trust-host-key` route shape.** The
  `changed` 409 stays exactly as it is today; the replace route is
  additive.

## Design requirements (load-bearing)

These are normative for any future implementation. Drift from any
single requirement is a spec bug, not an implementation freedom.

### R1 — TOFU posture preserved

- The route MUST NOT auto-overwrite a pinned host key on any code
  path.
- A `changed` outcome on the regular `trust-host-key` route continues
  to return `409 conflict { entity: "host_key" }`. This is unchanged.
- The replace route MUST require both an `expected_old_fingerprint`
  and an `expected_new_fingerprint`, and MUST verify both against the
  current state (active pin row + freshly-captured probe result).
- A revoked-and-reappearing fingerprint MUST refuse the replace —
  the same revoked-aware guard that the trust route enforces today
  ([`docs/agent/encountered-lessons.md`](../agent/encountered-lessons.md)
  2026-04-29 lesson) extends to this surface.

### R2 — Auditability

- A successful replace MUST write **two** audit rows, in order, in the
  same DB transaction as the row mutations:
  1. `host_key_revoked` — for the old pin.
  2. `host_key_accepted` — for the new pin.
- Both rows MUST carry `actor_id = caller`.
- Both payloads MUST cross-link via the other entry's id and
  fingerprint, so an audit feed can present the pair as a single
  intent.
- Audit payloads carry **public metadata only**. Forbidden:
  `private_key`, `encrypted_private_key`, public-key bytes (the
  fingerprint is the public form), raw russh error text, peer banner,
  vault internals, terminal I/O, full DB error text, any
  operator-supplied reason note, `client_info` blobs. (R3 keeps
  `reason_note` deliberately absent from the request schema; this
  clause is the second-line guard against future scope creep
  re-introducing it.)
  Sentinel-string redaction tests pin the invariant
  ([`AGENTS.md`](../../AGENTS.md) → "Things to avoid" row 1).
- Audit kinds reused: `host_key_revoked` and `host_key_accepted` both
  already exist in `audit_events.kind` (per
  `apps/backend/migrations/20260428000009_audit_events.sql`) and in
  the `AuditEventKind` Rust enum
  (`crates/relayterm-core/src/audit_event.rs`). **No new audit kind
  and no new `audit_events_kind_chk` migration is required.** The
  granular two-row taxonomy already anticipated by the schema is what
  this slice wires up.

### R3 — Data model

The current `known_host_entries` row carries `id, host_id, key_type,
fingerprint_sha256, public_key, first_seen_at, trusted_at,
revoked_at`. To make a revoked row self-describing (without forcing
audit-only navigation for routine "what replaced this?" reads) the
schema gains three columns:

```sql
-- New migration: apps/backend/migrations/<ts>_known_host_entries_revoke_metadata.sql

ALTER TABLE known_host_entries
    ADD COLUMN revoked_by              UUID
        REFERENCES users(id) ON DELETE SET NULL,
    ADD COLUMN revoked_reason_code     TEXT,
    ADD COLUMN replaced_by_id          UUID
        REFERENCES known_host_entries(id) ON DELETE SET NULL;

ALTER TABLE known_host_entries
    ADD CONSTRAINT known_host_entries_revoked_reason_chk CHECK (
        revoked_reason_code IS NULL
        OR revoked_reason_code IN (
            'server_reinstalled',
            'host_key_rotated',
            'lab_target_recreated',
            'operator_other'
        )
    );

ALTER TABLE known_host_entries
    ADD CONSTRAINT known_host_entries_revoked_columns_set_together CHECK (
        (revoked_at IS NULL
            AND revoked_by IS NULL
            AND revoked_reason_code IS NULL)
        OR (revoked_at IS NOT NULL
            AND revoked_by IS NOT NULL
            AND revoked_reason_code IS NOT NULL)
    );
```

Why these three columns and nothing else:

- **`revoked_by`** records the operator-of-record. `audit_events.actor_id`
  also records this, but having it on the row keeps revoked-pin reads
  one query instead of a join. `ON DELETE SET NULL` matches the rest
  of the schema's user-deletion posture.
- **`revoked_reason_code`** is a fixed enum, NOT free text — the
  enum is short, redaction-clean, and lets the future audit-feed UI
  render a stable label without rendering operator-supplied prose.
- **`replaced_by_id`** is the self-referencing link from old → new.
  Self-referencing FKs are well-supported in Postgres; `ON DELETE
  SET NULL` keeps the link absent if the new row is later deleted
  (admin-only future work, but the schema should not assume the new
  row is immortal).
- The "set-together" CHECK enforces the invariant that revoked rows
  carry full metadata. This catches incomplete UPDATEs at the DB
  layer, not just at the route layer.
- The reason enum is exhaustive in v1; an admin-only sweep for an
  edge case can use `operator_other`. The route's request validator
  is the canonical accept-list; the DB CHECK is the second-layer
  backstop.

**Reason note (free text):** the request schema does NOT carry a
`reason_note` string. Operator-supplied free text is the canonical
shape that smuggles secrets into audit (a stack-trace pasted into an
"Other" box, a hostname-with-credentials, etc.). Keeping the enum as
the only persisted reason removes that exposure entirely. The UI may
render a short helper sentence per code, but the operator does not
type prose into the request.

### R4 — API shape

Recommended:

```
POST /api/v1/server-profiles/:id/replace-host-key
```

The route lives on the existing server-profile surface for three
reasons:

1. Owner-scoping is already implemented for that path
   (`resolve_owned_profile` in
   [`crates/relayterm-api/src/routes/v1/server_profiles.rs`](../../crates/relayterm-api/src/routes/v1/server_profiles.rs))
   — foreign / missing collapse to byte-identical 404, free.
2. The probe, vault decrypt, and known-host fetch are all already
   wired through the profile. The new handler reuses them.
3. The frontend affordance lives inside `HostKeyPanel.svelte`, which
   is already keyed by `profileId`.

Alternative considered but **not** recommended:

```
POST /api/v1/known-host-entries/:id/revoke-and-replace
```

This shape would require a new route surface, a new owner-scope
helper (known-host entries don't carry an `owner_id` directly —
they cascade-own through `host_id → host.owner_id`), and would
make a future "revoke without replace" surface more awkward to
factor. Not worth the churn.

**Request body** (validated by `serde` + `validator` on Rust;
mirrored by a `zod`/`valibot` shape on the web side):

```jsonc
{
  // SHA256:<base64> — the fingerprint of the currently-active pinned
  // entry the operator is consenting to revoke. Validated by the same
  // helper that powers the `trust-host-key` route's
  // `validated_expected_fingerprint`.
  "expected_old_fingerprint": "SHA256:…",

  // SHA256:<base64> — the captured fingerprint the operator just
  // confirmed in the `changed` preflight result. Same shape rules.
  "expected_new_fingerprint": "SHA256:…",

  // Strict enum. The route rejects any value not in the accept-list
  // with 400 and a typed `reason: "invalid_reason_code"` envelope.
  "reason_code": "server_reinstalled"
                | "host_key_rotated"
                | "lab_target_recreated"
                | "operator_other"
}
```

**Response 200**:

```jsonc
{
  "profile_id": "…",
  "host_id": "…",
  "revoked_known_host_entry_id": "…",
  "revoked_fingerprint": "SHA256:…",
  "trusted_known_host_entry_id": "…",
  "trusted_fingerprint": "SHA256:…",
  "host_key_type": "ed25519",
  "trusted_at": "2026-…"
}
```

`host_key_type` echoes the captured key-type tag so a follow-on
`describeReplaceHostKeyResponse` SPA helper does not have to re-parse
the fingerprint string for an audit-feed badge. Mirrors the
`TrustHostKeyResponse` shape that already ships
(`crates/relayterm-api/src/dto/preflight.rs::TrustHostKeyResponse`).

The response carries only public-side identifiers and the public
fingerprints. No `public_key` byte blob. No vault payloads. No host
banner. No raw error text.

**Failure modes** (each 4xx surfaces a typed envelope; the formatter
on the SPA produces deliberate operator copy keyed off `kind / status
/ code / reason`, not the wire `message`):

| Status | Envelope | Trigger |
|---|---|---|
| `400` | `validation { reason: "invalid_old_fingerprint_shape" }` | `expected_old_fingerprint` malformed |
| `400` | `validation { reason: "invalid_new_fingerprint_shape" }` | `expected_new_fingerprint` malformed |
| `400` | `validation { reason: "invalid_reason_code" }` | `reason_code` outside the enum |
| `401` | `unauthorized` | session cookie missing/invalid |
| `403` | `csrf_origin_mismatch` | shared `CsrfGuard` extractor fires before any DB / body work, per [`AGENTS.md`](../../AGENTS.md) row 7 |
| `404` | `not_found { entity: "server_profile" }` | missing or foreign-owned profile (byte-identical to the genuine 404) |
| `409` | `conflict { entity: "host_key", reason: "active_pin_mismatch" }` | active pin's fingerprint ≠ `expected_old_fingerprint` **OR** no active trusted pin exists for this host (Phase 2 implementation collapses both subcases under one wire tag — see `replace_host_key_rejects_when_no_active_pin_exists`; Phase 3 frontend classifier accepts only `active_pin_mismatch`) |
| `409` | `conflict { entity: "host_key", reason: "captured_mismatch" }` | freshly-captured probe fingerprint ≠ `expected_new_fingerprint` |
| `409` | `conflict { entity: "host_key", reason: "captured_unchanged" }` | freshly-captured fingerprint matches the active pin (host hasn't actually changed; replace is a no-op) |
| `409` | `conflict { entity: "host_key", reason: "captured_revoked" }` | a revoked row exists for `(host_id, captured_fingerprint)` — refuse re-trust through the replace path |
| `409` | `conflict { entity: "host_key", reason: "new_fingerprint_already_active" }` | forward-compat: another operator raced through a duplicate trust before this replace landed. Currently impossible because the `captured_unchanged` / `captured_mismatch` / `captured_revoked` checks fire first; the repository's `FOR UPDATE` constraint surfaces it for future code paths that bypass those checks. |
| `409` | `conflict { entity: "server_profile", reason: "disabled" }` | profile is disabled (mirrors the launch / preflight / trust / auth-check guards) |
| `502` | `bad_gateway` | probe failure (unreachable, timeout, transport error, unsupported host-key algorithm). Wire body is the static `"bad gateway"` string so peer banners / topology never leak. |
| `503` | `service_unavailable` | vault disabled |

**Idempotency:** the route is **deliberately not idempotent**.
A successful replace transitions the host's active pin from
`old_fingerprint` to `new_fingerprint`. A second call with the same
`expected_old_fingerprint` will see no active pin matching it and
return `409 active_pin_mismatch`. This is the right shape: a replace
is a destructive lifecycle transition, not a query.

**CSRF / Origin posture:** placed exactly like the existing trust
route. The `CsrfGuard` extractor fires first (before any body
extractor), then `AuthenticatedUser`, then the typed `Json<Body>`
parse. This matches [`AGENTS.md`](../../AGENTS.md) "Things to avoid"
row 7 and the integration-test posture of
`bad_origin_rejects_before_body_parsing`.

### R5 — Race safety

The flow opens a write window that two concurrent operators could
race through. The design closes the race at the DB layer, not at
the application layer.

Sequence:

1. Validate request shape (`expected_old_fingerprint`,
   `expected_new_fingerprint`, `reason_code`).
2. Resolve `(profile, host, identity)` scoped to the caller.
3. Refuse if the profile is disabled.
4. Decrypt the identity into a `Zeroizing<Vec<u8>>`.
5. Run a fresh probe → `captured = (key_type, fingerprint,
   public_key)`.
6. Initial-shape checks against the in-memory `known` list — these
   produce a clean 409 before any write:
   - `captured_unchanged` if `captured.fingerprint ==
     expected_old_fingerprint`.
   - `captured_mismatch` if `captured.fingerprint !=
     expected_new_fingerprint`.
   - `captured_revoked` if any row for `(host_id,
     captured.fingerprint)` already has `revoked_at IS NOT NULL`.
7. Begin a transaction. Inside the transaction:
   - `SELECT … FOR UPDATE` the active pin matching `(host_id,
     expected_old_fingerprint, revoked_at IS NULL, trusted_at IS NOT
     NULL)`. If zero rows: ROLLBACK and return
     `409 active_pin_mismatch` (which also covers the
     "no active pin" subcase — collapse the two conflict subcases
     into the same 409 reason if their differentiation isn't
     useful for the SPA; the table above keeps them separate to
     give precise UI copy).
   - Re-assert the captured fingerprint is still not present as a
     revoked row by issuing a fresh `SELECT … FROM
     known_host_entries WHERE host_id = $1 AND fingerprint_sha256
     = $2 AND revoked_at IS NOT NULL` inside the open transaction
     (TOCTOU close — the in-memory list from step 6 is NOT
     authoritative for this re-check). `READ COMMITTED` isolation
     (Postgres default) is sufficient: the only relevant concurrent
     write is another revoke for `(host_id, captured_fingerprint)`,
     and a committed concurrent revoke is visible to a fresh
     `SELECT` inside our transaction.
   - INSERT the new `known_host_entries` row with
     `trusted_at = NOW()`, `revoked_at = NULL`, `replaced_by_id =
     NULL`. Capture the new id.
   - UPDATE the old row: `revoked_at = NOW()`, `revoked_by =
     caller`, `revoked_reason_code = req.reason_code`,
     `replaced_by_id = <new id>`.
   - INSERT `host_key_revoked` audit row (payload below).
   - INSERT `host_key_accepted` audit row (payload below).
   - COMMIT.

If any step inside the transaction fails (audit insert,
constraint violation), the entire transaction rolls back and the
route returns `500 internal_error` (fail-closed; matches the
profile-lifecycle audit policy in
[`crates/relayterm-api/src/routes/v1/server_profiles.rs`](../../crates/relayterm-api/src/routes/v1/server_profiles.rs)
`write_lifecycle_audit`'s rationale).

Two operators racing through the same replace will see exactly one
succeed (the one that gets the `FOR UPDATE` lock first); the second
will find the old row revoked and the active pin already advanced,
and will return `409 active_pin_mismatch`. No data loss, no
double-revoke, no double-trust.

### R6 — UX

The replace affordance lives inside the existing
[`apps/web/src/lib/app/views/HostKeyPanel.svelte`](../../apps/web/src/lib/app/views/HostKeyPanel.svelte).
Today that panel renders a deliberate refusal when the latest
preflight returns `changed`; this slice adds a secondary path off
that refusal.

**Affordance gating (load-bearing):**

1. The "Replace trusted host key…" button is rendered ONLY when
   the most-recent successful preflight returned
   `host_key_status === "changed"`. It is invisible (not just
   disabled) for `unknown` and `trusted` outcomes.
2. The button never auto-runs. Clicking it opens a modal; nothing
   else happens until the operator confirms.
3. The modal copy keeps the existing "scary" framing. The replace
   affordance is offered as an explicit recovery path, not as a
   path of least resistance.

**Modal copy and structure (proposed, content-locked):**

- **Title:** "Replace trusted host key"
- **Lede:** "RelayTerm will not silently overwrite a pinned host
  key. The fingerprint shown below is different from what you
  trusted previously. Replace it only if you can explain why the
  host key changed."
- **Target identification:** display name, `hostname:port`, and a
  short id of the profile.
- **Old fingerprint:** rendered with a `Revoking` label, full
  `SHA256:<base64>` selectable.
- **New fingerprint:** rendered with a `New` label, full
  `SHA256:<base64>` selectable, AND the captured key type.
- **Reason picker:** required; a `<select>` (or radio group) with
  these labels (the wire enum value in parentheses is NOT shown to
  the operator):
  - "Server reinstalled or rebuilt" (`server_reinstalled`)
  - "Host key rotated by the server operator" (`host_key_rotated`)
  - "Lab or staging target recreated" (`lab_target_recreated`)
  - "Other (acknowledged)" (`operator_other`)
- **Confirmation gate:** a text input that requires the operator
  to type `REPLACE` (uppercase, byte-exact). Same shape as the
  fingerprint-confirmation gate already in `HostKeyPanel`.
- **Submit button label:** "Replace pin" — never "Trust" (that
  word belongs to the unknown-key flow), never "Force trust",
  never "Override".
- **Cancel:** always available; never destructive.
- **Disclaimer:** "After replacement, run auth-check to confirm
  the configured SSH identity still authenticates against the new
  host key. Existing live terminal sessions on this profile are
  not killed by this action."

**State after success:**

- Inline success line on the panel: "Host key replaced. Run
  auth-check to confirm credentials still work against the new
  pin."
- The panel's classification badge transitions to
  `Trusted` (the new pin's status); the old fingerprint is no
  longer surfaced on this panel. A future "audit/history" surface
  may list the revoked pin; that is out of scope here.
- The `AuthCheckPanel` immediately below remains untouched —
  operator clicks it manually. No auto-run.

**Pure helpers** (proposed, all in
`apps/web/src/lib/app/hostKeyTrustState.ts` to keep the existing
testing posture):

```ts
export type ReplaceGate =
  | { kind: "ok" }
  | { kind: "not_changed_status" }
  | { kind: "invalid_old_fingerprint_shape" }
  | { kind: "invalid_new_fingerprint_shape" };

export function replaceGateForPreflight(
  preflight: HostKeyPreflightResponse,
  activePinFingerprint: string | null,
): ReplaceGate;

export function replaceConfirmationMatches(input: string): boolean;
//  ⇒ input === "REPLACE"

export type HostKeyReplaceReasonCode =
  | "server_reinstalled"
  | "host_key_rotated"
  | "lab_target_recreated"
  | "operator_other";

export function reasonCodeIsValid(value: string): value is HostKeyReplaceReasonCode;
```

**Stable selectors** (proposed `data-testid` hooks, mirroring the
existing `host-key-*` pattern):

`host-key-replace-button`, `host-key-replace-modal`,
`host-key-replace-old-fingerprint`,
`host-key-replace-new-fingerprint`,
`host-key-replace-reason-select`,
`host-key-replace-confirm-input`,
`host-key-replace-confirm-mismatch`,
`host-key-replace-submit`, `host-key-replace-cancel`,
`host-key-replace-error`, `host-key-replace-success`.

### R7 — Repository primitive

Recommended trait addition in
[`crates/relayterm-core/src/repository.rs`](../../crates/relayterm-core/src/repository.rs):

```rust
pub trait KnownHostEntryRepository: Send + Sync {
    // … existing methods stay unchanged …

    /// Atomically revoke an active trusted pin and insert a new trusted
    /// pin for the same host. Either both writes commit or neither does.
    /// Used exclusively by the `POST /api/v1/server-profiles/:id/
    /// replace-host-key` route.
    ///
    /// Refusal contract:
    /// - `Conflict { constraint: "no_active_pin_for_old_fingerprint" }`
    ///   if no active trusted row matches `expected_old_fingerprint`.
    /// - `Conflict { constraint: "captured_revoked" }` if any row for
    ///   `(host_id, new_fingerprint)` already exists with `revoked_at
    ///   IS NOT NULL`.
    ///
    /// The two audit rows are written by the API layer inside the same
    /// transaction (the trait stays storage-only; audit content lives
    /// with the route alongside its other audit emissions).
    async fn replace_active_pin(
        &self,
        input: ReplaceActivePin,
    ) -> Result<ReplacedKnownHostEntries, RepositoryError>;
}

pub struct ReplaceActivePin {
    pub host_id: HostId,
    pub expected_old_fingerprint: String,
    pub new_key_type: SshKeyType,
    pub new_fingerprint: String,
    pub new_public_key: Vec<u8>,
    pub revoked_by: UserId,
    pub reason_code: KnownHostRevocationReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KnownHostRevocationReason {
    ServerReinstalled,
    HostKeyRotated,
    LabTargetRecreated,
    OperatorOther,
}

pub struct ReplacedKnownHostEntries {
    pub revoked_old: KnownHostEntry,
    pub trusted_new: KnownHostEntry,
}
```

Implementation pattern (in
[`crates/relayterm-db/src/repositories/known_host_entry.rs`](../../crates/relayterm-db/src/repositories/known_host_entry.rs)):
open a transaction; `SELECT … FOR UPDATE` the active row; INSERT
the new row; UPDATE the old row; commit. The audit-row INSERTs run
inside the *same* transaction but are coordinated by the API layer —
the repository trait stays storage-only, mirroring the existing
boundary between `KnownHostEntryRepository` and
`AuditEventRepository`.

To make the cross-repository, single-transaction shape implementable
without forcing a leaky `Tx` parameter through both traits, the
two practical options are:

- **(a) Push the audit writes into the repository primitive.** The
  Postgres impl assembles both audit payloads internally —
  field-by-field, from the same primitive inputs already on
  `ReplaceActivePin` (`host_id`, `revoked_by`, `reason_code`, the
  resulting `revoked_old` and `trusted_new` rows). The repository
  writes both audit rows inside its own transaction alongside the
  row mutations. The trait input does NOT grow opaque
  `serde_json::Value` payload blobs; this matches the precedent set
  by `TerminalRecordingRepository::purge_for_retention` (see
  [`crates/relayterm-db/src/repositories/terminal_recording.rs`](../../crates/relayterm-db/src/repositories/terminal_recording.rs)
  `purge_for_retention`, where the `audit_payload = json!({ … })`
  expression lives inside the impl, not in any caller — the input
  struct carries only `terminal_session_id`, `retention_days`,
  `now`). The route handler stays thin: validates the request,
  resolves the profile, runs the probe, and calls
  `replace_active_pin(input)`; payload assembly never crosses the
  trait boundary. This keeps the redaction surface in one place
  (the repository impl) and matches the
  [`AGENTS.md`](../../AGENTS.md) "Things to avoid" row 12 invariant.
- **(b) Expose a tx-bearing variant.** Add a
  `replace_active_pin_in_tx(&mut PgTransaction<'_>, …)` shape on the
  repository, plus a similar `create_in_tx` for `AuditEventRepository`,
  and orchestrate them from the API handler. More layering churn;
  harder to test with mock repositories.

**Recommendation: (a).** It mirrors the retention-purge pattern
exactly — same single-transaction shape, same in-impl audit
assembly, same "audit failure inside the tx → ROLLBACK reverts the
row writes" semantics
([`AGENTS.md`](../../AGENTS.md) "Things to avoid" row 12) — and
keeps the route handler free of audit-payload prose.

### R8 — Test plan

The implementation slices land tests as they go. Listed here so a
reviewer can pin coverage at the design stage.

**Backend integration** (in
[`crates/relayterm-api/tests/api.rs`](../../crates/relayterm-api/tests/api.rs)
or a new `replace_host_key.rs` once the slice lands):

- Happy path: changed fingerprint, valid reason, valid old/new
  expected. Asserts: response shape; old row has `revoked_at`,
  `revoked_by = caller`, `revoked_reason_code`, `replaced_by_id`
  pointing at the new row; new row has `trusted_at`; both audit
  rows present, in order, with correct payloads.
- 400 on each malformed shape: old-fingerprint, new-fingerprint,
  reason code.
- 401 with no session cookie.
- 403 with bad `Origin` (covered by the shared
  `bad_origin_rejects_before_body_parsing` shape; one route-specific
  variant pins the order: CSRF guard fires before the body parse).
- 404 on a foreign-owned profile (byte-identical to a genuine 404;
  no body / status / `Content-Length` divergence).
- 409 `no_active_pin`: host has zero active trusted pins.
- 409 `active_pin_mismatch`: caller's `expected_old_fingerprint` is
  not the active pin.
- 409 `captured_mismatch`: probe captured a different fingerprint
  than `expected_new_fingerprint`.
- 409 `captured_unchanged`: probe captured the same fingerprint as
  the active pin.
- 409 `captured_revoked`: a revoked row already exists for
  `(host_id, captured_fingerprint)`.
- 409 `profile_disabled`.
- 502 on probe failure (unreachable / timeout / transport / bad
  host key) — wire body is the static `"bad gateway"` string.
- 503 with vault disabled.
- **Audit redaction sentinel:** scan the inserted `audit_events.payload`
  for every entry in `AUDIT_FORBIDDEN_SUBSTRINGS`
  ([`crates/relayterm-api/tests/api.rs`](../../crates/relayterm-api/tests/api.rs)).
  Both rows MUST pass.
- **Existing trust route unchanged:**
  - `unknown` → trust → success unchanged.
  - `changed` → regular trust → still 409 with no replace side-effect.
- **Transactional integrity:** the load-bearing property to pin is
  "audit-insert failure inside the transaction → ROLLBACK leaves
  the old row trusted and the new row absent." The exact harness
  technique (e.g., injecting a constraint violation through a
  test-only repository wrapper that fails the audit insert) is an
  implementation-time decision; the design specifies only the
  observable post-condition.
- **Race-safety smoke:** two parallel calls with the same
  `expected_old_fingerprint` — exactly one succeeds, the other
  returns `409 active_pin_mismatch`; no double-revoke; no
  double-trust.

**Backend unit** (repository tests; matches the
`repositories::known_host_entry` test shape):

- `replace_active_pin` happy path.
- Refuses if the active row was revoked between the API-layer
  precheck and the repository call (TOCTOU close inside the lock).
- Refuses if a revoked row exists for `(host_id, new_fingerprint)`.
- `revoked_columns_set_together` CHECK fires on partial UPDATE
  (defence-in-depth against future code paths that forget a
  column).

**Frontend (vitest):**

- `replaceGateForPreflight` returns `ok` only when status is
  `changed` AND both fingerprints pass the local shape check.
- `replaceConfirmationMatches` is byte-exact, case-sensitive,
  rejects `"replace"` / `" REPLACE "` / `"REPLACE\n"`.
- `reasonCodeIsValid` accepts the four canonical codes and rejects
  everything else.
- API helper rejects malformed fingerprints / unknown reason codes
  before any wire round-trip.
- API helper passes 409 reason codes through intact for the
  formatter.
- The formatter renders distinct copy per 409 reason and never
  echoes the wire `message` or transport `Error.message` (mirrors
  `describeTrustHostKeyError` posture).
- Response parser strips smuggled `private_key` / `encrypted_private_key`
  fields (sentinel-tested).
- Component test (when the harness lands): replace button is
  invisible unless `changed`; modal blocks submit until reason +
  typed `REPLACE` are both present; success collapses the panel
  to the trusted state.

**Staging smoke** (one-shot verification, in `docs/deployment/`):

- Recreate the throwaway SSH container.
- Run preflight → confirm `changed`.
- Run replace → confirm 200, new pin trusted, audit feed shows the
  paired `host_key_revoked` + `host_key_accepted` rows.
- Run auth-check → confirm credentials still work against the new
  pin.
- Re-run preflight → confirm `trusted`.

## Recommended rollout

Five small, separately-mergeable PRs. Each has a definition-of-done
that includes `cargo check + clippy + test`, `pnpm -r check + lint +
test`, sqlx prepare on the schema slice, and the redaction-sentinel
tests on slices that touch DB or audit.

| # | Branch | What lands | Tests |
|---|---|---|---|
| 1 ✅ | `feat/known-host-revoke-metadata` | **Landed.** Migration `20260510000022_known_host_entries_revoke_metadata.sql` + the three new columns + the two CHECK constraints + Rust row mapping + `KnownHostRevocationReason` enum + `replace_active_pin` repository primitive + repository tests. **No route, no UI.** | Repository tests in `crates/relayterm-db/tests/repositories.rs`. The project uses runtime sqlx queries, so no `.sqlx/` prepare cache. |
| 2 ✅ | `feat/replace-host-key-route` | **Landed.** The `POST /api/v1/server-profiles/:id/replace-host-key` route + `ReplaceHostKeyRequest` / `ReplaceHostKeyResponse` DTOs + paired-audit emission inside `replace_active_pin` (option (a)) + integration tests. **No UI yet.** | Route integration tests in `crates/relayterm-api/tests/api.rs` (`replace_host_key_*`); redaction-sentinel scan via `AUDIT_FORBIDDEN_SUBSTRINGS`; repository-level audit + atomic-rollback tests in `crates/relayterm-db/tests/repositories.rs`. |
| 3 ✅ | `feat/replace-host-key-api-helpers` | **Landed.** `replaceHostKey(...)` helper + `parseReplaceHostKeyResponse` + `describeReplaceHostKeyError` + `replaceGateForPreflight` + `replaceConfirmationMatches` + `reasonCodeIsValid` + `replacementReasonOptions`. Pure helpers, vitest only. **No component edits yet.** | vitest (`apps/web/tests/replaceHostKeyApi.test.ts`, 40 cases). |
| 4 ✅ | `feat/replace-host-key-ui` | **Landed.** Backend: `HostKeyPreflightResponse.active_pin_fingerprint` (Phase 4 enabler). SPA: parser update + `decideReplaceSubmit` / `synthesizePostReplacePreflight` helpers + `HostKeyPanel.svelte` modal + button gate + success/error states. **No schema or repository change.** | Backend integration tests for `active_pin_fingerprint` on `changed` / `unknown` / `trusted`; vitest helper tests + static-template scan in `apps/web/tests/hostKeyPanelReplace.test.ts`. |
| 5 | `chore/replace-host-key-staging-smoke` | One throwaway-target smoke run; update the deferred note in `vps-staging-smoke.md`; final docs sweep on `auth.md`, `inventory.md`, `SPEC.md`. | Manual smoke; doc-contracts guard. |

Splitting this way keeps each PR's blast radius small, lets the
schema land before any caller depends on it, and lets the route
land with full backend test coverage before the SPA is wired.

## Spec-area updates required at landing time

The implementation slices update these area docs as they go (not in
this design slice — only forward-pointers are added now):

- [`docs/spec/auth.md`](auth.md) → "Host-key preflight + known-host
  trust contract": add a "Host-key replace contract" subsection
  that names the route, the request/response shape, the failure
  envelope, and the audit pair. Cross-reference this design doc.
- [`docs/spec/inventory.md`](inventory.md) → "Production host-key
  preflight & trust UI": extend with the replace affordance scope,
  selectors, and copy contract.
- [`SPEC.md`](../../SPEC.md) → "Inventory lifecycle and
  destructive-action policy": the existing line
  "no route or UI yet writes [`revoked_at`]" gets the addition
  "(landed via the host-key replace route — see
  `docs/spec/host-key-replace.md`)".
- [`docs/deployment/vps-staging-smoke.md`](../deployment/vps-staging-smoke.md):
  the "Operator-initiated TOFU re-pin" deferred note closes; a
  final smoke run pins the flow on staging.

## Open questions deliberately left for the implementation slice

These do not block the design; they are concrete decisions that
the first implementation PR will make and pin in tests.

- **Conflict-reason granularity on the wire.** The table in R4 lists
  five distinct 409 reasons against `entity: "host_key"`. The first
  implementation PR may choose to collapse `no_active_pin` and
  `active_pin_mismatch` into a single reason (`active_pin_mismatch`)
  if the SPA copy doesn't differentiate them — at the cost of
  slightly less precise diagnostics. Either is fine; pin in tests.
- **Reason-code labels.** The four operator-facing labels in R6
  are proposals, not finals. The implementation PR may iterate on
  the prose; the wire enum values stay stable.
- **Past-pin history surface.** A future "revoked pins for this
  host" UI is out of scope here, but the schema (`replaced_by_id`,
  `revoked_at`, `revoked_reason_code`) is sufficient to power one
  without further migrations. Good thing to flag for the inventory
  detail-panel slice.

## What this design intentionally does NOT specify

- The exact wording of the modal's per-reason helper sentence
  (the implementation PR drafts it; the operator copy will be
  reviewed alongside the UI slice).
- The on-disk migration filename / timestamp.
- The exact `revoked_old.replaced_by_id` cross-link rendering on
  any future audit-feed UI.
- Whether the inventory detail panel surfaces the count of revoked
  pins per host (good idea, separate slice).
- The mobile / Tauri shell behavior — the production HostKeyPanel is
  shared via the existing `apps/web` bundle, so this surface picks
  up the bundled-shell behavior automatically with no shell-side
  change. Any mobile-specific layout adjustment is a separate UX
  pass.
