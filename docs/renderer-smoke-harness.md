# Renderer smoke input-harness plan

> Design doc for a repeatable browser / Tauri terminal smoke that
> can fairly compare RelayTerm's swappable renderers (xterm baseline +
> the experimental ghostty-web / restty / wterm adapters) on the
> evaluation-matrix rows the 2026-05-13 xterm baseline smoke deliberately
> deferred (Unicode / box drawing / wide chars, copy / paste,
> alternate-screen, mouse). This is a **plan**, not a contract:
> nothing here ships before a follow-up implementation slice and the
> matching SPEC entries land.
>
> The renderer-evaluation track this harness serves lives in
> [`docs/terminal-renderer-evaluation.md`](terminal-renderer-evaluation.md);
> the renderer-adapter contracts being measured live in
> [`docs/spec/terminal-adapters.md`](spec/terminal-adapters.md); the
> architectural invariants those sit under live in
> [`SPEC.md`](../SPEC.md) § "Architectural invariants" and AGENTS.md
> "Architectural rule (load-bearing)".

## Status

**Draft, design-only. No source / CI / deploy changes ship from this
slice.** The Option A runbook (see § "Option A — runbook +
permission-grant note (recommended)" below) landed alongside this
plan in
[`apps/web/e2e/SMOKE.md`](../apps/web/e2e/SMOKE.md) § "D. Renderer
evaluation smoke" — that section is the operator / Claude runbook
that turns this plan into a repeatable smoke procedure. The
xterm production-baseline renderer smoke landed on 2026-05-13 (see
[`docs/deployment/vps-staging-smoke.md`](deployment/vps-staging-smoke.md)
§ "2026-05-13 · Xterm production-baseline renderer smoke") and
deliberately recorded that four evaluation-matrix rows could not be
exercised with the input path it used:

- Unicode / box drawing / wide-char rendering
- Copy / paste round-trip
- Alternate-screen / full-screen apps
- Mouse support

This doc proposes a harness strategy that lets a future smoke cover
those rows fairly across renderers, without weakening the architectural
invariants the renderer track depends on. Until the harness is wired up
(or the follow-up implementation slice is explicitly declined), the
experimental renderer evaluations (ghostty-web → restty → wterm) stay
deferred behind it.

### 2026-05-14 · renderer-fair input affordance landed

The 2026-05-14c staging smoke mounted ghostty-web on the production
shell but could not drive the evaluation matrix because Playwright MCP
keyboard input did not consistently reach ghostty-web's xterm-compat
shim past the first keystroke. Root cause was a real focus-target
ambiguity, not a runbook wording gap: xterm routes keystrokes through a
hidden helper `<textarea>` (a child of the viewport), while ghostty-web
makes the viewport element itself `contenteditable` and attaches its
keydown listener there. There was no renderer-neutral selector for
"the element a real keystroke hits," so the runbook was guessing.

`feat/renderer-evaluation-input-fairness` closed that gap with the
smallest renderer-neutral change:

- `@relayterm/terminal-core`'s `TerminalRenderer` interface gained an
  optional `focusTarget(): HTMLElement | null` — the element `focus()`
  moves browser focus to (xterm's helper textarea; ghostty-web's
  contenteditable host). Implemented for the xterm and ghostty-web
  adapters; restty / wterm are deferred and simply omit it (optional
  method — builds are unaffected).
- The production workspace stamps a dedicated marker attribute
  `data-relayterm-terminal-input` on whatever `focusTarget()` reports,
  and reflects `data-renderer-input="marked"` on `production-terminal`.
  One stable selector now targets the correct input element across
  renderers.
- No backend / protocol / session / orchestrator change; no new wire
  surface; xterm stays the default; ghostty-web stays experimental and
  gated. The marker carries no payload bytes — it is a fixed boolean
  attribute, and input still flows exclusively through `onInput`.

The Path A and Path C entries below stay valid; what changed is that
the runbook (`apps/web/e2e/SMOKE.md` § "D. Renderer evaluation smoke"
→ "Renderer-fair input") now has a concrete focus + verify procedure
keyed on `[data-relayterm-terminal-input]`. The ghostty-web evaluation
matrix itself remains deferred until a smoke actually runs it under the
relaxed staging CSP using this affordance.

## Findings from the xterm baseline smoke

The 2026-05-13 entry is the load-bearing artifact for what the current
Playwright MCP path could and could not do. Carrying it forward
verbatim (do not overclaim and do not soften):

- **ASCII keyboard input over MCP works.** Keystrokes were delivered
  exclusively via `page.keyboard.press('<char>')` (one Chromium
  `Input.dispatchKeyEvent` per char). Those events arrive with
  `event.isTrusted === true`, which is what xterm's input handler
  requires. `echo`, `whoami`, `pwd`, and `uname -a` all round-tripped
  cleanly.
- **Synthetic `InputEvent` dispatch was rejected.** Direct
  `dispatchEvent(new InputEvent(...))` into `.xterm-helper-textarea`
  was dropped at the renderer's input handler because the resulting
  event carried `isTrusted === false`. This is xterm-internal behavior
  but is the same browser-platform rule every modern renderer relies
  on for paste / IME / drag-drop, so the workaround is **not** "find
  the next renderer that accepts untrusted input"; that path leads to
  security regressions.
- **Synthetic `ClipboardEvent` dispatch was rejected for the same
  reason.** Compounded by the page's clipboard read/write requiring an
  elevated browser permission that MCP did not have during the baseline
  smoke.
- **Resize via the browser-viewport handle works.** Resizing the
  viewport and clicking `production-terminal-fit` flowed the new size
  through `renderer.onResize` → `client.sendResize` → wire `resize`
  frame → PTY → fresh `stty size`. One `session_events.resized` row
  per resize, no chatter.
- **Long output works.** `seq 1 300` rendered all 300 lines; a
  subsequent ASCII echo round-tripped cleanly.
- **Detach / reconnect / replay is wire-correct under MCP control.**
  `production-terminal-detach` then `production-terminal-reconnect`
  inside the 30 s TTL window landed back on the same session UUID;
  fresh PTY output round-tripped post-reattach. The xterm DOM is a
  fresh mount on reattach (`xterm-dom-renderer-owner` bumped from `-1`
  to `-2`), so the visible viewport is empty until new output arrives —
  **wire-side** replay is correct; **renderer-side** scrollback parity
  is a separate property the baseline does not currently provide.
- **Narrow viewport works.** 390 × 844 + fit reflows and an ASCII
  echo round-trips.
- **Redaction posture held.** No payload bytes leaked into MCP
  tool-call payloads, DOM strings, audit, or logs.

The four deferred rows above are deferred specifically because the
input-side limitations (`isTrusted`, clipboard permission) are
load-bearing browser-platform rules, not bugs in the smoke. A fair
renderer comparison needs a way to drive those paths through the
same input pipeline the production user will hit.

## Input-path taxonomy

The candidate paths below are sorted by **how close they are to the
real renderer input pipeline**. Closer-to-real is better for evaluating
a renderer; closer-to-backend is better for evaluating the wire /
session / replay surfaces. **A single harness will likely combine
multiple paths**; what matters is being explicit about which row each
path proves.

### A. Playwright `keyboard.press` / `keyboard.type` (real keyboard events)

- **What it is.** Chromium / WebKit / Firefox dispatch `keydown` /
  `keypress` / `keyup` via the DevTools protocol's
  `Input.dispatchKeyEvent` (or the equivalent on Firefox / WebKit). The
  resulting events carry `isTrusted === true`. Playwright MCP exposes
  this as `browser_press_key` and friends.
- **What it proves.** End-to-end renderer input pipeline for printable
  keys and special keys (arrows, function, ctrl-chord, escape). Same
  pipeline a real user hits.
- **What it does NOT prove.** Anything that requires an IME
  composition surface (CJK / emoji-picker input), clipboard reads,
  drag-drop, or pointer / mouse-mode bytes.
- **Renderer-fairness.** Identical posture across xterm, ghostty-web,
  restty, wterm — all four take the same trusted `keydown` and turn it
  into wire `Input`.
- **MCP feasibility today.** Yes; this is the path the baseline used.

### B. Playwright `keyboard.type` with non-ASCII Unicode (mixed)

- **What it is.** `keyboard.type("你好")` and similar; chromium-level
  Input.insertText for non-mappable characters. Trusted but **not** a
  real IME composition session.
- **What it proves.** Whether the renderer's `onData` happily forwards
  multi-byte UTF-8 from a trusted text-input event. For some renderers
  this is sufficient; for others (DOM-rendered, IME-aware) this skips
  the composition-buffer code path the renderer is specifically
  designed for.
- **What it does NOT prove.** True IME composition (`compositionstart`
  / `compositionupdate` / `compositionend`). On wterm specifically,
  `keyboard.type` would prove the WASM bridge accepts multi-byte
  strings, NOT that the DOM `contenteditable` / IME path works. For
  the IME row, this path is **not** equivalent to a real user.
- **Renderer-fairness.** Roughly equivalent across canvas renderers
  (xterm / ghostty-web / restty); wterm specifically must be evaluated
  with the IME caveat called out in the smoke entry. **Do not** mark
  wterm as "Unicode works" or "Unicode fails" solely from
  `keyboard.type`.

### C. Clipboard write + trusted Ctrl+V (real paste)

- **What it is.** The harness grants the test browser context
  clipboard permissions (`page.context().grantPermissions(["clipboard-read",
  "clipboard-write"], { origin: <staging-origin> })`), writes a
  payload via `navigator.clipboard.writeText(payload)` inside a
  `page.evaluate`, focuses the renderer viewport, then dispatches a
  trusted `Control+V` via `keyboard.press`. The browser fires a real
  `ClipboardEvent` with `isTrusted === true` and the actual clipboard
  contents.
- **What it proves.** The real paste pipeline:
  1. Renderer's paste handler runs (xterm's `_handlePaste`, ghostty-web's
     equivalent, restty's `restty/xterm` shim, wterm's DOM paste
     listener).
  2. Renderer emits the pasted bytes as `onData` / `onInput`.
  3. RelayTerm's `evaluatePaste` policy fires inside
     `ProductionTerminal.svelte`'s `renderer.onInput` listener and
     gates the result through `safe` / `confirm` / `blocked` (see
     `apps/web/src/lib/app/terminal/pastePolicy.ts` and the
     `production-terminal-paste-confirm` /
     `production-terminal-paste-blocked` panels).
