<script lang="ts">
  import type { CurrentUser } from "../api/auth.js";
  import type { NavItem } from "./navigation.js";

  interface Props {
    current: NavItem;
    devMode: boolean;
    /** Authenticated user, if any. The sign-out affordance is rendered
     * only when both `user` and `onSignOut` are supplied. */
    user?: CurrentUser | null;
    /** Sign-out handler. The shell owns the wire call + local cleanup;
     * the top bar is just the trigger surface. */
    onSignOut?: () => void;
    /** Disables the sign-out button while a logout request is in flight. */
    signingOut?: boolean;
    /** Toggle the mobile nav drawer. Mobile-only via `sm:hidden`; the
     * persistent sidebar replaces this affordance from `sm:` and up. */
    onToggleMobileNav?: () => void;
    /** Mobile drawer open state — surfaced as `aria-expanded` on the
     * hamburger button. */
    mobileNavOpen?: boolean;
  }

  let {
    current,
    devMode,
    user = null,
    onSignOut,
    signingOut = false,
    onToggleMobileNav,
    mobileNavOpen = false,
  }: Props = $props();
</script>

<header
  class="flex items-center justify-between border-b border-zinc-800 bg-zinc-950/60 px-4 py-3 sm:px-6"
>
  <div class="flex min-w-0 items-center gap-2 sm:gap-3">
    {#if onToggleMobileNav}
      <button
        type="button"
        class="-ml-1 rounded-md p-2 text-zinc-300 transition hover:bg-zinc-900 hover:text-zinc-100 sm:hidden"
        aria-label={mobileNavOpen ? "Close navigation" : "Open navigation"}
        aria-expanded={mobileNavOpen ? "true" : "false"}
        aria-controls="app-mobile-nav-drawer"
        data-testid="app-mobile-nav-toggle"
        onclick={onToggleMobileNav}
      >
        <svg
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          stroke-width="2"
          stroke-linecap="round"
          stroke-linejoin="round"
          class="h-5 w-5"
          aria-hidden="true"
        >
          <line x1="3" y1="6" x2="21" y2="6" />
          <line x1="3" y1="12" x2="21" y2="12" />
          <line x1="3" y1="18" x2="21" y2="18" />
        </svg>
      </button>
    {/if}
    <div class="flex min-w-0 flex-col">
      <span
        class="truncate text-xs uppercase tracking-wide text-zinc-500"
      >
        {current.description}
      </span>
      <h1
        class="truncate text-base font-semibold tracking-tight text-zinc-100"
        data-testid="top-bar-title"
      >
        {current.label}
      </h1>
    </div>
  </div>
  <div class="flex items-center gap-3">
    {#if devMode}
      <span
        class="rounded-full border border-amber-900/40 bg-amber-950/20 px-2 py-0.5 font-mono text-[10px] uppercase tracking-wide text-amber-200/80"
        data-testid="dev-mode-badge"
      >
        dev build
      </span>
    {/if}
    {#if user && onSignOut}
      <span
        class="hidden text-xs text-zinc-400 sm:inline"
        data-testid="auth-current-user"
      >
        {user.display_name}
      </span>
      <button
        type="button"
        disabled={signingOut}
        data-testid="auth-sign-out"
        class="rounded-md border border-zinc-700 px-2 py-1 text-xs font-medium text-zinc-200 hover:border-zinc-500 hover:text-white disabled:cursor-not-allowed disabled:border-zinc-800 disabled:text-zinc-600"
        onclick={onSignOut}
      >
        {signingOut ? "Signing out…" : "Sign out"}
      </button>
    {/if}
  </div>
</header>
