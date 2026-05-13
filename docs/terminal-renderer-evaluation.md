# Terminal renderer evaluation plan

> Product / technical plan for RelayTerm's next phase after the
> single-user deployable baseline. This doc is a **plan**, not a
> contract: nothing here ships before the matching SPEC entries and
> per-package contracts land. The renderer-adapter contracts proper
> live in [`docs/spec/terminal-adapters.md`](spec/terminal-adapters.md);
> the architectural invariants they sit under live in
> [`SPEC.md`](../SPEC.md) § "Architectural invariants" and AGENTS.md.

## Status

**Draft, not started.** The deployable baseline is on `main` and was
exercised end-to-end against staging on 2026-05-13 (see
[`docs/deployment/vps-staging-smoke.md`](deployment/vps-staging-smoke.md)
§ "2026-05-13 · Deployable-baseline end-to-end staging smoke"). The
private-key import v1 smoke landed the same day. Renderer evaluation
is the next product track. Quota / metrics / dashboard work is
paused unless an explicit ask reopens it.

A **reference smoke for the production xterm baseline renderer**
landed on 2026-05-13 (see
[`docs/deployment/vps-staging-smoke.md`](deployment/vps-staging-smoke.md)
§ "2026-05-13 · Xterm production-baseline renderer smoke"). It
exercises launch, basic I/O, in-session resize / fit, 300-line
burst, wire-side detach / reconnect inside the 30 s TTL,
mobile-width workspace, and clean close against a hermetic
throwaway SSH target with no host-port exposure and zero
sentinel leakage in DOM / logs / audit. It is the comparison
point future ghostty-web / restty / wterm candidate smokes are
measured against. The baseline smoke deliberately did NOT
exercise Unicode, copy / paste, alternate screen, or mouse
support (input-path limitations recorded inline in the smoke
entry); each is its own deferred slice. The harness plan that
carries those deferred rows forward — input-path taxonomy,
recommended renderer-fairness strategy, command matrix, and
next-slice options — lives in
[`docs/renderer-smoke-harness.md`](renderer-smoke-harness.md);
the experimental renderer comparisons (ghostty-web → restty →
wterm) stay deferred until that harness plan's recommended
implementation slice lands. **xterm is and remains the
production compatibility baseline and the default renderer**
until a candidate clears the Gate 2 promotion criteria below.

## Purpose

Decide which terminal renderer RelayTerm should ship in production —
and on which device class — by exercising the four adapters that
already exist behind the renderer-neutral seam (`xterm`, `ghostty-web`,
`restty`, `wterm`) and a future native/Tauri candidate against the
same evaluation matrix. The output of this work is a recommendation
(which renderer becomes the default on desktop browser, on Tauri
desktop, and on Android), not a finished renderer-swap UX.

## Product positioning

- RelayTerm is a **secure SSH session and control-plane app** with
  renderer experimentation. The renderer is one swappable layer; the
  load-bearing differentiators (backend-owned SSH sessions,
  sequence-numbered replay, paste-safety policy, audit/redaction
  posture, host-key + private-key trust model) are independent of the
  renderer choice.
- RelayTerm **is not trying to clone tmux.** The product is "a web /
  Tauri client for SSH sessions whose state lives on the backend and
  survives client disconnects," not "tmux in a browser." Multi-pane
  workspaces, window layouts, copy-mode, and the rest of the tmux
  feature surface are explicitly out of scope for this track.
- **tmux / screen are a future optional integration**, not a current
  primitive. The current backend primitive is the in-memory replay
  ring (default `max_frames = 1024`, `max_bytes = 1 MiB`) plus the
  bounded `DETACHED_LIVE_PTY_TTL` reconnect window — see
  [`docs/spec/terminal.md`](spec/terminal.md) § "Output sequence +
  in-memory replay buffer contract" and § "Detached-session TTL
  contract." A host-side multiplexer (Option C) is one of several
  staged options in [`docs/persistent-sessions.md`](persistent-sessions.md)
  and is **deferred**. Nothing in the renderer track depends on it
  and nothing in the renderer track unlocks it.

## Current baseline

- `@relayterm/terminal-xterm` (xterm.js v5) is the **production
  compatibility baseline and the default renderer**. The production
  terminal workspace uses `@relayterm/terminal-core` +
  `@relayterm/terminal-xterm` only. The isolation rule is pinned by
  `apps/web/tests/appShellIsolation.test.ts`.