- **What it does NOT prove.** Bracketed-paste output flow (the PTY
  emitting `\e[200~ ... \e[201~`) is a separate render-side concern;
  paste-input above only drives the **input** side. Also does not prove
  OSC 52 clipboard-export.
- **Renderer-fairness.** Equivalent across renderers as long as each
  has a real `paste` event handler. wterm's DOM-rendered paste path
  diverges (uses `contenteditable` / `paste` on a DOM node rather than
  a `.helper-textarea`), which is **the point** — the harness should
  expect different behavior here, document it honestly, and not
  collapse a real divergence into a single pass/fail.
- **MCP feasibility today.** Playwright's MCP server exposes
  `browser_evaluate` and the keyboard primitives. Permission grant is
  not a default-on capability today and may need an explicit harness
  step (or a once-per-test Playwright-script fallback for the rows
  permission-grant is required for). Document explicitly which rows
  needed this elevation when a smoke runs.
- **Redaction posture.** The pasted bytes flow through the same
  evaluatePaste policy that already redacts the content from
  `$state` / DOM strings / audit / logs / `data-*` (the panels render
  metadata only); see
  [`docs/agent/redaction-rules.md`](agent/redaction-rules.md) § 10
  ("paste content redaction"). The harness MUST NOT print the paste
  body in any tool-call payload, smoke entry, or report.

