<script lang="ts">
  let backendStatus = $state<"unknown" | "ok" | "down">("unknown");

  async function ping() {
    try {
      const res = await fetch("/healthz");
      backendStatus = res.ok ? "ok" : "down";
    } catch {
      backendStatus = "down";
    }
  }
</script>

<main class="mx-auto flex max-w-2xl flex-col gap-6 p-8">
  <header>
    <h1 class="text-2xl font-semibold tracking-tight">RelayTerm</h1>
    <p class="text-sm text-zinc-400">Skeleton — no terminal yet.</p>
  </header>

  <section class="rounded-md border border-zinc-800 p-4">
    <div class="flex items-center justify-between">
      <span class="text-sm">Backend health</span>
      <button
        type="button"
        class="rounded-sm bg-zinc-800 px-3 py-1 text-sm hover:bg-zinc-700"
        onclick={ping}
      >
        check
      </button>
    </div>
    <p class="mt-2 text-sm">
      status: <span class="font-mono">{backendStatus}</span>
    </p>
  </section>
</main>
