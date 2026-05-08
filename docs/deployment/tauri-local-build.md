# Tauri local build (Phase 0)

> **Scope:** local developer builds only. No CI, no signing, no store distribution, no native command surface, no production backend selection. For everything beyond local builds, see [`tauri-ci-release-plan.md`](./tauri-ci-release-plan.md).

## Status of this slice

- ✅ `apps/desktop/` — Tauri v2 desktop wrapper. `cargo check -p relayterm-desktop` passes on Linux with the GTK stack installed.
- ✅ `apps/mobile/` — Tauri v2 mobile (Android-first) wrapper. `tauri android init` was run on a Linux host with JDK 17, Android SDK, and NDK 30.0.14904198 installed; the generated `gen/android/` Gradle/Kotlin scaffold is committed.
- ❌ `cargo check -p relayterm-mobile --target aarch64-linux-android` — not exercised on this host; needs the Android Rust target's link environment.
- ❌ `tauri build` (desktop bundle), `tauri android build` (APK) — not exercised in this slice.

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
3. **SDK packages** (verified set used to generate this slice's `gen/android/`):
   ```bash
   sdkmanager "platform-tools" "platforms;android-36" "build-tools;36.0.0" "ndk;30.0.14904198"
   ```
   Tauri's generated `gen/android/app/build.gradle.kts` pins `compileSdk = 36`, `targetSdk = 36`. The NDK version matches whatever directory the CLI finds under `$ANDROID_HOME/ndk/`.
4. **Android Rust targets** (Tauri's `--skip-targets-install` flag intentionally leaves this to you):
   ```bash
   rustup target add \
     aarch64-linux-android \
     armv7-linux-androideabi \
     i686-linux-android \
     x86_64-linux-android
   ```

### Environment variables

Add to your shell rc (`~/.bashrc`, `~/.zshrc`, or `~/.config/fish/config.fish`):

```bash
export JAVA_HOME=/usr/lib/jvm/java-17-openjdk
export ANDROID_HOME="$HOME/Android/Sdk"
export NDK_HOME="$ANDROID_HOME/ndk/$(ls -1 "$ANDROID_HOME/ndk" | sort -V | tail -1)"
export PATH="$JAVA_HOME/bin:$ANDROID_HOME/cmdline-tools/latest/bin:$ANDROID_HOME/platform-tools:$PATH"
```

`tauri android dev` and `tauri android build` need all three exported.

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

# Release bundle: produces .deb / .AppImage / .rpm under apps/desktop/src-tauri/target/release/bundle/
pnpm --filter @relayterm/desktop tauri:build
```

Output paths:

- AppImage: `apps/desktop/src-tauri/target/release/bundle/appimage/RelayTerm_<version>_amd64.AppImage`
- Debian package: `apps/desktop/src-tauri/target/release/bundle/deb/RelayTerm_<version>_amd64.deb`
- RPM: `apps/desktop/src-tauri/target/release/bundle/rpm/RelayTerm-<version>-1.x86_64.rpm`

### Mobile / Android

```bash
# (Re-runnable) regenerate gen/android/ scaffold — only needed if the scaffold drifts
pnpm --filter @relayterm/mobile tauri:android:init

# Dev: deploys to a connected device or running emulator
pnpm --filter @relayterm/mobile tauri:android:dev

# Debug APK build
pnpm --filter @relayterm/mobile tauri:android:build
```

Output paths (debug build):

- APK: `apps/mobile/src-tauri/gen/android/app/build/outputs/apk/universal/debug/app-universal-debug.apk`

> **Signing material is intentionally NOT tracked in the repo.** No `*.jks`, `*.keystore`, `local.properties`, `key.properties`, `keystore.properties`, `.gradle/`, `.cxx/`, or `build/` outputs are committed. Signing for release builds is deferred to a later phase; see [`tauri-ci-release-plan.md`](./tauri-ci-release-plan.md).

## Verification performed in this slice

| Command | Status on the slice's host |
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
| `pnpm --filter @relayterm/mobile tauri:android:init` | ✅ Verified (`gen/android/` scaffold committed from this run) |
| `pnpm --filter @relayterm/desktop tauri:dev` | ❌ Not exercised in this slice |
| `pnpm --filter @relayterm/desktop tauri:build` | ❌ Not exercised in this slice |
| `pnpm --filter @relayterm/mobile tauri:android:dev` | ❌ Not exercised in this slice |
| `pnpm --filter @relayterm/mobile tauri:android:build` | ❌ Not exercised in this slice |

`tauri:dev` / `tauri:build` rows are deferred to first-use validation by a contributor with a working desktop session and (for Android) an emulator or device.

## Troubleshooting

- **`pkg-config: webkit2gtk-4.1 was not found`** during `cargo check` or `cargo build`. Install `webkit2gtk-4.1` (CachyOS) or `libwebkit2gtk-4.1-dev` (Debian).
- **`tauri android init` fails with `cargo metadata` error: "current package believes it's in a workspace when it's not"**. The `apps/{desktop,mobile}/src-tauri` crates must be listed in the root `Cargo.toml` `[workspace] members`. They already are after Phase 0; if you regenerate from scratch into a different layout, re-add them.
- **`tauri android init` fails with NDK / SDK not found.** Confirm `JAVA_HOME`, `ANDROID_HOME`, and `NDK_HOME` are exported in the shell that runs the command. Tauri reads them from the environment, not from `local.properties`.
- **pnpm filter doesn't find `@relayterm/desktop` or `@relayterm/mobile`.** Confirm the package names in `apps/{desktop,mobile}/package.json` match the `--filter` argument and that `pnpm-workspace.yaml` lists `apps/desktop` and `apps/mobile`.
- **Backend connectivity in the built (non-dev) shell.** Phase 0 does **not** wire production backend selection. If the bundled SPA shows network errors, that is expected — see "Deferred work".

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