### D. Backend-side `echo` / `printf` / `cat` / `tput` (output-only)

- **What it is.** The harness runs an SSH command **as input typed via
  path A** (or a fixture preinstalled in the throwaway target's
  `.bashrc` / motd / a known script path) that emits the **output**
  bytes under test. Unicode glyphs, box-drawing, wide chars, ANSI 256
  color, alternate-screen enter / leave, and SGR mouse-mode enables
  all live on this path.
- **What it proves.** Renderer **output** handling — the parser, the
  cell grid, the scrollback reflow. This is the property the four
  deferred rows mostly care about for the renderer comparison.
- **What it does NOT prove.** Anything on the **input** side — typing
  Unicode, pasting, mouse-clicking, IME composition. Output-only.
- **Renderer-fairness.** Excellent. The same bytes hit each renderer
  through the same wire path; differences are renderer behavior, not
  test bias.
- **Redaction posture.** Output bytes are still terminal content —
  they must not appear in MCP tool-call payloads, smoke summaries, or
  logs beyond the renderer viewport. Use safe ASCII echoes (`echo
  relayterm-<row>-ok`) as round-trip sentinels and treat the
  Unicode / box-drawing bytes themselves as opaque (do not paste them
  into smoke reports; describe them).

### E. WebSocket-client direct injection (backend-side, NOT renderer)

- **What it is.** A small TS / Rust client that opens
  `/api/v1/terminal-sessions/:id/ws` directly (with a real cookie
  session for owner-scoping) and sends `Input` frames programmatically
  via the `relayterm-protocol` shapes.
- **What it proves.** Backend / orchestrator / replay correctness
  under synthetic input load. Useful for replay-ring regression tests
  and bandwidth ceilings.
- **What it does NOT prove.** **Anything renderer-side.** This path
  goes around the renderer entirely. **Do not mark renderer rows
  passed from this path.**
- **Renderer-fairness.** N/A — does not exercise the renderer.
- **Use it.** As a separate backend-side smoke when a row is wire /
  session-shape behavior. Keep its results in their own table; never
  fold them into the renderer evaluation matrix.

### F. Backend-side `tmux send-keys` or equivalent in-target driver

- **What it is.** Run `tmux send-keys` (or `expect`, or a custom
  in-target driver) **inside** the SSH session against the remote
  shell, so the remote shell drives its own input.
- **What it proves.** Output rendering of input echoes — useful for
  alternate-screen apps that change behavior on simulated keystrokes
  (e.g. `htop` quitting via `q`).
- **What it does NOT prove.** Any browser-side input pipeline.
- **Renderer-fairness.** Output-only; equivalent to path D for
  fairness.
