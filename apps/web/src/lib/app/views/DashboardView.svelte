<script lang="ts">
  import { checkHealth, type HealthStatus } from "../../api/health.js";
  import { listHosts, type Host } from "../../api/hosts.js";
  import {
    listServerProfiles,
    type ServerProfile,
  } from "../../api/serverProfiles.js";
  import {
    listSshIdentities,
    type SshIdentity,
  } from "../../api/sshIdentities.js";
  import {
    listTerminalSessions,
    type TerminalSession,
  } from "../../api/terminalSessions.js";
  import type { LoadResult } from "../../api/apiErrors.js";
  import StatusBadge from "../StatusBadge.svelte";
  import {
    DASHBOARD_NAV_ACTIONS,
    deriveChecklist,
    sessionStatusOrder,
    summarizeInventory,
    summarizeSessionStatuses,
    type CardState,
    type ChecklistStep,
  } from "../dashboard/dashboardSummary.js";
  import type { AppViewId } from "../navigation.js";

  interface Props {
    /** Internal navigation callback. Wired from `AppShell.svelte`'s
     * `navigate(id)` so dashboard CTAs route through the same pushState
     * + view-state path as the sidebar. */
    onNavigate?: (view: AppViewId) => void;
  }

  let { onNavigate }: Props = $props();

  let health = $state<HealthStatus>("unknown");
  let healthPending = $state(false);

  // Inventory load results. `null` is the pre-fetch state; the helpers
  // collapse that to `loading` and a failed `LoadResult` to `unavailable`
  // — the dashboard never invents a zero count.
  let hostsResult = $state<LoadResult<Host[]> | null>(null);
  let profilesResult = $state<LoadResult<ServerProfile[]> | null>(null);
  let identitiesResult = $state<LoadResult<SshIdentity[]> | null>(null);
  let sessionsResult = $state<LoadResult<TerminalSession[]> | null>(null);
  let inventoryPending = $state(false);

  let inventory = $derived(
    summarizeInventory({
      hosts: hostsResult,
      profiles: profilesResult,
      identities: identitiesResult,
      sessions: sessionsResult,
    }),
  );
  let sessionBreakdown = $derived(summarizeSessionStatuses(sessionsResult));
  let checklist = $derived(deriveChecklist(inventory));

  async function probeHealth() {
    healthPending = true;
    health = await checkHealth();
    healthPending = false;
  }

  async function loadInventory() {
    inventoryPending = true;
    // Parallel fetch; each section tolerates partial failure independently.
    const [hosts, profiles, identities, sessions] = await Promise.all([
      listHosts(),
      listServerProfiles(),
      listSshIdentities(),
      listTerminalSessions(),
    ]);
    hostsResult = hosts;
    profilesResult = profiles;
    identitiesResult = identities;
    sessionsResult = sessions;
    inventoryPending = false;
  }

  async function refreshAll() {
    // Manual refresh only — no polling, no auto-refresh. Drives both the
    // health probe and the inventory load in parallel.
    await Promise.all([probeHealth(), loadInventory()]);
  }

  // One-shot mount load. The effect deliberately reads no reactive state
  // so it runs once. Subsequent updates go through the explicit Refresh
  // button.
  $effect(() => {
    void refreshAll();
  });

  function navigateTo(view: AppViewId) {
    onNavigate?.(view);
  }

  function cardDisplay(card: CardState): string {
    return card.kind === "ready" ? String(card.value) : "—";
  }

  function cardStatusLabel(card: CardState): string {
    switch (card.kind) {
      case "loading":
        return "Loading…";
      case "unavailable":
        return "Unavailable";
      case "ready":
        return "";
    }
  }

  function checklistDotClass(step: ChecklistStep): string {
    switch (step.status) {
      case "complete":
        return "bg-emerald-400";
      case "incomplete":
        return "bg-zinc-500";
      case "manual":
        return "bg-amber-400/80";
      case "unknown":
        return "bg-zinc-700";
    }
  }

  function checklistStatusLabel(step: ChecklistStep): string {
    switch (step.status) {
      case "complete":
        return "complete";
      case "incomplete":
        return "not yet";
      case "manual":
        return "manual";
      case "unknown":
        return "unknown";
    }
  }
