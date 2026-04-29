<script lang="ts">
  /**
   * Dev-only lab for the @relayterm/terminal-xterm baseline renderer.
   *
   * This is NOT the production terminal UI. It exists to prove the
   * `TerminalRenderer` interface in `terminal-core` can drive xterm.js
   * end-to-end — keystrokes flow renderer → core client → backend, and
   * any backend-supplied `output` bytes flow back into the renderer
   * via `write`. Everything is scoped behind `import.meta.env.DEV`;
   * the production bundle drops this file via dead-code elimination.
   *
   * Backend PTY streaming is not implemented yet, so the renderer is
   * primed with a local banner. Inputs reach the backend and come back
   * as the existing `pty_not_implemented` rejection event — the lab
   * surfaces that explicitly so nobody mistakes this slice for a live
   * SSH terminal.
   */
  import { onDestroy } from "svelte";
  import {
    TerminalSessionClient,
    WebSocketTerminalTransport,
    type ServerMsg,
    type TerminalClientError,
    type TerminalSessionState,
  } from "@relayterm/terminal-core";
  import { XtermRenderer } from "@relayterm/terminal-xterm";
  import "@relayterm/terminal-xterm/styles";

  interface LogLine {
    id: number;
    direction: "in" | "out" | "info" | "error";
    text: string;
  }

  let sessionId = $state("");
  let cols = $state(80);
  let rows = $state(24);
  let clientState = $state<TerminalSessionState>("idle");
  let log = $state<LogLine[]>([]);
  let nextId = 0;
  let client: TerminalSessionClient | null = null;
  let renderer: XtermRenderer | null = null;
  let unsubInput: (() => void) | null = null;
  let unsubResize: (() => void) | null = null;
  let mountTarget: HTMLDivElement | null = null;

  const BANNER =
    "RelayTerm xterm baseline attached. PTY streaming is not implemented yet.\r\n" +
    "Keystrokes here are sent over the protocol but the backend currently rejects them with pty_not_implemented.\r\n";

  function append(direction: LogLine["direction"], text: string) {
    log = [...log.slice(-199), { id: nextId++, direction, text }];
  }

  function describeServerMsg(msg: ServerMsg): string {
    switch (msg.type) {
      case "session_attached":
        return `session_attached (${msg.status}): ${msg.message}`;
      case "ack":
        return `ack ${msg.kind}`;
      case "output":
        // Reserved for the future PTY slice. We do NOT format `data`
        // into the diagnostic log to avoid rendering escape sequences
        // into HTML; the renderer is the only consumer of those bytes.
        return `output seq=${msg.seq} (${msg.data.length} bytes)`;
      case "session_detached":
        return `session_detached attachment=${msg.attachment_id}`;
      case "session_closed":
        return "session_closed";
      case "replay_window_lost":
        return "replay_window_lost (reserved for future slice)";
      case "error":
        return `error ${msg.code}: ${msg.message}`;
      case "pong":
        return "pong";
    }
  }

  function describeError(err: TerminalClientError): string {
    if (err.kind === "server_error" && err.code) {
      return `server_error code=${err.code} message="${err.message}"`;
    }
    return `${err.kind}: ${err.message}`;
  }

  function buildWsUrl(id: string): string {
    const proto = window.location.protocol === "https:" ? "wss:" : "ws:";
    const path = `/api/v1/terminal-sessions/${encodeURIComponent(id)}/ws`;
    return `${proto}//${window.location.host}${path}`;
  }

  async function connect() {
    if (client) {
      append("info", "already connected; disconnect first");
      return;
    }
    if (!sessionId.trim()) {
      append("error", "session id is required");
      return;
    }
    if (!mountTarget) {
      append("error", "renderer mount point not yet available");
      return;
    }

    const r = new XtermRenderer({
      fontFamily:
        'ui-monospace, "JetBrains Mono", "Fira Code", "SFMono-Regular", Consolas, "Liberation Mono", Menlo, monospace',
      fontSize: 13,
      cursorBlink: true,
      cursorStyle: "block",
      scrollbackLines: 2000,
      theme: {
        background: "#0a0a0a",
        foreground: "#e4e4e7",
        cursor: "#e4e4e7",
      },
    });
    r.mount(mountTarget);
    r.write(BANNER);
    r.focus();
    renderer = r;

    const transport = new WebSocketTerminalTransport();
    const next = new TerminalSessionClient({ transport });
    next.on("state_change", (s) => {
      clientState = s;
      append("info", `state → ${s}`);
    });
    next.on("attached", (m) => {
      append("in", describeServerMsg(m));
      // Send the renderer's current cell-grid to the backend so the
      // server-side resize bookkeeping matches what the client sees.
      // We drive `renderer.resize` only — xterm fires `onResize`
      // synchronously inside `Terminal.resize`, and the subscriber
      // below is the single place that calls `client.sendResize`.
      // Calling `client.sendResize` directly from here would emit a
      // duplicate frame.
      r.resize(cols, rows);
    });
    next.on("detached", (m) => append("in", describeServerMsg(m)));
    next.on("closed", (m) => append("in", describeServerMsg(m)));
    next.on("ack", (m) => append("in", describeServerMsg(m)));
    next.on("pong", (m) => append("in", describeServerMsg(m)));
    next.on("output", (m) => {
      append("in", describeServerMsg(m));
      // Pipe backend bytes into the renderer once a PTY actually fires.
      // Today this is unreachable — the backend doesn't emit `output`
      // — but wiring it up means the future slice doesn't need to
      // touch this file.
      r.write(m.data);
    });
    next.on("replay_window_lost", (m) => append("in", describeServerMsg(m)));
    next.on("input_rejected_or_stubbed", (rej) =>
      append("info", `${rej.attempted} rejected: ${rej.reason}`),
    );
    next.on("error", (e) => append("error", describeError(e)));

    // Renderer → client. We log only the byte length, never the input
    // payload — the redaction rule is enforced inside the adapter and
    // the lab follows it too.
    unsubInput = r.onInput((data) => {
      const len = typeof data === "string" ? data.length : data.byteLength;
      append("out", `input (${len} bytes)`);
      next.sendInput(typeof data === "string" ? data : new TextDecoder().decode(data));
    });
    unsubResize = r.onResize?.((size) => {
      cols = size.cols;
      rows = size.rows;
      next.sendResize(size.cols, size.rows);
      append("out", `renderer resize cols=${size.cols} rows=${size.rows}`);
    }) ?? null;

    client = next;
    try {
      await next.attach({
        url: buildWsUrl(sessionId.trim()),
        sessionId: sessionId.trim(),
        clientId: "xterm-renderer-lab",
      });
      append("out", "attach frame sent");
    } catch (err) {
      append(
        "error",
        `attach failed: ${err instanceof Error ? err.message : String(err)}`,
      );
      teardown();
    }
  }

  function teardown() {
    unsubInput?.();
    unsubResize?.();
    unsubInput = null;
    unsubResize = null;
    client?.dispose();
    client = null;
    renderer?.dispose();
    renderer = null;
    clientState = "idle";
  }

  function disconnect() {
    teardown();
    append("info", "client + renderer disposed");
  }

  function ping() {
    client?.sendPing();
    append("out", "ping");
  }

  function applyResize() {
    // Renderer resize fires xterm's `onResize` synchronously, which the
    // subscriber translates into `client.sendResize`. We don't fire
    // `client.sendResize` here too — that would double the wire frame.
    // If the renderer isn't mounted yet (no client either), there is
    // nothing to send.
    renderer?.resize(cols, rows);
    append("out", `manual resize cols=${cols} rows=${rows}`);
  }

  function detach() {
    client?.detach();
    append("out", "detach");
  }

  function closeSession() {
    client?.close();
    append("out", "close");
  }

  function clearLog() {
    log = [];
  }

  onDestroy(() => {
    teardown();
  });
