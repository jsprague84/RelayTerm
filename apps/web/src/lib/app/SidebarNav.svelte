<script lang="ts">
  import { NAV_ITEMS, type AppViewId } from "./navigation.js";

  interface Props {
    selected: AppViewId;
    onselect: (id: AppViewId) => void;
    devToolsOpen?: boolean;
    onToggleDevTools?: () => void;
    showDevTools?: boolean;
    /**
     * Mobile drawer open state. Ignored at `sm:` and up — the sidebar
     * is always visible there. Below `sm` the sidebar slides off-screen
     * and the backdrop hides; toggle via the TopBar hamburger.
     */
    isOpen?: boolean;
    /** Close the mobile drawer (backdrop tap, close button, item select). */
    onClose?: () => void;
  }

  let {
    selected,
    onselect,
    devToolsOpen = false,
    onToggleDevTools,
    showDevTools = false,
    isOpen = false,
    onClose,
  }: Props = $props();

  /**
   * Tracks the `(max-width: 639px)` viewport so we can mark the drawer
   * `inert` when it is off-screen on mobile. Without this, an off-screen
   * drawer item is still in the Tab order even though the user can't
   * see it. On `sm:` and up the aside is part of the static layout —
   * `inert` would break it, so the predicate gates on `mobileMode`.
   * SSR-safe: the effect only runs in the browser.
   */
  let mobileMode = $state(false);
  $effect(() => {
    if (typeof window === "undefined" || !window.matchMedia) return;
    const mq = window.matchMedia("(max-width: 639px)");
    mobileMode = mq.matches;
    const onChange = (e: MediaQueryListEvent) => {
      mobileMode = e.matches;
    };
    mq.addEventListener("change", onChange);
    return () => mq.removeEventListener("change", onChange);
  });

  function handleSelect(id: AppViewId) {
    onselect(id);
    // Drawer auto-closes after picking a section on mobile so the
    // operator does not have to make a second tap to dismiss the
    // overlay before reading the chosen view. No-op on desktop, where
    // the sidebar is permanently visible.
    onClose?.();
  }
</script>

<!--
  Backdrop. Mobile-only via `sm:hidden`. A click outside the drawer
  dismisses it — same affordance as the close button. The element is
  always rendered so `transition-opacity` can animate the fade; when
  closed it gets `pointer-events-none` so it does not eat clicks
  intended for content underneath.
-->
<button
  type="button"
  class={[
    "fixed inset-0 z-30 bg-black/60 transition-opacity duration-200 sm:hidden",
    isOpen ? "opacity-100" : "pointer-events-none opacity-0",
  ].join(" ")}
  aria-hidden="true"
  tabindex="-1"
  data-testid="app-mobile-nav-backdrop"
  onclick={() => onClose?.()}
></button>

<aside
  class={[
    "z-40 flex w-56 shrink-0 flex-col gap-1 border-r border-zinc-800 bg-zinc-950 px-3 py-4",
    "fixed inset-y-0 left-0 transform transition-transform duration-200 ease-out",
    isOpen ? "translate-x-0" : "-translate-x-full",
    "sm:static sm:translate-x-0 sm:bg-zinc-950/60 sm:transition-none",
  ].join(" ")}
  id="app-mobile-nav-drawer"
  aria-label="Primary navigation"
  data-testid="app-mobile-nav-drawer"
  inert={mobileMode && !isOpen}
>
  <div class="flex items-center gap-2 px-2 py-2">
    <span
      class="inline-block h-2.5 w-2.5 rounded-sm bg-emerald-400/80"
      aria-hidden="true"
    ></span>
    <span class="text-sm font-semibold tracking-tight text-zinc-100">
      RelayTerm
    </span>
    <button
      type="button"
      class="ml-auto rounded-md p-1 text-zinc-400 transition hover:bg-zinc-900 hover:text-zinc-100 sm:hidden"
      aria-label="Close navigation"
      data-testid="app-mobile-nav-close"
      onclick={() => onClose?.()}
    >
      <svg
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        stroke-width="2"
        stroke-linecap="round"
        stroke-linejoin="round"
        class="h-4 w-4"
        aria-hidden="true"
      >
        <line x1="18" y1="6" x2="6" y2="18" />
        <line x1="6" y1="6" x2="18" y2="18" />
      </svg>
    </button>
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
        onclick={() => handleSelect(item.id)}
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
