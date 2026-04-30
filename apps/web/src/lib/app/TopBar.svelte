<script lang="ts">
  import type { NavItem } from "./navigation.js";

  interface Props {
    current: NavItem;
    devMode: boolean;
  }

  let { current, devMode }: Props = $props();
</script>

<header
  class="flex items-center justify-between border-b border-zinc-800 bg-zinc-950/60 px-6 py-3"
>
  <div class="flex flex-col">
    <span class="text-xs uppercase tracking-wide text-zinc-500">
      {current.description}
    </span>
    <h1
      class="text-base font-semibold tracking-tight text-zinc-100"
      data-testid="top-bar-title"
    >
      {current.label}
    </h1>
  </div>
  {#if devMode}
    <span
      class="rounded-full border border-amber-900/40 bg-amber-950/20 px-2 py-0.5 font-mono text-[10px] uppercase tracking-wide text-amber-200/80"
      data-testid="dev-mode-badge"
    >
      dev build
    </span>
  {/if}
</header>
