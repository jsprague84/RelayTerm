---
name: svelte-tasks
description: Situational guidance for Svelte 5 — load when editing .svelte or .svelte.ts files in the web app. Covers the runes API ($state/$derived/$effect/$props), event-attribute syntax, and migration traps from Svelte 4.
paths: "apps/web/**/*.svelte,apps/web/**/*.svelte.ts,apps/web/**/*.svelte.js"
---

# Svelte 5

> Auto-loads on Svelte source files. Project-wide rules in `AGENTS.md`.

## Version + rationale

**Pinned to `^5`.** Svelte 5 ships the runes API and is the new compiler default. Svelte 4 syntax (`export let`, `on:click`, top-level `$:`) compiles in legacy mode but RelayTerm targets runes-only — the codebase consistently uses the new syntax.

## Critical gotchas

### Reactive state

**Don't:**
```svelte
<script>
  let count = 0;            // not reactive in Svelte 5
  $: doubled = count * 2;   // legacy syntax
</script>
```

**Do:**
```svelte
<script>
  let count = $state(0);
  let doubled = $derived(count * 2);
  $effect(() => { console.log('count is', count); });
</script>
```

### Props

**Don't** `export let foo;` / `$$restProps` / `$$props`.

**Do** `let { foo, bar = 'default', class: klass, ...rest } = $props();`. For two-way binding, mark a prop with `$bindable()`.

### Event attributes

**Don't** `on:click={fn}`, `on:keydown={fn}` — that's Svelte 4 syntax.

**Do** `onclick={fn}`, `onkeydown={fn}` — DOM event names directly.

### Effects vs lifecycle

`onMount` and `onDestroy` still exist for one-shot mount/unmount work, but most "react when X changes" code that lived in `afterUpdate` / `beforeUpdate` should now be `$effect(...)` or `$effect.pre(...)` — the latter for read-before-DOM-mutation cases (e.g. preserving scroll position).

### Snippets

Svelte 5 snippets (`{#snippet name(args)}` and `{@render name(args)}`) replace many slot use cases. Slots with named content still work but snippets are the recommended path for new code.

### Reactivity at module scope

Top-level `$state` and `$derived` work in `.svelte.ts`/`.svelte.js` files (not plain `.ts`/`.js`). RelayTerm puts shared runes-based stores under `apps/web/src/lib/stores/*.svelte.ts`.

## Integration footguns

- **Mixing legacy and runes** in one file produces compiler errors. Choose one mode per component.
- **Reactivity does not cross network boundaries** — fetched data is reactive only if you bind it through `$state`. Don't expect a raw `await` result to update the view.

## Default tooling

| Task | Command |
|---|---|
| Type-check | `pnpm --filter web check` (svelte-check + tsc) |
| Lint | `pnpm --filter web lint` |
| Format | `pnpm --filter web format` |
| Test | `pnpm --filter web test` (vitest) |
| Dev server | `pnpm --filter web dev` |
| Production build | `pnpm --filter web build` |

<!-- agentic-init: curated above this line -->

## Project-specific patterns

*(no entries yet)*
