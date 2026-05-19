# RelayTerm v1 Draft Release Notes

> **Status: DRAFT.** RelayTerm v1 is **not yet tagged or released.**
> These notes describe the v1 release the project is building toward
> — a first version that is deployable for a single self-hosted
> operator. Intended audience: that operator (you).
>
> **Posture as of 2026-05-17:**
> - The v1 production-readiness cutline blockers stand at: **B1
>   DONE**, **B2 PENDING** (no production-walked end-to-end smoke
>   recorded yet — operator has not chosen a production hostname),
>   **B3 PENDING** (mobile portrait sanity walk, accepted as
>   production-use-driven for now).
> - The release notes therefore make NO claim that v1 has been
>   shipped, that any production smoke has passed, or that a
>   particular release tag exists.
> - When the operator cuts the v1 tag, the §11 sign-off block
>   below is copied into the release log entry; the §1–§10
>   content here is the body of the published release notes.
>
> These notes compose existing primitives — they do NOT redefine
> any contract in [`AGENTS.md`](../AGENTS.md), [`SPEC.md`](../SPEC.md),
> [`docs/spec/*`](spec/), or
> [`docs/deployment/production-runbook.md`](deployment/production-runbook.md).
> Where these notes and any upstream contract disagree, the upstream
> contract wins and these notes are the bug.

## 1. What RelayTerm v1 is

RelayTerm is a **self-hosted SSH terminal web app** for a single
operator. SSH sessions live on the backend; the browser is a
renderer. Clients can detach and reconnect within a bounded window
without losing the session.

A v1 deploy gives one operator:

- A web SPA they can sign in to from their own browser.
- Backend-managed SSH identities (generate or import an Ed25519
  key; the private bytes never leave the backend vault).
- Saved hosts and server profiles binding a host + identity to a
  launchable target.
- A first-trust workflow for SSH host keys (preflight, trust,
  re-key replacement).
- A default xterm.js terminal workspace, launched from a profile.
- Reconnectable terminal sessions within the configured
  `DETACHED_LIVE_PTY_TTL` window (default 30s; in-memory ring
  replay; not durable across backend restarts).
- A sessions list with status, reconnect, and idempotent close.
- Current-user auth: login / logout / `/me`, password change,
  browser-session list with revoke.
- An operational status panel inside Settings that surfaces
  backend reachability and effective deployment quotas without
  exposing secrets.
- A documented backup, restore, and rollback runbook.

What v1 is **not**: a multi-user product, a managed cloud service,
a `tmux`/`screen` replacement, a mobile app with native polish, or
a benchmark-grade terminal. It is "one operator can stand this up
and use it as their daily SSH terminal."

For the full cutline reasoning, see
[`docs/v1-production-readiness.md`](v1-production-readiness.md).

## 2. What v1 includes

### Auth & security

- First-user bootstrap (`POST /api/v1/auth/bootstrap`, one-shot
  token; production envelope refuses to start without it).
- Login / logout / `/me` with opaque server-side sessions in
  Postgres (no JWTs on the wire).
- Cookie posture: `relayterm_session`, `HttpOnly; SameSite=Strict;
  Secure` (in production), 30-day hard-expire.
- Password change route + UI; OTHER sessions revoked on success
  while the calling session stays valid.
- Browser-session list + per-session revoke +
  revoke-all-except-current.
- CSRF / `Origin` guard on every state-changing browser-write
  route, ahead of the body extractor. `403 csrf_origin_mismatch`
  on mismatch; offered `Origin` value never echoed back.
- Argon2id password hashing pinned at `OWASP_2023` parameters
  (`m=19456 KiB`, `t=2`, `p=1`).
- In-memory login throttler keyed on normalized email (5 failures
  / 15-minute window → 15-minute block). Both unknown-email and
  wrong-password branches are recorded.
