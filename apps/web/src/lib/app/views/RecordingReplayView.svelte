<script lang="ts">
  /**
   * Read-only durable terminal recording replay viewer.
   *
   * Scope: replay-only. No live SSH session, no input path, no
   * WebSocket attach. The viewer:
   *  1. Loads `/recording/metadata` for the addressed session.
   *  2. If `has_recording` is `true`, pages chunks via
   *     `/recording/chunks?from_seq=...&limit=...` and writes each
   *     decoded chunk into a read-only `XtermRenderer`.
   *  3. Loads markers (one page) for context but renders them as
   *     metadata only — never as terminal output.
   *
   * Honesty / redaction rules (load-bearing):
   *  - Banner copy and the "About replay" panel state plainly that the
   *    viewer is recorded output only, that input was NOT recorded,
   *    that the live SSH session cannot be resumed from a recording,
   *    and that backend-restart recovery is not implemented yet.
   *  - Decoded chunk bytes go directly to xterm via `renderer.write`.
   *    They are NEVER stashed in any reactive `$state`, never
   *    persisted to localStorage / sessionStorage, never logged, and
   *    never appear in `Error.message` / `data-*` attributes.
   *  - Marker `payload` JSON is rendered as a key/value preview only
   *    (truncated to a short snippet); the writer contract pins
   *    payloads as metadata.
   *  - Errors render through {@link describeRecordingError} /
   *    {@link describeDecodeFailure} — both are pure functions of the
   *    discriminant + status + code.
   *  - There is NO `onInput` subscription on the renderer; xterm is
   *    constructed with `disableStdin: true` so a stray DOM-level
   *    keystroke cannot reach the live session client (which doesn't
   *    exist here anyway).
   */
  import { onDestroy, onMount } from "svelte";
  import { XtermRenderer } from "@relayterm/terminal-xterm";
  import "@relayterm/terminal-xterm/styles";
  import {
    decodeRecordingChunk,
    describeDecodeFailure,
    describeRecordingError,
    getTerminalRecordingChunks,
    getTerminalRecordingMarkers,
    getTerminalRecordingMetadata,
    isSupportedChunk,
    type TerminalRecordingChunk,
    type TerminalRecordingMarker,
    type TerminalRecordingMetadata,
  } from "../../api/terminalRecordings.js";
  import {
    loadTerminalSettings,
    settingsToRendererOptions,
  } from "../settings/terminalSettings.js";

  interface Props {
    sessionId: string;
    /** Operator-facing label (usually the originating profile name). */
    profileLabel?: string;
    /** Returns to the Sessions list — the shell wires this to clear
     * `activeReplaySessionId`. */
    onExit?: () => void;
  }

  let { sessionId, profileLabel, onExit }: Props = $props();

  type LoadStatus =
    | { kind: "idle" }
    | { kind: "loading_metadata" }
    | { kind: "loading_chunks"; chunksWritten: number }
    | { kind: "ready"; chunksWritten: number }
    | { kind: "empty" }
    | { kind: "error"; summary: string }
    | { kind: "decode_warning"; summary: string; chunksWritten: number };

  let status = $state<LoadStatus>({ kind: "idle" });
  let metadata = $state<TerminalRecordingMetadata | null>(null);
  let markers = $state<TerminalRecordingMarker[]>([]);
  let mountTarget: HTMLDivElement | null = null;
  let renderer: XtermRenderer | null = null;
  /**
   * Bumped on EVERY mount + every refresh + dispose so an in-flight
   * chunk page load from a superseded run cannot reach into a renderer
   * that has been disposed or replaced.
   */
  let generation = 0;

  /** Page size used when pulling chunks. The backend clamps to
   * `1..=1024`; 256 mirrors the route default and is small enough to
   * keep response bodies under a few MB even on very long recordings. */
  const CHUNK_PAGE_SIZE = 256;
  /** Marker page size. The viewer only loads ONE page of markers — a
   * future refresh-button could pull more, but the viewer only renders
   * a brief metadata strip today. */
  const MARKER_PAGE_SIZE = 256;

  function bumpGeneration(): number {
    generation += 1;
    return generation;
  }

  /**
   * Construct a read-only `XtermRenderer`. The `xtermOnly` escape hatch
   * forces `disableStdin: true` so DOM keystrokes never produce wire
   * input (we don't subscribe to `onInput` either; the disabled-stdin
   * flag is belt-and-suspenders).
   */
  function buildReadOnlyRenderer(): XtermRenderer {
    const settings = loadTerminalSettings();
    const opts = settingsToRendererOptions(settings);
    return new XtermRenderer({
      ...opts,
      cursorBlink: false,
      xtermOnly: { disableStdin: true },
    });
  }

  /**
   * Drive the metadata → chunks → markers load sequence. Owns the
   * generation gating so a refresh while a previous load is in flight
   * cannot interleave writes into the renderer.
   */
  async function load() {
    if (!mountTarget) return;
    const myGen = bumpGeneration();

    // Fresh renderer per load — a refresh tears the prior one down so
    // the previous viewport doesn't bleed into the new pass.
    renderer?.dispose();
    renderer = null;
    const r = buildReadOnlyRenderer();
    r.mount(mountTarget);
    if (myGen !== generation) {
      r.dispose();
      return;
    }
    renderer = r;

    status = { kind: "loading_metadata" };
    metadata = null;
    markers = [];

    const metaResult = await getTerminalRecordingMetadata(sessionId);
    if (myGen !== generation) return;
    if (!metaResult.ok) {
      status = { kind: "error", summary: describeRecordingError(metaResult.error) };
      return;
    }
    metadata = metaResult.data;

    if (!metaResult.data.has_recording) {
      status = { kind: "empty" };
      return;
    }

    // Page chunks until empty. The page size and `from_seq` cursor
    // mirror the backend's `?from_seq=&limit=` contract.
    let written = 0;
    let cursor =
      typeof metaResult.data.first_seq === "number"
        ? metaResult.data.first_seq
        : 1;
    let firstDecodeWarning: string | null = null;
    let loopGuard = 0;
    const HARD_LOOP_CAP = 4096; // 4096 * 256 = ~1M chunks, vastly larger than any real recording
    status = { kind: "loading_chunks", chunksWritten: 0 };

    while (true) {
      loopGuard += 1;
      if (loopGuard > HARD_LOOP_CAP) {
        firstDecodeWarning ??=
          "Recording is unexpectedly large; stopped loading additional chunks.";
        break;
      }
      const chunkResult = await getTerminalRecordingChunks(sessionId, {
        fromSeq: cursor,
        limit: CHUNK_PAGE_SIZE,
      });
      if (myGen !== generation) return;
      if (!chunkResult.ok) {
        status = {
          kind: "error",
          summary: describeRecordingError(chunkResult.error),
        };
        return;
      }
      const page = chunkResult.data;
      if (page.length === 0) break;

      for (const chunk of page) {
        if (myGen !== generation) return;
        if (!isSupportedChunk(chunk)) {
          firstDecodeWarning ??= describeDecodeFailure(
            chunk.encryption !== "none"
              ? "unsupported_encryption"
              : "unsupported_compression",
          );
          continue;
        }
        const decoded = decodeRecordingChunk(chunk);
        if (!decoded.ok) {
          firstDecodeWarning ??= describeDecodeFailure(decoded.reason);
          continue;
        }
        // Write a fresh Uint8Array view — the decoded buffer is owned
        // by the function call and dropped after this loop iteration.
        // xterm copies the bytes synchronously into its parser queue.
        renderer?.write(decoded.bytes);
        written += 1;
      }
      status = { kind: "loading_chunks", chunksWritten: written };

      // Advance cursor past the highest seq_end on this page. If the
      // backend ever returns rows in non-monotonic order we still
      // make forward progress because `seq_end >= seq_start`.
      const lastSeqEnd = page.reduce((acc, c) => Math.max(acc, c.seq_end), cursor);
      cursor = lastSeqEnd + 1;
      if (page.length < CHUNK_PAGE_SIZE) break;
    }

    if (myGen !== generation) return;

    // Load one page of markers — best-effort. A failure here is NOT
    // promoted to a viewer error; the chunks already played and the
    // markers strip is metadata-only.
    const markerResult = await getTerminalRecordingMarkers(sessionId, {
      limit: MARKER_PAGE_SIZE,
    });
    if (myGen !== generation) return;
    if (markerResult.ok) {
      markers = markerResult.data;
    }

    if (firstDecodeWarning) {
      status = {
        kind: "decode_warning",
        summary: firstDecodeWarning,
        chunksWritten: written,
      };
    } else {
      status = { kind: "ready", chunksWritten: written };
    }
  }

  function refreshClicked() {
    void load();
  }

  onMount(() => {
    void load();
  });

  onDestroy(() => {
    bumpGeneration();
    renderer?.dispose();
    renderer = null;
  });

  /**
   * Render a marker payload as a short, safe preview. The payload is
   * metadata-only by writer contract — counts, dimensions, reason
   * codes — but we still cap the rendered string length to keep a bug
   * at the writer layer from blowing up the markers strip.
   */
  function previewPayload(payload: unknown): string {
    if (payload === null || payload === undefined) return "";
    try {
      const s = JSON.stringify(payload);
      if (s.length > 120) return `${s.slice(0, 117)}…`;
      return s;
    } catch {
      return "";
    }
  }

  /**
   * Short hex-prefix used in the header to disambiguate sessions
   * without printing the entire UUID.
   */
  function shortId(id: string): string {
    if (id.length <= 8) return id;
    return id.slice(0, 8);
  }

  function chunksWrittenLabel(): string {
    if (status.kind === "loading_chunks") return `${status.chunksWritten} chunks loaded so far…`;
    if (status.kind === "ready") return `${status.chunksWritten} chunks replayed.`;
    if (status.kind === "decode_warning") return `${status.chunksWritten} chunks replayed.`;
    return "";
  }