- `@relayterm/terminal-ghostty-web`, `@relayterm/terminal-restty`,
  and `@relayterm/terminal-wterm` are **experimental and dev-only**.
  Each adapter is wired into the dev-only live terminal lab
  (`apps/web/src/lib/dev/XtermLiveTerminalLab.svelte`); production
  bundles tree-shake all three out. None of them is promoted into
  any production view as part of this track without passing the
  promotion gates below.
- The renderer-neutral seam is the contract these candidates sit
  behind: `TerminalRenderer` from `@relayterm/terminal-core`,
  consumed by `TerminalSessionClient`. The wire protocol (control
  plane JSON + `RTB1` binary `Output`/`Input` envelope) does not
  change for any renderer experiment.

## Evaluation candidates

| Candidate | Package | Today's status |
|---|---|---|
| xterm.js baseline | `@relayterm/terminal-xterm` | Production default. Compatibility baseline. |
| ghostty-web | `@relayterm/terminal-ghostty-web` | Experimental (dev lab only). libghostty-vt via WASM; xterm-API-compatible. |
| restty | `@relayterm/terminal-restty` | Experimental (dev lab only). libghostty-vt + WebGPU/WebGL2 + text shaping; ~3 MB JS + inlined WASM. |
| wterm | `@relayterm/terminal-wterm` | Experimental (dev lab only). DOM-rendered cell grid + Zig/WASM core; mobile / accessibility / native-text-input oriented. |
| Future native / Tauri | not started | Hypothetical native renderer embedded inside the Tauri shells. Not in this slice; listed so the matrix has a place for it when the time comes. |

## Non-negotiable architecture rules

Every evaluation step MUST respect these. If a renderer requires
breaking one of them to look good, that renderer fails the
evaluation, not the architecture.

1. **Backend protocol stays RelayTerm-shaped.** Wire-stable JSON
   control plane and `RTB1` binary `Output`/`Input` envelope. A
   renderer that needs new data transforms what is already on the
   wire — the protocol does not reshape itself for any renderer.
2. **`terminal-core` stays renderer-neutral.** No imports of
   `@xterm/*`, `ghostty-web`, `restty`, or `@wterm/*` are allowed
   inside `terminal-core`. The neutral shapes (`TerminalRenderer`,
   `BaseTerminalRendererOptions`, `RendererTheme`,
   `RendererThemeAnsi`, `RendererCursorStyle`) are the only thing
   adapters extend.
3. **Renderer-specific APIs stay behind adapter escape hatches.**
   Each adapter exposes a local `<renderer>Only` options slot
   (`xtermOnly`, `ghosttyOnly`, `resttyOnly`, `wtermOnly`) that is
   explicitly **non-portable** across adapters and never leaks into
   `TerminalRenderer` or `BaseTerminalRendererOptions`.
4. **No raw terminal input / output logging.** Keystrokes, PTY
   output bytes, and decoded terminal strings never appear in
   `console.*`, log lines, thrown `Error` messages, `audit_events`,
   `data-*` attributes, `localStorage`, `sessionStorage`, the
   recording chunk wire shape (chunk bytes cross only via
   `data_b64`), or any DOM string outside the terminal viewport
   itself. The sentinel-string redaction tests pinned for each
   adapter (`packages/terminal-*/tests/*Renderer.test.ts`)
   are the executable backstop and remain green for any new
   candidate.
5. **No renderer owns reconnect state.** Sequence numbers, the
   in-memory replay ring, the detach window, and resume-from-`seq`
   handshake are all orchestrator-owned. A renderer is free to
   maintain a transient local scroll position or selection, but
   nothing about correctness across `client_dropped → reconnect →
   resume_at_sequence_n` is allowed to live in the renderer.
6. **Production shell isolation.** `apps/web/src/lib/app/**` cannot
   import from `lib/dev/` or any experimental renderer adapter
   package. The dev lab is the only consumer of experimental
   renderers until promotion (see "Promotion criteria" below).

## Evaluation matrix

The matrix below is the **set of dimensions** a candidate is graded
on. It is intentionally not a benchmark harness — the harness is
deferred (see "Explicitly deferred" below). Each row is something a
human evaluator can answer with a short verdict (`works` / `works
with caveats` / `regression vs. baseline` / `blocker`) plus
free-form notes.

### Core correctness