- **Operational caveat.** Requires tmux / expect inside the throwaway
  target's image. The current
  `linuxserver/openssh-server:latest` target does not ship with tmux
  preinstalled; either pin a target image that does (or extend the
  smoke's setup to install `tmux` once after first attach) or use
  `expect` / a custom shell script. **Do not** install ad-hoc tools
  inside a real staging target — the throwaway is the right place.

### G. Production paste button / command-palette action (new UI surface)

- **What it is.** A new "Paste from clipboard" button or
  command-palette action on `ProductionTerminal.svelte` that reads
  `navigator.clipboard.readText()` inside a click handler and feeds the
  result through the existing `evaluatePaste` → `client.sendInput`
  pipeline.
- **What it proves.** Same paste pipeline as path C; the test harness
  can drive a real `data-testid="production-terminal-paste"` button
  click instead of synthesizing Ctrl+V.
- **What it does NOT prove.** Real OS-keychord paste through the
  renderer's own paste handler (xterm's `_handlePaste` would still
  need to be exercised separately — the new button bypasses it). Two
  rows, not one.
- **Renderer-fairness.** Identical across renderers — the button
  predates the renderer.
- **Operator value.** Independent of testing: a paste-from-clipboard
  affordance is useful on mobile (no Ctrl+V) and inside the Tauri
  desktop shell. Worth landing on its own merits but **not** in this
  doc-only slice.
- **Security caveat.** Whatever production surface this adds must
  honor the same `evaluatePaste` decision and same redaction posture
  as the current OS-level paste path. No "trusted source, skip
  evaluation" bypass. See
  [`docs/agent/redaction-rules.md`](agent/redaction-rules.md) § 10.

### H. Tauri shell automation (`tauri-driver` / WebDriver)

- **What it is.** Tauri exposes a WebDriver-compatible automation
  surface via `tauri-driver` for the desktop shell. Mobile (Android)
  is its own story (`uiautomator` / Espresso).
- **What it proves.** Tauri-shell-specific renderer behavior:
  WebKitGTK on Linux vs. WebView2 on Windows vs. Android WebView on
  Android. Mobile IME, soft keyboard, autocorrect.
- **What it does NOT prove.** Browser-only behavior (Firefox / Safari
  variance), and is not where the first comparison should happen —
  the desktop browser is the cheaper, faster baseline.
- **Operational caveat.** Tauri CI / release automation is itself
  deferred (see SPEC.md "Out of scope (v1)" → "iOS Tauri build" and
  [`docs/deployment/tauri-ci-release-plan.md`](deployment/tauri-ci-release-plan.md)).
  Wiring Tauri WebDriver into the renderer smoke is **after** desktop
  browser + Android WebView pass Gate 1. Do not block this slice on
  it.

### I. Dev-only test-harness route (rejected)

- **Why it is listed.** Tempting shortcut: add a dev-only HTTP route
  like `POST /dev/terminal-sessions/:id/inject-input` that takes a
  body and forwards bytes onto the session's `Input` channel. Lets
  Playwright drive arbitrary bytes without `isTrusted` or clipboard
  permission.
