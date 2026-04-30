<script lang="ts">
  /**
   * Dev-only launcher that pairs `POST /api/v1/terminal-sessions` with
   * the existing `XtermLiveTerminalLab`. The workbench owns three jobs:
   *
   *  1. Collect a `server_profile_id` (and optional cols/rows) from the
   *     operator. Manual entry is intentional — building host/profile/
   *     identity CRUD UI is explicitly out of scope for this slice.
   *  2. POST the create-session request via the typed helper in
   *     `lib/api/terminalSessions.ts`. Validation runs client-side
   *     before any network round-trip.
   *  3. Hand the returned session id to `XtermLiveTerminalLab` and ask
   *     it to auto-attach. The lab is remounted via `{#key launchId}`
   *     so each create gets a fresh client/renderer pair — the lab's
   *     internal state never leaks across launches.
   *
   * Scope contracts re-asserted here:
   *  - Gated behind `import.meta.env.DEV` at the call site
   *    (`App.svelte`). The production bundle drops this component the
   *    same way it drops the bare lab — see App.svelte for the pin.
   *  - Status text never echoes wire-body strings. Errors collapse via
   *    `describeCreateError` to a static `code/status` summary; the
   *    backend's safe `message` field is intentionally dropped at the
   *    formatter boundary. The `created` state DOES include the new
   *    session id (a Postgres UUID — operator-visible by design, not
   *    secret), the wire `status`, and `pty_live`. Those three fields
   *    are diagnostic outputs the operator needs to confirm the row was
   *    created and a live PTY is bound; they do NOT come from the
   *    `error` envelope and are not under the redaction rule.
   *  - The launcher does NOT log the request body or the response
   *    object. The lab itself follows the redaction rules pinned by
   *    `labLog.test.ts`; this component's `status` line is the only
   *    visible trace of a create.
   *
   * Out of scope (deferred): host/profile/identity CRUD UI, listing
   * existing sessions, persistent-reconnect/resume, alternate renderers,
   * mobile/Tauri shell integration, real auth UI.
   */
  import {
    CELL_GRID_MAX,
    CELL_GRID_MIN,
    type CreateTerminalSessionResponse,
    createTerminalSession,
    describeCreateError,
  } from "../api/terminalSessions";
  import XtermLiveTerminalLab from "./XtermLiveTerminalLab.svelte";

  type LauncherState =
    | { kind: "idle" }
    | { kind: "creating" }
    | {
        kind: "created";
        session: CreateTerminalSessionResponse;
        /** Monotonic counter so `{#key}` remounts the lab on every create. */
        launchId: number;
      }
    | { kind: "error"; summary: string };

  let serverProfileId = $state("");
  let cols = $state(80);
  let rows = $state(24);
  let launcher = $state<LauncherState>({ kind: "idle" });
  // Plain `let`, not `$state`: this counter is only read inside `launch()`
  // to compute `launcher.launchId` and never appears in the template, so
  // it does not need to be reactive.
  let launchSeq = 0;

  function disabled(): boolean {
    if (launcher.kind === "creating") return true;
    return serverProfileId.trim().length === 0;
  }

  async function launch() {
    if (launcher.kind === "creating") return;
    launcher = { kind: "creating" };
    const result = await createTerminalSession({
      server_profile_id: serverProfileId,
      cols,
      rows,
    });
    if (!result.ok) {
      launcher = { kind: "error", summary: describeCreateError(result.error) };
      return;
    }
    launchSeq += 1;
    launcher = { kind: "created", session: result.session, launchId: launchSeq };
  }

  function reset() {
    launcher = { kind: "idle" };
  }

  function statusText(s: LauncherState): string {
    switch (s.kind) {
      case "idle":
        return "no session created yet";
      case "creating":
        return "creating session…";
      case "created":
        return `created session ${s.session.id} (status=${s.session.status}, pty_live=${s.session.pty_live})`;
      case "error":
        return s.summary;
    }
  }

  function statusTone(s: LauncherState): string {
    switch (s.kind) {
      case "idle":
        return "text-zinc-400";
      case "creating":
        return "text-sky-300";
      case "created":
        return "text-emerald-300";
      case "error":
        return "text-rose-300";
    }
  }
</script>

<section class="rounded-md border border-emerald-700/60 bg-emerald-950/20 p-4 text-sm">
  <header class="flex items-baseline justify-between">
    <h2 class="text-base font-semibold text-emerald-200">
      Dev Terminal Workbench
    </h2>
    <span class="font-mono text-xs text-emerald-400">
      dev-only diagnostic — not the product UI
    </span>
  </header>
  <p class="mt-1 text-xs text-emerald-200/80">
    Creates a live terminal session via
    <code>POST /api/v1/terminal-sessions</code> and hands the returned id
    to <code>XtermLiveTerminalLab</code> for auto-attach. Manual
    <code>server_profile_id</code> entry — host/profile CRUD UI is future work.
  </p>

  <div class="mt-3 grid grid-cols-1 gap-2 sm:grid-cols-3">
    <label class="flex flex-col gap-1">
      <span class="text-xs text-zinc-400">server_profile_id</span>
      <input
        type="text"
        class="rounded-sm border border-zinc-700 bg-zinc-900 px-2 py-1 font-mono"
        placeholder="uuid"
        bind:value={serverProfileId}
      />
    </label>
    <label class="flex flex-col gap-1">
      <span class="text-xs text-zinc-400">cols ({CELL_GRID_MIN}–{CELL_GRID_MAX})</span>
      <input
        type="number"
        min={CELL_GRID_MIN}
        max={CELL_GRID_MAX}
        class="rounded-sm border border-zinc-700 bg-zinc-900 px-2 py-1 font-mono"
        bind:value={cols}
      />
    </label>
    <label class="flex flex-col gap-1">
      <span class="text-xs text-zinc-400">rows ({CELL_GRID_MIN}–{CELL_GRID_MAX})</span>
      <input
        type="number"
        min={CELL_GRID_MIN}
        max={CELL_GRID_MAX}
        class="rounded-sm border border-zinc-700 bg-zinc-900 px-2 py-1 font-mono"
        bind:value={rows}
      />
    </label>
  </div>

  <div class="mt-3 flex flex-wrap gap-2">
    <button
      type="button"
      class="rounded-sm bg-emerald-700 px-3 py-1 text-xs hover:bg-emerald-600 disabled:opacity-50"
      onclick={launch}
      disabled={disabled()}
    >
      create live session
    </button>
    <button
      type="button"
      class="rounded-sm bg-zinc-700 px-3 py-1 text-xs hover:bg-zinc-600 disabled:opacity-50"
      onclick={reset}
      disabled={launcher.kind === "creating" || launcher.kind === "idle"}
    >
      clear status
    </button>
  </div>

  <div class="mt-3 text-xs">
    status: <span class={`font-mono ${statusTone(launcher)}`}>{statusText(launcher)}</span>
  </div>
</section>

{#if launcher.kind === "created"}
  {#key launcher.launchId}
    <XtermLiveTerminalLab
      initialSessionId={launcher.session.id}
      initialCols={launcher.session.cols}
      initialRows={launcher.session.rows}
      autoConnect={true}
    />
  {/key}
{:else}
  <XtermLiveTerminalLab />
{/if}