- Redaction posture: payloads, audit rows, logs, response bodies,
  and DOM strings are screened against a canonical sentinel set
  (vault bytes, private keys, session-token plaintext, password
  hashes, terminal payload, recording chunks). Sentinel tests are
  the in-CI backstop on every audit-emitting path.

### Inventory

- SSH identity **generate** (Ed25519, backend keypair, public-key
  + SHA-256 fingerprint in API responses; never
  `encrypted_private_key`, never raw PEM).
- SSH identity **import** (Ed25519 unencrypted OpenSSH only). RSA
  / ECDSA / DSA / passphrase-protected imports are explicitly out
  of scope for v1 — see
  [`docs/private-key-import.md`](private-key-import.md).
- SSH identity rename + delete UI. Delete refuses with a typed
  `409 conflict { entity: "ssh_identity", reason: "referenced" }`
  when an owned server profile references the identity; deletion
  is the only allowed path to remove vault-encrypted private-key
  bytes from durable storage.
- Host create / edit / delete UI. Delete refuses with `409
  conflict { entity: "host", reason: "referenced" }` when an
  owned server profile **or** any pinned `known_host_entries` row
  references the host (pin history is retained as a deliberate
  safety property).
- Server profile create / edit / disable / enable / delete UI.
  Disable is the recommended user-facing destructive action for
  profiles with session history (blocks new launches; existing
  live sessions are unaffected); delete is refused with `409
  conflict { entity: "server_profile", reason: "referenced" }`
  when any terminal-session row references the profile.
- Host-key preflight + trust UI. The preflight disconnects
  without authenticating; the trust route refuses to silently
  re-trust a revoked entry and requires the caller's
  `expected_fingerprint` to match the freshly-captured one.
- Host-key replace (atomic revoke + re-trust) with reason-code
  modal and fingerprint confirmation.
- Authenticated SSH credential check UI (`AuthCheckPanel`) — no
  PTY, no shell, hard outer timeout, process-wide concurrency
  cap.
- Identity detail panels show **public-key metadata only** — no
  `private_key`, no `encrypted_private_key`, no raw PEM, no
  `BEGIN OPENSSH PRIVATE KEY` substring anywhere.
- Inventory filters / search.
- Audit-event read API (`recent_for_actor`) and the Recent
  Activity panel in the dashboard (current-user scope only;
  pre-auth NULL-actor events excluded by SQL filter).

### Terminal

- Default **xterm.js** renderer on every surface (production
  compatibility baseline).
- Launch a terminal from a trusted, auth-checked profile via the
  production shell.
- Live russh PTY bridge in the backend (not a stub).
- Binary `RTB1` data plane for the hot terminal path; JSON
  control plane. `Output.payload_len ≤ 1 MiB`.
- Output sequence + in-memory replay ring (default `max_frames =
  1024`, `max_bytes = 1 MiB`, FIFO eviction).
- Attach / detach / reconnect inside the configured
  `DETACHED_LIVE_PTY_TTL` window (default 30s, operator-tunable
  per deployment). Reconnect replays missed output via
  `(session_id, last_seen_seq)`; out-of-window replay returns a
  single `replay_window_lost` marker.
- Window resize honored end-to-end (`Resize` wire message →
  `russh::window_change`).
- Idempotent session close (`POST
  /api/v1/terminal-sessions/:id/close`).
- Sessions list + status UI with refresh, reconnect, and close
  affordances. Closed sessions remain visible as historical
  metadata.
- Per-user live / starting / per-deployment session quotas, all
  operator-tunable through env knobs.
- Renderer-neutral autofit (operator opt-in via Settings; off by
  default).
- Launch timing diagnostics in the SPA (payload-free counters;
  no terminal bytes in any timing field).
- Paste safety pipeline (`safe` / `confirm` / `blocked` shape
  classification; metadata-only panels; sentinel-string redaction
  tests around it).
