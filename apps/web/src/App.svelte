<script lang="ts">
  import TerminalProtocolLab from "./lib/dev/TerminalProtocolLab.svelte";

  // `import.meta.env.DEV` is statically `true` under `vite dev` / vitest
  // and statically `false` for `vite build` (see vite.config.ts). Vite
  // inlines this constant at build time, so the production bundle's
  // dead-code elimination drops the lab branch entirely.
  const isDev = import.meta.env.DEV;

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

  {#if isDev}
    <TerminalProtocolLab />
  {:else}
    <section class="rounded-md border border-zinc-800 p-4 text-sm">
      <h2 class="text-base font-semibold">Terminal</h2>
      <p class="mt-1 text-zinc-400">
        The production terminal UI is not implemented yet. Backend session
        lifecycle and the renderer-neutral protocol/client layer are in
        place; the renderer adapter lands in a later slice.
      </p>
    </section>
  {/if}
</main>
