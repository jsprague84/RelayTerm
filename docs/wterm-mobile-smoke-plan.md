# wterm mobile / WebView smoke — plan

> Plan for evaluating `@relayterm/terminal-wterm` on mobile and
> WebView surfaces. The intent is to *measure* whether wterm — already
> verified on the desktop production shell — is viable as RelayTerm's
> browser-native / mobile UX renderer candidate, before any
> mobile-specific UX implementation slice is started.
>
> **This is a planning doc. It does not run a smoke, does not change
> any renderer, `terminal-core`, production shell, protocol, backend /
> session / orchestrator, CSP, deploy, or CI code.** It changes no
> renderer default. It does not promote wterm.

## 1. Purpose and scope

- **In scope.** Define how to evaluate wterm on mobile-class surfaces:
  desktop-browser responsive / mobile viewport, Android Chrome against
  staging, Android Tauri / WebView shell, and (for cross-WebView
  context) the existing desktop Tauri shell. Define a smoke-row
  catalogue, instrumentation, redaction / privacy posture, and
  decision outputs.
- **Not in scope.** This plan does **not** promote wterm. It does
  **not** flip the xterm default on any surface. It does **not**
  change production CSP (staging stays on `script-src 'self'
  'wasm-unsafe-eval'`; production deploy templates stay strict). It
  does **not** add a renderer-benchmark harness — the benchmark
  harness is a separately deferred slice
  ([`docs/terminal-renderer-evaluation.md`](terminal-renderer-evaluation.md)
  § "Explicitly deferred"). It does **not** add iOS coverage — there
  is no iOS shell in the repo today.
- **Honest distinction kept throughout.** "Mobile viewport in a
  desktop browser" is **not** the same as "Android Chrome" and
  neither is the same as "Android WebView via Tauri." Each surface
  has different keyboard / IME / WebView / WASM behaviour. The plan
  separates them rather than smearing them together.

## 2. Why wterm is the mobile candidate

- **DOM-rendered cell grid.** wterm's cell grid is ordinary DOM
  nodes (`.term-row > span` inside the `.wterm` host), not a canvas
  or WebGPU surface. That changes what platform-native text behaviour
  the renderer can inherit *for free*:
  - native browser text selection across cells (no canvas hit-test
    reimplementation);
  - native browser copy via `Ctrl/⌘+C` or the OS context menu;
  - native browser find-in-page potential (`Ctrl/⌘+F`) — *not yet
    verified anywhere on RelayTerm*;
  - native screen-reader / a11y traversal potential — *not yet
    verified anywhere on RelayTerm*;
  - native soft-keyboard / IME composition behaviour on mobile —
    the row in the evaluation matrix wterm is specifically motivated
    by.
- **Existing production-shell evidence.** wterm has cleared the
  production-shell gate smoke (2026-05-14g), the renderer-fair
  matrix smoke (2026-05-14i — Core-correctness rows `pass`), and the
  renderer-neutral autofit resmoke (2026-05-15 — PTY genuinely
  reflows under operator opt-in: 1440×900 → `24 80`, narrowed to
  390×844 → `24 35`, back to 1440×900 → `24 103`). The diagnostic
  fix verified on 2026-05-15b ensures `data-renderer-autofit`
  reports the truth from the production shell.
- **Renderer-fair input seam available.** `WtermRenderer.focusTarget()`
  returned wterm's hidden keyboard `<textarea>` since `bde039e`, and
  the production workspace stamps `[data-relayterm-terminal-input]` /
  `data-renderer-input="marked"` on a wterm mount — so a smoke can
  drive trusted keystrokes without guessing per-renderer selectors.
- **Not the GPU candidate.** wterm's advantage is **DOM integration,
  not GPU acceleration.** The Zig + WASM core lives behind the DOM
  grid; there is no canvas, no WebGPU, no WebGL2. restty is the
  GPU / WebGPU + libghostty-vt research candidate (and is
  independently blocked on CSP / font / WebGPU on the evaluated
  surface — see
  [`docs/renderer-comparison-scorecard.md`](renderer-comparison-scorecard.md)
  § 3 "restty"). Do not conflate the two when reasoning about wterm's
  mobile story.
- **Potential, not promotion.** Every claim above is "rendering style
  motivates this" or "verified on desktop." None of these rows has
  been *measured on a mobile surface*. The point of this plan is to
  retire that gap.

## 3. Current verified baseline (what is in scope to build on)

Pinned facts, current as of 2026-05-15. Cite these — don't expand
them.

- wterm mounts cleanly on the production shell under the staging
  `wasm-unsafe-eval` CSP (2026-05-14g).
- Renderer-fair Path A input works on wterm via `focusTarget()` →
  `[data-relayterm-terminal-input]` (`WtermRenderer.focusTarget()`
  implemented in `packages/terminal-wterm/`; landed in commit
  `bde039e` and verified by the 2026-05-14i matrix smoke).
- The wterm production-shell matrix smoke's Core-correctness rows
  (basic I/O, long output, trusted paste through wterm's DOM textarea
  paste handler, detach / reconnect / replay) all `pass`; Unicode /
  box / wide-CJK output and the alt-screen probe render correctly
  (2026-05-14i).