- Mobile shell usability improvements (the `mobileNavOpen`
  drawer and mobile-aware components landed; the production
  shell terminal mounts on Android Chrome). Polish past "usable"
  is post-v1.

### Operations

- Operational Status panel in Settings: backend reachability
  (`/healthz`), browser-session count, terminal-session counts by
  status, the deployment-configured detached-PTY TTL and quotas,
  the local experimental-renderer gate posture, the local autofit
  posture, the read-only "next session will mount X" copy, and
  three production-readiness reminders. **No new backend
  endpoints; no secrets / env values / DB URLs exposed.**
- Production runbook for first deploy, upgrade, rollback,
  migration, backup, restore, reverse-proxy contract, and secret
  rotation
  ([`docs/deployment/production-runbook.md`](deployment/production-runbook.md)).
- Operator-facing backup / restore / rollback runbook
  ([`docs/deployment/backup-restore-runbook.md`](deployment/backup-restore-runbook.md))
  with a sensitive-material warning, a rehearsal record template,
  and the v1 / post-v1 non-goal split.
- v1 release-day checklist
  ([`docs/v1-release-checklist.md`](v1-release-checklist.md))
  with a decision table and a sign-off template.
- v1 production smoke template
  ([`docs/deployment/v1-production-smoke.md`](deployment/v1-production-smoke.md))
  — the skeleton an operator copies once they pick a production
  hostname.
- Staging-smoke runbook
  ([`docs/deployment/vps-staging-smoke.md`](deployment/vps-staging-smoke.md))
  with an extensive history of throwaway-slot evidence.
- SPA smoke runbook ([`apps/web/e2e/SMOKE.md`](../apps/web/e2e/SMOKE.md))
  with the renderer-fair input methodology used by the v1 smokes.
- A documented "lost the only password" recovery path
  ([`docs/production-auth.md`](production-auth.md) §8).

## 3. Known caveats and honest limitations

These are the things v1 does **not** claim. None of them block the
release for a single self-hosted operator; all of them are real.

- **No production-walked end-to-end smoke recorded yet.** B2
  remains PENDING until the operator picks a production hostname
  and walks
  [`docs/deployment/v1-production-smoke.md`](deployment/v1-production-smoke.md)
  §5 against it. Until that happens, v1 has staging evidence and
  the release-day checklist — not production evidence.
- **Mobile portrait sanity is production-use-driven, not
  smoke-recorded.** B3 remains PENDING. The default xterm path
  attaches cleanly under real Android Chrome in staging, but no
  operator-recorded portrait walk against a non-throwaway target
  has been committed. Polish past "usable" (typography,
  soft-keyboard tuning, paste affordance) is explicitly post-v1.
- **xterm.js is the default renderer.** Experimental renderers
  (`ghostty-web`, `wterm`, `restty`) are evaluation-only and do
  NOT carry v1 commitments. See §4.
- **Reconnect is bounded by the in-memory TTL window.** Default
  30s, operator-tunable. Backend restarts drop live PTY state and
  the in-memory ring; there is no v1-supported durable resume
  across restarts. See
  [`docs/persistent-sessions.md`](persistent-sessions.md) for the
  longer roadmap.
- **Durable terminal recording exists as a foundation but is OFF
  by default.** Writer, durable read API, replay viewer,
  retention worker, and audit are landed end-to-end, but enabling
  recording is a config-and-restart action. An in-product
  enable / disable / export / search UI is post-v1. See
  [`docs/terminal-recording.md`](terminal-recording.md).
- **Restore-from-backup rehearsal is pending.** The runbook is
  shipped; an operator-recorded rehearsal against a throwaway
  Postgres has not been committed. Restoring from a never-tested
  backup on a real incident is the failure mode the rehearsal
  exists to prevent — walk
  [`docs/deployment/backup-restore-runbook.md`](deployment/backup-restore-runbook.md)
  §5 Case R-B before relying on the backups.
