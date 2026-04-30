<script lang="ts">
  import { checkHealth, type HealthStatus } from "../../api/health.js";
  import StatusBadge from "../StatusBadge.svelte";

  let status = $state<HealthStatus>("unknown");
  let pending = $state(false);

  async function probe() {
    pending = true;
    status = await checkHealth();
    pending = false;
  }
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
    class="flex flex-col gap-2 rounded-lg border border-zinc-800 bg-zinc-950/40 p-6 text-sm text-zinc-300"
  >
    <h3 class="text-sm font-semibold text-zinc-100">What's wired up</h3>
    <ul class="flex flex-col gap-1.5 text-zinc-400">
      <li>· Backend session orchestrator + reconnect TTL.</li>
      <li>· Vault-backed SSH identities and host-key trust flow.</li>
      <li>· WebSocket attach/detach with sequenced replay.</li>
      <li>· Renderer-neutral terminal protocol and renderer adapters.</li>
    </ul>
    <p class="mt-2 text-xs text-zinc-500">
      Production CRUD, real auth UI, and the live terminal workspace are
      future work — see the placeholder views in the sidebar.
    </p>
  </article>
</section>