</script>

<section
  class="flex flex-col gap-4"
  data-testid="recording-replay-view"
  data-session-id={sessionId}
  data-status={status.kind}
>
  <header class="flex flex-wrap items-baseline justify-between gap-3">
    <div class="flex flex-col gap-0.5">
      <h2 class="text-lg font-semibold tracking-tight text-zinc-100">
        Recording replay
      </h2>
      <p class="text-xs text-zinc-500">
        {profileLabel ? `${profileLabel} · ` : ""}<span
          class="font-mono"
          title={sessionId}>{shortId(sessionId)}</span>
      </p>
    </div>
    <div class="flex flex-wrap gap-2">
      <button
        type="button"
        class="rounded-md border border-zinc-700 bg-zinc-800 px-3 py-1 text-xs text-zinc-100 transition hover:border-zinc-600 hover:bg-zinc-700 disabled:cursor-not-allowed disabled:opacity-50"
        onclick={refreshClicked}
        disabled={status.kind === "loading_metadata" || status.kind === "loading_chunks"}
        data-testid="recording-replay-refresh"
        title="Reload metadata, chunks, and markers from the backend"
      >
        Reload recording
      </button>
      {#if onExit}
        <button
          type="button"
          class="rounded-md border border-zinc-700 bg-zinc-900 px-3 py-1 text-xs text-zinc-300 transition hover:border-zinc-600 hover:bg-zinc-800"
          onclick={onExit}
          data-testid="recording-replay-back"
        >
          Back to sessions
        </button>
      {/if}
    </div>
  </header>

  <p
    class="rounded-md border border-amber-700/60 bg-amber-950/30 px-3 py-2 text-xs text-amber-100"
    data-testid="recording-replay-banner"
    role="note"
  >
    <strong class="font-semibold uppercase tracking-wide">Replay only.</strong>
    This is recorded terminal output. It is not connected to a live SSH
    session. Input was not recorded; the live SSH session cannot be
    resumed from a recording. Backend-restart recovery is not
    implemented yet, so a recording may end mid-output if the backend
    restarted while the session was alive.
  </p>

  {#if status.kind === "loading_metadata"}
    <p class="text-sm text-zinc-400" data-testid="recording-replay-loading">
      Loading recording metadata…
    </p>
  {:else if status.kind === "loading_chunks"}
    <p
      class="text-sm text-zinc-400"
      data-testid="recording-replay-loading-chunks"
    >
      Loading recorded output… {chunksWrittenLabel()}
    </p>
  {:else if status.kind === "error"}
    <p
      class="rounded-md border border-rose-900/40 bg-rose-950/20 px-3 py-2 text-sm text-rose-200/80"
      data-testid="recording-replay-error"
    >
      {status.summary}
    </p>
  {:else if status.kind === "empty"}
    <article
      class="flex flex-col gap-2 rounded-lg border border-zinc-800 bg-zinc-950/40 p-6"
      data-testid="recording-replay-empty"
    >
      <h3 class="text-sm font-semibold text-zinc-200">No recording available</h3>
      <p class="text-sm text-zinc-400">
        This session has no durable recording. Recording captures PTY
        output asynchronously while the session is alive — sessions
        that ran before recording was enabled, or sessions whose
        recording was purged, will not have replay material.
      </p>
    </article>
  {:else if status.kind === "decode_warning"}
    <p
      class="rounded-md border border-amber-900/40 bg-amber-950/20 px-3 py-2 text-xs text-amber-200/80"
      data-testid="recording-replay-decode-warning"
    >
      {status.summary} {chunksWrittenLabel()}
    </p>
  {:else if status.kind === "ready"}
    <p
      class="rounded-md border border-emerald-900/40 bg-emerald-950/20 px-3 py-2 text-xs text-emerald-200/80"
      data-testid="recording-replay-complete"
    >
      Replay complete. {chunksWrittenLabel()}
    </p>
  {/if}

  {#if metadata}
    <dl
      class="grid grid-cols-2 gap-x-4 gap-y-1 text-xs text-zinc-400 sm:grid-cols-4"
      data-testid="recording-replay-metadata"
    >
      <div class="flex flex-col">
        <dt class="text-zinc-500">chunks</dt>
        <dd class="font-mono text-zinc-200">{metadata.chunk_count}</dd>
      </div>
      <div class="flex flex-col">
        <dt class="text-zinc-500">markers</dt>
        <dd class="font-mono text-zinc-200">{metadata.marker_count}</dd>
      </div>
      <div class="flex flex-col">
        <dt class="text-zinc-500">first seq</dt>
        <dd class="font-mono text-zinc-200">{metadata.first_seq ?? "—"}</dd>
      </div>
      <div class="flex flex-col">
        <dt class="text-zinc-500">last seq</dt>
        <dd class="font-mono text-zinc-200">{metadata.last_seq ?? "—"}</dd>
      </div>
      <div class="flex flex-col">
        <dt class="text-zinc-500">first recorded at</dt>
        <dd>
          {#if metadata.first_recorded_at}
            <time
              class="font-mono text-zinc-200"
              datetime={metadata.first_recorded_at}>{metadata.first_recorded_at}</time>
          {:else}
            <span class="font-mono text-zinc-200">—</span>
          {/if}
        </dd>
      </div>
      <div class="flex flex-col">
        <dt class="text-zinc-500">last recorded at</dt>
        <dd>
          {#if metadata.last_recorded_at}
            <time
              class="font-mono text-zinc-200"
              datetime={metadata.last_recorded_at}>{metadata.last_recorded_at}</time>
          {:else}
            <span class="font-mono text-zinc-200">—</span>
          {/if}
        </dd>
      </div>
    </dl>
  {/if}

  <!--
    The viewport is rendered REGARDLESS of `status.kind` so the renderer
    can mount immediately and writes during `loading_chunks` are visible
    as they stream. `bind:this` runs before `onMount`, which is what
    `load()` depends on.
  -->
  <div
    bind:this={mountTarget}
    class="h-[28rem] overflow-hidden rounded-md border border-zinc-800 bg-black"
    data-testid="recording-replay-viewport"
  ></div>

  {#if markers.length > 0}
    <details
      class="rounded-md border border-zinc-800 bg-zinc-950/40 px-3 py-2"
      data-testid="recording-replay-markers"
    >
      <summary class="cursor-pointer text-xs text-zinc-300">
        Markers ({markers.length})
      </summary>
      <ul class="mt-2 flex flex-col gap-1 text-[11px] text-zinc-400">
        {#each markers as marker (`${marker.seq}-${marker.kind}-${marker.created_at}`)}
          <li
            class="flex flex-wrap items-baseline gap-x-3 gap-y-0.5"
            data-testid="recording-replay-marker"
            data-marker-kind={marker.kind}
          >
            <span class="font-mono text-zinc-200">seq={marker.seq}</span>
            <span class="font-mono text-zinc-200">{marker.kind}</span>
            <time class="font-mono text-zinc-500" datetime={marker.created_at}>
              {marker.created_at}
            </time>
            {#if previewPayload(marker.payload)}
              <span class="font-mono text-zinc-500">
                {previewPayload(marker.payload)}
              </span>
            {/if}
          </li>
        {/each}
      </ul>
    </details>
  {/if}

  <p
    class="rounded-md border border-zinc-800 bg-zinc-950/40 px-3 py-2 text-[11px] text-zinc-500"
    data-testid="recording-replay-about"
  >
    Recording may contain sensitive terminal output (anything the
    operator's shell printed: env-var dumps, decrypted file contents,
    API tokens echoed by tooling). The replay viewer is output-only —
    keyboard input is disabled and no input is sent anywhere. Recording
    bytes are streamed directly into the terminal viewport and are not
    persisted in browser storage.
  </p>
</section>