- **No multi-user / admin / RBAC.** v1 is single-tenant by
  design. The schema's `users` table is a single-row surface for
  the bootstrap operator. No team features, no shared sessions,
  no admin route.
- **No passkeys / WebAuthn.** The session shape is
  forward-compatible (an enum-tagged credential type), but v1
  ships password-only authentication.
- **No email-based password reset.** Self-hosted operators have a
  documented DB-level recovery path
  ([`docs/production-auth.md`](production-auth.md) §8). Mail
  transport is out of scope.
- **No signed Tauri desktop / Android release artifact.** The
  shells exist as scaffolds under `apps/desktop/` and
  `apps/mobile/` and can be built locally per
  [`docs/deployment/tauri-local-build.md`](deployment/tauri-local-build.md).
  CI workflows, code signing, and store bundling are deferred per
  [`docs/deployment/tauri-ci-release-plan.md`](deployment/tauri-ci-release-plan.md).
  iOS is later than Android.
- **No `tmux` / `screen` replacement.** RelayTerm does not host a
  long-lived host-side multiplexer. The reconnect contract is the
  in-memory TTL window above; deliberate host-side persistence is
  a separate roadmap item.
- **No performance / throughput guarantees.** v1 has no
  benchmark harness. The xterm baseline is judged on
  correctness and operator usability, not on benchmark numbers.
- **No production CSP widening.** The strict production CSP
  (`default-src 'self'`) stays. The staging-only
  `'wasm-unsafe-eval'` relaxation used to evaluate experimental
  renderers does NOT migrate to production.
- **`restty` is a research track.** It needs broader CSP / font /
  WebGPU choices before it can even be matrix-evaluated.
  Deferred per the scorecard.
- **Single-instance only.** The in-memory login throttler and the
  in-memory session orchestrator are single-instance-correct;
  multi-instance / HA / Kubernetes coordination is post-v1.
- **Backup automation, off-site replication, SBOM, signed
  images, managed-secrets integration.** Operator-side concerns;
  no v1 tooling for any of them.

## 4. Renderer posture

The full long-form contract is in
[`docs/spec/terminal-adapters.md`](spec/terminal-adapters.md); the
v1 cutline is in
[`docs/v1-production-readiness.md`](v1-production-readiness.md)
§8; the evaluation plan and snapshot live in
[`docs/terminal-renderer-evaluation.md`](terminal-renderer-evaluation.md)
and
[`docs/renderer-comparison-scorecard.md`](renderer-comparison-scorecard.md).
The load-bearing rules for v1:

- **xterm.js is the v1 production default on every surface** and
  the production compatibility baseline. No renderer default flips
  at v1.
- **`wterm` (`@wterm/dom`)** — promising for mobile and
  browser-native UX (DOM-rendered emulator with a Zig+WASM core
  and CSS-themed grid). Useful for evaluation; **post-v1**.
- **`ghostty-web`** — the correctness candidate (libghostty-vt
  parser via WASM, xterm.js-API-compatible). Useful for
  evaluation; **post-v1**. Needs the resize / reflow Gate-1
  decision and a v1-independent CSP conversation before any
  promotion discussion.
- **`restty`** — research track (libghostty-vt + WebGPU/WebGL2 +
  text-shaper). Needs a viability decision per scorecard §7;
  **post-v1**.
- **The experimental-renderer gate stays OFF by default.**
  Operators may flip
  `experimentalRendererEvaluationEnabled` in Settings for
  personal evaluation. The v1 production recommendation is to
  leave it OFF on the production deploy.
- **Experimental adapters reach the production shell ONLY**
  through the gated lazy loader at
  `apps/web/src/lib/app/terminal/rendererLoader.ts`, via dynamic
  `import()`, AND ONLY when the operator has flipped the gate
  and picked the matching renderer id. Any other path (gate off,
  unknown id, dynamic-import or constructor failure, mount
  failure) collapses to xterm with a typed fallback reason
  surfaced on `data-renderer-fallback`.
