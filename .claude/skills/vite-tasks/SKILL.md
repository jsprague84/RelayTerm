---
name: vite-tasks
description: Situational guidance for Vite ^7 — load when editing vite.config.* or the web app's entry. Covers conditional config callback, oxc minifier default, rolldownOptions, and SSR/preview flags.
paths: "apps/web/vite.config.*,apps/web/src/main.*,apps/web/index.html"
---

# Vite (^7)

> Auto-loads on Vite config + entry files. Project-wide rules in `AGENTS.md`.

## Version + rationale

**Pinned to `^7`.** v7 stabilizes the Environment API and switches the default minifier to `oxc` (faster, also Rust-based). v8 exists but is still settling at the time this doc was generated — pin v7 unless a feature requires v8.

## Critical gotchas

### Config as a callback

For env-conditional behavior, use the function form rather than separate config files:

```ts
import { defineConfig } from 'vite';

export default defineConfig(({ command, mode, isSsrBuild, isPreview }) => {
  if (command === 'serve') return { /* dev config */ };
  return { /* build config */ };
});
```

Compare with `=== true` / `=== false` explicitly — some Vite-config loaders pass `undefined` for the optional flags.

### Minifier default

`build.minify` defaults to `'oxc'` in v7+ (was `'esbuild'`). If a third-party plugin assumes esbuild output it may misbehave; set `build.minify: 'esbuild'` to opt back in if needed.

### Rolldown opt-in

Vite is migrating its bundler to Rolldown; opt in via `build.rolldownOptions: { ... }`. Until you're explicitly using rolldown, stay with the default Rollup-backed pipeline. Plugin compatibility for Rolldown is still uneven.

### Tauri integration

Vite must produce assets that Tauri can serve. In `tauri.conf.json` set `build.frontendDist` to Vite's `outDir` (default `dist`) and `build.devUrl` to the Vite dev server URL. Avoid changing `outDir` without updating the Tauri config in lockstep.

### Tailwind v4 integration

Use the official Vite plugin, not PostCSS:

```ts
import tailwindcss from '@tailwindcss/vite';
export default defineConfig({ plugins: [tailwindcss()] });
```

## Default tooling

| Task | Command |
|---|---|
| Dev server | `pnpm --filter web dev` |
| Production build | `pnpm --filter web build` |
| Preview built bundle | `pnpm --filter web preview` |

<!-- agentic-init: curated above this line -->

## Project-specific patterns

*(no entries yet)*
