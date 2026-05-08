# AGENTS.md / SPEC.md split — preservation map

> Generated 2026-05-06 in branch `docs/split-agent-spec-context`.
>
> This file is the audit trail for the context-split refactor. It lists
> every load-bearing rule that moved out of `AGENTS.md` or `SPEC.md`
> and where it landed, plus what was archived, what was summarised,
> and what was deleted as duplicate.
>
> Cite this file in any review that asks "where did rule X go?"

## Goals

1. Reduce AGENTS.md context weight (was 56 KB / 275 lines) so the
   session-start system reminder stops surfacing the >40 KB warning
   while keeping every load-bearing rule reachable.
2. Reduce SPEC.md context weight (was 407 KB / 2083 lines) by indexing
   the per-surface contract into `docs/spec/*` files.
3. Preserve every redaction, auth, session, and architectural rule.
4. Preserve every product behaviour contract. No rewriting of meaning.
5. Avoid creating dozens of tiny files — one doc per surface area.

## Non-goals

- This refactor does NOT change Rust, Svelte, Dockerfile, Compose, or
  CI behaviour.
- This refactor does NOT change product contracts. Where a moved rule
  is paraphrased in the AGENTS / SPEC summary, the full prose lives in
  the linked file.

## New files

| File | Purpose |
|---|---|
| `docs/agent/context-split-map.md` | This map. |
| `docs/agent/encountered-lessons.md` | Archived one-off lessons (kept the most recent / cross-cutting ones inline in AGENTS.md). |
| `docs/agent/redaction-rules.md` | Long-form prose for the audit / session / paste / recording / CSRF / login-throttle redaction rules. |
| `docs/agent/task-patterns.md` | Long-form step-by-step procedures for renderer adapters, production views, and data fetches. |
| `docs/spec/README.md` | Per-area index of the split SPEC. |
| `docs/spec/terminal.md` | Renderer-independent terminal/session/workspace behavior: terminal-session lifecycle, WebSocket attach/detach, terminal-core, live PTY bridge, replay buffer, terminal launch / sessions list / settings / viewport / paste / local recovery / status refresh. Renderer adapter packages are summarized here and fully specified in `terminal-adapters.md`. |
| `docs/spec/terminal-adapters.md` | Concrete renderer adapter contracts for `terminal-xterm` (production baseline) and `terminal-ghostty-web` / `terminal-restty` / `terminal-wterm` (experimental, dev-only). Created 2026-05-07 in `docs/split-terminal-renderer-spec` to drop terminal.md from ~155 KB to ~127 KB so an agent following SPEC.md → terminal.md no longer pulls 33 KB of adapter detail it doesn't need. |
| `docs/spec/auth.md` | Credential creation, host-key trust, auth-check, production authentication architecture. |
| `docs/spec/auth-implementation-history.md` | Per-slice landed-state narrative split out of `auth.md` on 2026-05-07. Append-only as new auth slices land; not normative on its own (the contracts live in `auth.md`). |
| `docs/spec/inventory.md` | Inventory views, identity / host / profile creation UI, host-key preflight UI, auth-check UI, dashboard, recent activity, server-profile disable/enable backend + audit + UI. |
| `docs/spec/recording.md` | Load-bearing invariants for durable recording, plus pointer to `docs/terminal-recording.md`. |
| `docs/spec/web-shell.md` | Production web-app shell chrome and URL routing. |
| `docs/spec/tauri-runtime-backend-url.md` | Design-only doc (no implementation yet) for the runtime backend URL chosen by built Tauri desktop/mobile shells. Recommends path A — remote web shell — to keep the existing `SameSite=Strict` cookie + `CsrfGuard` posture unchanged; explicitly defers path B (cross-origin bundled SPA) because it would weaken auth. Created 2026-05-08 in branch `docs/tauri-runtime-backend-url-design` to close `tauri-ci-release-plan.md` § 9 questions 5 and 7 (for path A) and to give the launch-smoke `Cannot Reach RelayTerm` modal a documented next step. |

## AGENTS.md — sections moved or compacted

| Source section | Destination | Action |
|---|---|---|
| Stack table (renderer rows: ghostty-web / restty / @wterm/dom) | Stack table stays; per-renderer adapter prose lives in `docs/spec/terminal.md` § "ghostty-web …", "restty …", "wterm …" | Compact: keep version pin + 1-line "why"; long API caveats moved to the matching adapter doc. |
| Things to avoid (rows 14–24, the multi-paragraph rules) | `docs/agent/redaction-rules.md` §§ 1–15 | Replace each long row with a 1-line don't / do summary plus a `(see `docs/agent/redaction-rules.md` §N)` pointer. |
| Task patterns (renderer adapter / production view / data fetch) | `docs/agent/task-patterns.md` §§ 1–4 | Replace each multi-step procedure with a 1-line summary plus a `(see `docs/agent/task-patterns.md` §N)` pointer. |
| Encountered Lessons (entries from 2026-04-28 through 2026-05-04 plus the two CI lessons) | `docs/agent/encountered-lessons.md` | Archive. The 2026-05-06 nginx lesson and the most cross-cutting recent lessons stay inline so they ride session-start context. |