- **Why it is rejected.**
  1. **Renderer-bypass.** Same as path E — proves nothing about the
     renderer.
  2. **Production-bypass risk.** A dev-gated route that lets any
     authenticated caller inject input into a session by id, **if
     accidentally reachable in production**, is an unauthenticated
     paste / input bypass with the existing CSRF / `Origin` posture
     and a redaction-rule blast radius (the body would contain
     arbitrary terminal input).
  3. **Posture drift.** The renderer track's load-bearing guardrails
     ("backend protocol stays RelayTerm-shaped"; "no dev-only debug
     branch in production") are exactly the kind of rule that a "just
     this once" dev route quietly erodes.
- **Use instead.** Path E (a separate backend-side WebSocket-client
  smoke) covers any legitimate "drive the backend from a script" need
  without adding a route. The backend-side smoke is read /
  control-plane-shape only and does not need a new wire surface.

## What each path proves at a glance

| Path | Renderer input proven? | Renderer output proven? | Backend / session proven? | Production-safe? |
|---|---|---|---|---|
| A. Playwright `keyboard.press` (ASCII) | Yes (printable + special keys) | Indirect (renderer echoes typed bytes) | Indirect | Yes |
| B. Playwright `keyboard.type` (Unicode) | Partial (no real IME composition) | Indirect | Indirect | Yes |
| C. Clipboard write + trusted Ctrl+V | Yes (real `paste` event) | Indirect | Indirect | Yes |
| D. Backend-side `echo` / `tput` (output-only) | No | Yes | Indirect | Yes |
| E. WebSocket-client direct injection | **No** | No | Yes | Yes (no new route) |
| F. In-target `tmux send-keys` / `expect` | No | Yes (output echoes) | Indirect | Yes |
| G. Production paste button / palette action | Partial (button-only path; renderer's own paste handler is NOT exercised) | Indirect | Indirect | Yes (if `evaluatePaste` honored) |
| H. Tauri / mobile WebDriver | Yes (shell-specific) | Yes (shell-specific) | Yes | Yes |
| I. Dev-only inject-input route | No (skips renderer) | No | Yes | **No — rejected** |

## Recommended harness strategy

### Near-term, doc-only carry-forward

Pick the smallest mix that fairly exercises the four deferred rows
across renderers. Use only paths that exist today.

- **ASCII I/O, resize, long output, detach / reconnect, narrow viewport,
  redaction.** Path A only. Already proven by the 2026-05-13 xterm
  baseline smoke. No change.
- **Unicode / box drawing / wide chars (output).** Path D. Run
  `echo`-style fixtures from the remote shell. Renderer fairness is
  excellent; this is the right path for measuring renderer **output**
  behavior on these glyph categories. Use small, well-known fixtures
  (`echo -e "─┬┐"` for box drawing,
  `echo -e "你好"` for CJK, `echo -e "\U0001F600"` for emoji)
  and treat the bytes as opaque in any smoke summary.
- **Paste round-trip (real renderer paste handler).** Path C
  (clipboard + trusted Ctrl+V). The harness MUST grant clipboard-write
  permission explicitly, MUST write a benign sentinel string
  (`relayterm-paste-roundtrip-ok`) plus a multiline / large / control-char
  variant exercising the `confirm` and `blocked` panels, and MUST verify
  the panel metadata, not the body. Skip path G (production paste button)
  until the harness has motivated its existence beyond testability.
- **Alternate-screen / full-screen apps.** Path D first (`tput smcup;
  sleep; tput rmcup` and a no-tmux `less` invocation on a small static
  file). `tput smcup` / `tput rmcup` is the **minimum viable** alt-screen
  probe and exercises only the enter / leave transition; the full
  evaluation-matrix row (htop / vim / less per
  [`docs/terminal-renderer-evaluation.md`](terminal-renderer-evaluation.md)
  § "Core correctness → Alternate screen / full-screen apps") stays
  partially deferred until a target image with the larger tooling set
  is confirmed. Path F second if the target image gains tmux. Treat
  htop / vim as nice-to-have, not required: they add target-image
  bloat and their exit paths are the failure-prone bit.
- **Mouse support.** Path D for the **mode-enable** half (the renderer
  sends mouse-tracking enable in response to `\e[?1000h` etc., which is
  output-driven). Defer the **mode-input** half (clicks and drags
  translated into wire `Input`) until a small purpose-built fixture
  exists. Suggested fixture is a single-purpose static page or remote
  script that prints click coordinates to stdout; alternative is
  `less` in mouse-tracking mode against a long fixture file. **Do not**
  use vim / htop as the mouse fixture — too many other moving parts.
- **WebSocket / replay / TTL regression checks.** Path E. Keep it as
  a **separate** smoke entry under `vps-staging-smoke.md`; do not fold
  results into a renderer-evaluation entry.

### Recommended boundary

- The renderer evaluation matrix is graded by **paths A / B / C / D**.
- Backend / session matrix is graded by **path E** (separate report).
- **Path E results never merge into a renderer-evaluation matrix
  row.** A backend-only smoke entry stays in its own staging-smoke
  entry; a future reader of the renderer evaluation matrix must not
  be able to mistake a wire / replay result for renderer-input or
  renderer-output evidence.
- Path G is **deliberately not** introduced as part of the harness:
  adding a new production UI surface specifically for testability is
  the wrong direction; the smoke should drive the production surfaces a
  real user hits. If a paste button lands later for product reasons,
  the harness gains an extra hook for free.
- Path I (dev-only inject route) is rejected and is **not** part of the
  strategy at any tier.

### What this strategy does NOT promise

- **It does not prove input fidelity in the Tauri / Android shells.**
  The desktop-browser smoke is necessary but not sufficient. Tauri-shell
  smokes (path H) are a separate, later track and are gated by Tauri
  CI / release automation that itself is deferred.
- **It does not replace a microbenchmark harness.** Performance rows
  remain human-readable observations per
  [`docs/terminal-renderer-evaluation.md`](terminal-renderer-evaluation.md)
  § "Memory / CPU rough observations" and § "Explicitly deferred →
  Performance benchmark automation."
- **It does not promote a committed Playwright runner.** The smoke
  remains manual per
  [`apps/web/e2e/SMOKE.md`](../apps/web/e2e/SMOKE.md). Promoting to a
  CI-driven runner is its own slice (already deferred in the
  evaluation plan).

## Command matrix

Each row lists the input path(s), what is exercised, and a concrete
fixture command. **Fixtures are illustrative**; the smoke author may
choose equivalents. **Sentinels are mandatory** — every row should
include a unique ASCII sentinel so the smoke entry can record a
round-trip without quoting any opaque bytes.

| Row | Path(s) | Fixture (illustrative) | Sentinel ASCII | Notes |
|---|---|---|---|---|
| ASCII I/O | A | `echo relayterm-ascii-ok` | `relayterm-ascii-ok` | Baseline parity row; carry-forward from 2026-05-13 smoke. |
| Unicode CJK output | D | `printf '%s\n' "$(printf '你好')-ok"` | `cjk-ok` (suffix only) | Treat glyph bytes as opaque in the smoke entry; record sentinel only. |
| Box-drawing output | D | `printf '%s\n' "$(printf '┌─┐')-ok"` | `box-ok` (suffix only) | Verify column alignment visually; same posture as above. |
| Wide-char output | D | `printf '%s\n' "$(printf 'ＡＢＣ')-ok"` | `wide-ok` (suffix only) | Fullwidth Latin A/B/C is a stable wide-char fixture (no emoji-variation-selector complexity). |
| Emoji output (optional) | D | `printf '%s\n' "$(printf '\U0001F600')-ok"` | `emoji-ok` (suffix only) | Some renderers will land on graceful-fallback glyphs; record honestly, do not block on this row. |
| Paste safe (small, single line) | C | Clipboard write `relayterm-paste-safe-ok\n`, focus viewport, `Ctrl+V` | `relayterm-paste-safe-ok` | Expect `safe` decision; no confirm / blocked panel; sentinel echoes to PTY. |
| Paste confirm (multiline) | C | Clipboard write a 6-line script of `echo step-N`, `Ctrl+V` | metadata only | Expect `production-terminal-paste-confirm` panel with `data-paste-reason="multiline"`; verify `data-paste-reason` + line / byte counts; **do not record** the pasted body. |
| Paste blocked (NUL byte) | C | Clipboard write `relayterm-x\0y`, `Ctrl+V` | metadata only | Expect `production-terminal-paste-blocked` panel with `data-paste-reason="nul_byte"`; verify metadata only. |
| Long output | A + D | `seq 1 300` typed via path A | `300` plus tail | Carry-forward from 2026-05-13 baseline. |
| Resize | A + browser viewport handle | `stty size` before / after `production-terminal-fit` | Two `stty size` lines | Verify `session_events.resized` row count. |
| Narrow / mobile viewport | A + browser viewport handle | 390 × 844 + fit + ASCII echo | `relayterm-mobile-width-ok` | Carry-forward from 2026-05-13 baseline. |
| Alternate screen (enter / leave) | D | `tput smcup; printf 'alt-screen-ok\n'; sleep 1; tput rmcup` | `alt-screen-ok` | Verify cursor restored to pre-`smcup` cell; record visible scroll-back parity honestly. |
| Alternate screen (full-screen app) | D | `less /etc/issue` then `q` to exit | n/a | Optional; gated on target image shipping `less`. Do **not** use `vim` / `htop` — too many moving parts. |
| Mouse mode enable (output half) | D | `printf '\e[?1000h'; sleep 1; printf '\e[?1000l'` | n/a | Verify renderer entered mouse-tracking mode (visible behavior depends on renderer). Defer the **click-translates-to-wire-input** half until a purpose-built fixture is chosen. |
| Detach / reconnect / replay (wire) | A + production buttons | `production-terminal-detach`, wait, `production-terminal-reconnect`, fresh echo | `relayterm-after-reconnect-ok` | Carry-forward from 2026-05-13 baseline; do not overclaim visual replay parity (renderer remounts on reattach today). |
| Redaction sweep | every row | DOM scan for `BEGIN OPENSSH PRIVATE KEY`, `openssh-key-v1`, `encrypted_private_key`, `session_token`, `token_hash`, `data_b64`, paste sentinels | n/a | Mandatory final step on every smoke entry. Sentinel matches per [`docs/agent/redaction-rules.md`](agent/redaction-rules.md). |
| Backend / replay regression (wire) | E | Direct `Input` frames at 1 / 10 / 100 KB | n/a | **Separate** smoke entry; explicitly NOT a renderer-evaluation row. |

## Recommended next implementation slice

Two staged options. **Recommendation: Option A first.** Option B is
listed for completeness; it is a deliberate-later if the manual
procedure stops scaling.

### Option A — runbook + permission-grant note (recommended)

- **Shape.** Docs-only follow-up. Extend
  [`apps/web/e2e/SMOKE.md`](../apps/web/e2e/SMOKE.md) with a "Renderer
  evaluation smoke" section that walks the command matrix above, names
  the input path for each row, and documents the one-time clipboard-
  permission grant required for the paste-row Path C steps (with a
  concrete Playwright MCP / `browser_evaluate` snippet for the grant).
  Cross-link from this doc and from the renderer-evaluation plan.
- **Source / CI / deploy changes.** None.
- **Production posture.** No new surface. Posture unchanged.
- **Cost.** A single docs slice. Same shape as the 2026-05-13 baseline
  smoke + this plan slice.
- **Why first.** Resolves the four deferred rows for the renderer-
  evaluation track at the minimum cost. Lets the ghostty-web → restty
  → wterm comparisons start without committing to a UI / backend change
  whose justification is testability alone.

### Option B — committed Playwright runner (deliberate-later)

- **Shape.** Source slice: add `@playwright/test` as a dev-dep under
  `apps/web/`, commit a small `playwright.config.ts`, commit a
  `tests/renderer-smoke.spec.ts` that drives the matrix against the
  staging stack. CI-runnable.
- **Source / CI / deploy changes.** Yes. Triggers AGENTS.md "Stack"
  table review (new top-level dep) and a CI lane.
- **Production posture.** No new surface; the runner targets staging
  only.
- **Cost.** Non-trivial. Browsers in CI, a fresh CI job, a fresh
  failure mode (browser flake).
- **Why later, not now.** The renderer-evaluation plan already records
  this as deferred. The doc-only runbook (Option A) covers the
  matrix at adequate fidelity for the human-evaluator pass per the
  evaluation plan; promoting to CI is a separate, deliberate
  decision.

### Explicitly NOT recommended

- **A dev-only `POST /dev/.../inject-input` route.** Rejected as path
  I above. The backend protocol stays RelayTerm-shaped; the
  renderer-evaluation track does not justify a new wire surface.
- **A renderer-specific backend protocol extension.** Out of scope per
  the renderer evaluation plan's "Non-negotiable architecture rules"
  → "Backend protocol stays RelayTerm-shaped."
- **A production paste button motivated by testability alone.** If
  one lands later for product reasons (mobile UX, Tauri shells), the
  harness can opportunistically use it; it is **not** introduced for
  the harness.
- **Browsing-permission auto-grant in production.** Any clipboard-grant
  step lives in the smoke harness's Playwright context only and is
  **never** wired into the production `apps/web` bundle's runtime
  permission requests.

## Smoke runbook updates

Independent of which implementation option lands, the existing
runbook needs three updates the next time someone touches it. The
updates are **doc-only** and called out here so they happen alongside
the harness work, not separately:

1. **`apps/web/e2e/SMOKE.md` — new section "Renderer evaluation
   smoke."** Add a procedure block per the command matrix above. Each
   row names the input path (A / B / C / D / E / F as above), the
   fixture, the expected sentinel, and the failure mode. The block
   sits **after** the existing dev-lab + production-shell sections and
   **before** the staging-smoke template links.
2. **`docs/deployment/vps-staging-smoke.md` template — renderer-row
   field set.** Future renderer smoke entries should add a "Path key"
   field per matrix row (e.g. `Unicode CJK output — path D`) so a
   future reader can tell at a glance whether a row was proven through
   the renderer or through the backend / output channel.
3. **`docs/terminal-renderer-evaluation.md` § "Smoke plan" → "Per-
   candidate smoke shape."** Add a one-line link to this doc from the
   "Goal" subsection so future candidate smokes inherit the path
   taxonomy.

## Security / redaction rules

Every harness step must honor these. Each is restated from
[`docs/agent/redaction-rules.md`](agent/redaction-rules.md) and
AGENTS.md "Things to avoid" for clarity:

- **No payload bytes in tool-call inputs or outputs.** The harness's
  Playwright MCP calls (including `browser_evaluate`, `browser_type`,
  `browser_press_key`, `browser_snapshot`, `browser_take_screenshot`)
  must not transit terminal output bytes, paste bodies, or private-key
  bytes. Clipboard payloads are constructed inside a single
  `browser_evaluate` from local fixtures, never pasted into the MCP
  call as a literal.
- **No paste bodies in smoke entries.** Paste rows record metadata
  only (line count, byte length, decision panel `data-paste-reason`).
  Sentinels are the only ASCII recorded. Same posture as
  [`docs/agent/redaction-rules.md`](agent/redaction-rules.md) § 10.
- **No private-key material on disk after the smoke.** Match the
  2026-05-13 baseline pattern: generate identities backend-side
  (preferred) OR base64-sidecar an imported key with `atob` inside a
  single `page.evaluate` and shred the sidecar at cleanup. The
  harness does NOT print public-key bodies into smoke entries either
  (per `docs/agent/redaction-rules.md` § 1: even public-key bytes are
  forbidden in audit payloads; the smoke entry mirrors that posture).
- **No dev-only production bypass.** Any added route, hook, or button
  must be reachable in production OR clearly gated behind
  `import.meta.env.DEV` AND the gated branch dead-code-eliminated by
  Rollup. The harness rejects Option I (dev-only inject route)
  entirely.
- **No clipboard permission grant in production.** The
  `grantPermissions(["clipboard-read", "clipboard-write"], ...)` call
  is harness-side only; the production `apps/web` does not request
  clipboard permission at startup, only at the moment a paste action
  is initiated (current behavior — unchanged by this harness).
- **No renderer-specific backend protocol changes.** The wire
  protocol stays RelayTerm-shaped; the harness drives existing
  surfaces. Any new harness behavior that wants a new wire frame
  triggers AGENTS.md "Task patterns" → "Adding a new backend
  WebSocket message type" and is its own slice.
- **No relaxation of `CsrfGuard` / `Origin` posture for the
  harness.** The harness runs against the same surfaces a real user
  hits; if it cannot, the surface is what is wrong, not the guard.
- **Redaction sentinel sweep is mandatory on every smoke entry.**
  Mirroring the 2026-05-13 baseline: scan `document.documentElement.
  outerHTML` for `BEGIN OPENSSH PRIVATE KEY`, `openssh-key-v1`,
  `encrypted_private_key`, `session_token`, `token_hash`, `data_b64`,
  plus the paste-row sentinels for that smoke. Zero matches outside
  the terminal viewport.

## Open questions

These are the calls the owner should make before the next
implementation slice. Each carries the current default this doc
assumes; reversing a default is a deliberate decision, not a silent
drift.

1. **Do we ever want a production paste button (path G)?** Current
   default: no, not motivated by testability. Reconsider if mobile /
   Tauri shell UX justifies it; harness gains a hook for free if
   yes.
2. **What's the target image for in-target driver fixtures (path
   F)?** Current default: stay on
   `linuxserver/openssh-server:latest` and skip tmux / expect
   fixtures. Reconsider if alternate-screen + mouse rows demand it;
   pinning a tmux-shipping image is a one-line Compose change in the
   smoke setup, not a production change.
3. **Which mouse fixture do we commit to?** Current default: defer
   the mouse-input half of path D until a fixture is chosen.
   Candidates: a single-purpose remote script that prints click
   coordinates to stdout (simplest, controllable); `less` with
   mouse tracking against a long fixture file (no extra deps but
   ties the row to `less`'s mouse behavior); `tmux`'s own mouse
   mode (requires the image change in Q2). **Avoid** vim / htop —
   too many other moving parts.
4. **Does the harness ever run against a Tauri shell, or only the
   browser?** Current default: browser only for the first
   ghostty-web / restty / wterm pass per
   [`docs/terminal-renderer-evaluation.md`](terminal-renderer-evaluation.md)
   "Surfaces." Tauri (path H) is a later track.
5. **Does the harness exercise IME composition (real
   `compositionstart` / `compositionupdate` / `compositionend`)?**
   Current default: no — path B (Playwright `keyboard.type` for
   Unicode) is treated as **not equivalent** to a real IME session,
   and the IME row of the evaluation matrix stays graded as a manual
   human-driven step on a real mobile / Android-WebView surface
   (where the wterm adapter's motivating story lives). Reconsider
   only if a future automation surface lands a credible IME driver.
6. **Is the clipboard-permission-grant Playwright snippet adequate
   over MCP today, or does it need a small once-per-test scripted
   Playwright wrapper?** Current default: try MCP first per Option
   A; if the grant call is not reachable from MCP, scope a
   minimal Playwright-script wrapper as part of the same smoke
   slice (still doc-only — no committed runner, no CI lane).
7. **How are renderer-specific output-byte expectations encoded
   without quoting opaque bytes in smoke entries?** Current default:
   describe behavior in words, record only the ASCII sentinel from
   the matrix, and treat any non-ASCII bytes as opaque. Screenshots
   of the renderer viewport are allowed if they redact host data and
   contain no paste bodies; recording terminal-content bytes
   verbatim is not.

## See also

- [`docs/terminal-renderer-evaluation.md`](terminal-renderer-evaluation.md)
  — the evaluation plan this harness serves.
- [`docs/spec/terminal-adapters.md`](spec/terminal-adapters.md) — the
  four renderer-adapter contracts under evaluation.
- [`docs/deployment/vps-staging-smoke.md`](deployment/vps-staging-smoke.md)
  § "2026-05-13 · Xterm production-baseline renderer smoke" — the
  reference baseline this harness extends.
- [`apps/web/e2e/SMOKE.md`](../apps/web/e2e/SMOKE.md) — manual smoke
  procedure and stable-selector table.
- [`docs/agent/redaction-rules.md`](agent/redaction-rules.md) §§ 1,
  10, 11 — audit-payload, paste-content, and recording-byte redaction
  rules the harness honors verbatim.
- [`SPEC.md`](../SPEC.md) "Architectural invariants" — load-bearing
  invariants the harness must not weaken.
