<script lang="ts">
  import { checkHealth, type HealthStatus } from "../../api/health.js";
  import { listHosts } from "../../api/hosts.js";
  import { listServerProfiles } from "../../api/serverProfiles.js";
  import { listSshIdentities } from "../../api/sshIdentities.js";
  import StatusBadge from "../StatusBadge.svelte";

  type CountState =
    | { kind: "idle" }
    | { kind: "loading" }
    | {
        kind: "ready";
        hosts: number;
        profiles: number;
        identities: number;
      }
    | { kind: "error" };

  let status = $state<HealthStatus>("unknown");
  let pending = $state(false);
  let counts = $state<CountState>({ kind: "idle" });

  async function probe() {
    pending = true;
    status = await checkHealth();
    pending = false;
  }

  async function loadCounts() {
    counts = { kind: "loading" };
    const [hosts, profiles, identities] = await Promise.all([
      listHosts(),
      listServerProfiles(),
      listSshIdentities(),
    ]);
    if (!hosts.ok || !profiles.ok || !identities.ok) {
      // Counts are nice-to-have on the dashboard. A failure collapses to
      // an unobtrusive "unavailable" state — the per-view error surface
      // is the place the operator goes to triage.
      counts = { kind: "error" };
      return;
    }
    counts = {
      kind: "ready",
      hosts: hosts.data.length,
      profiles: profiles.data.length,
      identities: identities.data.length,
    };
  }

  // One-shot mount probe: this effect deliberately reads no reactive
  // state, so Svelte runs it once on mount only. If a future revision
  // ever reads `counts` / `status` from inside the body, this becomes
  // a refresh-on-every-change effect — not the intent. The explicit
  // reload path is the "Refresh" button below.
  $effect(() => {
    void loadCounts();
  });
</script>

<section
  class="flex flex-col gap-6"
  data-testid="production-view-dashboard"
>
  <header class="flex flex-col gap-1">
    <h2 class="text-lg font-semibold tracking-tight text-zinc-100">
      Dashboard
    </h2>
    <p class="text-sm text-zinc-400">
      RelayTerm is a web/mobile SSH terminal where sessions live on the
      backend and clients can detach and reconnect. The renderer is
      replaceable; the SSH session is not.
    </p>
  </header>

  <article
    class="flex flex-col gap-4 rounded-lg border border-zinc-800 bg-zinc-950/40 p-6"
  >
    <div class="flex items-center justify-between">
      <div class="flex flex-col gap-1">
        <span class="text-sm font-medium text-zinc-200">Backend health</span>
        <span class="text-xs text-zinc-500">
          One-shot probe of <span class="font-mono">/healthz</span>. No
          polling.
        </span>
      </div>
      <StatusBadge {status} />
    </div>
    <div class="flex items-center gap-2">
      <button
        type="button"
        class="rounded-md border border-zinc-700 bg-zinc-800 px-3 py-1.5 text-sm text-zinc-100 transition hover:border-zinc-600 hover:bg-zinc-700 disabled:opacity-50"
        onclick={probe}
        disabled={pending}
        data-testid="health-check-button"
      >
        {pending ? "Checking…" : "Check now"}
      </button>
    </div>
  </article>

  <article
    class="flex flex-col gap-3 rounded-lg border border-zinc-800 bg-zinc-950/40 p-6"
    data-testid="dashboard-inventory-counts"
  >
    <header class="flex items-baseline justify-between">
      <h3 class="text-sm font-semibold text-zinc-100">Inventory</h3>
      <button
        type="button"
        class="text-xs text-zinc-400 transition hover:text-zinc-200 disabled:opacity-50"
        onclick={loadCounts}
        disabled={counts.kind === "loading"}
        data-testid="dashboard-counts-refresh"
      >
        {counts.kind === "loading" ? "Loading…" : "Refresh"}
      </button>
    </header>
    <dl class="grid grid-cols-3 gap-3 text-sm">
      <div class="flex flex-col gap-0.5">
        <dt class="text-xs uppercase tracking-wide text-zinc-500">Hosts</dt>
        <dd
          class="font-mono text-xl text-zinc-100"
          data-testid="dashboard-count-hosts"
        >
          {counts.kind === "ready" ? counts.hosts : "—"}
        </dd>
      </div>
      <div class="flex flex-col gap-0.5">
        <dt class="text-xs uppercase tracking-wide text-zinc-500">Profiles</dt>
        <dd
          class="font-mono text-xl text-zinc-100"
          data-testid="dashboard-count-profiles"
        >
          {counts.kind === "ready" ? counts.profiles : "—"}
        </dd>
      </div>
      <div class="flex flex-col gap-0.5">
        <dt class="text-xs uppercase tracking-wide text-zinc-500">Identities</dt>
        <dd
          class="font-mono text-xl text-zinc-100"
          data-testid="dashboard-count-identities"
        >
          {counts.kind === "ready" ? counts.identities : "—"}
        </dd>
      </div>
    </dl>
    {#if counts.kind === "error"}
      <p
        class="text-xs text-zinc-500"
        data-testid="dashboard-counts-error"
      >
        Counts unavailable. Check the per-section views for details.
      </p>
    {/if}
  </article>

  <article
    class="flex flex-col gap-2 rounded-lg border border-zinc-800 bg-zinc-950/40 p-6 text-sm text-zinc-300"
  >
    <h3 class="text-sm font-semibold text-zinc-100">What's wired up</h3>
    <ul class="flex flex-col gap-1.5 text-zinc-400">
      <li>· Backend session orchestrator + reconnect TTL.</li>
      <li>· Vault-backed SSH identities and host-key trust flow.</li>
      <li>· WebSocket attach/detach with sequenced replay.</li>
      <li>· Renderer-neutral terminal protocol and renderer adapters.</li>
      <li>· Read-only inventory views (hosts, profiles, identities).</li>
    </ul>
    <p class="mt-2 text-xs text-zinc-500">
      Production CRUD, real auth UI, and the live terminal workspace are
      future work — see the placeholder views in the sidebar.
    </p>
  </article>
</section>
