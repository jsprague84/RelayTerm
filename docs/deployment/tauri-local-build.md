# Tauri local build (Phase 0)

> **Scope:** local developer builds only. No CI, no signing, no store distribution, no native command surface, no production backend selection. For everything beyond local builds, see [`tauri-ci-release-plan.md`](./tauri-ci-release-plan.md).

## Status of this slice

- ✅ `apps/desktop/` — Tauri v2 desktop wrapper. `cargo check -p relayterm-desktop` passes on Linux with the GTK stack installed.
- ✅ `pnpm --filter @relayterm/desktop tauri:build` — verified on CachyOS / Arch with WebKitGTK 4.1 and libayatana-appindicator. Produces the native binary, `.deb`, and `.rpm`. The AppImage stage requires `NO_STRIP=true` on this host (see "AppImage strip incompatibility" below). This verifies packaging/build only — runtime backend connectivity is not exercised because Phase 0 has no production backend URL wired into the bundled SPA.
- ✅ `apps/mobile/` — Tauri v2 mobile (Android-first) wrapper. `tauri android init` was run on a Linux host with JDK 17, Android SDK, and NDK 30.0.14904198 installed; the generated `gen/android/` Gradle/Kotlin scaffold is committed.
- ✅ `pnpm --filter @relayterm/mobile exec tauri android build --debug --apk --ci` — verified on CachyOS (Arch-derived Linux) with JDK 17, Android SDK at `~/Android/Sdk`, NDK 30.0.14904198, and the four Android Rust targets (`aarch64-linux-android`, `armv7-linux-androideabi`, `i686-linux-android`, `x86_64-linux-android`). Produces a debug, unsigned, universal APK at `apps/mobile/src-tauri/gen/android/app/build/outputs/apk/universal/debug/app-universal-debug.apk` (≈ 437 MB; bundles all 4 ABI `.so`s with debuginfo, plus the bundled SPA). Verifies local Android packaging/build only — does NOT verify emulator/device runtime, backend connectivity, mobile session behaviour, signing/release readiness, or Play Store/AAB distribution.
- ❌ `cargo check -p relayterm-mobile --target aarch64-linux-android` (standalone) — not exercised; the Android cross-compile *is* exercised transitively as part of `tauri android build` (which produced libs for all four ABIs into `target/<android-target>/debug/`), but the explicit standalone `cargo check` against an Android target was not run.
- ❌ `tauri:dev` (desktop GUI), `tauri android dev` (live device/emulator) — not exercised in this slice.

## Frontend reuse model

Both wrappers consume the existing `apps/web` Svelte SPA:

- **Dev mode** (`tauri dev`, `tauri android dev`): Tauri loads `devUrl: http://localhost:5173`, which is the existing `apps/web` Vite dev server. `apps/web/vite.config.ts` already proxies `/api` and `/healthz` → `http://127.0.0.1:8080`, so dev-mode backend connectivity reuses the proxy with no new wiring.
- **Build mode** (`tauri build`, `tauri android build`): Tauri reads `frontendDist: ../../web/dist` (relative to `apps/{desktop,mobile}/src-tauri/`) and bundles the static SPA. **There is no Vite dev proxy in the bundled SPA.** Phase 0 does not solve production backend connectivity for the bundled shells — runtime API base URL / backend selection is an explicit deferred design item (see "Deferred work" below).

Successfully running `tauri build` proves the shell can bundle and package the SPA. It does **not** prove live backend/API connectivity unless a backend URL has been configured out-of-band and tested.

## Bundle identifiers

| Surface | tauri.conf.json `identifier` | Android `applicationId` |
|---|---|---|
| Desktop | `cc.js-node.relayterm.desktop` | (n/a) |
| Mobile  | `cc.js-node.relayterm.mobile`  | `cc.js_node.relayterm.mobile` |

Tauri's Android scaffold transliterates hyphens to underscores in the Java/Kotlin namespace (`applicationId`, package directory) because Java identifiers cannot contain hyphens. The canonical identifier in `tauri.conf.json` keeps the hyphenated form. Debug builds add `.debug` to `applicationId` (default scaffold setting).

## Linux desktop prerequisites

### CachyOS / Arch (verified on this host)

```bash
sudo pacman -S --needed \
  webkit2gtk-4.1 \
  gtk3 \
  base-devel \
  curl wget file \
  openssl \
  libayatana-appindicator \
  librsvg \
  xdotool
```

