<script lang="ts">
  import { NAV_ITEMS, type AppViewId } from "./navigation.js";

  interface Props {
    selected: AppViewId;
    onselect: (id: AppViewId) => void;
    devToolsOpen?: boolean;
    onToggleDevTools?: () => void;
    showDevTools?: boolean;
  }

  let {
    selected,
    onselect,
    devToolsOpen = false,
    onToggleDevTools,
    showDevTools = false,
  }: Props = $props();
</script>

<aside
  class="flex w-56 shrink-0 flex-col gap-1 border-r border-zinc-800 bg-zinc-950/60 px-3 py-4"
  aria-label="Primary navigation"
>
  <div class="flex items-center gap-2 px-2 py-2">
    <span
      class="inline-block h-2.5 w-2.5 rounded-sm bg-emerald-400/80"
      aria-hidden="true"
    ></span>
    <span class="text-sm font-semibold tracking-tight text-zinc-100">
      RelayTerm
    </span>
  </div>

  <nav class="mt-2 flex flex-col gap-0.5" aria-label="Sections">
    {#each NAV_ITEMS as item (item.id)}
      {@const active = item.id === selected}
      <button
        type="button"
        class="group flex flex-col items-start gap-0.5 rounded-md px-2 py-1.5 text-left text-sm transition {active
          ? 'bg-zinc-800 text-zinc-100'
          : 'text-zinc-400 hover:bg-zinc-900 hover:text-zinc-100'}"
        aria-current={active ? "page" : undefined}
        data-testid="nav-{item.id}"
        onclick={() => onselect(item.id)}
      >
        <span class="font-medium">{item.label}</span>
        <span
          class="text-[11px] {active
            ? 'text-zinc-400'
            : 'text-zinc-500 group-hover:text-zinc-400'}"
        >
          {item.description}
        </span>
      </button>
    {/each}
  </nav>

  {#if showDevTools && onToggleDevTools}
    <div class="mt-auto pt-4">
      <button
        type="button"
        class="flex w-full items-center justify-between rounded-md border border-amber-900/40 bg-amber-950/20 px-2 py-1.5 text-xs text-amber-200/80 transition hover:bg-amber-950/40"
        data-testid="nav-devtools-toggle"
        onclick={onToggleDevTools}
      >
        <span class="font-mono uppercase tracking-wide">developer tools</span>
        <span aria-hidden="true">{devToolsOpen ? "▾" : "▸"}</span>
      </button>
    </div>
  {/if}
</aside>
