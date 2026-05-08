# Tauri v2 desktop & mobile — CI / release plan

> Phased plan for taking the RelayTerm Tauri v2 desktop and mobile shells
> from "stub directory" to "signed, store-distributable artifacts."
> This is a **planning document**. It does not change runtime behavior,
> add CI workflows, collect signing secrets, or alter any release path.

The companion documents
[`docs/deployment/docker-compose.md`](./docker-compose.md) and
[`docs/deployment/production-runbook.md`](./production-runbook.md) cover
the **server/web** track only; that track is mature (Docker images,
Forgejo CI, registry publish, manual operator runbook) and is **not
affected** by the work this document describes.

---

## Docs freshness

The version-specific facts in this document were captured on
**2026-05-07** from the official Tauri v2 documentation, retrieved via
the Context7 MCP server (library ID `/tauri-apps/tauri-docs`). Tauri's
mobile, distribution, and signing docs change more frequently than its
desktop core docs, and the official action (`tauri-apps/tauri-action`),
its supported runner labels, and Apple/Google signing requirements are
the most volatile.

**Before starting any implementation phase below, re-confirm the
phase-relevant facts against the current
[Tauri v2 docs](https://v2.tauri.app/) and the current
[`tauri-apps/tauri-action`](https://github.com/tauri-apps/tauri-action)
README.** When this document and the upstream docs disagree, upstream
wins; update this document in the same change.

---

## 1. Purpose and scope

This document is a **plan**, not an implementation. It exists so that
when a future session picks up "wire up desktop or mobile CI," there is
a single staged plan that:

- Reflects the current state of the repo (the Tauri shells are
  presently `.gitkeep` stubs — see § 3).
- Cites the current Tauri v2 docs for prerequisites, build commands,
  and signing-secret shapes.
- Sequences work so that signing, store submission, and unfamiliar
  runner platforms (Windows / macOS / iOS) are explicitly deferred
  until earlier, simpler phases have proved out the toolchain.

This document **does not**:

- Add or modify any `.forgejo/workflows/*` file.
- Generate Tauri scaffolds in `apps/desktop/` or `apps/mobile/`.
- Add code-signing material, certificates, keystores, or App Store /
  Play Store credentials.
- Change `pnpm-workspace.yaml`, the root `Cargo.toml` workspace
  members, or the root `package.json`.
- Touch the existing server/web Docker image build or publish path.
- Add release-tag automation, auto-deploy, SBOM generation, or image /
  artifact signing of any kind.
- Add any `apps/web` behavior or any backend behavior.

---

## 2. Current RelayTerm release tracks

| Track | State | Where it lives |
|---|---|---|
| Server/web Docker | Mature | `apps/backend/`, `apps/web/`, `Dockerfile.{backend,web}`, `.forgejo/workflows/ci.yml`, [`docker-compose.md`](./docker-compose.md), [`production-runbook.md`](./production-runbook.md) |
| Tauri **desktop** shell | Empty stub (see § 3) | `apps/desktop/` |
| Tauri **mobile** shell (Android first) | Empty stub (see § 3) | `apps/mobile/` |

Both Tauri shells, when they exist, will consume the built `apps/web`
SPA via Tauri's `frontendDist` config. The server/web track produces
the backend the Tauri shells will talk to but is otherwise independent;
nothing in this plan changes the server/web track.

`SPEC.md` § "Out of scope (v1)" already lists "iOS Tauri build (Android
first; iOS later)" and notes "the Tauri desktop and mobile shells …
ship with no automated CI/build pipeline yet." This plan is the staged
path out of that state.

---

## 3. Current repo inventory (2026-05-07)

A precise read of what exists today, so the plan does not
over-promise.

**Tauri shells:**

- `apps/desktop/` contains exactly `.gitkeep` (0 bytes). No
  `src-tauri/`, no `package.json`, no `tauri.conf.json`, no
  `Cargo.toml`, no `capabilities/`, no `icons/`, no `gen/`.
- `apps/mobile/` contains exactly `.gitkeep` (0 bytes). Same
  emptiness; no `gen/android/`.
- Last touch on either path is the foundation commit `cd9da47
  Establish RelayTerm foundation` (2026-04-28).

**Workspace declarations:**

- `pnpm-workspace.yaml` lists `apps/desktop` and `apps/mobile` (along
  with `apps/web` and `packages/*`). pnpm currently skips them because
  neither holds a `package.json`. `pnpm -r check` is green on a clean
  tree.
- Root `Cargo.toml` `members` includes only `apps/backend` and the ten
  `crates/relayterm-*` crates. Neither `apps/desktop/src-tauri` nor
  `apps/mobile/src-tauri` is a workspace member yet.

**CI:**

- Single workflow file: `.forgejo/workflows/ci.yml`. Jobs are
  `rust-checks`, `web-checks`, `docker-build`, and `publish-images`,
  all running on `runs-on: docker` inside a
  `catthehacker/ubuntu:act-latest` container against a Docker DinD
  sidecar.
- No reference to `tauri`, `android`, `ios`, `mobile`, or `desktop`
  anywhere in `.forgejo/`.
- Registry: `git.js-node.cc`, namespace `jsprague`. Auth via
  `FORGEJO_REGISTRY_TOKEN` repo secret. Tag policy: `:main`,
  `:vX.Y.Z`, `:sha-<short>`; **no `:latest`**.

**Stack pin:** `AGENTS.md` fixes `tauri | ^2 | Adds Android/iOS; v1
conf schema is incompatible.` No mobile-specific crate pins.

**Tooling references:** No file in the repo mentions `ANDROID_HOME`,
`ANDROID_NDK_HOME`, `JAVA_HOME`, Xcode, or iOS tooling.

**Verdict:** the wrappers are not skeletal — they are *absent*. The
first implementation slice is therefore scaffolding plus local-build
docs, not a Linux desktop CI smoke. (See § 10.)

---

## 4. Platform build matrix

> **Refresh before implementing each row.** Tauri's bundle defaults,
> Rust target lists, and runner-label conventions evolve. Treat this
> table as the planning baseline; verify each row against current
> Tauri docs at the start of the relevant implementation phase.

| Target | Runner | Build command | Prerequisites | Artifacts | Signing | Feasible today? | Phase |
|---|---|---|---|---|---|---|---|
| Linux desktop | Existing Forgejo Linux runner (Docker DinD) — needs Tauri Linux deps installed in-job or in a custom image | `pnpm tauri build` | `libwebkit2gtk-4.1-dev`, `build-essential`, `curl`, `wget`, `file`, `libxdo-dev`, `libssl-dev`, `libayatana-appindicator3-dev`, `librsvg2-dev` | Depends on `tauri.conf.json` bundle targets; common outputs include `.AppImage`, `.deb`, `.rpm`, plus a raw binary. **Final Linux bundle targets are a Phase 0/1 decision** to be made when the wrapper is scaffolded. | None initially (unsigned smoke) | After scaffold, yes — same runner family as the existing CI | Phase 1 |
| Windows desktop | Native Windows runner (`windows-latest` on a host that supports it, or self-hosted Windows VM) | `pnpm tauri build` | MSVC build tools (Visual Studio Build Tools); WebView2 runtime for end users | `.msi`, `.exe`, NSIS installer (verify exact set against current Tauri docs at Phase 2 time) | Authenticode (`WINDOWS_CERTIFICATE`) — **deferred** | Not until a Windows runner is sourced | Phase 2 |
| macOS desktop | `macos-latest` (or self-hosted macOS) | `pnpm tauri build --target aarch64-apple-darwin` and `… x86_64-apple-darwin` (one job per arch per upstream pattern) | Xcode | `.dmg`, `.app` | Apple Developer ID + notarization — **deferred** | Not until a macOS runner is sourced | Phase 2 |
| Android mobile | Linux runner with JDK + Android SDK + Android NDK + Android Rust targets | `pnpm tauri android build --apk` (smoke) → `pnpm tauri android build --aab` (Play Store) | JDK 17+, Android SDK, Android NDK; `rustup target add aarch64-linux-android armv7-linux-androideabi i686-linux-android x86_64-linux-android`; `tauri android init` already executed and committed | Unsigned `.apk` for smoke; signed `.aab` for release | Android keystore (`ANDROID_KEY_*`) — **deferred** | Possible on the Linux runner once tooling is installed | Phase 3 |
| iOS mobile | `macos-latest` only | `pnpm tauri ios build` | Xcode, Apple Developer membership, provisioning profile, signing identity; `rustup target add aarch64-apple-ios x86_64-apple-ios aarch64-apple-ios-sim`; `tauri ios init` already executed and committed | `.ipa` | Apple Developer + `APPLE_API_KEY_PATH` (required for iOS bundling per current Tauri docs) — **deferred** | Not until macOS runner + Apple Developer account are sourced | Phase 5+ |

---

## 5. Recommended staged implementation

Five phases. Each names what it does and what it intentionally defers.
**No phase begins until the previous phase has landed and is green on
`main`.**

### Phase 0 — scaffold + local-build docs *(strongly recommended next slice; see § 10)*

> **Status (2026-05-07):** Phase 0 implemented in branch `feat/tauri-shell-scaffold`. Both shells are scaffolded with Tauri v2 (CLI 2.11.1, `tauri = 2.11.1`, `tauri-build = 2.6.1`); identifiers are `cc.js-node.relayterm.{desktop,mobile}`; Android `minSdkVersion = 28`; `apps/mobile/src-tauri/gen/android/` is committed; `apps/{desktop,mobile}/src-tauri` are registered as Cargo workspace members. `cargo check --workspace` passes after installing the GTK stack. `tauri dev` / `tauri build` / `tauri android dev` / `tauri android build` are documented but not exercised in this slice — see [`tauri-local-build.md`](./tauri-local-build.md).

- Generate the desktop and mobile Tauri scaffolds locally, using the
  **official Tauri CLI** for the version pinned in `AGENTS.md`. The
  expected commands are `tauri init` (run inside `apps/desktop/`) and
  `tauri android init` (run inside `apps/mobile/` after a desktop
  scaffold exists). **Confirm the exact CLI commands and flags
  against the current Tauri v2 docs in the implementation session
  itself** — the CLI surface evolves and these are the most likely
  facts to drift between this plan and Tauri's release notes.
- Wire `apps/{desktop,mobile}/package.json` into the pnpm workspace
  (already declared, needs concrete `package.json` files) with at
  minimum `tauri:dev` and `tauri:build` scripts on desktop, and
  `tauri:android:dev` / `tauri:android:build` scripts on mobile.
- Add the new `apps/desktop/src-tauri` and `apps/mobile/src-tauri`
  Cargo crates to the root `Cargo.toml` `members` list.
- Document the exact local prerequisites for Linux desktop builds and
  Android builds in a new
  [`docs/deployment/tauri-local-build.md`](./tauri-local-build.md) (the
  Phase 4 prerequisite list, verbatim, plus the `rustup target add …`
  invocations from § 4).
- Validate locally: `pnpm tauri dev`, `pnpm tauri build`, and (only if
  the developer has Android SDK / NDK installed) `pnpm tauri android
  build --apk`. Capture the exact output paths in the local-build doc.
- **No CI changes in this phase.** The point of Phase 0 is to produce
  a scaffolded, locally-buildable repo state that a later CI phase can
  build against without making scaffold decisions under CI pressure.
- Local prerequisites, commands, output paths, and the exact set of
  verifications performed in this slice are recorded in
  [`tauri-local-build.md`](./tauri-local-build.md). Treat that file as
  the authoritative entry point for any contributor who wants to do
  a local desktop or Android build against this scaffold.

### Phase 1 — Linux desktop CI smoke

- New workflow file `.forgejo/workflows/desktop-linux.yml`. Per § 6,
  desktop and mobile workflows live in workflow files separate from
  the existing `ci.yml`; any exception to this must explicitly revise
  § 6's workflow-separation rule in the same change.
- Runner: existing Forgejo Docker runner.
- Container: `catthehacker/ubuntu:act-latest` (matches the rest of CI),
  with the Phase 4 Debian package list installed in a per-run step
  initially. Switch to a custom prepared image **only** if per-run
  install adds more than ~5 minutes of wall time.
- Build: unsigned `pnpm tauri build` for Linux. Bundle targets per
  `tauri.conf.json` (decided in Phase 0).
- Artifact: upload via Forgejo's artifact upload primitive, named
  `relayterm-desktop-linux-<sha-short>`. **Verify Forgejo's artifact
  primitive meets needs at Phase 1 time** — if it falls short, defer
  the upload step and treat the smoke as build-only until an external
  bucket is sourced.
- No registry publish. No `:latest` equivalent. No release tag
  automation in this phase.
- Trigger: `pull_request` + push-to-main, mirroring the existing
  `ci.yml` policy. Concurrency: same `cancel-in-progress` shape as
  `ci.yml`.

### Phase 2 — Windows + macOS desktop

- Adds `windows-latest` and `macos-latest` matrix entries.
- **Precondition:** a runner strategy is in place for both OSes (see §
  6 for options). Phase 2 does not begin until that strategy is
  chosen.
- Adopts `tauri-apps/tauri-action@v0` to keep the matrix YAML small
  and to track upstream's evolving signing/notarization integration.
  Confirm the action's then-current major version at Phase 2 time.
- Still unsigned. Artifact upload only. No release tag automation.

### Phase 3 — Android build smoke

- Linux runner with JDK 17+, Android SDK, Android NDK, and the
  Android Rust targets installed. Per-run install initially; promote
  to a prepared image when per-run install exceeds ~5 minutes of wall
  time.
- Build: `pnpm tauri android build --apk` (unsigned debug). Confirm
  current flag set against Tauri docs at Phase 3 time — `--target`
  filtering and `--apk` vs `--aab` semantics are documented but
  evolve.
- Artifact upload as `relayterm-mobile-android-<sha-short>.apk`.
- **No keystore**. No Play Store submission.

### Phase 4 — Signing & release tags

- Three independent secret families introduced. Each is a separate
  sub-slice with its own approval gate; **none is collected before
  this phase begins**.
  - **Apple (macOS desktop and iOS):** `APPLE_ID`, `APPLE_PASSWORD`,
    `APPLE_TEAM_ID`, `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`,
    `KEYCHAIN_PASSWORD`. Or the JWT alternative: `APPLE_API_KEY`,
    `APPLE_API_ISSUER`, `APPLE_API_KEY_PATH`. `APPLE_PASSWORD`
    supports `@keychain:` and `@env:` indirection per Tauri's
    environment-variable docs.
  - **Windows authenticode:** `WINDOWS_CERTIFICATE` (base64-encoded
    `.pfx`) plus `WINDOWS_CERTIFICATE_PASSWORD`. Imported in CI via
    `certutil` + `Import-PfxCertificate`.
  - **Android keystore:** `ANDROID_KEY_ALIAS`, `ANDROID_KEY_PASSWORD`,
    `ANDROID_KEY_BASE64`. CI step writes
    `apps/mobile/src-tauri/gen/android/keystore.properties` and a
    temporary `.jks` from base64.
- Release tag (`v*`) drives signed builds; non-tag pushes stay
  unsigned, mirroring the server-image tag policy in `ci.yml`.
- Decide at this phase whether to add SBOM, cosign signatures, or
  artifact checksums. Do **not** sneak any of these into Phases 1–3.
- Human approval gate documented as `workflow_dispatch` plus
  per-environment protected secrets (Forgejo equivalent of GitHub's
  protected environments — confirm exact mechanism at Phase 4 time).

### Phase 5 — Mobile release distribution

- Android: Play Console internal track → closed → open. AAB only.
  This phase requires a Play Developer account and its own gating
  process.
- iOS: TestFlight → App Store. Requires macOS runner and a paid
  Apple Developer membership ($99/yr at the time of writing).
- Deliberately the latest phase. Independent of server/web release
  cadence.

---

## 6. Forgejo runner strategy

- The current Docker DinD runner labeled `docker` is correct for the
  backend / web container builds and is **probably** sufficient for
  Linux Tauri desktop and Android mobile builds, *if* the build job
  installs (or pulls a prepared image with) the Phase 4 Linux deps and
  the Android SDK / NDK / JDK. The current
  `catthehacker/ubuntu:act-latest` container does not include any of
  this; per-run install is the v0 path. A custom runner image is the
  v1 optimization, justified only once cadence and runtime cost
  warrant the maintenance burden.
- Windows / macOS / iOS each need a different runner. Three options
  to evaluate at Phase 2 (desktop) and Phase 5 (iOS) time:
  1. **Self-hosted native runner per OS.** Most control; ongoing
     maintenance burden; macOS hardware is a fixed cost.
  2. **GitHub-hosted runner via mirror + Actions bridge.** Simpler
     for low cadence; mirror configuration becomes load-bearing.
  3. **SaaS CI** (BuildJet, Codemagic, etc.) for those targets only.
- iOS specifically requires macOS hardware and a paid Apple Developer
  membership. Treat this as a **money-and-hardware decision**, not a
  code decision — the code path is straightforward once both are in
  place.
- **Desktop and mobile workflows must live in workflow files separate
  from the existing `ci.yml`.** The server/web track is mature and
  green; coupling Tauri build failures into the server-image gate
  would create false signals on unrelated changes.

---

## 7. Security and secrets

- The repo currently has exactly one CI secret:
  `FORGEJO_REGISTRY_TOKEN`, used only by the existing `publish-images`
  job. Don't disturb it.
- **No signing keys, certificates, keystores, or App Store / Play
  Store credentials live in the repo.** Ever. Not in `apps/`, not in
  `deploy/`, not in `docs/`. Phases 0–3 do not collect any.
- All future Apple / Windows / Android secrets are repository (or
  organization) secrets, never committed. Names follow Tauri's
  upstream docs verbatim — see Phase 4 for the canonical list.
- Artifact upload steps must opt in to bundle outputs only. They must
  not include locally-generated `.env` files, `keystore.properties`
  files, signing material, developer keychain dumps, build caches, or
  the `target/` directory.
- No secret value is ever echoed in workflow logs. Mirror the
  `ci.yml` `publish-images` token-handling pattern: length-zero
  pre-check, env-only injection, no `set -x`, no `echo $SECRET`.
- The server-deploy registry PAT and the future signing PATs live in
  **different secret namespaces**. A signing PAT must not have
  registry write scope, and the registry PAT must not have signing
  scope.

---

## 8. Artifact policy

- **Smoke artifacts** (Phases 1–3): retain ~14 days, named
  `<target>-<sha-short>` (e.g.
  `relayterm-desktop-linux-9573c3c.AppImage`).
- **Release artifacts** (Phase 4+): retain at least 1 year, named
  `<target>-vX.Y.Z`, driven by `git tag`.
- **Never publish a `:latest` equivalent.** Operators pin explicitly,
  mirroring the server-image policy already documented in `ci.yml`.
- Checksums (`SHA256SUMS`), SBOMs, and cosign signatures are deferred.
  Phase 4 decides; Phases 1–3 do not pre-empt the choice.
- File-size sanity bounds are a Phase 4 nice-to-have, not a Phase 1
  gate.

---

## 9. Open questions

These remain unresolved at the time of writing. Each must be answered
before the relevant phase begins, but **no phase is blocked on the
others' questions** — keep them sequenced as in § 5.

1. **First desktop OS target.** Linux only (the Phase 1 default), or
   simultaneous Linux + Windows + macOS once a runner picture is
   known?
2. **Linux bundle output set.** AppImage, `.deb`, `.rpm`, all three,
   or a different subset? Decided in Phase 0/1, recorded in
   `tauri.conf.json`.
3. **Windows / macOS runners.** Self-hosted, GitHub-hosted via
   bridge, or SaaS? Where do self-hosted hosts live?
4. **Forgejo artifact upload primitive.** Sufficient for desktop /
   mobile artifact sizes, or is an external bucket needed? Test in
   Phase 1.
5. **Backend URL configuration.** Does each Tauri shell embed a
   build-time backend URL, or is the host configurable at runtime
   from the user's settings panel? Affects whether builds need
   per-environment baking.
6. **Environment identification.** Does a Tauri shell identify
   dev/staging/prod via build-time env, runtime detection, or a
   bundled config? `apps/web` uses build-time `VITE_*`; the Tauri
   shell may want a different answer.
7. **Mobile session storage.** Same `HttpOnly; Secure; SameSite=Strict`
   cookie path as the web SPA, or platform-native secure storage
   (Android Keystore, iOS Keychain)? Affects the auth contract.
8. **Mobile saved server profiles and SSH identities.** Same
   encrypted vault pathway as the backend's, or a platform-keystore-
   backed pathway?
9. **Native Tauri commands.** Will Tauri be a thin shell around the
   web SPA (cheapest, default for v1), or add native commands
   (filesystem, biometrics, push, BLE)?
10. **Useful smoke beyond "binary builds".** A "binary launches and
    shows the web SPA loading" smoke is the minimum next step after
    Phase 1; deeper smokes (login flow, terminal attach) are deferred
    until Phase 1 lands and is green.

---

## 10. Recommended next implementation slice

**Recommendation: Phase 0 — Tauri desktop + Android shell scaffold +
local-build docs.**

Rationale: the wrappers are *absent* (not just skeletal — see § 3).
Skipping Phase 0 forces the implementer to make scaffold decisions
inside a CI-pressured branch, which is the wrong order. Phase 0
produces a clean, locally-buildable starting state on which Phase 1's
Linux desktop CI smoke can build without surprises.

Phase 0's deliverable, in order:

1. Generate `apps/desktop/src-tauri/` and `apps/mobile/src-tauri/`
   using the official Tauri CLI (commands: confirm against current
   Tauri v2 docs at slice-execution time; expected baseline is `tauri
   init` for desktop and `tauri android init` for mobile).
2. Wired `apps/{desktop,mobile}/package.json` files with `tauri:*`
   scripts.
3. `apps/desktop/src-tauri/Cargo.toml` and `apps/mobile/src-tauri/
   Cargo.toml` registered in the root `Cargo.toml` `members` list.
4. New
   [`docs/deployment/tauri-local-build.md`](./tauri-local-build.md)
   capturing exact local prerequisites, exact `pnpm` commands, and
   exact output paths for Linux desktop and Android — verified by the
   implementer running each command at least once.
5. **No CI changes.** No signing. No app behavior beyond what the
   Tauri CLI's default scaffold produces.

A separate later branch implements **Phase 1 — Linux desktop CI
smoke** on top of Phase 0. Phases 2+ follow the order in § 5.