`base-devel` covers `gcc`, `make`, `pkg-config`, etc.

### Debian / Ubuntu (canonical upstream list, NOT verified here)

```bash
sudo apt update
sudo apt install \
  libwebkit2gtk-4.1-dev \
  build-essential \
  curl wget file \
  libxdo-dev \
  libssl-dev \
  libayatana-appindicator3-dev \
  librsvg2-dev
```

### Rust + Node toolchain

- Rust `>= 1.85` (matches `rust-version` in the workspace `Cargo.toml`). Use `rustup`.
- Node `>= 20` and `pnpm 10.33.0` (matches `packageManager` in the root `package.json`).

### Windows / macOS

Documented as future work. Not exercised in this slice. See the [Tauri v2 prerequisites page](https://tauri.app/start/prerequisites) for the canonical Windows / macOS dep lists.

## Android prerequisites

> **JDK guidance.** Phase 0 is verified against **JDK 17**. JDK 17 is the recommended baseline for Android / Tauri Android builds because the Android Gradle Plugin still centers on JDK 17 as its minimum/default supported Java version. JDK 21 is now an Android-supported alternative; JDK 25 is the current Java LTS but its Android tooling support is younger. Newer JDKs may work, but Phase 0 is verified against 17 unless official Android / Tauri docs require otherwise — do **not** use JDK 26 for Android builds just because it is the latest feature release.

### Required tooling

1. **JDK 17.** On CachyOS / Arch:
   ```bash
   sudo pacman -S --needed jdk17-openjdk
   ```
   Path: `/usr/lib/jvm/java-17-openjdk`.
2. **Android SDK + NDK.** Install via Android Studio's SDK Manager *or* via the standalone command-line tools: <https://developer.android.com/studio#command-line-tools-only>. Unzip into `$HOME/Android/Sdk/cmdline-tools/latest/`.
3. **SDK packages** (verified set used to generate this slice's `gen/android/` and to produce the local APK on 2026-05-07):
   ```bash
   sdkmanager "platform-tools" "platforms;android-36" "build-tools;36.0.0" "ndk;30.0.14904198"
   ```
   Tauri's generated `gen/android/app/build.gradle.kts` pins `compileSdk = 36`, `targetSdk = 36`, `minSdk = 28`. The NDK version matches whatever directory the CLI finds under `$ANDROID_HOME/ndk/`. The Android Gradle Plugin accepts a higher *minor* than `compileSdk` requests — the local APK smoke succeeded against `platforms;android-36.1` and `build-tools;{36.1.0,37.0.0}` already installed on the host (Android Studio's bundled set), without needing to downgrade. Stick with the `android-36` / `build-tools;36.0.0` baseline above for new installs unless a future Tauri scaffold change bumps `compileSdk`.
4. **Android Rust targets** (Tauri's `--skip-targets-install` flag intentionally leaves this to you):
   ```bash
   rustup target add \
     aarch64-linux-android \
     armv7-linux-androideabi \
     i686-linux-android \
     x86_64-linux-android
   ```

### Environment variables

The contributor running this slice's verification uses Bash; persist these in `~/.bashrc` or `~/.bash_profile`:

```bash
# ~/.bashrc or ~/.bash_profile
export JAVA_HOME=/usr/lib/jvm/java-17-openjdk
export ANDROID_HOME="$HOME/Android/Sdk"
export NDK_HOME="$ANDROID_HOME/ndk/$(ls -1 "$ANDROID_HOME/ndk" | sort -V | tail -1)"
export PATH="$JAVA_HOME/bin:$ANDROID_HOME/cmdline-tools/latest/bin:$ANDROID_HOME/platform-tools:$PATH"
```

`tauri android dev` and `tauri android build` need all three (`JAVA_HOME`, `ANDROID_HOME`, `NDK_HOME`) exported in the shell that runs them. Other shells (zsh, fish) need their own rc adapted from the snippet above. For a one-shot build without modifying any rc file, the same `export` block works inline in the current Bash session.

## Local commands

All commands run from the repo root unless noted otherwise.

### One-time setup

```bash
pnpm install                # installs @tauri-apps/cli for the desktop and mobile workspaces
```

### Desktop (Linux)

```bash
# Dev: launches the Vite server (apps/web) and a Tauri window pointed at localhost:5173
pnpm --filter @relayterm/desktop tauri:dev

# Release bundle: produces .deb / .rpm (and AppImage when supported on the host) under target/release/bundle/
# (Cargo workspaces share a single root `target/`; see the artifact path list below.)
pnpm --filter @relayterm/desktop tauri:build

# Release bundle, deb + rpm only (matches the Phase 1 Linux desktop CI smoke):
# Skips the AppImage stage entirely, so the DT_RELR `linuxdeploy` strip
# incompatibility documented under Troubleshooting cannot be hit.
pnpm --filter @relayterm/desktop exec tauri build --bundles deb,rpm
```

Artifacts land at the **workspace** target directory, not the per-crate one — Cargo workspaces share a single `target/`:

- Native binary: `target/release/relayterm-desktop`
- Debian package: `target/release/bundle/deb/RelayTerm_<version>_amd64.deb`
- RPM: `target/release/bundle/rpm/RelayTerm-<version>-1.x86_64.rpm`
- AppImage: `target/release/bundle/appimage/RelayTerm-x86_64.AppImage` (only when the AppImage stage succeeds — see "AppImage strip incompatibility" below)

All paths are relative to the repo root. The intermediate `target/release/bundle/{deb,appimage}/RelayTerm_<version>_amd64/` and `target/release/bundle/appimage/RelayTerm.AppDir/` directories are scratch staging areas for the bundlers and are safe to ignore.

### Mobile / Android

```bash
# (Re-runnable) regenerate gen/android/ scaffold — only needed if the scaffold drifts
pnpm --filter @relayterm/mobile tauri:android:init

# Dev: deploys to a connected device or running emulator
pnpm --filter @relayterm/mobile tauri:android:dev

# Local debug, unsigned, universal APK (verified — see "Verification performed" below).
# `--debug` selects the debug Cargo profile (no signing config needed); `--apk` skips
# AAB; `--ci` skips Tauri's interactive prompts. Use the explicit invocation rather
# than the bare npm script `tauri:android:build`, which defers to `tauri android
# build`'s release-mode default and is therefore meant for the eventual signed-AAB
# release path (Phase 4+ in tauri-ci-release-plan.md), not local smoke.
pnpm --filter @relayterm/mobile exec tauri android build --debug --apk --ci
```

Output paths (debug build):

- APK: `apps/mobile/src-tauri/gen/android/app/build/outputs/apk/universal/debug/app-universal-debug.apk`

The "universal" debug APK bundles all four ABIs (`arm64-v8a`, `armeabi-v7a`, `x86`, `x86_64`) into one file along with the Rust libraries built with debuginfo, so it is large (≈ 437 MB on 2026-05-07). Production release builds use `--aab` with split ABIs to ship a much smaller per-device package; that path is **deferred** (no signing in this slice).

> **`version` ≥ `0.0.1` is mandatory for Android.** Tauri rejects `version: "0.0.0"` in `apps/mobile/src-tauri/tauri.conf.json` with `"The default value '0.0.0' is not allowed for Android package and must be at least '0.0.1'"`. The mobile config was bumped to `0.0.1` for this slice. The desktop config keeps `0.0.0` because Linux `.deb`/`.rpm` accept it; if a later phase needs to align desktop and mobile versions, do that as a deliberate version-policy change, not a side-effect of an Android build.

> **Signing material is intentionally NOT tracked in the repo.** No `*.jks`, `*.keystore`, `local.properties`, `key.properties`, `keystore.properties`, `.gradle/`, `.cxx/`, or `build/` outputs are committed. Signing for release builds is deferred to a later phase; see [`tauri-ci-release-plan.md`](./tauri-ci-release-plan.md).

### Mobile / Android — local device install + launch smoke

This is a **runtime smoke** for the prebuilt debug APK on a real device or emulator. It proves the APK installs, the launcher activity dispatches, the process stays alive, and the bundled SPA renders inside the Android WebView. It does **not** prove backend connectivity, login, terminal attach, runtime backend URL config, mobile session lifecycle, signing, Play Store readiness, or anything beyond first-frame render.

The verifying contributor connects an Android device with USB debugging enabled (or boots an existing AVD), confirms the device is `device` (not `unauthorized`) in `adb devices -l`, then runs:

```bash
# Identify the target — pick the right serial if more than one device is attached
adb devices -l

# Install (replace if already present, do NOT uninstall on signature mismatch
# without operator approval)
adb -s <serial> install -r \
  apps/mobile/src-tauri/gen/android/app/build/outputs/apk/universal/debug/app-universal-debug.apk

# Launch the LAUNCHER activity
adb -s <serial> shell monkey -p cc.js_node.relayterm.mobile.debug -c android.intent.category.LAUNCHER 1

# Liveness probe (see "pidof race" troubleshooting note below — re-probe after 1–2s)
adb -s <serial> shell pidof cc.js_node.relayterm.mobile.debug || true
adb -s <serial> shell "ps -A | grep relayterm" || true
adb -s <serial> shell "dumpsys activity activities | grep -E 'mResumedActivity|cc.js_node' | head -20" || true

# Bounded, filtered logcat snapshot — no streaming, no broad capture
adb -s <serial> logcat -d -t 300 | grep -Ei 'relayterm|tauri|webview|crash|fatal|exception|ANR' || true
```

**Package id gotcha (debug builds).** `apps/mobile/src-tauri/gen/android/app/build.gradle.kts` sets `applicationIdSuffix = ".debug"` on the debug build type, so the installed package id for the debug APK is **`cc.js_node.relayterm.mobile.debug`**, not the canonical `cc.js_node.relayterm.mobile` from `tauri.conf.json`. All `monkey -p`, `pidof`, and `logcat` filters must use the suffixed id. The launcher activity stays under the unsuffixed namespace at `cc.js_node.relayterm.mobile.MainActivity` (standard Android behaviour — `applicationId` is the install identity, `namespace` is the Java/Kotlin package).

**Expected outcome on a phone with no backend reachable.** The bundled SPA renders a `Cannot Reach RelayTerm` modal with `Cannot reach the backend: Malformed response` and a `Retry` button. This is the **expected** failure path because runtime backend URL / production API base config is deferred (see "Deferred work"). Treat it as a successful render, not a launch failure.

### Mobile / Android — runtime caveats

- **No emulator/device launch was exercised by `tauri android dev`** in this verification slice. The verified runtime path is the prebuilt debug APK + `adb install -r` + `monkey ... LAUNCHER 1`, not the Tauri-managed dev server.
- **Backend connectivity is not wired** for the bundled (non-dev) shell. Anything past first-frame render — login, identity list, terminal attach, recordings — will fail with a backend-reach error until runtime API base URL configuration lands.
- **Native secure storage** for SSH credentials (Android Keystore) is not implemented. Do not commission a device for real SSH use against this build.
- **Mobile session lifecycle** (background → foreground transitions, doze, low-memory kill, push-driven wake) is unverified. The smoke only proves cold-launch render.
- **Signing / keystore / Play Store / `--aab`** are out of scope for this slice and remain Phase 4+ (see `tauri-ci-release-plan.md`).
- **Android CI** is not yet wired. The Phase 3 prerequisite is now cleared (build + local launch verified), but the workflow file is future work.

## Verification performed

| Command | Status on the verifying host |
|---|---|
| `pnpm install` | ✅ Verified |
| `pnpm -r check` | ✅ Verified |
| `pnpm -r lint` | ✅ Verified |
| `pnpm -r build` | ✅ Verified |
| `pnpm -r test` | ✅ Verified |
| `cargo fmt --all -- --check` | ✅ Verified |
| `cargo check --workspace --all-targets` | ✅ Verified (after `pacman -S webkit2gtk-4.1 libayatana-appindicator`) |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | ✅ Verified |
| `cargo test --workspace` | ✅ Verified |
| `bash scripts/check-doc-contracts.sh` | ✅ Verified |
| `cargo check -p relayterm-desktop` | ✅ Verified |
| `cargo check -p relayterm-mobile` | ✅ Verified (host-target only — Android cross-compile not exercised) |
| `pnpm --filter @relayterm/mobile tauri:android:init` | ✅ Verified (`gen/android/` scaffold committed from the original Phase 0 run) |
| `pnpm --filter @relayterm/desktop tauri:build` (binary + `.deb` + `.rpm`) | ✅ Verified on CachyOS (Arch-derived Linux, kernel 7.0.3-1-cachyos), 2026-05-07, with WebKitGTK 4.1 (`webkit2gtk-4.1 2.52.3`) and libayatana-appindicator (`0.5.94`); rustc 1.95.0, pnpm 10.33.0, Node 25.9.0. Built `target/release/relayterm-desktop` (5.8 MB) in ~2m 29s; `RelayTerm_0.0.0_amd64.deb` (2.4 MB) and `RelayTerm-0.0.0-1.x86_64.rpm` (2.4 MB) bundled. Verifies packaging/build only — does NOT verify backend/API connectivity (Phase 0 has no production backend URL wired into the bundled SPA). |
| `pnpm --filter @relayterm/desktop exec tauri build --bundles deb,rpm` (deb + rpm only — matches Phase 1 CI smoke) | ✅ Verified on the same CachyOS host, 2026-05-07. Tauri reports "Finished 2 bundles"; only `target/release/bundle/{deb,rpm}/` are populated for this run. The AppImage stage is skipped entirely (no `linuxdeploy` invocation), which avoids the DT_RELR strip incompatibility. This is the exact command run by `.forgejo/workflows/desktop-linux.yml`. |
| `pnpm --filter @relayterm/desktop tauri:build` (AppImage) | ⚠ Conditional. The AppImage stage of `tauri:build` fails on this CachyOS host because `linuxdeploy`'s bundled `strip` cannot parse the `.relr.dyn` (DT_RELR) ELF section emitted by modern glibc-built libs. Re-running with `NO_STRIP=true pnpm --filter @relayterm/desktop tauri:build` (or invoking `linuxdeploy` directly with `NO_STRIP=true`) produces a working `RelayTerm-x86_64.AppImage` (93 MB). See "AppImage strip incompatibility" under Troubleshooting. This is an upstream packaging-tool host issue, not a Tauri scaffold bug — `package.json` keeps `tauri build` as the canonical command. |
| `pnpm --filter @relayterm/desktop tauri:dev` | ❌ Not exercised — opens a GUI window and needs an interactive desktop session. |
| `pnpm --filter @relayterm/mobile tauri:android:dev` | ❌ Not exercised — needs a connected device or running emulator. |
| `pnpm --filter @relayterm/mobile exec tauri android build --debug --apk --ci` (debug, unsigned, universal APK) | ✅ Verified on the same CachyOS host (Arch-derived Linux, kernel 7.0.3-1-cachyos), 2026-05-07. JDK 17 (`openjdk 17.0.19`), Android SDK at `~/Android/Sdk` (cmdline-tools/latest, platforms/android-36.1, build-tools/{36.1.0,37.0.0}, platform-tools), NDK `30.0.14904198`, and the four `*-linux-android` Rust targets installed; `JAVA_HOME` / `ANDROID_HOME` / `NDK_HOME` exported in the build shell. Tauri reports "Finished 1 APK"; `app-universal-debug.apk` lands at `apps/mobile/src-tauri/gen/android/app/build/outputs/apk/universal/debug/` (≈ 437 MB) with libraries for all four ABIs (`arm64-v8a`, `armeabi-v7a`, `x86`, `x86_64`). Required scaffold change: bump `apps/mobile/src-tauri/tauri.conf.json` `version` from `0.0.0` to `0.0.1` (Android packaging rejects `0.0.0`). No keystore, no signing, no AAB, no device install. Verifies local Android packaging only — does NOT verify emulator/device runtime, backend connectivity, mobile session behaviour, signing/release readiness, or Play Store distribution. |
| `adb install -r app-universal-debug.apk` + `adb shell monkey -p cc.js_node.relayterm.mobile.debug -c android.intent.category.LAUNCHER 1` (local device install + launch smoke) | ✅ Verified on the same CachyOS host, 2026-05-08, against a physical Samsung Galaxy S10e (`SM-G970U`, codename `beyond0q`, serial `R38N500TY3E`) connected over USB with debugging authorised. `adb install -r` reported `Performing Streamed Install` → `Success`; `monkey` reported `Events injected: 1`; `dumpsys activity activities` showed `mResumedActivity: cc.js_node.relayterm.mobile.debug/cc.js_node.relayterm.mobile.MainActivity` (top + resumed); `ps -A` and re-probed `pidof` confirmed the process alive (PID 13565); the bounded filtered logcat snapshot showed zero `crash`/`fatal`/`exception`/`ANR`/`signal 1[0-9]`/`libc:` lines. The bundled SPA rendered inside the Android WebView and surfaced the expected `Cannot Reach RelayTerm` / `Cannot reach the backend: Malformed response` modal — that is the deferred-runtime-backend-URL failure path, not a launch failure. Verifies cold-launch render only — does NOT verify backend connectivity, login/auth, terminal session attach, runtime backend URL config, background/foreground mobile session lifecycle, signing/release readiness, Play Store distribution, or Android CI. |

`tauri:dev` / `tauri android dev` rows remain deferred to first-use validation by a contributor with a working desktop session and (for `tauri android dev`) a reusable device/emulator workflow — the local install + launch smoke above proves the APK launches, but does not exercise Tauri's managed dev server.

## Troubleshooting

- **`pkg-config: webkit2gtk-4.1 was not found`** during `cargo check` or `cargo build`. Install `webkit2gtk-4.1` (CachyOS) or `libwebkit2gtk-4.1-dev` (Debian).
- **`tauri android init` fails with `cargo metadata` error: "current package believes it's in a workspace when it's not"**. The `apps/{desktop,mobile}/src-tauri` crates must be listed in the root `Cargo.toml` `[workspace] members`. They already are after Phase 0; if you regenerate from scratch into a different layout, re-add them.
- **`tauri android init` fails with NDK / SDK not found.** Confirm `JAVA_HOME`, `ANDROID_HOME`, and `NDK_HOME` are exported in the shell that runs the command. Tauri reads them from the environment, not from `local.properties`.
- **pnpm filter doesn't find `@relayterm/desktop` or `@relayterm/mobile`.** Confirm the package names in `apps/{desktop,mobile}/package.json` match the `--filter` argument and that `pnpm-workspace.yaml` lists `apps/desktop` and `apps/mobile`.
- **Backend connectivity in the built (non-dev) shell.** Phase 0 does **not** wire production backend selection. If the bundled SPA shows network errors, that is expected — see "Deferred work".
- **AppImage strip incompatibility** — `tauri build` ends with `failed to bundle project ´failed to run linuxdeploy´` after producing the `.deb` and `.rpm`. Direct invocation of `~/.cache/tauri/linuxdeploy-x86_64.AppImage` shows repeated `ERROR: Strip call failed: ... unknown type [0x13] section ´.relr.dyn´` lines for libs in `usr/lib/`. Cause: `linuxdeploy` ships a bundled `binutils` whose `strip` predates DT_RELR support, but modern glibc / Arch / CachyOS toolchains emit `.relr.dyn` sections. Workaround: run with `NO_STRIP=true`, e.g. `NO_STRIP=true pnpm --filter @relayterm/desktop tauri:build`. This is an upstream `linuxdeploy` issue; do not change the canonical `tauri build` command in `package.json` to mask it. The `.deb` and `.rpm` are unaffected and remain the recommended Linux distribution targets in this slice.
- **`pidof` returns empty immediately after `monkey ... LAUNCHER 1`.** The process registers with the kernel slightly after `monkey` returns, so a `pidof <package>` invoked back-to-back with `monkey` can race the registration and produce empty output even on a healthy launch. Confirmed on the Galaxy S10e during the 2026-05-08 launch smoke: re-probing `pidof` (and `ps -A | grep <package>`, and `dumpsys activity activities | grep mResumedActivity`) one or two seconds later all returned a PID and `Resumed:` activity record. The reliable liveness check after a `monkey` launch is `dumpsys activity activities | grep mResumedActivity` (or `pidof` after a brief delay). An empty `pidof` immediately after `monkey` is **not** sufficient evidence of a crash; cross-check with `ps -A` and `dumpsys` before declaring a launch failure.

## Deferred work

The following are intentionally out of scope for Phase 0 and tracked in [`tauri-ci-release-plan.md`](./tauri-ci-release-plan.md):

- **Forgejo CI workflows** for desktop or Android builds (no `.forgejo/workflows/` for the Tauri shells in this slice).
- **Code signing**: Tauri updater key, Apple notarization, Google Play upload key, Microsoft Store cert.
- **App store submission and distribution.**
- **Custom Tauri IPC commands** and the corresponding capability rows. The capability set is `core:default` only.
- **Secure native storage** for SSH credentials (Linux Secret Service / Android Keystore / macOS Keychain / Windows Credential Manager).
- **Runtime API base URL / backend selection** for built (non-dev) desktop and mobile shells — no production proxy, no env-driven URL, no in-app picker, no native command bridge for config. The bundled SPA has no Vite dev proxy.
- **iOS shell init** (`tauri ios init`).
- **Production CSP** for the Tauri WebView (`tauri.conf.json` ships with `"security": { "csp": null }`).
- **App icons beyond the Tauri-default placeholders** (the scaffold's icons show the Tauri logo, not RelayTerm branding).
- **Mobile session model**: background/foreground SSH lifecycle, push notifications, lockscreen handling.