## AGENTS.md — sections preserved verbatim

| Section | Reason |
|---|---|
| Project / architectural rule / session start ritual | Load-bearing entry point. |
| Stack table | Pinned versions are normative; only the renderer "why" prose is compacted. |
| Critical gotchas (one-liners) | Short by design; nothing moves. |
| Web app defaults | Three-bullet overlay; nothing moves. |
| Folder conventions | Tree diagram + Tauri shell rule; nothing moves. |
| Decision tables | Where-does-this-go and ownership tables stay verbatim. |
| Things to avoid (rows 1–13, the short don't/do pairs) | These are the high-frequency rules that need to stay in session-start context. |
| Git workflow | Three bullets; nothing moves. |
| Definition of done | 10-step checklist; load-bearing for every PR. |
| Maintenance protocol | Trigger table; nothing moves. |
| When unsure | Two short paragraphs; nothing moves. |

## SPEC.md — sections moved

| Source section (line range in pre-split file) | Destination |
|---|---|
| Credential creation contract (52–63) | `docs/spec/auth.md` |
| Host-key preflight + known-host trust contract (64–84) | `docs/spec/auth.md` |
| Terminal-session lifecycle contract (85–103) | `docs/spec/terminal.md` |
| Terminal WebSocket attach/detach contract (105–202) | `docs/spec/terminal.md` |
| Frontend terminal-core contract (203–279) | `docs/spec/terminal.md` |
| xterm.js baseline renderer adapter (281–315) | `docs/spec/terminal-adapters.md` (initially `docs/spec/terminal.md`; moved 2026-05-07) |
| ghostty-web experimental renderer adapter (317–366) | `docs/spec/terminal-adapters.md` (initially `docs/spec/terminal.md`; moved 2026-05-07) |
| restty experimental renderer adapter (368–418) | `docs/spec/terminal-adapters.md` (initially `docs/spec/terminal.md`; moved 2026-05-07) |
| wterm experimental renderer adapter (420–465) | `docs/spec/terminal-adapters.md` (initially `docs/spec/terminal.md`; moved 2026-05-07) |
| Production web app shell (467–509) | `docs/spec/web-shell.md` |
| Production inventory read-only views (511–540) | `docs/spec/inventory.md` |
| Production read-only inventory detail panels (541–571) | `docs/spec/inventory.md` |
| Production inventory client-side search & filters (572–597) | `docs/spec/inventory.md` |
| Production SSH identity generation UI (598–633) | `docs/spec/inventory.md` |
| Production host & server-profile creation UI (634–670) | `docs/spec/inventory.md` |
| Production host-key preflight & trust UI (671–711) | `docs/spec/inventory.md` |
| Production SSH auth-check UI (712–752) | `docs/spec/inventory.md` |
| Production terminal launch UI (754–805) | `docs/spec/terminal.md` |
| Production terminal sessions list/status UI (806–845) | `docs/spec/terminal.md` |
| Production terminal settings foundation (846–890) | `docs/spec/terminal.md` |
| Production terminal viewport controls (891–949) | `docs/spec/terminal.md` |
| Production terminal paste safety (950–1001) | `docs/spec/terminal.md` |
| Production active terminal local recovery (1002–1044) | `docs/spec/terminal.md` |
| Production session status refresh and stale-session handling (1045–1098) | `docs/spec/terminal.md` |
| URL-driven production view routing (1099–1137) | `docs/spec/web-shell.md` |
| Production dashboard summary (1138–1180) | `docs/spec/inventory.md` |
| Dashboard recent activity (1181–1209) | `docs/spec/inventory.md` |
| Live SSH PTY bridge contract (1210–1373) | `docs/spec/terminal.md` |
| Output sequence + in-memory replay buffer contract (1374–1439) | `docs/spec/terminal.md` |
| Durable terminal recording and replay architecture (1440–1478) | `docs/spec/recording.md` |
| Authenticated SSH credential check contract (1479–1509) | `docs/spec/auth.md` |
| Server profile disable / enable backend + audit + read API + UI (1613–1759) | `docs/spec/inventory.md` |
| Future implementation order (1760–1776) | `docs/spec/inventory.md` |
| Production authentication architecture (1777–2047) | `docs/spec/auth.md` |

## SPEC.md — sections preserved verbatim

| Section | Reason |
|---|---|
| Header + Overview (1–15) | Load-bearing entry point. |
| Architectural invariants (16–25) | Normative; never moves. |
| Data model (26–47) | Schema source-of-truth pointer + entity list. |
| Behavior contracts (1510–1517) | Short, normative cross-surface invariants. |
| Inventory lifecycle and destructive-action policy (1518–1612) | Normative load-bearing policy that governs every destructive surface. The per-entity tables, FK rules, and audit-event expectations stay in SPEC.md so a reviewer never has to chase them. |
| Integration points (2048–2056) | Short; nothing moves. |
| Out of scope (v1) (2057–2070) | Short; nothing moves. |
| Open questions (2071–2082) | Short; nothing moves. |

## SPEC.md — sections summarized in place

For each "moved" section above, SPEC.md now carries a short summary
under the Surfaces index, with a link to the matching `docs/spec/*`
document. The summary is short enough to keep SPEC.md indexable but
preserves the surface name and the key contract so an agent skimming
the index sees the load-bearing facts before deciding to follow the
link.

## Sections deleted as duplicate

None. Every section that left AGENTS.md or SPEC.md was relocated, not
deleted. The exact prose is preserved in the destination file.

## Open questions

- ~~Should `docs/spec/terminal.md` itself be split per-surface in a
  future slice?~~ **Partially addressed (2026-05-07):** renderer adapter
  contracts moved to `docs/spec/terminal-adapters.md` in branch
  `docs/split-terminal-renderer-spec`. terminal.md dropped from ~940
  lines / 154 KB to ~772 lines / ~127 KB; new terminal-adapters.md is
  ~218 lines / ~33 KB. Multi-review's Should-fix is resolved for the
  renderer-adapter slice — adapter detail no longer rides every
  terminal-question read. terminal.md still owns lifecycle, transport,
  PTY bridge, replay buffer, production UI, paste safety, local
  recovery, and status refresh; further per-surface splits are not
  scheduled.
- ~~`docs/spec/auth.md` is 109 KB. Multi-review (2026-05-07) flagged it
  as a candidate for trimming — implementation-status narrative could
  move to code comments / archive, leaving contract-shape only.
  Tracking, not yet scheduled.~~ **Resolved (2026-05-07):** in branch
  `docs/trim-auth-spec`, the per-slice "✅ Landed" narrative for steps
  1–13 of "Implementation order", the "Status today" mega-paragraph,
  the inline `AuthenticatedUser` Rust pseudo-code, the "Active sessions
  list" landed bullet, the throttling landed sub-bullet, and the legacy
  `dev@relayterm.local` paragraph were all replaced in place with
  1–3-sentence summaries that link to the new
  `docs/spec/auth-implementation-history.md`. auth.md dropped from 104
  KB / 355 lines to 62 KB / 301 lines (~40% reduction). All 14
  load-bearing auth contracts (production/dev mode distinction,
  bootstrap, cookie-backed auth, opaque session model, session_token /
  password_hash redaction, CSRF/Origin guard, login throttle without
  user-existence oracle, logout / password-change / session-management
  semantics, audit-event payload boundaries, deferred work, no
  remaining dev-auth wording) remain discoverable via grep. Drift
  policy: when a new auth slice lands, append the slice's status
  paragraph to `auth-implementation-history.md`; the contract goes in
  `auth.md`.
- ~~The `Encountered Lessons` cap of ~20 entries needs an explicit
  rotation policy.~~ **Resolved (2026-05-06):** AGENTS.md "Maintenance
  protocol" now says `archive cap ~10 entries; older lessons graduate
  to docs/agent/encountered-lessons.md`, and the Encountered Lessons
  header restates the cap. The 2026-05-03 Forgejo CI runner lesson
  was archived under this policy; if a future review decides it is
  cross-cutting enough to ride session-start context, promote it back
  to the inline section.

## Verification

- AGENTS.md and SPEC.md both still link to each other and to
  `docs/spec/*`, `docs/agent/*`.
- Every "(see X)" pointer in AGENTS.md resolves to a real file.
- `grep` smoke for the high-risk anchors (private_key,
  encrypted_private_key, session_token, token_hash, data_b64,
  Origin, CSRF, tokio::spawn, recording_purged, terminal_sessions,
  Tauri) shows each rule still present in either AGENTS.md, SPEC.md,
  one of the new docs, or an existing operational doc.
- The grep smoke is now codified in
  [`scripts/check-doc-contracts.sh`](../../scripts/check-doc-contracts.sh)
  (also wired as `pnpm run check:docs-contracts`). Run it after editing
  AGENTS.md, SPEC.md, `docs/spec/*`, or `docs/agent/*`. It verifies the
  required files exist, the AGENTS.md and SPEC.md section anchors are
  intact, the high-risk cross-corpus terms remain discoverable, the
  stale dev-auth phrasings stay absent, the `docs/spec/*` cross-file
  links resolve, and the renderer / auth / recording contract terms
  stay reachable in their respective corpora. It is read-only, runs
  without network access, and uses only standard shell tools. CI
  enforcement is intentionally deferred — run it locally for now.

## Drift policy

When a contract changes:

1. Update the destination file in `docs/spec/*` or `docs/agent/*`.
2. Update the matching summary in `SPEC.md` or `AGENTS.md`.
3. Add an entry to this map only if the destination changes (the
   shape of the split moves).
4. Re-run `pnpm run check:docs-contracts` (which invokes
   `scripts/check-doc-contracts.sh` from the repo root). If a removed
   term is intentional, update the script's term list in the same
   change and note the rationale here.
