---
name: tailwindcss-tasks
description: Situational guidance for Tailwind CSS v4 — load when editing CSS or Svelte component files. Covers the CSS-first @theme block, @import "tailwindcss", Vite plugin integration, and v3-to-v4 migration traps.
paths: "apps/web/**/*.css,apps/web/**/*.svelte,packages/terminal-*/**/*.css,packages/terminal-*/**/*.svelte"
---

# Tailwind CSS (v4)

> Auto-loads on stylesheets and component files. Project-wide rules in `AGENTS.md`.

## Version + rationale

**Pinned to `^4`.** v4 is CSS-first: theme, content detection, and most config live in CSS, not JavaScript. Significantly faster builds. v3 syntax does NOT work — the directive names changed.

## Critical gotchas

### Entry directive

**Don't:**
```css
@tailwind base;
@tailwind components;
@tailwind utilities;
```

**Do:**
```css
@import "tailwindcss";
```

That single line generates Preflight, all utilities, and theme variables.

### Theme tokens

**Don't** edit `tailwind.config.js` — it's optional/legacy in v4.

**Do** put theme tokens in CSS using `@theme`:

```css
@theme {
  --color-relay: oklch(0.7 0.15 260);
  --font-mono: "JetBrains Mono", ui-monospace, monospace;
}
```

CSS custom-property names map to utilities (`--color-relay` → `bg-relay`, `text-relay`).

### Content detection

v4 auto-detects template files in your repo — no `content: ['./src/**/*.{...}']` array required for typical projects. If a file in a non-standard location isn't getting scanned, use `@source` to add it:

```css
@source "../../packages/terminal-xterm/src";
```

### Vite plugin

Use `@tailwindcss/vite` (not the PostCSS plugin):

```ts
import tailwindcss from '@tailwindcss/vite';
export default defineConfig({ plugins: [tailwindcss()] });
```

### CLI

The CLI moved to its own package: `npx @tailwindcss/cli -i input.css -o output.css`. The bare `npx tailwindcss` still works but emits a v3 deprecation path.

### Migration tool

If you ever inherit a v3 codebase: `npx @tailwindcss/upgrade` automates ~80% of the v3→v4 transition. Requires Node ≥ 20.

## Default tooling

| Task | Command |
|---|---|
| Dev (via Vite plugin) | `pnpm --filter web dev` |
| Format with class-sort | `pnpm --filter web format` (uses `prettier-plugin-tailwindcss`) |

<!-- agentic-init: curated above this line -->

## Project-specific patterns

*(no entries yet)*