- The renderer-neutral autofit capability ships off by default;
  under operator opt-in (`autofitEnabled = true`) wterm maps it to
  `WTerm.autoResize` and the PTY genuinely reflows on container
  resize (2026-05-15). `data-renderer-autofit="active"` is honest
  post-mount (2026-05-15b fix).
- wterm remains **experimental and operator-gated** via Settings →
  Experimental renderer evaluation. xterm is the default on every
  surface.
- The Android Tauri shell exists at `apps/mobile/` (`@relayterm/mobile`,
  Tauri v2). A debug APK has been built (`pnpm --filter
  @relayterm/mobile exec tauri android build --debug --apk --ci`) and
  the existing 2026-05-09 Android Tauri smokes verified bundled-shell
  handoff (Path A) → login → inventory / preflight / trust / auth-check
  → terminal attach → PTY round-trip on a Samsung Galaxy S10e with
  the xterm baseline renderer. No mobile renderer-evaluation row has
  ever run.

## 4. Mobile surfaces to evaluate

Each surface is its own staging smoke entry, not extra rows on
another entry. The surfaces are **ordered cheapest-first** so a
blocker on an earlier surface short-circuits the more expensive
ones.

1. **Desktop browser at a narrow viewport.** Chromium and Firefox at
   `390 × 844` (the same dims the 2026-05-15 autofit resmoke
   exercised). This is **viewport-shape coverage only** — it proves
   wterm continues to reflow and stays usable at a mobile column
   count, but it does **not** prove anything about touch, soft
   keyboards, IME, or native-text-selection-on-touch. Treat this as
   the precondition for the real mobile rows, not a substitute.
2. **Android Chrome against staging.** A real Android device, real
   Chrome, real touch, real soft keyboard, real IME, real
   `visualViewport`. This is the cheapest *real-mobile* surface — no
   Tauri build required, no WebView quirks. Use the existing
   throwaway staging stack
   (`https://relayterm-staging.js-node.cc`). If wterm fails here, it
   will fail in WebView too.
3. **Android Tauri / WebView shell.** The existing
   `@relayterm/mobile` Tauri v2 debug APK (`tauri android build
   --debug --apk --ci`) talking to the same staging stack. The
   bundled-shell handoff (Path A,
   [`docs/spec/tauri-runtime-backend-url.md`](spec/tauri-runtime-backend-url.md))
   is already proven on the Galaxy S10e; this surface adds **Android
   System WebView** (Chrome-derived, but a different release cadence
   and component) to the test matrix. Some Android keyboards / IMEs
   behave differently in WebView than in Chrome; that is the
   measurement.
4. **Desktop Tauri (Linux WebKitGTK).** Already covered for xterm by
   prior staging smokes; useful here only as a **cross-WebView
   sanity check** — does wterm behave the same in WebKitGTK as it
   does in Chrome / Android WebView? If a wterm issue appears on
   WebKitGTK only, the renderer interacts with WebView differences in
   ways that affect Tauri portability. Optional; defer if Android
   rows are conclusive on their own.
5. **iOS / iOS Safari / iOS Tauri — explicitly out of scope.** No
   iOS shell exists in this repo. Adding one is its own deliberate
   slice; the renderer evaluation does not chase a surface that has
   no integration yet.

## 5. Proposed smoke rows

Each row records: surface(s) it runs on, input path
([`docs/renderer-smoke-harness.md`](renderer-smoke-harness.md) §
"Input-path taxonomy"), what counts as `pass` vs. `works with
caveats` vs. `regression vs. baseline` vs. `blocker`, and what
must be observable without reading payload bytes. The rows are
ordered so a failure on an earlier row blocks later rows.

