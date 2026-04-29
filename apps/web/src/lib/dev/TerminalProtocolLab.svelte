<script lang="ts">
  /**
   * Diagnostic UI for the @relayterm/terminal-core protocol/client layer.
   *
   * This is NOT the production terminal UI. It is a developer-only lab
   * that exercises the wire protocol against a live backend WebSocket so
   * we can see attach/detach/resize/ping/error frames flow without
   * pulling in xterm.js or any renderer. The renderer-neutral rule means
   * the renderer integration is a separate slice.
   */
  import { onDestroy } from "svelte";
  import {
    TerminalSessionClient,
    WebSocketTerminalTransport,
    type ServerMsg,
    type TerminalClientError,
    type TerminalSessionState,
  } from "@relayterm/terminal-core";

  interface LogLine {
    id: number;
    direction: "in" | "out" | "info" | "error";
    text: string;
  }

  let sessionId = $state("");
  let cols = $state(80);
  let rows = $state(24);
  let fakeInput = $state("echo hello");
  let clientState = $state<TerminalSessionState>("idle");
  let log = $state<LogLine[]>([]);
  let nextId = 0;
  let client: TerminalSessionClient | null = null;

  function append(direction: LogLine["direction"], text: string) {
    log = [...log.slice(-199), { id: nextId++, direction, text }];
  }

  function describeServerMsg(msg: ServerMsg): string {
    switch (msg.type) {
      case "session_attached":
        // `msg.message` is the backend's pinned static stub string today.
        // If the contract ever lets it carry dynamic content this becomes
        // a logging channel — revisit then.
        return `session_attached (${msg.status}): ${msg.message}`;
      case "ack":
        return `ack ${msg.kind}`;
      case "output":
        // The output frame is reserved for the future PTY slice; in the
        // current stub it is never emitted. We deliberately do NOT format
        // `data` here so the diagnostic UI doesn't accidentally render
        // arbitrary terminal escape sequences in plain HTML.
        return `output seq=${msg.seq} (${msg.data.length} bytes)`;
      case "session_detached":
        return `session_detached attachment=${msg.attachment_id}`;
      case "session_closed":
        return `session_closed`;
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
    const transport = new WebSocketTerminalTransport();
    const next = new TerminalSessionClient({ transport });
    next.on("state_change", (s) => {
      clientState = s;
      append("info", `state → ${s}`);
    });
    next.on("attached", (m) => append("in", describeServerMsg(m)));
    next.on("detached", (m) => append("in", describeServerMsg(m)));
    next.on("closed", (m) => append("in", describeServerMsg(m)));
    next.on("ack", (m) => append("in", describeServerMsg(m)));
    next.on("pong", (m) => append("in", describeServerMsg(m)));
    next.on("output", (m) => append("in", describeServerMsg(m)));
    next.on("replay_window_lost", (m) => append("in", describeServerMsg(m)));
    next.on("input_rejected_or_stubbed", (r) =>
      append("info", `${r.attempted} rejected: ${r.reason}`),
    );
    next.on("error", (e) => append("error", describeError(e)));
    client = next;
    try {
      await next.attach({
        url: buildWsUrl(sessionId.trim()),
        sessionId: sessionId.trim(),
        clientId: "protocol-lab",
      });
      append("out", "attach frame sent");
    } catch (err) {
      append("error", `attach failed: ${err instanceof Error ? err.message : String(err)}`);
      next.dispose();
      client = null;
    }
  }

  function disconnect() {
    if (!client) return;
    client.dispose();
    client = null;
    clientState = "idle";
    append("info", "client disposed");
  }

  function ping() {
    client?.sendPing();
    append("out", "ping");
  }

  function resize() {
    client?.sendResize(cols, rows);
    append("out", `resize cols=${cols} rows=${rows}`);
  }

  function sendInput() {
    client?.sendInput(fakeInput);
    // Deliberately do not echo `fakeInput` into the log. The whole point
    // of the redaction rule is that the diagnostic UI also doesn't
    // shoulder-surf user input. The byte length is enough.
    append("out", `input (${fakeInput.length} bytes)`);
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
    client?.dispose();
    client = null;
  });
</script>

<section class="rounded-md border border-zinc-800 p-4 text-sm">
  <header class="flex items-baseline justify-between">
    <h2 class="text-base font-semibold">Terminal Protocol Lab</h2>
    <span class="font-mono text-xs text-zinc-400">
      diagnostic — not the terminal UI
    </span>
  </header>
  <p class="mt-1 text-xs text-zinc-400">
    Drives <code>@relayterm/terminal-core</code> against
    <code>/api/v1/terminal-sessions/:id/ws</code>. No renderer attached.
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

  <label class="mt-2 flex flex-col gap-1">
    <span class="text-xs text-zinc-400">
      fake input (sent as <code>input</code>; backend currently rejects with
      <code>pty_not_implemented</code>)
    </span>
    <input
      type="text"
      class="rounded-sm border border-zinc-700 bg-zinc-900 px-2 py-1 font-mono"
      bind:value={fakeInput}
    />
  </label>

  <div class="mt-3 flex flex-wrap gap-2">
    <button
      type="button"
      class="rounded-sm bg-emerald-700 px-3 py-1 text-xs hover:bg-emerald-600 disabled:opacity-50"
      onclick={connect}
      disabled={clientState !== "idle"}
    >
      connect + attach
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
      onclick={resize}
      disabled={clientState !== "attached"}
    >
      resize
    </button>
    <button
      type="button"
      class="rounded-sm bg-zinc-700 px-3 py-1 text-xs hover:bg-zinc-600 disabled:opacity-50"
      onclick={sendInput}
      disabled={clientState !== "attached"}
    >
      input
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
      dispose client
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
    class="mt-2 max-h-72 overflow-auto rounded-sm border border-zinc-800 bg-zinc-950 p-2 font-mono text-xs"
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