| Dimension | What to verify |
|---|---|
| Basic input/output | Round-trip a typed line through `Input` → PTY → `Output` → renderer. No garbled bytes, no lost keystrokes, no echoed plaintext outside the viewport. |
| Resize / fit / reflow | Container resize fires `onResize(cols, rows)`, the wire `resize` frame flows, the PTY honours it, and the renderer reflows existing scrollback without corruption. |
| Detach / reconnect / replay | Tear down the client, reconnect with `(session_id, last_seen_seq)`, observe the `replay_start → output … → replay_end` handshake, confirm the renderer state is consistent with what the orchestrator replayed. `replay_window_lost` surfaces as a clean error, not a crash. |
| Long output / backpressure | Stream a `yes`-style firehose for a fixed window. Renderer must accept all bytes the orchestrator delivers; no internal queue stalls, no torn frames, no missing tail. Watch for `Terminal.write` callbacks if the adapter exposes one (xterm does). |
| Alternate screen / full-screen apps | `htop`, `vim`, `less`, `ssh -t … bash` — the alternate-screen switch and restore round-trips cleanly; cursor returns to the right cell on exit. |
| Mouse support | Where the renderer claims SGR mouse mode, click + drag + wheel translate into wire-correct input. Where it does not, the failure is documented, not silently wrong. |

### Text / typography

| Dimension | What to verify |
|---|---|
| Unicode, emoji, box drawing, wide chars | CJK + emoji + `┌─┐` table characters render at the right cell width. Wide chars do not desync the column count. ZWJ sequences render as one glyph or are documented as not supported. |
| Theme / settings parity | The neutral knobs (`fontFamily`, `fontSize`, `lineHeight`, `cursorStyle`, `cursorBlink`, `scrollbackLines`, `theme`) apply on the candidate, OR are documented as silently dropped (with the alternative — e.g. CSS custom properties on the `.wterm` host — captured in the adapter docs). |
| Scrollback behavior | `scrollbackLines` honoured (or the renderer's analogue documented). Scrollback survives renderer-internal redraws; it is wiped on `dispose` (correct — replay restores from the orchestrator). |

### Platform fit

| Dimension | What to verify |
|---|---|
| Copy / paste (clipboard, paste-safety) | Browser clipboard read/write integrates with the renderer's selection model. `evaluatePaste` policy still gates large / multiline / control-char-heavy pastes — paste handling lives above the renderer; this row confirms the renderer does not bypass it. |
| Mobile keyboard / soft IME | On Android (Tauri + browser), virtual keyboard typing, autocorrect, swipe-to-type, CJK IME, and emoji picker reach the wire as the expected bytes. The `wterm` DOM-rendered grid is specifically motivated by this row; the canvas renderers (`xterm`, `ghostty-web`, `restty`) may have rougher mobile behavior — document the gap honestly. |
| Accessibility | Screen-reader-friendly output (the DOM-rendered `wterm` adapter is the candidate here); focus ring, keyboard-only navigation, contrast under the curated themes. Document gaps; do not block on this row in the first pass. |
| Bundle size / tree-shaking | Confirm the candidate stays tree-shaken out of the production `apps/web` bundle while `import.meta.env.DEV` gates the dev lab. Caveats from `docs/spec/terminal-adapters.md` (none of the upstreams currently declare `sideEffects: false` in their own `package.json`) still hold. |
| Memory / CPU rough observations | Human-readable notes only ("scrolling 50 MiB feels smooth on this laptop"). Not a microbenchmark. |

### Safety / redaction

| Dimension | What to verify |
|---|---|
| Redaction / log safety | Re-run each adapter's redaction test (`packages/terminal-*/tests/*Renderer.test.ts`). Add a manual sweep: trigger an Error inside `onInput`, write a sentinel string, dump the rendered HTML — the sentinel appears only inside the terminal viewport, nowhere in `console`, `localStorage`, `sessionStorage`, the lab event log, or any `data-*` attribute. |
| Renderer swap during attach | Switching renderers tears down the previous adapter and `TerminalSessionClient`, reconnects via the same `(session_id, last_seen_seq)` path, and replays. No payload bytes appear in the lab event log on switch (only the renderer name). |

## Promotion criteria

A candidate moves through three states: **experimental (dev-lab
only)** → **production opt-in** → **production default**. Each
state has a hard gate.

### Gate 1 — experimental → production opt-in

The candidate must pass before it is allowed to render in any
production-shell view (even behind a user-facing "experimental"
toggle). All of:

1. Every row of the **Core correctness** group is `works` or
   `works with caveats` on at least one of the three target
   surfaces (desktop browser, desktop Tauri, Android Tauri /
   Android browser), and the caveats are documented in
   `docs/spec/terminal-adapters.md`.
2. The four non-negotiable architecture rules above hold. The
   adapter's redaction tests are green; the `terminal-core`
   neutrality rule is unchanged; `appShellIsolation.test.ts`
   passes after the production-shell wiring.
3. A staged dev / production smoke against the throwaway staging
   stack exercises the candidate end-to-end at least once
   (login → inventory → launch → live PTY → detach → reconnect →
   replay → close), with `data-testid="renderer-option-<id>"`
   selectors documented in `apps/web/e2e/SMOKE.md`.
4. Bundle-size impact is captured and signed off. If the
   candidate carries an inlined WASM payload or large JS, the
   production-bundle size diff is recorded; if the increase is
   significant, the production opt-in landing path puts the
   adapter behind a dynamic import so the default-renderer bundle
   does not regress.
5. SPEC.md and `docs/spec/terminal-adapters.md` updated to
   describe the candidate's production opt-in surface (which
   views, which device class, what the user-facing label says,
   what fallback fires if the renderer fails to load).

