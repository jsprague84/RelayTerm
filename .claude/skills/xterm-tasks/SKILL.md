---
name: xterm-tasks
description: Situational guidance for xterm.js (@xterm/xterm v5) — load when editing renderer adapters or terminal-* packages. Covers scoped package names, addon imports, write-callback backpressure, and SerializeAddon for replay.
paths: "packages/terminal-*/**,apps/web/src/terminals/**"
---

# xterm.js (`@xterm/xterm` ^5)

> Auto-loads on renderer-package and terminal-adapter source files. Project-wide rules in `AGENTS.md`.

## Version + rationale

**Pinned to `@xterm/xterm@^5`.** The package was rescoped from `xterm` to `@xterm/xterm` (and addons to `@xterm/addon-*`). The unscoped `xterm` package is unmaintained — older docs that import from it are out of date.

## Critical gotchas

### Package imports

**Don't:**
```ts
import { Terminal } from 'xterm';
import { FitAddon } from 'xterm-addon-fit';
```

**Do:**
```ts
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import { WebglAddon } from '@xterm/addon-webgl';
import { SerializeAddon } from '@xterm/addon-serialize';
import '@xterm/xterm/css/xterm.css';
```

### Renderer choice

`@xterm/addon-webgl` is fastest for high-volume PTY output but requires a working WebGL context. Detect failure (`addon.onContextLoss`) and fall back to `@xterm/addon-canvas` (or the default DOM renderer). For RelayTerm's mobile path, canvas is often the right default — the WebGL renderer's GPU upload cost can dominate on phones.

### Backpressure

`term.write(data, callback)` invokes `callback` once the data has been parsed. When relaying high-volume output (`cat /var/log/...`), don't write the next chunk until the previous callback fires — otherwise the parser queue grows unbounded and memory balloons:

```ts
function writeChunk(data: Uint8Array): Promise<void> {
  return new Promise(resolve => term.write(data, resolve));
}
```

### Resize timing

After mounting, call `fitAddon.fit()` once to size the terminal to its container. On window/container resize, call `fit()` again — and emit a `window_change` to the backend so russh resizes the PTY (see `russh-tasks`). Skipping the backend resize causes redraw artifacts in `vim`/`htop`.

### SerializeAddon for replay

The `SerializeAddon` snapshots the current framebuffer to an ANSI string. RelayTerm uses this on the *backend* side conceptually (the orchestrator owns replay), but on the client it's useful for a "save scrollback to file" feature.

```ts
const term = new Terminal();
const serializeAddon = new SerializeAddon();
term.loadAddon(serializeAddon);
const snapshot: string = serializeAddon.serialize();
```

### Disposal

Call `term.dispose()` when unmounting; addons attached via `loadAddon` are cleaned up automatically. Forgetting `dispose()` leaks event listeners and (with WebGL) GPU contexts.

## Renderer-adapter contract (RelayTerm)

Each `packages/terminal-<name>/` package exports:
- `mount(container: HTMLElement, options): RendererHandle`
- `RendererHandle.write(bytes: Uint8Array): Promise<void>` (resolves on parse-completion)
- `RendererHandle.resize(cols: number, rows: number): void`
- `RendererHandle.onInput(callback: (bytes: Uint8Array) => void): () => void`
- `RendererHandle.dispose(): void`

The architectural rule says renderers don't own state — implementations MUST NOT persist anything across `dispose`/`mount` cycles.

## Default tooling

| Task | Command |
|---|---|
| Type-check the package | `pnpm --filter terminal-xterm check` |
| Build | `pnpm --filter terminal-xterm build` |

<!-- agentic-init: curated above this line -->

## Project-specific patterns

- xterm.js is isolated in `packages/terminal-xterm`. `packages/terminal-core` MUST NOT import `@xterm/*`; the protocol and session client stay renderer-neutral.
- xterm's stylesheet is exposed via the `@relayterm/terminal-xterm/styles` subpath import, not the bare package entry. JS tree-shaking is preserved by the package's `sideEffects: ["./src/styles.ts", "**/*.css"]` declaration; xterm.css still rides into the production CSS bundle as a documented compromise.
- The `xtermOnly` field on `XtermRendererOptions` is an adapter-local escape hatch for xterm-only knobs and MUST NOT be promoted to RelayTerm's persisted terminal-preference model — that surface stays renderer-neutral.
- The renderer adapter MUST NOT log, echo, or include raw terminal input bytes in any error, event, or debug output. Listener errors are swallowed inside the fanout for the same reason.
