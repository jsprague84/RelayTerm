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
| `docs/spec/terminal.md` | Terminal-session lifecycle, WebSocket attach/detach, terminal-core, four renderer adapters, live PTY bridge, replay buffer, terminal launch / sessions list / settings / viewport / paste / local recovery / status refresh. |
| `docs/spec/auth.md` | Credential creation, host-key trust, auth-check, production authentication architecture. |
| `docs/spec/inventory.md` | Inventory views, identity / host / profile creation UI, host-key preflight UI, auth-check UI, dashboard, recent activity, server-profile disable/enable backend + audit + UI. |
| `docs/spec/recording.md` | Load-bearing invariants for durable recording, plus pointer to `docs/terminal-recording.md`. |
| `docs/spec/web-shell.md` | Production web-app shell chrome and URL routing. |

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
| xterm.js baseline renderer adapter (281–315) | `docs/spec/terminal.md` |
| ghostty-web experimental renderer adapter (317–366) | `docs/spec/terminal.md` |
| restty experimental renderer adapter (368–418) | `docs/spec/terminal.md` |
| wterm experimental renderer adapter (420–465) | `docs/spec/terminal.md` |
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

- Should `docs/spec/terminal.md` itself be split per-surface in a
  future slice? It is now ~940 lines / 154 KB. The threshold the user
  warned about is the AGENTS.md 40 KB session-start budget; SPEC area
  docs are not loaded at session start, so 940 lines is fine for the
  steady state. **Multi-review (2026-05-07) flagged this as a Should-fix
  because an agent that follows SPEC.md → `docs/spec/terminal.md` for
  any terminal question pulls 154 KB at once.** A future slice may
  split renderer-adapter contracts to `docs/spec/terminal-adapters.md`
  or per-adapter files. Tracking, not yet scheduled.
- `docs/spec/auth.md` is 109 KB. Multi-review (2026-05-07) flagged it
  as a candidate for trimming — implementation-status narrative could
  move to code comments / archive, leaving contract-shape only.
  Tracking, not yet scheduled.
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

## Drift policy

When a contract changes:

1. Update the destination file in `docs/spec/*` or `docs/agent/*`.
2. Update the matching summary in `SPEC.md` or `AGENTS.md`.
3. Add an entry to this map only if the destination changes (the
   shape of the split moves).