- **Renderer evaluation does not block v1.** The track continues
  independently in its own lane and does not appear on the v1
  critical path.

## 5. Deployment notes

The operator runbooks are the load-bearing source; this section
is the index.

- First-deploy planning page (recommended read before the
  release-day checklist):
  [`docs/deployment/first-production-deploy-plan.md`](deployment/first-production-deploy-plan.md)
  — opinionated, short. Says what to decide, the recommended
  posture, the §4 deploy outline, the §5 first-smoke rows that
  resolve cutline blocker B2, and what counts as "ready for
  personal use." Composes the release-day checklist + production
  runbook + backup-restore runbook into a single page.
- Release-day checklist:
  [`docs/v1-release-checklist.md`](v1-release-checklist.md) —
  walk this top to bottom before tagging and deploying v1. The
  §12 decision table is the "is it shippable?" gate.
- Production smoke template:
  [`docs/deployment/v1-production-smoke.md`](deployment/v1-production-smoke.md)
  — copy §5 into a new dated entry when walking the smoke
  against the real production hostname.
- Backup / restore / rollback procedure:
  [`docs/deployment/backup-restore-runbook.md`](deployment/backup-restore-runbook.md)
  — read §4 (pre-upgrade backup) and §5 (restore) end-to-end
  before any risky upgrade.
- Production runbook (canonical operator manual):
  [`docs/deployment/production-runbook.md`](deployment/production-runbook.md)
  — §3 (tag policy), §4 (first deploy), §6 (rollback), §7
  (migration), §8 (backup / restore), §9 (reverse proxy), §10
  (post-deploy smoke), §11 (secret rotation).
- Docker Compose reference and CI image-publish detail:
  [`docs/deployment/docker-compose.md`](deployment/docker-compose.md);
  example Compose files at
  [`deploy/docker-compose.images.example.yml`](../deploy/docker-compose.images.example.yml)
  and
  [`deploy/docker-compose.example.yml`](../deploy/docker-compose.example.yml).
- Migration / health-check / smoke step references:
  - Migrations are profile-gated:
    `docker compose --profile migrate run --rm relayterm-migrate`.
  - Healthchecks defined on `postgres`, `relayterm-backend`,
    `relayterm-web`; the smoke walks them in §5 of the release
    checklist.
  - Auth-flow smoke: [`docs/auth-smoke.md`](auth-smoke.md).
  - SPA smoke: [`apps/web/e2e/SMOKE.md`](../apps/web/e2e/SMOKE.md).

Before any first serious deployment or upgrade:

- Take a Postgres backup (`pg_dump -Fc` to off-host storage) and
  a config backup (`.env` with `chmod 600` preserved) per
  backup-restore-runbook §4.
- Record the image tag AND the digest (`vX.Y.Z` and
  `sha-<short>`) for `relayterm-backend`, `relayterm-web`,
  `relayterm-backend-migrate`. `:main` and unpinned `:latest`
  are NEVER the v1 release tag.
- Know the rollback tag — the previous `vX.Y.Z` you would
  redeploy if the new image misbehaves.
- Verify the production auth envelope: `RELAYTERM_AUTH__MODE =
  production`, `RELAYTERM_AUTH__SESSION_SIGNING_KEY_B64` set and
  ≠ `RELAYTERM_VAULT__MASTER_KEY_B64`, `cookie_secure = true`,
  `allowed_origins` is a byte-equality list containing only the
  production origin, `FIRST_USER_BOOTSTRAP_TOKEN` set on first
  deploy and unset immediately after.

## 6. Upgrade and rollback notes

- **Migrations are forward-only by default.** v1 migrations under
  `apps/backend/migrations/` do not ship `down` steps as a
  routine matter. Treat any forward migration as a one-way
  schema change for backup-and-restore planning.
