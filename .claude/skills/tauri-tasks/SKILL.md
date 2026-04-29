---
name: tauri-tasks
description: Situational guidance for Tauri v2 — load when editing the desktop/Android shell under src-tauri/. Covers the v2 conf schema, capabilities/permissions, mobile init/build, IPC plugin layout, and Android keyboard/viewport quirks.
paths: "apps/web/src-tauri/**,apps/web/src/lib/tauri/**"
---

# Tauri (v2)

> Auto-loads on `apps/web/src-tauri/**`. Project-wide rules in `AGENTS.md`.

## Version + rationale

**Pinned to `^2`.** v2 is the line that supports Android and iOS. v1 is incompatible: the `tauri.conf.json` schema changed, the allowlist was replaced with capabilities, and the plugin system was rewritten.

## Critical gotchas

### Conf schema split

`tauri.conf.json` v2 is structured as:
```jsonc
{
  "app": {           // window config, security, withGlobalTauri
    "windows": [...],
    "security": { "csp": "..." }
  },
  "build": {         // bundler integration
    "frontendDist": "../dist",
    "devUrl": "http://localhost:5173",
    "beforeBuildCommand": "pnpm --filter web build",
    "beforeDevCommand": "pnpm --filter web dev"
  },
  "bundle": { ... },
  "plugins": { ... }
}
```

v1's flat schema does not work. If you've inherited a v1 config, run `cargo tauri migrate` to convert.

### Capabilities replace the allowlist

Each window declares which permissions it has via JSON files in `src-tauri/capabilities/`. There is no global `allowlist` block. Adding a new IPC command requires both the Rust side (`#[tauri::command]`) and a capability entry that includes its permission identifier.

### Mobile init

```bash
pnpm tauri android init
pnpm tauri ios init
```

These scaffold platform projects under `src-tauri/gen/android/` and `src-tauri/gen/ios/`. **Do not edit those generated files by hand** — re-run `init` if the toolchain/scaffolding ever changes shape. The capability files live alongside the regular ones.

### Mobile dev/build

```bash
pnpm tauri android dev                    # connected device or emulator
pnpm tauri android build --aab            # for Play Store
pnpm tauri android build --aab --target aarch64 --target armv7    # subset
```

Default target set is `aarch64,armv7,i686,x86_64`. Trim it during dev for faster builds.

### Android signing

Production builds need `src-tauri/gen/android/keystore.properties` with `password`, `keyAlias`, `storeFile` pointing at a `.jks` keystore generated via `keytool`. RelayTerm's keystore is NOT in-repo — it's loaded from a per-developer secret path.

### Plugin pairs

A v2 plugin is two pieces: a Rust crate (`tauri-plugin-<name>`) and a JS binding (`@tauri-apps/plugin-<name>`). Install both. The Rust side registers via `tauri::Builder::default().plugin(...)`; the JS side imports the typed wrappers.

## Mobile-specific quirks

- **Keyboard / IME** — when the on-screen keyboard opens on Android, the WebView's `visualViewport` shrinks. Listen for `visualViewport.addEventListener('resize', ...)` and reflow the terminal so the prompt stays above the keyboard. The Tauri shell forwards no extra event for this; it's a pure web-platform concern.
- **Safe-area insets** — use CSS `env(safe-area-inset-bottom)` etc. Don't hardcode pixel margins.
- **WebView ≠ Chrome** — Android's WebView (System WebView, often Chromium-derived but lagging) sometimes lacks the latest CSS or Web Platform features. Test on a real device, not just desktop Chrome.
- **Hardware keyboard support** — Android with a Bluetooth keyboard sends key events directly to the WebView; verify your renderer adapter handles them without relying on the on-screen IME's `composing` events.

## Default tooling

| Task | Command |
|---|---|
| Desktop dev | `pnpm tauri dev` |
| Desktop build | `pnpm tauri build` |
| Android dev | `pnpm tauri android dev` |
| Android build (AAB) | `pnpm tauri android build --aab` |
| iOS dev (later) | `pnpm tauri ios dev` |
| Migrate v1→v2 (if ever needed) | `cargo tauri migrate` |

<!-- agentic-init: curated above this line -->

## Project-specific patterns

*(no entries yet)*
