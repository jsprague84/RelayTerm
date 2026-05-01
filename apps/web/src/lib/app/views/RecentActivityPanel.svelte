<script lang="ts">
  /**
   * Read-only recent audit activity for the current user.
   *
   * Scope rules (load-bearing):
   *  - Current-user only. There is NO admin / cross-user view.
   *  - The wire payload is always rendered through
   *    `summarizeAuditEvent` and the structured `AuditPayloadSummary`
   *    types in `lib/api/auditEvents.ts` — raw JSON payload is NEVER
   *    surfaced to the DOM.
   *  - No retry storms. The mount fetches once; the operator presses
   *    "Refresh" to re-fetch.
   *  - No polling, no background re-fetch, no cross-view broadcast.
   *  - Errors collapse through `describeLoadError` so transport /
   *    operator detail can not leak into the rendered string.
   */
  import {
    listRecentAuditEvents,
    summarizeAuditEvent,
    type AuditEvent,
  } from "../../api/auditEvents.js";
  import { describeLoadError } from "../../api/apiErrors.js";

  type LoadState =
    | { kind: "idle" }
    | { kind: "loading" }
    | { kind: "ready"; events: AuditEvent[] }
    | { kind: "error"; summary: string };

  let view = $state<LoadState>({ kind: "idle" });

  async function load() {
    view = { kind: "loading" };
    const result = await listRecentAuditEvents({ limit: 20 });
    if (!result.ok) {
      view = {
        kind: "error",
        summary: describeLoadError("audit events", result.error),
      };
      return;
    }
    view = { kind: "ready", events: result.data };
  }

  $effect(() => {
    void load();
  });

  function formatRecordedAt(rfc3339: string): string {
    const t = Date.parse(rfc3339);
    if (Number.isNaN(t)) return rfc3339;
    return new Date(t).toLocaleString();
  }
</script>

<article
  class="flex flex-col gap-3 rounded-lg border border-zinc-800 bg-zinc-950/40 p-6"
  data-testid="settings-recent-activity"
>
  <header class="flex items-center justify-between gap-3">
    <div class="flex flex-col gap-1">
      <h3 class="text-sm font-semibold text-zinc-100">
        Recent activity
      </h3>
      <p class="text-xs text-zinc-500">
        Most recent audit events for your account. Read-only; this is
        not an admin or global audit view.
      </p>
    </div>
    <button
      type="button"
      class="rounded-md border border-zinc-700 bg-zinc-900 px-3 py-1.5 text-xs text-zinc-200 transition hover:border-zinc-600 hover:bg-zinc-800 disabled:cursor-not-allowed disabled:opacity-60"
      onclick={() => void load()}
      disabled={view.kind === "loading"}
      data-testid="settings-recent-activity-refresh"
    >
      {view.kind === "loading" ? "Refreshing…" : "Refresh"}
    </button>
  </header>

  {#if view.kind === "idle" || view.kind === "loading"}
    <p
      class="text-xs text-zinc-500"
      data-testid="settings-recent-activity-loading"
    >
      Loading recent activity…
    </p>
  {:else if view.kind === "error"}
    <p
      class="rounded-md border border-rose-900/40 bg-rose-950/20 px-3 py-2 text-xs text-rose-200"
      data-testid="settings-recent-activity-error"
    >
      {view.summary}
    </p>
  {:else if view.events.length === 0}
    <p
      class="text-xs text-zinc-500"
      data-testid="settings-recent-activity-empty"
    >
      No audit events yet. Server-profile create / disable / enable
      actions appear here.
    </p>
  {:else}
    <ul
      class="flex flex-col gap-1.5 text-sm text-zinc-200"
      data-testid="settings-recent-activity-list"
    >
      {#each view.events as event (event.id)}
        <li
          class="flex items-baseline justify-between gap-3 rounded-md border border-zinc-800 bg-zinc-900/40 px-3 py-2"
          data-testid="settings-recent-activity-row"
          data-kind={event.kind}
        >
          <span class="truncate">{summarizeAuditEvent(event)}</span>
          <time
            class="shrink-0 font-mono text-[11px] text-zinc-500"
            datetime={event.recorded_at}
          >
            {formatRecordedAt(event.recorded_at)}
          </time>
        </li>
      {/each}
    </ul>
  {/if}

  <p
    class="rounded-md border border-amber-900/40 bg-amber-950/20 px-3 py-2 text-[11px] text-amber-200/80"
  >
    <span class="font-mono uppercase tracking-wide">future work</span> ·
    Cross-user / admin audit views, search, filtering, export, and
    payload detail panes are deliberate later slices.
  </p>
</article>