### Gate 2 — production opt-in → production default

The bar to replace xterm as the default. All of:

1. Gate 1 holds on every target surface, not just one.
2. **Text / typography** and **Platform fit** rows are at least
   `works with caveats` on every target surface. Any `regression
   vs. baseline` row is a blocker.
3. The candidate has been the production opt-in choice for a
   stated soak period (at least two release cycles) without
   redaction or correctness regressions captured in
   `docs/agent/encountered-lessons.md` or the staging smoke log.
4. Default-flip lands as its own deliberate slice with: a
   migration plan for users who pinned xterm, an update to
   [`docs/agent/task-patterns.md`](agent/task-patterns.md) §
   "Recurring rules for renderer work" (the rule "`xterm` is the
   compatibility baseline and the default" is what changes), and an
   updated SPEC.md statement.

A candidate may stay at Gate 1 forever — production opt-in is a
fine end state for a renderer that targets a specific device class
(e.g. `wterm` for mobile / accessibility while `xterm` stays
default on desktop). The default flip is the deliberate harder
decision.

## Smoke plan

Each gate above is verified by a manual smoke. The smoke is
**deliberately manual** — committing a Playwright runner for the
renderer surface is its own slice (see "Explicitly deferred"). The
shape is the same shape used by every other staging smoke entry in
`docs/deployment/vps-staging-smoke.md`. The renderer-fairness
strategy for the rows the 2026-05-13 baseline could not exercise
(Unicode / box drawing / wide chars, copy / paste,
alternate-screen, mouse) is closed in
[`docs/renderer-smoke-harness.md`](renderer-smoke-harness.md) —
each per-candidate smoke shape below inherits its input-path
taxonomy and command matrix.

### Surfaces

1. **Desktop browser via Playwright MCP.** Firefox at 1440 × 900
   against the throwaway staging stack
   (`https://relayterm-staging.js-node.cc`). Selector hooks:
   `data-testid="renderer-option-<id>"` (already wired in the dev
   lab; production opt-in landing slice adds the matching
   production-shell hook). The procedure mirrors
   `apps/web/e2e/SMOKE.md` § "Dev renderer + production shell
   smoke."
2. **Desktop Tauri.** The path-A bundled-shell handoff (see
   `docs/spec/tauri-runtime-backend-url.md`) against the same
   staging stack. The WebView is WebKitGTK on Linux, WebView2 on
   Windows. Repro the Gate 1 / Gate 2 matrix once per OS the
   shell ships on. Cache pitfall (WebKitGTK + nginx immutable
   `/assets/*`) is documented in
   `docs/deployment/tauri-local-build.md` Troubleshooting and
   in AGENTS.md "Encountered Lessons" (2026-05-09).
3. **Android WebView.** Tauri Android shell (debug APK via
   `pnpm --filter @relayterm/mobile exec tauri android build
   --debug --apk --ci`) against staging. Pay particular attention
   to the **Platform fit** row "Mobile keyboard / soft IME" —
   this is the row `wterm` is motivated by.
4. **Throwaway staging SSH target.** Pattern is fixed:
   `linuxserver/openssh-server:latest` named
   `relayterm-staging-<smoke-id>-ssh`, attached only to the
   internal Compose network, no host port published. Public-key
   pasted in via the same redaction-safe pattern the deployable-
   baseline smoke used (base64 sidecar + `atob` inside a single
   `page.evaluate`; PEM bytes never appear in any tool-call
   payload, log, Error, or DOM string). The container is
   `docker stop && docker rm`'d at teardown.

### Per-candidate smoke shape

Each candidate gets one staging entry per gate, in
`docs/deployment/vps-staging-smoke.md`, following the existing
template:

- Date + UTC time window, stack pin (web + backend image digests),
  branch, browser surface.
- Goal (gate being verified) and explicit slice boundary (no
  source / schema / API / auth / CSRF / deploy changes — same
  posture as the deployable-baseline smoke).
- Throwaway SSH target identity + DNS alias + redaction posture.
- Identity path (generated or imported), redaction sweep over
  `document.documentElement.outerHTML` for
  `BEGIN OPENSSH PRIVATE KEY`, `openssh-key-v1` magic prefix,
  `encrypted_private_key`, `session_token`, `token_hash`,
  `data_b64`, `REDACT-MARKER`.
- Renderer-switch sequence: walk the renderer radio group, click
  each `data-testid="renderer-option-<id>"`, observe the lab
  event log records ONLY `[info] renderer set to <label> (idle)`
  (no payload bytes), confirm reattach behavior across switches.
- End with the renderer left on the gate's candidate (or back on
  xterm for the baseline parity step). Cleanup: kill the smoke
  SSH container, shred any local key material.

