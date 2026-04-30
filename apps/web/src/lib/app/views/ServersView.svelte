<script lang="ts">
  import { listHosts, type Host } from "../../api/hosts.js";
  import {
    listServerProfiles,
    resolveProfileLinks,
    type ServerProfile,
  } from "../../api/serverProfiles.js";
  import { describeLoadError } from "../../api/apiErrors.js";

  type LoadState =
    | { kind: "idle" }
    | { kind: "loading" }
    | { kind: "ready"; hosts: Host[]; profiles: ServerProfile[] }
    | { kind: "error"; summary: string };

  let view = $state<LoadState>({ kind: "idle" });

  async function load() {
    view = { kind: "loading" };
    const [hostsResult, profilesResult] = await Promise.all([
      listHosts(),
      listServerProfiles(),
    ]);
    if (!hostsResult.ok) {
      view = {
        kind: "error",
        summary: describeLoadError("hosts", hostsResult.error),
      };
      return;
    }
    if (!profilesResult.ok) {
      view = {
        kind: "error",
        summary: describeLoadError("server profiles", profilesResult.error),
      };
      return;
    }
    view = {
      kind: "ready",
      hosts: hostsResult.data,
      profiles: profilesResult.data,
    };
  }

  // One-shot mount load: the body reads no reactive state, so Svelte
  // runs it once on mount. The explicit reload path is the "Refresh"
  // button below.
  $effect(() => {
    void load();
  });

  function formatPort(port: number): string {
    return port === 22 ? "22 (default)" : String(port);
  }
</script>

<section
  class="flex flex-col gap-6"
  data-testid="production-view-servers"