</script>

<section
  class="flex flex-col gap-6"
  data-testid="production-view-dashboard"
>
  <header class="flex items-start justify-between gap-3">
    <div class="flex flex-col gap-1">
      <h2 class="text-lg font-semibold tracking-tight text-zinc-100">
        Dashboard
      </h2>
      <p class="text-sm text-zinc-400">
        Read-only summary of backend health, your inventory, and the
        connection-flow checklist. Refresh is manual; no polling.
      </p>
    </div>
    <button
      type="button"
      class="rounded-md border border-zinc-700 bg-zinc-800 px-3 py-1.5 text-sm text-zinc-100 transition hover:border-zinc-600 hover:bg-zinc-700 disabled:opacity-50"
      onclick={refreshAll}
      disabled={inventoryPending || healthPending}
      data-testid="dashboard-refresh"
    >
      {inventoryPending || healthPending ? "Refreshing…" : "Refresh"}
    </button>
  </header>

  <div
    class="grid grid-cols-1 gap-3 md:grid-cols-2 lg:grid-cols-3"
    data-testid="dashboard-summary-cards"
  >
    <article
      class="flex flex-col gap-2 rounded-lg border border-zinc-800 bg-zinc-950/40 p-4"
      data-testid="dashboard-card-health"
    >
      <div class="flex items-center justify-between gap-2">
        <span class="text-xs uppercase tracking-wide text-zinc-500">
          Backend
        </span>
        <StatusBadge status={health} />
      </div>
      <p class="text-sm text-zinc-400">
        One-shot probe of <span class="font-mono">/healthz</span>.
      </p>
      <button
        type="button"
        class="self-start text-xs text-zinc-400 transition hover:text-zinc-200 disabled:opacity-50"
        onclick={probeHealth}
        disabled={healthPending || inventoryPending}
        data-testid="dashboard-health-probe"
      >
        {healthPending ? "Checking…" : "Check now"}
      </button>
    </article>

    {#each [
      { id: "hosts", label: "Hosts", card: inventory.hosts, view: "servers" as AppViewId },
      { id: "profiles", label: "Server profiles", card: inventory.profiles, view: "servers" as AppViewId },
      { id: "identities", label: "SSH identities", card: inventory.identities, view: "identities" as AppViewId },
      { id: "sessions", label: "Terminal sessions", card: inventory.sessions, view: "sessions" as AppViewId },
    ] as tile (tile.id)}
      <article
        class="flex flex-col gap-1 rounded-lg border border-zinc-800 bg-zinc-950/40 p-4"
        data-testid="dashboard-card-{tile.id}"
      >
        <span class="text-xs uppercase tracking-wide text-zinc-500">
          {tile.label}
        </span>
        <span
          class="font-mono text-2xl text-zinc-100"
          data-testid="dashboard-count-{tile.id}"
        >
          {cardDisplay(tile.card)}
        </span>
        {#if tile.card.kind !== "ready"}
          <span
            class="text-xs text-zinc-500"
            data-testid="dashboard-card-{tile.id}-status"
          >
            {cardStatusLabel(tile.card)}
          </span>
        {/if}
        <button
          type="button"
          class="self-start text-xs text-zinc-400 transition hover:text-zinc-200"
          onclick={() => navigateTo(tile.view)}
          data-testid="dashboard-card-{tile.id}-cta"
        >
          Open →
        </button>
      </article>
    {/each}
  </div>

  <article
    class="flex flex-col gap-3 rounded-lg border border-zinc-800 bg-zinc-950/40 p-4"
    data-testid="dashboard-session-breakdown"
  >
    <header class="flex items-center justify-between gap-2">
      <span class="text-sm font-semibold text-zinc-100">
        Terminal sessions by status
      </span>
      <button
        type="button"
        class="text-xs text-zinc-400 transition hover:text-zinc-200"
        onclick={() => navigateTo("sessions")}
        data-testid="dashboard-sessions-cta"
      >
        Open sessions →
      </button>
    </header>
    {#if sessionBreakdown.kind === "ready"}
      <dl class="grid grid-cols-2 gap-3 text-sm sm:grid-cols-4">
        {#each sessionStatusOrder() as status (status)}
          <div class="flex flex-col gap-0.5">
            <dt class="text-xs uppercase tracking-wide text-zinc-500">
              {status}
            </dt>
            <dd
              class="font-mono text-lg text-zinc-100"
              data-testid="dashboard-session-status-{status}"
            >
              {sessionBreakdown.counts[status]}
            </dd>
          </div>
        {/each}
      </dl>
    {:else if sessionBreakdown.kind === "loading"}
      <p class="text-xs text-zinc-500" data-testid="dashboard-session-loading">
        Loading…
      </p>
    {:else}
      <p
        class="text-xs text-zinc-500"
        data-testid="dashboard-session-unavailable"
      >
        Unavailable. Check the Sessions view for details.
      </p>
    {/if}
  </article>

  <article
    class="flex flex-col gap-3 rounded-lg border border-zinc-800 bg-zinc-950/40 p-4"
    data-testid="dashboard-setup-checklist"
  >
    <header class="flex flex-col gap-1">
      <span class="text-sm font-semibold text-zinc-100">
        Connection-flow checklist
      </span>
      <span class="text-xs text-zinc-500">
        Counts can prove a step happened; they cannot prove host-key
        trust, public-key install, or auth-check. Those rows stay
        manual.
      </span>
    </header>
    <ol class="flex flex-col gap-2">
      {#each checklist as step, index (step.id)}
        <li
          class="flex flex-col gap-1 rounded-md border border-zinc-800/60 bg-zinc-900/30 px-3 py-2"
          data-testid="dashboard-checklist-{step.id}"
          data-status={step.status}
        >
          <div class="flex items-center justify-between gap-2">
            <div class="flex items-center gap-2">
              <span
                class="h-2 w-2 shrink-0 rounded-full {checklistDotClass(step)}"
                aria-hidden="true"
              ></span>
              <span class="text-sm text-zinc-100">
                <span class="text-zinc-500">{index + 1}.</span>
                {step.label}
              </span>
            </div>
            <span
              class="text-[11px] uppercase tracking-wide text-zinc-500"
              data-testid="dashboard-checklist-{step.id}-status"
            >
              {checklistStatusLabel(step)}
            </span>
          </div>
          <p class="pl-4 text-xs text-zinc-400">{step.detail}</p>
          {#if step.cta}
            <button
              type="button"
              class="self-start pl-4 text-xs text-zinc-400 transition hover:text-zinc-200"
              onclick={() => navigateTo(step.cta!.view)}
              data-testid="dashboard-checklist-{step.id}-cta"
            >
              {step.cta.label} →
            </button>
          {/if}
        </li>
      {/each}
    </ol>
  </article>

  <article
    class="flex flex-col gap-3 rounded-lg border border-zinc-800 bg-zinc-950/40 p-4"
    data-testid="dashboard-nav-actions"
  >
    <span class="text-sm font-semibold text-zinc-100">Quick actions</span>
    <div class="flex flex-wrap gap-2">
      {#each DASHBOARD_NAV_ACTIONS as action (action.id)}
        <button
          type="button"
          class="rounded-md border border-zinc-700 bg-zinc-800 px-3 py-1.5 text-sm text-zinc-100 transition hover:border-zinc-600 hover:bg-zinc-700"
          onclick={() => navigateTo(action.view)}
          data-testid="dashboard-nav-{action.id}"
        >
          {action.label}
        </button>
      {/each}
    </div>
  </article>
</section>