### Browser-side smoke selectors

The dev lab already exposes the renderer radio hooks
(`renderer-option-xterm`, `renderer-option-ghostty-web`,
`renderer-option-restty`, `renderer-option-wterm`). The Gate 1
slice for each candidate adds the matching production-shell
selector under a stable name (TBD when the production renderer-
swap UX lands; not in this slice).

## Explicitly deferred

These are out of scope for the renderer evaluation track. Each
will be its own slice if and when it ships:

- **tmux clone features.** Multi-pane workspace, window layouts,
  copy-mode keybindings, named-window UX, status-line bars. None
  of these are part of RelayTerm's product.
- **Multi-pane workspace.** Even without tmux semantics, a tiling
  multi-pane layout is its own design surface and is deferred.
- **True backend-restart persistence.** `tmux` / `screen` /
  managed-agent integration to survive a RelayTerm backend
  restart — see [`docs/persistent-sessions.md`](persistent-sessions.md)
  Option C / Option D. Not unlocked by renderer work; not blocked
  by renderer work.
- **VT snapshot persistence.** Phase 2 of the persistent-sessions
  roadmap (libghostty-vt-driven snapshot on detach to reconstruct
  display state on resume) is independent of renderer choice and
  remains deferred.
- **Renderer production-default switch.** Flipping the default
  away from xterm is a deliberate later slice gated on Gate 2;
  this plan does not flip it.
- **Performance benchmark automation.** A committed benchmark
  harness (microbenchmarks of `write` throughput, reflow cost,
  scrollback memory pressure) is deferred. The matrix above is
  graded by human evaluators in the first pass.
- **Committed Playwright runner for renderer smokes.** The smoke
  remains manual per `apps/web/e2e/SMOKE.md`. Promoting it to a
  CI-driven runner is its own slice.
- **Passphrase-protected private-key import.** Tracked in
  [`docs/private-key-import.md`](private-key-import.md) § 10;
  unrelated to renderer evaluation.
- **`ssh-copy-id` automation.** Unrelated to renderer evaluation;
  out of scope of v1 inventory work per
  [`docs/spec/auth.md`](spec/auth.md).
- **Per-session-per-device renderer preference persistence.**
  Today the dev lab defaults to xterm on every page load; a
  persistent per-user / per-device renderer preference is a
  production UX slice that lands with or after Gate 1, not as
  part of this plan.

## See also

- [`SPEC.md`](../SPEC.md) — architectural invariants and surface
  index.
- [`docs/spec/terminal.md`](spec/terminal.md) — terminal session
  lifecycle, WebSocket attach/detach, replay ring buffer, detach
  TTL.
- [`docs/spec/terminal-adapters.md`](spec/terminal-adapters.md) —
  the per-adapter contract for every candidate.
- [`docs/persistent-sessions.md`](persistent-sessions.md) —
  long-term persistence roadmap, including the deferred host-side
  multiplexer option.
- [`docs/deployment/vps-staging-smoke.md`](deployment/vps-staging-smoke.md)
  — staging smoke template and history.
- [`apps/web/e2e/SMOKE.md`](../apps/web/e2e/SMOKE.md) — manual
  smoke procedure with the dev-lab + production-shell selector
  table.
- [`docs/renderer-smoke-harness.md`](renderer-smoke-harness.md) —
  the renderer-smoke input-harness plan that carries the
  2026-05-13 baseline's deferred matrix rows forward (Unicode /
  paste / alt-screen / mouse), including input-path taxonomy,
  what each path proves, command matrix, and recommended
  follow-up slice options.
- AGENTS.md § "Task patterns" → renderer adapter task pattern
  (long form in [`docs/agent/task-patterns.md`](agent/task-patterns.md)
  § 1) — the recurring rules for renderer work.