>
  <header class="flex flex-col gap-1">
    <h2 class="text-lg font-semibold tracking-tight text-zinc-100">
      Server profiles
    </h2>
    <p class="text-sm text-zinc-400">
      Read-only inventory of hosts and the server profiles that bind them
      to an SSH identity. CRUD UI, host-key trust, auth-check, and
      terminal launch are future work.
    </p>
  </header>

  <div class="flex items-center gap-2">
    <button
      type="button"
      class="rounded-md border border-zinc-700 bg-zinc-800 px-3 py-1.5 text-sm text-zinc-100 transition hover:border-zinc-600 hover:bg-zinc-700 disabled:opacity-50"
      onclick={load}
      disabled={view.kind === "loading"}
      data-testid="servers-refresh-button"
    >
      {view.kind === "loading" ? "Loading…" : "Refresh"}
    </button>
  </div>

  {#if view.kind === "loading" || view.kind === "idle"}
    <p
      class="rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-6 text-sm text-zinc-400"
      data-testid="servers-loading"
    >
      Loading inventory…
    </p>
  {:else if view.kind === "error"}
    <p
      class="rounded-md border border-rose-900/40 bg-rose-950/20 px-4 py-3 text-sm text-rose-200/80"
      data-testid="servers-error"
    >
      {view.summary}
    </p>
  {:else}
    <article
      class="flex flex-col gap-3 rounded-lg border border-zinc-800 bg-zinc-950/40 p-6"
    >
      <header class="flex items-baseline justify-between gap-2">
        <h3 class="text-sm font-semibold text-zinc-100">Hosts</h3>
        <span class="text-xs text-zinc-500" data-testid="hosts-count">
          {view.hosts.length}
          {view.hosts.length === 1 ? "host" : "hosts"}
        </span>
      </header>
      {#if view.hosts.length === 0}
        <p class="text-sm text-zinc-400" data-testid="hosts-empty">
          No hosts yet. CRUD UI is not implemented yet — hosts are
          created through the backend API today.
        </p>
      {:else}
        <ul
          class="flex flex-col divide-y divide-zinc-800/60"
          data-testid="hosts-list"
        >
          {#each view.hosts as host (host.id)}
            <li
              class="flex flex-col gap-1 py-3 first:pt-0 last:pb-0"
              data-testid="host-row"
            >
              <div class="flex items-baseline justify-between gap-3">
                <span class="text-sm font-medium text-zinc-100">
                  {host.display_name}
                </span>
                <span class="font-mono text-xs text-zinc-500">
                  {host.hostname}:{formatPort(host.port)}
                </span>
              </div>
              <span class="text-xs text-zinc-400">
                Default user
                <span class="font-mono text-zinc-300"
                  >{host.default_username}</span
                >
              </span>
            </li>
          {/each}
        </ul>
      {/if}
    </article>

    <article
      class="flex flex-col gap-3 rounded-lg border border-zinc-800 bg-zinc-950/40 p-6"
    >
      <header class="flex items-baseline justify-between gap-2">
        <h3 class="text-sm font-semibold text-zinc-100">Profiles</h3>
        <span class="text-xs text-zinc-500" data-testid="profiles-count">
          {view.profiles.length}
          {view.profiles.length === 1 ? "profile" : "profiles"}
        </span>
      </header>
      {#if view.profiles.length === 0}
        <p class="text-sm text-zinc-400" data-testid="profiles-empty">
          No server profiles yet. Profile creation is not implemented in
          this UI — profiles are created through the backend API today.
        </p>
      {:else}
        <ul
          class="flex flex-col divide-y divide-zinc-800/60"
          data-testid="profiles-list"
        >
          {#each view.profiles as profile (profile.id)}
            {@const links = resolveProfileLinks(profile, view.hosts)}
            <li
              class="flex flex-col gap-1.5 py-3 first:pt-0 last:pb-0"
              data-testid="profile-row"
            >
              <div class="flex items-baseline justify-between gap-3">
                <span class="text-sm font-medium text-zinc-100">
                  {profile.name}
                </span>
                {#if links.host}
                  <span class="font-mono text-xs text-zinc-500">
                    {links.host.hostname}:{formatPort(links.host.port)}
                  </span>
                {:else}
                  <span
                    class="font-mono text-xs text-amber-300/80"
                    data-testid="profile-host-missing"
                  >
                    host not in your inventory
                  </span>
                {/if}
              </div>
              <div class="flex flex-wrap items-center gap-x-4 gap-y-1 text-xs text-zinc-400">
                {#if links.effectiveUsername !== null}
                  <span>
                    User
                    <span class="font-mono text-zinc-300"
                      >{links.effectiveUsername}</span
                    >
                    {#if links.inheritedFromHost}
                      <span class="text-zinc-500">(host default)</span>
                    {:else}
                      <span class="text-zinc-500">(override)</span>
                    {/if}
                  </span>
                {:else}
                  <span class="text-amber-300/80">
                    Username unavailable (host link unresolved)
                  </span>
                {/if}
                {#if profile.last_connected_at}
                  <span>
                    Last connected
                    <time class="font-mono text-zinc-300"
                      >{profile.last_connected_at}</time
                    >
                  </span>
                {:else}
                  <span class="text-zinc-500">Never connected</span>
                {/if}
              </div>
              {#if profile.tags.length > 0}
                <ul class="flex flex-wrap gap-1.5" data-testid="profile-tags">
                  {#each profile.tags as tag (tag)}
                    <li
                      class="rounded border border-zinc-700/80 bg-zinc-900/60 px-1.5 py-0.5 font-mono text-[11px] text-zinc-300"
                    >
                      {tag}
                    </li>
                  {/each}
                </ul>
              {/if}
            </li>
          {/each}
        </ul>
      {/if}
    </article>
  {/if}

  <p
    class="rounded-md border border-amber-900/40 bg-amber-950/20 px-3 py-2 text-xs text-amber-200/80"
  >
    <span class="font-mono uppercase tracking-wide">future work</span> ·
    Create / edit / delete forms, host-key trust, auth-check, and
    terminal launch land alongside the production terminal workspace.
  </p>
</section>