| # | Row | Surfaces | Input path | What "pass" looks like (observable) |
|---|---|---|---|---|
| 1 | **Renderer identity + mount diagnostic** | all | none | `data-renderer="wterm"`, `data-renderer-experimental="true"`, `data-renderer-fallback=""`, `data-renderer-gate="on"`, `data-renderer-input="marked"`. No `production-terminal-error` panel. |
| 2 | **Autofit posture** | all | none | With operator opt-in (`autofitEnabled = true`): `data-renderer-autofit="active"` (the 2026-05-15b precondition). With it off: `"off"`. Capture the value verbatim; do **not** infer from visual fit. |
| 3 | **Tap-to-focus** | 2, 3, 4 | touch (real device) / pointer (Tauri) | Tap the workspace; `document.activeElement === [data-relayterm-terminal-input]`; soft keyboard rises on devices 2 / 3. The `production-terminal-focus` button is the renderer-neutral fallback. |
| 4 | **Soft-keyboard open / close + layout** | 2, 3 | OS soft keyboard | When the keyboard opens, the terminal viewport does **not** scroll the page so the prompt disappears under the keyboard *without an honest accommodation* (either the workspace resizes its container or the autofit reflow keeps the visible prompt anchored). Capture `window.innerHeight` and `window.visualViewport?.height` before / during / after open. Pass = the prompt remains visible; "works with caveats" = visible after one scroll; "regression" = becomes unreachable. |
| 5 | **Viewport-height changes drive autofit** | 2, 3 (with autofit on) | OS soft keyboard | When the soft keyboard opens and the visual viewport shrinks, `data-renderer-autofit="active"` stays `"active"` and `stty size` reports a fewer-rows geometry (the wire `resize` frame is what we expect to see). When the keyboard closes, geometry restores. Backend `terminal_sessions` row stays `active`. |
| 6 | **Single-character ASCII input — Path A via OS soft keyboard** | 2, 3 | OS soft keyboard | A typed `a` reaches the renderer (`a` appears at the prompt) and a typed Enter submits. The 2026-05-09 Android xterm smoke established the cold-start race (first paint may need one Enter); the *new* check here is whether wterm shows it too. |
| 7 | **Modifier / control-key affordances — Ctrl, Esc, Tab, arrows** | 2, 3 | OS soft keyboard + on-screen modifier UI **if it exists** | RelayTerm has no mobile command bar yet (this is one of the candidate next slices in § 9). For row 7, **record the current gap honestly**: what is the path to send `Ctrl-C`, `Esc`, `Tab`, arrow keys from an OS keyboard? Document which Android keyboards expose them (Hacker's Keyboard, Termux:Keys) and which do not. This row's purpose is to **scope the gap**, not pass / fail. |
| 8 | **Paste flow** | 2, 3 | OS share-sheet "paste" / long-press → Paste | Use the throwaway-target prompt fixture (an ASCII sentinel from the existing renderer smoke matrix). Verify the production paste-safety pipeline still runs (`evaluatePaste` → `production-terminal-paste-confirm` panel when applicable, sourced from `PasteDecision` metadata only — no payload echoed in DOM). Pass = the panel renders metadata, the sentinel reaches the remote shell on confirm. **Mobile fallback:** Android Chrome / WebView may refuse `navigator.clipboard.readText()` without a preceding user gesture, and the MCP-style permission grant ([`apps/web/e2e/SMOKE.md`](../apps/web/e2e/SMOKE.md) § "D — Clipboard permission step") does not translate to a real device. If the OS paste path is unavailable on the mobile surface under test, mark this row `deferred — clipboard permission unavailable on this surface` rather than synthesising a `ClipboardEvent` (the renderer-smoke-harness plan rejects synthetic events for renderer-fairness reasons). |
| 9 | **Copy / select flow** | 2, 3, 4 | touch / pointer | Long-press to start selection on the wterm DOM grid; drag to extend; OS context-menu Copy; paste into another app to confirm the bytes survived. xterm canvas baseline cannot be selected this way — this row is the **wterm differentiator** the plan is built to verify. Pass = a multi-line selection survives copy / paste unchanged. |
| 10 | **Long output / scroll** | 2, 3 | Path A trigger via existing soft-keyboard or paste row's input | Trigger the existing renderer smoke matrix's long-output row (300-line burst). Pass = no torn frames, scrollback is reachable via OS scroll gesture, no JS error in the workspace, no payload appears in `console.*`. |
| 11 | **Narrow-viewport reflow / autofit** | 1, 2, 3 | OS rotate or viewport resize | With autofit on: rotating portrait↔landscape (or resizing the simulated viewport) reflows `stty size`; the `data-renderer-autofit` attribute stays `"active"` throughout; no panel collapses, no overflow scrollbar appears horizontally inside the grid. Pass = reflow without intermediate broken paint. |
| 12 | **Detach / reconnect** | 2, 3 | Path A / UI | From the workspace, click Detach (`production-terminal-detach`); confirm `data-phase="detached"` and the TTL hint banner appears; switch app focus away (Android home button or app-switch) for under `DETACHED_LIVE_PTY_TTL_SECONDS`; return and click Reconnect (`production-terminal-reconnect`); confirm `data-phase="attached"` and the prompt is restored from the orchestrator-side replay. Wire-side reattach is what is under test; the row is the same shape as the 2026-05-14i wterm matrix detach row but on a real mobile surface. |
| 13 | **Profile / session navigation usability** | 2, 3 | touch / OS keyboard | Walk Dashboard → Servers → profile detail → Launch terminal → Detach → Sessions list (`nav-sessions`) → Reconnect from the list. Pass = every nav button is hit-target-reachable on a small viewport without zooming; selector `data-testid` set holds; no overflow-clipped controls. This is a workspace-shell row, not a renderer row, but it gates the renderer rows operationally. |
| 14 | **Orientation change** | 2, 3 | OS orientation | Rotate during an active session. With autofit on, geometry reflows (covered by row 11). Additionally verify the session does **not** disconnect on orientation change and the wterm DOM grid does not blank permanently. |
| 15 | **Browser back / forward** | 2 only (Chrome) | OS gesture / button | The SPA's in-process navigation is what is under test; pressing the OS back gesture inside Android Chrome should **not** drop the WebSocket / kill the session unless the operator explicitly navigates away. Document the actual behaviour without judgement; if the session dies, that is a workspace concern, not a renderer concern. |
| 16 | **Redaction / storage / log sweep** | all | none | After each surface's rows, dump `document.documentElement.outerHTML`, `localStorage`, `sessionStorage`, `console` history. Sweep for the smoke's ASCII sentinels (from the existing renderer smoke matrix) **outside the terminal viewport**. Sweep for any leak of `BEGIN OPENSSH PRIVATE KEY`, `openssh-key-v1`, `encrypted_private_key`, `session_token`, `token_hash`, `data_b64`. Fail = any payload byte appears outside the viewport. |
| 17 | **xterm control comparison** | 2, 3 | mirrored | Re-run rows 3, 4, 6, 8, 9, 11, 14 with the renderer flipped back to xterm. The point is not to grade xterm — it is to record **whether wterm regressed against xterm on the rows the operator can subjectively rank**. Without this, a wterm "works with caveats" cannot be interpreted as "better or worse than the current default." Note that xterm canvas cannot be selected for native copy (row 9), so xterm is expected to fail row 9 — that is the *justification* for wterm's mobile lane, not an xterm bug. |

Rows 3–6 + 9 are the load-bearing wterm-on-mobile rows; rows 11–12
gate viability; rows 16–17 prevent the smoke from being interpreted
without controls.

### Status after the 2026-05-15c surface-2 first execution

The first execution of this runbook against surface 2
(Android Chrome) landed as the
[`2026-05-15c · wterm Android Chrome (surface 2) browser smoke`](deployment/vps-staging-smoke.md#2026-05-15c-wterm-android-chrome-surface-2-browser-smoke-mount-rotation-pass-live-pty-attach-not-reached-open-question)
dated entry in the staging-smoke log. Per-row status from that
first execution (surface 2 only; surfaces 3 and 4 not yet
attempted):

| # | Row | Surface 2 status (2026-05-15c) | Why |
|---|---|---|---|
| 1 | Renderer mount diagnostic | **PASS** | wterm grid + visible block cursor mounted on the production-terminal element on both Launch attempts. |
| 2 | Autofit posture | NOT GRADED on phone | Carried forward; surface-1 autofit already covered by 2026-05-15 / 2026-05-15b dated entries. |
| 3 | Tap-to-focus | PARTIAL PASS | `production-terminal-focus` button worked; soft keyboard rose. Selector-side verification skipped because Chrome's WebView is opaque to uiautomator. |
| 4 | Soft-keyboard open / layout | PARTIAL PASS | wterm cursor stayed visible above the IME; grid did not reflow on IME open / close (autofit is mount-time only in the current `@wterm/dom` 0.2.x adapter). |
| 5 | Viewport-height drives autofit | **DEFERRED** | Blocked by Row 12 detach finding — no live PTY to round-trip `stty size` against. |
| 6 | ASCII input (Path A) | **DEFERRED** | Blocked by Row 12; no live PTY. |
| 7 | Modifier-key affordance scoping | DEFERRED | Confirmed by inspection (no in-workspace modifier bar on the samsung IME); full scope deferred to its own slice. |
| 8 | Paste flow | **DEFERRED** | Blocked by Row 12; the paste-safety panel could not be exercised against a live remote prompt. |
| 9 | Copy / select | **DEFERRED** | The wterm DOM is mountable, but the copy/select flow's "bytes survive paste into another app" check needs visible payload to copy. With no PTY output, no payload existed. |
| 10 | Long output / scroll | **DEFERRED** | Blocked by Row 12; no PTY. |
| 11 | Narrow-viewport reflow on rotation | **PARTIAL PASS** | Rotating portrait↔landscape reflowed the workspace nav rail and control row cleanly; the wterm grid itself did not re-fit (autofit is mount-time only — see Row 4). No broken paint. |
| 12 | **Detach / reconnect (the headline)** | **OPEN QUESTION** | Two consecutive sessions reached `detached (TTL window) seq=0`; backend nginx confirmed POST→201 + GET ws→101 for both with a consistent ~60s POST→WS dial gap; SSH-target container shows zero inbound connections (russh never dialed). Reconnect within the 30s TTL window did not flip to live. Not yet root-caused. See the 2026-05-15c dated entry for the full evidence trail. |
| 13 | Profile / session nav usability | PARTIAL PASS | Every nav button reachable on 1080-wide screen; control-row buttons sit at tight thumb spacing. |
| 14 | Orientation change on active session | **DEFERRED** | Blocked by Row 12; no active session to rotate against. |
| 15 | Browser back / forward | **DEFERRED** | Skipped — see "What was NOT done" in the dated entry. |
| 16 | Redaction / storage / log sweep | **PASS** | Sentinel-clean grep across backend + nginx + SSH-target logs; no `private_key`, `encrypted_private_key`, session-token, cookie, PEM, argon2, or throwaway-email substring. |
| 17 | xterm control comparison | **DEFERRED → first row of next slice** | The next slice (`docs/wterm-android-browser-resmoke`) MUST run this row first; the Row 12 question is workspace-vs-renderer and the xterm control is what distinguishes them. |

The pass / partial / deferred markings are *as of 2026-05-15c
only* and are not a renderer judgement; they are an honest
record of what the first surface-2 execution could and could
not reach. Promotion / xterm-default decisions are NOT on the
table from this row set alone.

### Status after the 2026-05-16 xterm-control resmoke

The follow-on slice `docs/wterm-android-browser-resmoke` ran
**Row 17 (xterm control comparison) first** on the same
Samsung phone / same home wifi / same staging stack — see the
[`2026-05-16 · docs/wterm-android-browser-resmoke (surface 2,
xterm control)`](deployment/vps-staging-smoke.md#2026-05-16-docswterm-android-browser-resmoke-surface-2-xterm-control--first-launch-reproduces-the-2026-05-15c-detach-pattern-retries-recover-bug-is-workspace-bound--transient-not-wterm-specific)
dated entry in the staging-smoke log. The xterm control result
resolves the 2026-05-15c Row 12 open question:

- **Row 12** is reclassified from **OPEN QUESTION** (wterm-bound
  vs workspace-bound) to **WORKSPACE-BOUND + TRANSIENT**. The
  very first xterm launch reproduced the 68 s POST→WS gap and
  immediate detach-at-seq-0 pattern; launches 2 and 3 on the
  same renderer / phone / network / throwaway went live within
  ≈2 s of POST. wterm is therefore **not implicated** as the
  cause of the 2026-05-15c detach finding. The next slice owner
  is *workspace-side WS-open timing*, not *wterm*.
- **Row 17** is **PASS for xterm** as a renderer (it mounted,
  attached, and round-tripped operator input on Android Chrome
  in launches 2 and 3). It is **not yet PASS for wterm** — but
  the comparison the row was added to make is now possible: the
  xterm result tells us the detach is *not* a wterm-vs-xterm
  delta, so a future re-test of wterm on the same surface
  should be evaluated on the workspace-side fix, not on the
  renderer.
- **Methodology correction.** The 2026-05-15c "SSH-target
  container shows zero inbound connections" reading was based
  on `docker logs` of the linuxserver/openssh-server throwaway,
  which only writes init / boot lines to stdout. Runtime sshd
  activity goes to syslog inside the container. The correct
  probe is `netstat -tn` inside the throwaway (or
  `ps -ef | grep sshd-session`). With the corrected probe,
  launches 2 and 3 showed a sustained ESTABLISHED connection
  from the backend container to the throwaway on port 2222;
  the russh dial path is fine. The 2026-05-15c detach
  finding is **most plausibly** the WS upgrade arriving after
  the orchestrator's server-side attach-timeout — not "russh
  never dialed". This slice does not rewrite the 2026-05-15c
  entry in place; the 2026-05-16 dated entry flags the
  interpretation correction at its end.
- **Rows D–H / L / surface-3-Tauri** remain deferred — the
  same workspace-side question blocks them, and the next
  executable slice is workspace-investigation, not another
  surface-2 row sweep.

## 6. Test prerequisites

- **Staging URL.** `https://relayterm-staging.js-node.cc` (the
  canonical staging stack per
  [`docs/deployment/vps-staging-smoke.md`](deployment/vps-staging-smoke.md)).
  Verify the staging-only CSP includes `script-src 'self'
  'wasm-unsafe-eval'` (it is what wterm needs to compile its WASM
  core). Production deploy templates remain strict — no production
  CSP change for this evaluation.
- **Staging-only operator account.** Bootstrap a fresh throwaway
  user via the production sign-in / first-time-setup flow on staging,
  per § C of `docs/deployment/vps-staging-smoke.md`. **Never** a
  production account. **Never** a personal SSH key. Sign out and
  delete the bootstrap user at the end of the smoke if the staging
  stack is shared.
- **Throwaway SSH target.** The hermetic
  `linuxserver/openssh-server:latest` container pattern
  (`relayterm-staging-<smoke-id>-ssh`) the existing staging smokes
  use. Internal-only Compose network; no host port published; user
  `smoke` with `PASSWORD_ACCESS=false`, `SUDO_ACCESS=false`; ed25519
  identity generated backend-side or imported via the existing
  base64-sidecar + `atob` pattern (PEM bytes never appear in any
  tool-call payload, log, Error, or DOM string). Tear the container
  down at teardown.
- **Android device or emulator.**
  - A **real device** is preferred for IME / soft-keyboard / touch
    rows (4–9). The 2026-05-09 Android Tauri smokes used a Samsung
    Galaxy S10e (`SM-G970U`, Android 12) which is still a reasonable
    smoke target.
  - An **emulator** (`avdmanager` / Android Studio AVD) is acceptable
    for the diagnostic / autofit / scrolling rows (1, 2, 10, 11, 14)
    but soft-keyboard / IME results from an emulator are not
    representative — soft-keyboard rows must be run on a real device
    or be marked `deferred — emulator soft keyboard not
    representative`.
- **Tauri Android dev shell.** Already scaffolded under
  `apps/mobile/src-tauri/` (`@relayterm/mobile`, Tauri v2,
  `minSdkVersion 28`). Build the debug APK with the canonical
  command:

  ```sh
  pnpm --filter @relayterm/mobile exec tauri android build --debug --apk --ci
  ```

  `--ci` skips signing prompts; `--debug --apk` is the local-smoke
  command (the AGENTS.md gotcha: `--aab` is the Phase 4 / Play Store
  path, **not** the local smoke path). The APK lands under
  `apps/mobile/src-tauri/gen/android/app/build/outputs/apk/debug/`.
  Install via `adb install -r <apk>`.
- **adb available.** `adb devices` lists the target. `adb logcat`
  with a Chromium / WebView filter captures WebView console output
  for the diagnostic rows. The previous Android Tauri smokes used
  `adb shell input text` / `KEYCODE_ENTER` (66) for input — that is a
  fallback for emulators / unreachable IME, *not* the soft-keyboard
  row.
- **Remote WebView debugging.** For surface 3 (Android Tauri), enable
  WebView debugging in the Tauri config (Tauri v2 enables it by
  default in `--debug` builds), then attach Chrome DevTools at
  `chrome://inspect` on the desktop to see the WebView's console and
  DOM. Required for the redaction sweep on row 16.
- **No production hosts.** No production SSH target. No production
  user. No production deploy.
- **No personal private keys.** The throwaway identity generated for
  the smoke is the only key material in scope.
- **Internal throwaway SSH target pattern.** See "Throwaway SSH
  target" above; the pattern is fixed and matches the existing
  staging smoke entries.

## 7. Instrumentation / diagnostics

Capture the items below for each row. **None of them carry payload
bytes** — every entry is a closed-vocabulary attribute, a numeric
viewport dim, or a presence-only check.

- `data-renderer` (closed vocab: `xterm` / `ghostty-web` / `restty` /
  `wterm` / `unmounted`).
- `data-renderer-experimental` (`true` / `false`).
- `data-renderer-fallback` (`""` / `experimental_gate_off` /
  `unknown_renderer_id` / `adapter_load_failed` / `adapter_mount_failed`).
- `data-renderer-gate` (`on` / `off`).
- `data-renderer-input` (`marked` / `none`).
- `data-renderer-autofit` (`off` / `active` / `unsupported`).
- `data-phase` on `production-terminal` (`idle` / `creating` /
  `connecting` / `attached` / `replaying` / `detached` / `closed` /
  `error`).
- `window.innerWidth` / `window.innerHeight` before / during / after
  the soft keyboard opens.
- `window.visualViewport?.width` / `window.visualViewport?.height` /
  `window.visualViewport?.offsetTop` for the same transitions
  (visual viewport is the load-bearing API for soft-keyboard layout
  on Android Chrome / WebView; fall back gracefully if it is
  undefined and record that).
- `document.activeElement` identity (`tagName`, the value of any
  `[data-testid]` attribute, the value of `[data-relayterm-terminal-input]`
  presence — never `value` / `textContent`).
- `localStorage` and `sessionStorage` key set (keys only — record
  no values for any key matching `auth*`, `session*`, `paste*`,
  `terminal*`, or anything not in the documented terminal-settings
  schema).
- `chrome://inspect` console history (WebView surface) — record only
  the count of errors / warnings and the first 80 chars of each
  message **after** confirming no message starts with a renderer
  payload sentinel. If a message contains a sentinel, redact it to
  `[REDACTED-SENTINEL-MATCH]`.
- Network errors only (HTTP status, URL host, never request /
  response body). Capture for the `/api/v1/*` and WebSocket handshake
  surfaces specifically.
- `stty size` from inside the remote shell at each geometry
  transition (this is *remote-shell output*, allowed inside the
  viewport — the smoke entry records the value, not the surrounding
  output frame).
- Screenshots and screen recordings: **do not** capture by default.
  If absolutely necessary for a regression report, capture into a
  *local* directory outside the repo and confirm before sharing that
  no terminal viewport contents, no sentinel strings, no auth cookies
  / URLs with tokens, and no inventory rows (host names, profile
  names, identity fingerprints) appear in the frame. Do not commit
  screenshots to the repo unless they have been deliberately
  sanitised and the sanitisation step is documented in the smoke
  entry.

## 8. Risk / privacy / redaction posture

- **No production data anywhere.** No production SSH hosts, no
  production user accounts, no production session cookies, no
  personal keypairs. Every secret in scope is throwaway and
  destroyed at teardown.
- **HttpOnly session cookies stay unreadable.** The smoke must not
  attempt to read the auth cookie value via `document.cookie` or
  WebView devtools; it remains HttpOnly + SameSite as the production
  contract requires. Recording "cookie present" is fine; recording
  the cookie value is a failure.
- **Treat terminal viewport contents as payload.** Anything that
  reaches the viewport is operator-visible by design, but it is not
  recordable outside the viewport. The redaction-rules envelope
  (`docs/agent/redaction-rules.md`) applies verbatim — no payload
  bytes in `console.*`, `localStorage`, `sessionStorage`, audit
  rows, thrown Errors, `data-*` attributes, panel bodies, or
  screenshots.
- **No paste contents in stable state.** The production paste-safety
  pipeline (`evaluatePaste` → `production-terminal-paste-confirm`
  panel sourced from `PasteDecision` *metadata only*; the panel
  never echoes the paste body) is the contract the row 8 check
  inherits. Do not bypass it on any surface. Pasting the throwaway
  ed25519 public key for the auth-check step is allowed exactly once
  and only via the base64-sidecar / `atob` pattern the existing
  staging smokes use — not by typing it on the soft keyboard, not by
  pasting it into a panel that would render it.
- **Mobile screenshots and screen recordings.** Stay local unless
  explicitly sanitised. The Android screen capture often includes
  the status bar, notification shade preview, and adjacent app
  contents — additional redaction surface a desktop screenshot does
  not have. If a video is genuinely needed (regression report), trim
  to the smallest reproducing window and confirm no payload appears.
- **WebView devtools console.** `adb logcat` and `chrome://inspect`
  both surface JavaScript console output from the WebView. The
  redaction posture for these is identical to the workspace's own
  `console.*` posture — no payload bytes. Recording an error stack
  is fine *only if* the stack does not contain a payload sentinel; if
  it does, redact before recording.
- **CSP unchanged.** The staging `wasm-unsafe-eval` relaxation is
  required for wterm to compile its WASM core under any browser
  (desktop or mobile). Production CSP stays strict and this plan
  changes neither.
- **No new wire shape.** The plan exercises the existing
  `RTB1`-binary `Output` / `Input` plane and the existing control-
  plane JSON. No new protocol variant.

## 9. Decision outputs

After running the smoke (in a separate, dated entry), we should be
able to answer the following four questions and pick the next
slice:

1. **Is wterm viable on Android WebView at all?** Answered by rows
   1, 2, 12. If the answer is no (mount fails, autofit reports
   `unsupported` when it should be `active`, or reconnect dies),
   the wterm mobile lane is blocked until the underlying issue is
   fixed and the smoke re-runs.
2. **What UX gaps block a usable Android app?** Answered by rows
   3–9 + 11 + 13. The honest expected gap set (informed by the
   existing 2026-05-09 Android Tauri xterm smoke) is at least:
   the absence of a mobile command / modifier bar (no easy
   `Ctrl-C` / `Esc` / `Tab` / arrows), uncertainty about
   soft-keyboard viewport-shrink behaviour, and copy-paste UX that
   needs verification end-to-end. The smoke replaces speculation
   with a measured list.
3. **Does wterm regress against xterm on any operator-rankable
   row?** Answered by row 17 — the xterm control re-run on the same
   surface. If wterm regresses against xterm on rows 4, 6, 11, 14 (
   layout, basic input, reflow, orientation), that is a wterm
   blocker; if wterm only wins on row 9 (native selection), that
   alone may not justify a per-surface default flip but does keep
   wterm as the credible mobile candidate.
4. **What is the next executable slice?** One of the following
   (these are mutually exclusive next slices; the smoke picks
   one):
   - **`feat/mobile-command-bar`** — add an on-screen modifier
     row above the terminal (`Ctrl` / `Esc` / `Tab` / arrows /
     `Ctrl-C` / `Ctrl-D` etc.). Most likely outcome if rows 6 /
     7 expose the modifier gap as the dominant UX blocker.
   - **`feat/soft-keyboard-viewport`** — wire `visualViewport`
     into the workspace so the prompt stays anchored when the
     keyboard opens / closes. Most likely outcome if row 4 / 5 /
     11 expose layout breakage as the dominant blocker.
   - **`feat/copy-paste-mobile-ux`** — refine the paste-confirm
     panel for touch and the copy gesture for touch-selection on
     wterm. Most likely if rows 8 / 9 expose touch-specific paste
     UX issues.
   - **`fix/wterm-mobile-<specific>`** — a targeted wterm
     adapter fix if a row exposes a wterm-specific bug. The
     adapter package is `packages/terminal-wterm/`; any fix
     respects the existing redaction tests and the
     renderer-neutral seam.
   - **`feat/mobile-fallback-to-xterm`** — if wterm regresses
     against xterm enough that the mobile lane is not viable
     yet, the next slice is to **explicitly route mobile-class
     surfaces back to the xterm default while keeping wterm
     gated for operator opt-in**. xterm has its own mobile
     caveats (no native cell selection) but is the proven
     baseline. This is the conservative fallback, not a wterm
     promotion failure.
   - **`docs/wterm-mobile-defer`** — if the smoke reveals that
     mobile rendering is dependent on a broader workspace UX
     reshape (e.g. the existing AppShell sidebar consumes most
     of a small viewport), the next slice is a separate
     mobile-app-shell plan rather than continuing the renderer
     lane.

The smoke's deliverable is a `docs/deployment/vps-staging-smoke.md`
dated entry (matching the existing entry shape) plus a
recommendation pointer in
[`docs/renderer-comparison-scorecard.md`](renderer-comparison-scorecard.md)
to whichever of the slices above is picked.

## 10. Non-goals

- **No renderer promotion.** wterm stays experimental and
  operator-gated for the entire duration of this plan and the
  smoke that follows it.
- **No xterm default flip.** xterm remains the default on the
  desktop browser, desktop Tauri, and Android Tauri shells.
- **No production CSP change.** Production deploy templates stay
  strict. The staging `wasm-unsafe-eval` relaxation already in
  place is sufficient for wterm to compile its WASM core on the
  staging surface.
- **No desktop / mobile release packaging.** The Android Tauri APK
  is a **debug** APK signed with the debug keystore. `--aab` (Play
  Store) is explicitly out of scope.
- **No benchmark automation.** Throughput / memory / reflow-cost
  numbers are not part of this plan — the benchmark harness is its
  own deferred slice
  ([`docs/terminal-renderer-evaluation.md`](terminal-renderer-evaluation.md)
  § "Explicitly deferred").
- **No tmux / screen / host-side multiplexer integration.**
  Host-side multiplexer persistence is independent of the renderer
  track and remains deferred per
  [`docs/persistent-sessions.md`](persistent-sessions.md).
- **No backend / session / orchestrator change.** No new wire
  message, no new control-plane variant, no replay-buffer change.
  The existing detach / reconnect / replay contract is what row 12
  exercises.
- **No iOS coverage.** No iOS shell exists in the repo.

## 11. Recommended next executable slice

**`docs/wterm-android-browser-smoke`.** Run the smoke against
**Android Chrome** (surface 2) first, *before* the Tauri WebView
shell (surface 3). Rationale:

- Surface 2 needs **no APK build** — just a real Android device,
  Chrome, and the existing staging stack. Setup cost is the lowest.
- Surface 2 isolates "is wterm itself viable on mobile?" from
  "does the Android System WebView differ from Chrome in a way that
  affects wterm?" — those are two separate questions, and surface 2
  answers the first cleanly. If surface 2 fails, surface 3 cannot
  succeed, and the Tauri rebuild was wasted.
- Surface 2 is also the path the **previous Android xterm Tauri
  smokes did not exercise** — they went straight to the Tauri shell
  (because the Tauri handoff was the load-bearing surface for
  *those* slices). A wterm-on-Chrome data point is genuinely new.

**`docs/wterm-android-tauri-smoke`** is the immediate follow-on
slice — same rows, same surface scope, run after the Chrome row is
green and the existing Tauri Android debug APK has been rebuilt
against the production-shell bundle that contains wterm. The Tauri
shell smoke piggy-backs on Galaxy S10e + the existing bundled-shell
handoff infrastructure the 2026-05-09 entries established, so the
incremental cost from the Chrome smoke is small.

**Update after the 2026-05-15c surface-2 execution.** The Chrome
smoke is **not** green — see the per-row table in §5 above and
the dated entry it references. The follow-on Tauri smoke
(`docs/wterm-android-tauri-smoke`) therefore stays deferred
until the Row 12 detach question has at least one re-investigation
slice behind it. The next executable slice is
`docs/wterm-android-browser-resmoke` (xterm-first control rerun on
the same phone, same network, same staging stack), not the Tauri
shell. See the "Next slice proposed" subsection in the 2026-05-15c
dated entry for the exact scope.

**Update after the 2026-05-16 xterm-control resmoke.** The
xterm-first rerun landed (see the 2026-05-16 dated entry in
`docs/deployment/vps-staging-smoke.md`). Row 12 is reclassified
as **workspace-bound + transient** (xterm reproduced the 68 s
POST→WS gap + detach-at-seq-0 on its first launch; launches 2
and 3 went live in ≈2 s). wterm is exonerated as the cause of
the 2026-05-15c finding. The **next executable slice is now
workspace-side**, not surface-2 / surface-3 wterm: a
`docs/mobile-first-launch-ws-investigation` (working title)
that instruments the mobile-Chrome WS-open timing and the
orchestrator's server-side attach-timeout. Running the Tauri
smoke (`docs/wterm-android-tauri-smoke`) or a wterm surface-2
re-test before the workspace-side fix lands would re-collect
the same intermittent first-launch detach pattern across every
renderer and is not useful evidence.

If the Chrome smoke (surface 2) reveals that wterm is **not yet**
mobile-ready as built — likely the modifier-bar gap or a
soft-keyboard layout regression — defer the Tauri smoke
(`docs/wterm-android-tauri-smoke`) until the fix slice from § 9
lands. Running the Tauri smoke against a known-broken renderer is
not a useful data point; it just doubles the redaction surface.

The desktop browser narrow-viewport rows (surface 1, rows 1, 2, 11)
do **not** need their own dated entry — they were already covered
by the 2026-05-15 autofit resmoke and the 2026-05-15b diagnostic
resmoke. If a future regression suggests the narrow-viewport
behaviour has changed, re-run that resmoke first, before the mobile
smoke.

## See also

- [`docs/renderer-comparison-scorecard.md`](renderer-comparison-scorecard.md)
  — current renderer evidence; the "next recommended action" for
  wterm points here.
- [`docs/terminal-renderer-evaluation.md`](terminal-renderer-evaluation.md)
  — Gate 1 / Gate 2 promotion criteria, "Surfaces" list this plan
  realises.
- [`docs/spec/terminal-adapters.md`](spec/terminal-adapters.md)
  § "wterm experimental renderer adapter" — adapter contract,
  `focusTarget()`, autofit precedence, redaction posture.
- [`docs/renderer-smoke-harness.md`](renderer-smoke-harness.md) —
  input-path taxonomy (Path A / C / D / E / I) and command-matrix
  source-of-truth that the rows in § 5 inherit.
- [`docs/renderer-neutral-autofit.md`](renderer-neutral-autofit.md)
  — autofit design + per-adapter implemented contract; the basis
  for row 2 / row 5.
- [`apps/web/e2e/SMOKE.md`](../apps/web/e2e/SMOKE.md) § "D. Renderer
  evaluation smoke" — operator runbook the mobile smoke extends
  (renderer-fair input procedure, clipboard permission step,
  command matrix, sentinel discipline).
- [`docs/deployment/vps-staging-smoke.md`](deployment/vps-staging-smoke.md)
  — the existing dated wterm smoke entries (2026-05-14g,
  2026-05-14i, 2026-05-15, 2026-05-15b) the mobile smoke will sit
  alongside.
- [`docs/agent/redaction-rules.md`](agent/redaction-rules.md) —
  the long-form rules § 8 inherits.