- **Rollback by image tag works only when the schema is
  backward-compatible.** If the older code can tolerate the
  newer schema (added nullable columns, additive tables), the
  cheap path is to pin `RELAYTERM_IMAGE_TAG` to the previous
  known-good tag and redeploy
  ([backup-restore-runbook §6.1](deployment/backup-restore-runbook.md#61-rollback-by-image-tag-backward-compatible-schema-only)).
- **When the upgrade was backward-incompatible, the documented
  path is restore-from-backup AND image-tag rollback.** There is
  no v1-supported automated `migrate down` for an
  incompatible upgrade. See
  [backup-restore-runbook §6.2](deployment/backup-restore-runbook.md#62-rollback-by-restoring-the-database-backward-incompatible-schema).
- **A DB backup before any risky upgrade is required.** The
  release-checklist §5 names this as a required row; the runbook
  is the procedure.
- **The restore runbook is the escape hatch.** If you cannot
  quickly decide between rollback paths, assume the
  backward-incompatible case and restore from the dump you took
  in step §5 of the release checklist.

## 7. Security notes

- **Private keys remain backend-managed.** Plaintext private
  bytes only exist inside `VaultService::generate_ssh_identity`
  / `import_ssh_identity` and are wiped before return. The
  encrypted bytes are durable in
  `ssh_identities.encrypted_private_key` but MUST NOT cross the
  API boundary for read or list calls.
- **Frontend sees public-key metadata only.** Identity detail
  panels render public-key + SHA-256 fingerprint; no
  `private_key`, no `encrypted_private_key`, no raw PEM, no
  `BEGIN OPENSSH PRIVATE KEY` substring anywhere in the DOM.
  Parser-level redaction backstops are pinned by unit tests.
- **No leakage in logs, audit rows, browser storage, or
  `data-*`.** The canonical sentinel set includes
  `private_key`, `encrypted_private_key`, `BEGIN OPENSSH PRIVATE
  KEY`, `password_hash`, `session_token`, `token_hash`,
  `bootstrap_token`, `argon2id`, `client_info`, `data_b64`,
  `openssh-key-v1`, `passphrase`, and the cookie-value pattern.
  Both the release-checklist §11 grep and the v1 production
  smoke §5.3 sweep query exercise this set.
- **Audit rows carry public metadata only.** Allowed: target id,
  target kind, caller id, public fingerprints, `key_type`,
  `name`, timestamps, reference counts, reason codes.
  **Forbidden:** vault internals, plaintext PEM, raw russh / DB
  error text, peer banners, terminal I/O, `client_info` blobs.
  Sentinel tests in `crates/relayterm-api/tests/api.rs`
  (`AUDIT_FORBIDDEN_SUBSTRINGS`) are the in-CI backstop on every
  audit-emitting kind.
- **Recording chunks are never logged or echoed.** When
  recording is enabled, chunk bytes cross the wire ONLY through
  `TerminalRecordingChunkResponse::data_b64`; they never appear
  in logs, audit rows, response bodies, UI cells, `data-*`
  attributes, or browser storage.
- **Operator responsibilities.** Protect the vault master key
  (`RELAYTERM_VAULT__MASTER_KEY_B64`) — it AEAD-wraps every
  stored SSH private key, and the encrypted bytes are useless
  without it and full-disclosure with it. Protect the session
  signing key (`RELAYTERM_AUTH__SESSION_SIGNING_KEY_B64`) — keep
  it separate from the vault key. Protect the bootstrap token
  before first deploy and unset it immediately after. Protect
  the `.env` file (`chmod 600`) and the Postgres backups; treat
  the combination as full disclosure of all stored SSH
  identities. See
  [backup-restore-runbook §3](deployment/backup-restore-runbook.md#3-sensitive-material-warning).

## 8. Post-v1 roadmap

Concrete follow-on work, in roughly the order the v1 cutline
ranks it. None of these block v1; each is real future work.

- **Record the production-walked end-to-end smoke** once a
  production hostname is chosen (resolves cutline B2).
- **Record a mobile portrait sanity walk** on a real Android
  phone against the default xterm production path (resolves
  cutline B3).
- **Restore-from-backup rehearsal** against a throwaway Postgres
  (`docs/backup-restore-rehearsal-record`).
- **Operational status enhancements** (more deployment signals
  in the existing Settings panel; no new backend endpoints).
- **Mobile command / modifier bar** and a deliberate
  portrait-polish pass (typography, soft-keyboard affordance,
  paste UX).
- **Renderer evaluation continuation** for `wterm` and
  `ghostty-web` (see
  [`docs/terminal-renderer-evaluation.md`](terminal-renderer-evaluation.md)
  and
  [`docs/renderer-comparison-scorecard.md`](renderer-comparison-scorecard.md)).
- **Recording UI** — in-product enable / disable / export /
  search; per-chunk encryption envelope; recording master key
  rotation.
- **Tauri release pipeline** — CI workflows, code signing, Play
  Store / App Store bundling (per
  [`docs/deployment/tauri-ci-release-plan.md`](deployment/tauri-ci-release-plan.md)).
  iOS shell.
- **Passkeys / WebAuthn**, email password reset, IP-aware
  throttling.
- **Multi-user / admin / RBAC**, shared sessions, admin /
  operator surface.
- **Multi-instance / HA / Kubernetes deploy story** (shared
  throttler, shared session orchestrator).
- **Backup automation, off-site replication, SBOM, signed
  images, managed-secrets integration.**
- **Persistent host-side sessions** (`tmux`-like multiplexing;
  see [`docs/persistent-sessions.md`](persistent-sessions.md)).
- **Restty viability decision** (CSP / font / WebGPU choice).

## 9. Release checklist pointer

The release-day operator checklist is
[`docs/v1-release-checklist.md`](v1-release-checklist.md). Walk
its §3–§11 top to bottom; treat its §12 decision table as the
"is it shippable?" gate. The release notes here are the body of
the published changelog; the checklist is the operator-facing
release gate that produces the evidence those notes summarize.

**Which sign-off goes where.** The checklist §13 sign-off
template and the release notes §11 sign-off block are two views
of the same release event with different audiences:

- The release notes §11 block is the **release-log entry** — the
  operator-recorded source of truth for "v1 was deployed at
  hostname X with image tag Y, with these caveats accepted." Copy
  it on tag day.
- The checklist §13 block is the **walk-evidence template** — the
  per-section `§3 Repo pre-release  PASS` style checkboxes that
  record which gates were actually walked. Use it (or its
  inlined `Checklist walk` row in §11 below) to capture that
  evidence next to the release-log entry.

Either commit both blocks together, or use the `Checklist walk`
row in §11 below to inline the walk-status into the single
release-log entry. The point is that no evidence row goes
unrecorded.

## 10. See also

- [`docs/v1-production-readiness.md`](v1-production-readiness.md)
  — the v1 cutline these notes summarize.
- [`docs/v1-release-checklist.md`](v1-release-checklist.md) —
  the release-day operator gate.
- [`docs/deployment/first-production-deploy-plan.md`](deployment/first-production-deploy-plan.md)
  — short opinionated planning page for the first personal
  production deploy; pairs with the release-day checklist.
- [`docs/deployment/v1-production-smoke.md`](deployment/v1-production-smoke.md)
  — production smoke template (NOT EXECUTED until B2 lands).
- [`docs/deployment/backup-restore-runbook.md`](deployment/backup-restore-runbook.md)
  — operator-facing backup / restore / rollback procedure.
- [`docs/deployment/production-runbook.md`](deployment/production-runbook.md)
  — load-bearing operator runbook.
- [`docs/deployment/docker-compose.md`](deployment/docker-compose.md)
  — Compose stack reference + CI image-publish detail.
- [`docs/deployment/vps-staging-smoke.md`](deployment/vps-staging-smoke.md)
  — staging-smoke history.
- [`docs/spec/inventory.md`](spec/inventory.md) — inventory
  lifecycle and destructive-action policy.
- [`docs/spec/terminal-adapters.md`](spec/terminal-adapters.md)
  — xterm-as-default rule and experimental-renderer gate
  contract.
- [`docs/spec/auth.md`](spec/auth.md) — auth contract.
- [`docs/spec/recording.md`](spec/recording.md) +
  [`docs/terminal-recording.md`](terminal-recording.md) —
  recording design and operator contract.
- [`docs/private-key-import.md`](private-key-import.md) — v1
  Ed25519 unencrypted OpenSSH import constraint.
- [`docs/production-auth.md`](production-auth.md) — auth
  operator surface and the "lost the only password" recovery
  path.
- [`docs/terminal-renderer-evaluation.md`](terminal-renderer-evaluation.md)
  +
  [`docs/renderer-comparison-scorecard.md`](renderer-comparison-scorecard.md)
  — renderer evaluation lane (post-v1).
- [`docs/persistent-sessions.md`](persistent-sessions.md) —
  longer-term reconnect / persistence roadmap.
- [`apps/web/e2e/SMOKE.md`](../apps/web/e2e/SMOKE.md) — SPA
  smoke runbook.

---

## 11. Draft sign-off block (template)

This block is the **release-log entry**, not part of the
published changelog body (§1–§10 is the body). Copy it into the
release log on tag day. The fields are deliberately the union of
the release-checklist §13 sign-off, the v1 production smoke §5
entry header, and (via `Checklist walk:` below) the §13
walk-section status lines, so a single committed entry carries
the full evidence trail without a second template alongside it.

```
## YYYY-MM-DD · RelayTerm v1 release sign-off

- Release tag:           vX.Y.Z
- Image tag deployed:    vX.Y.Z   (commit sha-<short>)
- Image digests:         relayterm-backend          sha256:<…>
                         relayterm-web              sha256:<…>
                         relayterm-backend-migrate  sha256:<…>
- Production hostname:   https://<origin>
- DB migration version:  <latest applied migration ID>
- Pre-deploy backup:     <off-host path / object key>
- Config backup:         <off-host path / object key>
- Rollback tag:          vX.Y.(Z-1)   (commit sha-<short>)
- Operator:              <name / handle>

Checklist walk (from docs/v1-release-checklist.md §13):
- [ ] §3  Repo pre-release        PASS
- [ ] §4  Configuration           PASS
- [ ] §5  Deployment              PASS
- [ ] §6  First-user / auth       PASS
- [ ] §7  Inventory (production)  PASS
- [ ] §8  Terminal (production)   PASS
- [ ] §9  Mobile portrait sanity  PASS    (resolves B3)
- [ ] §10 Backup / restore / rollback   PASS
- [ ] §11 Security / redaction    PASS

Smoke status:
- B2 production smoke:        PASS  /  PENDING  /  BLOCKED
  Evidence: docs/deployment/v1-production-smoke.md § <date>
- B3 mobile portrait sanity:  PASS  /  PENDING  /  BLOCKED
  Evidence: <doc path or "production-use-driven, deferred">
- Restore-from-backup rehearsal:  PASS  /  PENDING
  Evidence: <doc path>

Posture confirmed:
- xterm.js is the default renderer; experimental gate OFF.
- Recording enabled:  no   (or: yes — and docs/terminal-recording.md re-read)
- Production CSP unchanged from strict default-src 'self'.
- Single-instance Docker Compose deploy.

Known caveats accepted for ship (post-v1, not blocking):
-
-

Decision:  SHIP  /  HOLD
Notes:
```

Do not let this entry sit uncommitted. The release notes here +
the sign-off entry above are the public v1 record.