</script>

<section class="rounded-md border border-zinc-800 p-4 text-sm">
  <header class="flex items-baseline justify-between">
    <h2 class="text-base font-semibold">Xterm Renderer Lab</h2>
    <span class="font-mono text-xs text-zinc-400">
      diagnostic — baseline renderer
    </span>
  </header>
  <p class="mt-1 text-xs text-zinc-400">
    Mounts <code>@relayterm/terminal-xterm</code> behind the
    <code>TerminalRenderer</code> interface. Backend PTY streaming is not
    implemented; keystrokes route through the protocol and surface as
    <code>pty_not_implemented</code>.
  </p>

  <div class="mt-3 grid grid-cols-1 gap-2 sm:grid-cols-3">
    <label class="flex flex-col gap-1">
      <span class="text-xs text-zinc-400">terminal_session_id</span>
      <input
        type="text"
        class="rounded-sm border border-zinc-700 bg-zinc-900 px-2 py-1 font-mono"
        placeholder="uuid"
        bind:value={sessionId}
      />
    </label>
    <label class="flex flex-col gap-1">
      <span class="text-xs text-zinc-400">cols</span>
      <input
        type="number"
        min="1"
        max="4096"
        class="rounded-sm border border-zinc-700 bg-zinc-900 px-2 py-1 font-mono"
        bind:value={cols}
      />
    </label>
    <label class="flex flex-col gap-1">
      <span class="text-xs text-zinc-400">rows</span>
      <input
        type="number"
        min="1"
        max="4096"
        class="rounded-sm border border-zinc-700 bg-zinc-900 px-2 py-1 font-mono"
        bind:value={rows}
      />
    </label>
  </div>

  <div class="mt-3 flex flex-wrap gap-2">
    <button
      type="button"
      class="rounded-sm bg-emerald-700 px-3 py-1 text-xs hover:bg-emerald-600 disabled:opacity-50"
      onclick={connect}
      disabled={clientState !== "idle"}
    >
      connect + attach + mount renderer
    </button>
    <button
      type="button"
      class="rounded-sm bg-zinc-700 px-3 py-1 text-xs hover:bg-zinc-600 disabled:opacity-50"
      onclick={ping}
      disabled={clientState !== "attached"}
    >
      ping
    </button>
    <button
      type="button"
      class="rounded-sm bg-zinc-700 px-3 py-1 text-xs hover:bg-zinc-600 disabled:opacity-50"
      onclick={applyResize}
      disabled={clientState !== "attached"}
    >
      apply resize
    </button>
    <button
      type="button"
      class="rounded-sm bg-amber-700 px-3 py-1 text-xs hover:bg-amber-600 disabled:opacity-50"
      onclick={detach}
      disabled={clientState !== "attached"}
    >
      detach
    </button>
    <button
      type="button"
      class="rounded-sm bg-rose-700 px-3 py-1 text-xs hover:bg-rose-600 disabled:opacity-50"
      onclick={closeSession}
      disabled={clientState !== "attached"}
    >
      close
    </button>
    <button
      type="button"
      class="rounded-sm bg-zinc-800 px-3 py-1 text-xs hover:bg-zinc-700"
      onclick={disconnect}
    >
      dispose renderer + client
    </button>
    <button
      type="button"
      class="ml-auto rounded-sm bg-zinc-800 px-3 py-1 text-xs hover:bg-zinc-700"
      onclick={clearLog}
    >
      clear log
    </button>
  </div>

  <div class="mt-3 text-xs text-zinc-400">
    state: <span class="font-mono text-zinc-200">{clientState}</span>
  </div>

  <div
    bind:this={mountTarget}
    class="mt-2 h-72 overflow-hidden rounded-sm border border-zinc-800 bg-black"
  ></div>

  <div
    class="mt-2 max-h-48 overflow-auto rounded-sm border border-zinc-800 bg-zinc-950 p-2 font-mono text-xs"
  >
    {#each log as line (line.id)}
      <div
        class:text-emerald-400={line.direction === "in"}
        class:text-sky-300={line.direction === "out"}
        class:text-zinc-400={line.direction === "info"}
        class:text-rose-400={line.direction === "error"}
      >
        <span class="select-none">[{line.direction}]</span>
        {line.text}
      </div>
    {/each}
    {#if log.length === 0}
      <div class="text-zinc-600">no events yet</div>
    {/if}
  </div>
</section>
