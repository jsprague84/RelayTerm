<script lang="ts">
  import {
    createHost,
    describeCreateHostError,
    listHosts,
    DEFAULT_SSH_PORT,
    type Host,
  } from "../../api/hosts.js";
  import {
    canSubmitServerProfile,
    createServerProfile,
    describeCreateServerProfileError,
    listServerProfiles,
    parseTagsInput,
    resolveProfileLinks,
    type ServerProfile,
  } from "../../api/serverProfiles.js";
  import {
    listSshIdentities,
    type SshIdentity,
  } from "../../api/sshIdentities.js";
  import { describeLoadError } from "../../api/apiErrors.js";
  import HostKeyPanel from "./HostKeyPanel.svelte";

  type LoadState =
    | { kind: "idle" }
    | { kind: "loading" }
    | {
        kind: "ready";
        hosts: Host[];
        profiles: ServerProfile[];
        identities: SshIdentity[];
      }
    | { kind: "error"; summary: string };

  type CreateHostState =
    | { kind: "idle" }
    | { kind: "submitting" }
    | { kind: "success"; host: Host }
    | { kind: "error"; summary: string };

  type CreateProfileState =
    | { kind: "idle" }
    | { kind: "submitting" }
    | { kind: "success"; profile: ServerProfile }
    | { kind: "error"; summary: string };

  type Panel = "none" | "host" | "profile";

  let view = $state<LoadState>({ kind: "idle" });
  let panel = $state<Panel>("none");

  // Host create form state
  let hostName = $state("");
  let hostHostname = $state("");
  let hostPort = $state<number>(DEFAULT_SSH_PORT);
  let hostUsername = $state("");
  let hostState = $state<CreateHostState>({ kind: "idle" });

  // Profile create form state
  let profileName = $state("");
  let profileHostId = $state("");
  let profileIdentityId = $state("");
  let profileUsernameOverride = $state("");
  let profileTagsInput = $state("");
  let profileState = $state<CreateProfileState>({ kind: "idle" });

  async function load() {
    view = { kind: "loading" };
    const [hostsResult, profilesResult, identitiesResult] = await Promise.all([
      listHosts(),
      listServerProfiles(),
      listSshIdentities(),
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
    if (!identitiesResult.ok) {
      view = {
        kind: "error",
        summary: describeLoadError("SSH identities", identitiesResult.error),
      };
      return;
    }
    view = {
      kind: "ready",
      hosts: hostsResult.data,
      profiles: profilesResult.data,
      identities: identitiesResult.data,
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

  function openHostPanel() {
    if (panel === "host") return;
    panel = "host";
    if (hostState.kind !== "submitting") {
      hostState = { kind: "idle" };
    }
  }

  function openProfilePanel() {
    if (panel === "profile") return;
    panel = "profile";
    if (profileState.kind !== "submitting") {
      profileState = { kind: "idle" };
    }
    // Pre-fill the host/identity selects when only one option exists,
    // so the form is ready to submit without an extra click.
    if (view.kind === "ready") {
      if (view.hosts.length === 1 && profileHostId === "") {
        profileHostId = view.hosts[0].id;
      }
      if (view.identities.length === 1 && profileIdentityId === "") {
        profileIdentityId = view.identities[0].id;
      }
    }
  }

  function closePanel(kind: Panel) {
    if (kind === "host" && hostState.kind === "submitting") return;
    if (kind === "profile" && profileState.kind === "submitting") return;
    panel = "none";
  }

  function resetHostForm() {
    hostName = "";
    hostHostname = "";
    hostPort = DEFAULT_SSH_PORT;
    hostUsername = "";
  }

  function resetProfileForm() {
    profileName = "";
    profileHostId = "";
    profileIdentityId = "";
    profileUsernameOverride = "";
    profileTagsInput = "";
  }

  async function submitHost(event: Event) {
    event.preventDefault();
    if (hostState.kind === "submitting") return;
    hostState = { kind: "submitting" };
    const result = await createHost({
      display_name: hostName,
      hostname: hostHostname,
      port: hostPort,
      default_username: hostUsername,
    });
    if (!result.ok) {
      hostState = {
        kind: "error",
        summary: describeCreateHostError(result.error),
      };
      return;
    }
    if (view.kind === "ready") {
      const exists = view.hosts.some((h) => h.id === result.host.id);
      view = exists
        ? view
        : {
            kind: "ready",
            hosts: [result.host, ...view.hosts],
            profiles: view.profiles,
            identities: view.identities,
          };
    } else {
      void load();
    }
    hostState = { kind: "success", host: result.host };
    resetHostForm();
  }

  async function submitProfile(event: Event) {
    event.preventDefault();
    if (profileState.kind === "submitting") return;
    profileState = { kind: "submitting" };
    const tags = parseTagsInput(profileTagsInput);
    const result = await createServerProfile({
      name: profileName,
      host_id: profileHostId,
      ssh_identity_id: profileIdentityId,
      username_override:
        profileUsernameOverride.length === 0
          ? null
          : profileUsernameOverride,
      tags,
    });
    if (!result.ok) {
      profileState = {
        kind: "error",
        summary: describeCreateServerProfileError(result.error),
      };
      return;
    }
    if (view.kind === "ready") {
      const exists = view.profiles.some((p) => p.id === result.profile.id);
      view = exists
        ? view
        : {
            kind: "ready",
            hosts: view.hosts,
            profiles: [result.profile, ...view.profiles],
            identities: view.identities,
          };
    } else {
      void load();
    }
    profileState = { kind: "success", profile: result.profile };
    resetProfileForm();
  }

  // Whether the "Create server profile" button is allowed to open the
  // panel. We guard at the toolbar so the operator sees the precise
  // empty-state hint before the form ever renders.
  function profileCreatability(state: LoadState): {
    allowed: boolean;
    summary: string;
  } {
    if (state.kind !== "ready") {
      return { allowed: false, summary: "Loading inventory…" };
    }
    const c = canSubmitServerProfile(
      state.hosts.length,
      state.identities.length,
    );
    if (c.kind === "ok") return { allowed: true, summary: "" };
    if (c.kind === "no_hosts_or_identities") {
      return {
        allowed: false,
        summary:
          "Create at least one host AND one SSH identity before adding a profile.",
      };
    }
    if (c.kind === "no_hosts") {
      return {
        allowed: false,
        summary: "Create at least one host before adding a profile.",
      };
    }
    return {
      allowed: false,
      summary:
        "Create at least one SSH identity before adding a profile.",
    };
  }

  let creatability = $derived(profileCreatability(view));

  let hostSubmitDisabled = $derived(
    hostState.kind === "submitting" ||
      hostName.trim().length === 0 ||
      hostHostname.trim().length === 0 ||
      hostUsername.trim().length === 0,
  );

  let profileSubmitDisabled = $derived(
    profileState.kind === "submitting" ||
      profileName.trim().length === 0 ||
      profileHostId.length === 0 ||
      profileIdentityId.length === 0,
  );
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
      Hosts are reachable target definitions. Server profiles bind a
      host to an SSH identity. Run host-key preflight per profile to
      capture and explicitly trust the server's host key. SSH auth-check
      and terminal launch are future work — creating or trusting here
      does NOT verify SSH authentication or install the public key.
    </p>
  </header>

  <div class="flex flex-wrap items-center gap-2">
    <button
      type="button"
      class="rounded-md border border-zinc-700 bg-zinc-800 px-3 py-1.5 text-sm text-zinc-100 transition hover:border-zinc-600 hover:bg-zinc-700 disabled:opacity-50"
      onclick={load}
      disabled={view.kind === "loading"}
      data-testid="servers-refresh-button"
    >
      {view.kind === "loading" ? "Loading…" : "Refresh"}
    </button>
    {#if panel !== "host"}
      <button
        type="button"
        class="rounded-md border border-emerald-800/60 bg-emerald-900/20 px-3 py-1.5 text-sm text-emerald-100 transition hover:border-emerald-700 hover:bg-emerald-900/40"
        onclick={openHostPanel}
        data-testid="servers-create-host-open"
      >
        Create host
      </button>
    {/if}
    {#if panel !== "profile"}
      <button
        type="button"
        class="rounded-md border border-emerald-800/60 bg-emerald-900/20 px-3 py-1.5 text-sm text-emerald-100 transition hover:border-emerald-700 hover:bg-emerald-900/40 disabled:cursor-not-allowed disabled:opacity-50"
        onclick={openProfilePanel}
        disabled={!creatability.allowed}
        data-testid="servers-create-profile-open"
      >
        Create server profile
      </button>
    {/if}
    {#if !creatability.allowed && view.kind === "ready"}
      <span
        class="text-xs text-zinc-500"
        data-testid="servers-create-profile-blocked"
      >
        {creatability.summary}
      </span>
    {/if}
  </div>

  {#if panel === "host"}
    <article
      class="flex flex-col gap-4 rounded-lg border border-emerald-900/40 bg-emerald-950/10 p-6"
      data-testid="servers-create-host-panel"
    >
      <header class="flex items-baseline justify-between gap-2">
        <h3 class="text-sm font-semibold text-zinc-100">Create a host</h3>
        <button
          type="button"
          class="rounded-md border border-zinc-800 bg-zinc-900 px-2 py-1 text-xs text-zinc-300 transition hover:border-zinc-700 hover:bg-zinc-800 disabled:opacity-50"
          onclick={() => closePanel("host")}
          disabled={hostState.kind === "submitting"}
          data-testid="servers-create-host-close"
        >
          Close
        </button>
      </header>

      <ul class="flex flex-col gap-1 text-xs text-zinc-400">
        <li>
          A host is a metadata-only target definition: display name,
          hostname, port, default username.
        </li>
        <li>
          No SSH connection is attempted. Host-key trust and
          auth-check are deliberate later slices.
        </li>
      </ul>

      <form
        class="flex flex-col gap-3"
        onsubmit={submitHost}
        data-testid="servers-create-host-form"
      >
        <label class="flex flex-col gap-1 text-sm text-zinc-200">
          <span class="text-xs uppercase tracking-wide text-zinc-400">
            Display name
          </span>
          <input
            type="text"
            class="rounded-md border border-zinc-700 bg-zinc-900 px-3 py-1.5 text-sm text-zinc-100 placeholder:text-zinc-600 focus:border-emerald-700 focus:outline-none disabled:opacity-50"
            bind:value={hostName}
            placeholder="e.g. Bastion (us-east-1)"
            maxlength="128"
            disabled={hostState.kind === "submitting"}
            data-testid="servers-create-host-display-name"
            autocomplete="off"
            spellcheck="false"
            required
          />
        </label>

        <label class="flex flex-col gap-1 text-sm text-zinc-200">
          <span class="text-xs uppercase tracking-wide text-zinc-400">
            Hostname or IP
          </span>
          <input
            type="text"
            class="rounded-md border border-zinc-700 bg-zinc-900 px-3 py-1.5 font-mono text-sm text-zinc-100 placeholder:text-zinc-600 focus:border-emerald-700 focus:outline-none disabled:opacity-50"
            bind:value={hostHostname}
            placeholder="bastion.example.internal"
            maxlength="253"
            disabled={hostState.kind === "submitting"}
            data-testid="servers-create-host-hostname"
            autocomplete="off"
            spellcheck="false"
            required
          />
        </label>

        <label class="flex flex-col gap-1 text-sm text-zinc-200">
          <span class="text-xs uppercase tracking-wide text-zinc-400">
            SSH port
          </span>
          <input
            type="number"
            class="rounded-md border border-zinc-700 bg-zinc-900 px-3 py-1.5 font-mono text-sm text-zinc-100 placeholder:text-zinc-600 focus:border-emerald-700 focus:outline-none disabled:opacity-50"
            bind:value={hostPort}
            min="1"
            max="65535"
            step="1"
            disabled={hostState.kind === "submitting"}
            data-testid="servers-create-host-port"
            required
          />
        </label>

        <label class="flex flex-col gap-1 text-sm text-zinc-200">
          <span class="text-xs uppercase tracking-wide text-zinc-400">
            Default username
          </span>
          <input
            type="text"
            class="rounded-md border border-zinc-700 bg-zinc-900 px-3 py-1.5 font-mono text-sm text-zinc-100 placeholder:text-zinc-600 focus:border-emerald-700 focus:outline-none disabled:opacity-50"
            bind:value={hostUsername}
            placeholder="deploy"
            maxlength="64"
            disabled={hostState.kind === "submitting"}
            data-testid="servers-create-host-username"
            autocomplete="off"
            spellcheck="false"
            required
          />
        </label>

        <div class="flex items-center gap-2">
          <button
            type="submit"
            class="rounded-md border border-emerald-700 bg-emerald-800 px-3 py-1.5 text-sm text-emerald-50 transition hover:border-emerald-600 hover:bg-emerald-700 disabled:opacity-50"
            disabled={hostSubmitDisabled}
            data-testid="servers-create-host-submit"
          >
            {hostState.kind === "submitting" ? "Creating…" : "Create host"}
          </button>
          {#if hostState.kind === "submitting"}
            <span class="text-xs text-zinc-400">Saving target…</span>
          {/if}
        </div>
      </form>

      {#if hostState.kind === "error"}
        <p
          class="rounded-md border border-rose-900/40 bg-rose-950/20 px-3 py-2 text-xs text-rose-200/80"
          data-testid="servers-create-host-error"
        >
          {hostState.summary}
        </p>
      {:else if hostState.kind === "success"}
        {@const created = hostState.host}
        <article
          class="flex flex-col gap-1 rounded-md border border-emerald-900/50 bg-emerald-950/30 p-4 text-sm text-emerald-50"
          data-testid="servers-create-host-success"
        >
          <span class="text-sm font-semibold">
            Host saved: {created.display_name}
          </span>
          <span class="font-mono text-xs text-emerald-100/80">
            {created.hostname}:{formatPort(created.port)} · user
            <span class="text-emerald-50">{created.default_username}</span>
          </span>
          <span class="text-xs text-emerald-200/70">
            Reachability and host-key trust are not verified by this
            action.
          </span>
        </article>
      {/if}
    </article>
  {/if}

  {#if panel === "profile"}
    <article
      class="flex flex-col gap-4 rounded-lg border border-emerald-900/40 bg-emerald-950/10 p-6"
      data-testid="servers-create-profile-panel"
    >
      <header class="flex items-baseline justify-between gap-2">
        <h3 class="text-sm font-semibold text-zinc-100">
          Create a server profile
        </h3>
        <button
          type="button"
          class="rounded-md border border-zinc-800 bg-zinc-900 px-2 py-1 text-xs text-zinc-300 transition hover:border-zinc-700 hover:bg-zinc-800 disabled:opacity-50"
          onclick={() => closePanel("profile")}
          disabled={profileState.kind === "submitting"}
          data-testid="servers-create-profile-close"
        >
          Close
        </button>
      </header>

      <ul class="flex flex-col gap-1 text-xs text-zinc-400">
        <li>
          A server profile binds a host, a username, and an SSH identity
          into a single connect target.
        </li>
        <li>
          Creating a profile does NOT trust the host key, does NOT
          verify SSH authentication, and does NOT install the public
          key on the target server. Run host-key trust and auth-check
          later (future slices).
        </li>
      </ul>

      {#if view.kind === "ready"}
        <form
          class="flex flex-col gap-3"
          onsubmit={submitProfile}
          data-testid="servers-create-profile-form"
        >
          <label class="flex flex-col gap-1 text-sm text-zinc-200">
            <span class="text-xs uppercase tracking-wide text-zinc-400">
              Name
            </span>
            <input
              type="text"
              class="rounded-md border border-zinc-700 bg-zinc-900 px-3 py-1.5 text-sm text-zinc-100 placeholder:text-zinc-600 focus:border-emerald-700 focus:outline-none disabled:opacity-50"
              bind:value={profileName}
              placeholder="e.g. Prod / us-east-1"
              maxlength="64"
              disabled={profileState.kind === "submitting"}
              data-testid="servers-create-profile-name"
              autocomplete="off"
              spellcheck="false"
              required
            />
          </label>

          <label class="flex flex-col gap-1 text-sm text-zinc-200">
            <span class="text-xs uppercase tracking-wide text-zinc-400">
              Host
            </span>
            <select
              class="rounded-md border border-zinc-700 bg-zinc-900 px-3 py-1.5 text-sm text-zinc-100 focus:border-emerald-700 focus:outline-none disabled:opacity-50"
              bind:value={profileHostId}
              disabled={profileState.kind === "submitting"}
              data-testid="servers-create-profile-host"
              required
            >
              <option value="" disabled>Select a host…</option>
              {#each view.hosts as host (host.id)}
                <option value={host.id}>
                  {host.display_name} — {host.hostname}:{formatPort(host.port)}
                </option>
              {/each}
            </select>
          </label>

          <label class="flex flex-col gap-1 text-sm text-zinc-200">
            <span class="text-xs uppercase tracking-wide text-zinc-400">
              SSH identity
            </span>
            <select
              class="rounded-md border border-zinc-700 bg-zinc-900 px-3 py-1.5 text-sm text-zinc-100 focus:border-emerald-700 focus:outline-none disabled:opacity-50"
              bind:value={profileIdentityId}
              disabled={profileState.kind === "submitting"}
              data-testid="servers-create-profile-identity"
              required
            >
              <option value="" disabled>Select an SSH identity…</option>
              {#each view.identities as identity (identity.id)}
                <option value={identity.id}>
                  {identity.name} ({identity.key_type})
                </option>
              {/each}
            </select>
          </label>

          <label class="flex flex-col gap-1 text-sm text-zinc-200">
            <span class="text-xs uppercase tracking-wide text-zinc-400">
              Username override (optional)
            </span>
            <input
              type="text"
              class="rounded-md border border-zinc-700 bg-zinc-900 px-3 py-1.5 font-mono text-sm text-zinc-100 placeholder:text-zinc-600 focus:border-emerald-700 focus:outline-none disabled:opacity-50"
              bind:value={profileUsernameOverride}
              placeholder="leave blank to use the host's default"
              maxlength="64"
              disabled={profileState.kind === "submitting"}
              data-testid="servers-create-profile-username-override"
              autocomplete="off"
              spellcheck="false"
            />
          </label>

          <label class="flex flex-col gap-1 text-sm text-zinc-200">
            <span class="text-xs uppercase tracking-wide text-zinc-400">
              Tags (optional, comma-separated)
            </span>
            <input
              type="text"
              class="rounded-md border border-zinc-700 bg-zinc-900 px-3 py-1.5 font-mono text-sm text-zinc-100 placeholder:text-zinc-600 focus:border-emerald-700 focus:outline-none disabled:opacity-50"
              bind:value={profileTagsInput}
              placeholder="e.g. prod, us-east-1"
              disabled={profileState.kind === "submitting"}
              data-testid="servers-create-profile-tags"
              autocomplete="off"
              spellcheck="false"
            />
            <span class="text-[11px] text-zinc-500">
              Letters, digits, '-' and '_' only. Max 32 tags.
            </span>
          </label>

          <div class="flex items-center gap-2">
            <button
              type="submit"
              class="rounded-md border border-emerald-700 bg-emerald-800 px-3 py-1.5 text-sm text-emerald-50 transition hover:border-emerald-600 hover:bg-emerald-700 disabled:opacity-50"
              disabled={profileSubmitDisabled}
              data-testid="servers-create-profile-submit"
            >
              {profileState.kind === "submitting"
                ? "Creating…"
                : "Create profile"}
            </button>
            {#if profileState.kind === "submitting"}
              <span class="text-xs text-zinc-400">Saving profile…</span>
            {/if}
          </div>
        </form>
      {/if}

      {#if profileState.kind === "error"}
        <p
          class="rounded-md border border-rose-900/40 bg-rose-950/20 px-3 py-2 text-xs text-rose-200/80"
          data-testid="servers-create-profile-error"
        >
          {profileState.summary}
        </p>
      {:else if profileState.kind === "success"}
        {@const created = profileState.profile}
        <article
          class="flex flex-col gap-1 rounded-md border border-emerald-900/50 bg-emerald-950/30 p-4 text-sm text-emerald-50"
          data-testid="servers-create-profile-success"
        >
          <span class="text-sm font-semibold">
            Profile saved: {created.name}
          </span>
          <span class="text-xs text-emerald-200/70">
            The host key is not yet trusted and SSH authentication has
            not been verified for this profile.
          </span>
        </article>
      {/if}
    </article>
  {/if}

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
          No hosts yet. Use “Create host” above to add one.
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
          No server profiles yet. Use “Create server profile” above to
          add one — at least one host AND one SSH identity must exist
          first.
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
              <HostKeyPanel profileId={profile.id} />
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
    Edit / delete forms, auth-check, and terminal launch land alongside
    the production terminal workspace. Host-key preflight and trust
    are above; SSH authentication has not been verified by either.
  </p>
</section>
