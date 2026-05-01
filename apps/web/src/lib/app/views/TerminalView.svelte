<script lang="ts">
  /**
   * Production terminal workspace view. Two states only:
   *
   *  1. No active launch — show an honest empty state pointing the
   *     operator at the Server profiles view, where the launch action
   *     lives.
   *  2. Active launch — render `ProductionTerminal`, keyed by
   *     `sessionId` so a fresh launch tears down the previous renderer
   *     and client cleanly.
   *
   * The view is intentionally thin: the per-session lifecycle and the
   * xterm wiring live in `ProductionTerminal.svelte`. This keeps the
   * `AppViewId` switch in `AppShell.svelte` decoupled from the
   * imperative renderer plumbing.
   */
  import ProductionTerminal from "../terminal/ProductionTerminal.svelte";
  import type { ActiveLaunch } from "../terminal/activeLaunch.js";

  interface Props {
    launch: ActiveLaunch | null;
    onExit?: () => void;
  }

  let { launch, onExit }: Props = $props();
</script>

{#if launch}
  {#key launch.sessionId}
    <ProductionTerminal
      sessionId={launch.sessionId}
      cols={launch.cols}
      rows={launch.rows}
      profileLabel={launch.profileLabel}
      {onExit}
    />
  {/key}
{:else}
  <section
    class="flex flex-col gap-4 rounded-lg border border-zinc-800 bg-zinc-950/40 p-6"
    data-testid="production-view-terminal"
  >
    <header class="flex flex-col gap-1">
      <h2 class="text-lg font-semibold tracking-tight text-zinc-100">
        Terminal workspace
      </h2>
      <p class="text-sm text-zinc-400">
        Launch a terminal from a server profile.
      </p>
    </header>
    <ul class="flex flex-col gap-2 text-sm text-zinc-300">
      <li class="flex items-start gap-2">
        <span class="mt-1 h-1.5 w-1.5 shrink-0 rounded-full bg-zinc-600"></span>
        <span>
          Use the <strong>Server profiles</strong> view to pick a profile,
          then press <strong>Launch terminal</strong>.
        </span>
      </li>
      <li class="flex items-start gap-2">
        <span class="mt-1 h-1.5 w-1.5 shrink-0 rounded-full bg-zinc-600"></span>
        <span>
          Run host-key trust and SSH auth-check on the profile first; the
          backend will refuse the launch otherwise.
        </span>
      </li>
      <li class="flex items-start gap-2">
        <span class="mt-1 h-1.5 w-1.5 shrink-0 rounded-full bg-zinc-600"></span>
        <span>
          Detached sessions survive only briefly (~30s); replay is
          in-memory and does not survive a backend restart.
        </span>
      </li>
    </ul>
    <p
      class="rounded-md border border-amber-900/40 bg-amber-950/20 px-3 py-2 text-xs text-amber-200/80"
    >
      <span class="font-mono uppercase tracking-wide">future work</span> ·
      Multi-tab workspace, durable session list, and a renderer selector
      land in later slices. Today the workspace shows one session at a
      time and uses the xterm baseline only.
    </p>
  </section>
{/if}
