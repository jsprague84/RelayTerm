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
  import {
    createTerminalSession,
  } from "../../api/terminalSessions.js";
  import { describeLaunchError } from "../terminal/terminalLaunch.js";
  import type { ActiveLaunch } from "../terminal/activeLaunch.js";
  import HostKeyPanel from "./HostKeyPanel.svelte";
  import AuthCheckPanel from "./AuthCheckPanel.svelte";
  import {
    describeReadinessFromKnownState,
    hostProfileCount,
    relatedProfilesForHost,
    resolveProfileDetail,
    safeDisplayValue,
    shortId,
  } from "../inventory/inventoryDetails.js";
  import {
    collectProfileTags,
    countFilteredResults,
    filterHosts,
    filterProfiles,
  } from "../inventory/inventoryFilters.js";

  interface Props {
    /**
     * Hand a successful launch back to the parent shell. The shell is
     * responsible for switching to the Terminal view; this component
     * only owns the create call and the per-row launch state.
     */
    onLaunch?: (launch: ActiveLaunch) => void;
  }

  let { onLaunch }: Props = $props();

  /**
   * Per-profile launch state. Keyed on `server_profile_id` so each row
   * tracks its own button state independently — a launch on one profile
   * must not freeze every other row's button. `idle` is the implicit
   * default; absence of an entry means "not in flight, no error
   * pending."
   */
  type ProfileLaunchState =
    | { kind: "submitting" }
    | { kind: "error"; summary: string };

  let launchStates = $state<Record<string, ProfileLaunchState>>({});

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

  /**
   * Currently-selected detail target. Selection is mutually exclusive
   * across host vs. profile so the operator only sees one detail panel
   * at a time. A re-click on the same row closes the panel.
   */
  let selectedHostId = $state<string | null>(null);
  let selectedProfileId = $state<string | null>(null);

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

  async function launchProfile(profile: ServerProfile) {
    const existing = launchStates[profile.id];
    if (existing?.kind === "submitting") return;
    launchStates = { ...launchStates, [profile.id]: { kind: "submitting" } };
    // Cols/rows are intentionally omitted: the helper falls through to
    // the wire-stable 80×24 defaults, and the workspace reads
    // `result.session.cols/rows` back to seed the renderer. Resize-to-fit
    // on mount is future work; until then, the row's create dims are the
    // canonical pair the workspace uses.
    const result = await createTerminalSession({
      server_profile_id: profile.id,
    });
    if (!result.ok) {
      launchStates = {
        ...launchStates,
        [profile.id]: {
          kind: "error",
          summary: describeLaunchError(result.error),
        },
      };
      return;
    }
    // Drop the per-row state on success — the Terminal view owns the
    // attachment from here. Leaving a stale `submitting` would freeze
    // the button if the operator returns to this view via "Back to
    // servers" while the session is still alive.
    const next = { ...launchStates };
    delete next[profile.id];
    launchStates = next;
    onLaunch?.({
      sessionId: result.session.id,
      cols: result.session.cols,
      rows: result.session.rows,
      profileLabel: profile.name,
    });
  }

  function dismissLaunchError(profileId: string) {
    if (launchStates[profileId]?.kind !== "error") return;
    const next = { ...launchStates };
    delete next[profileId];
    launchStates = next;
  }

  function selectHost(id: string) {
    selectedHostId = selectedHostId === id ? null : id;
    selectedProfileId = null;
  }

  function selectProfile(id: string) {
    selectedProfileId = selectedProfileId === id ? null : id;
    selectedHostId = null;
  }

  function closeHostDetail() {
    selectedHostId = null;
  }

  function closeProfileDetail() {
    selectedProfileId = null;
  }

  let selectedHost = $derived.by<Host | null>(() => {
    if (view.kind !== "ready" || selectedHostId === null) return null;
    return view.hosts.find((h) => h.id === selectedHostId) ?? null;
  });

  let selectedProfile = $derived.by<ServerProfile | null>(() => {
    if (view.kind !== "ready" || selectedProfileId === null) return null;
    return view.profiles.find((p) => p.id === selectedProfileId) ?? null;
  });

  // ----------------------------------------------------------------
  // Client-side search & filter state.
  //
  // In-memory only: the helpers below operate over `view.hosts` /
  // `view.profiles` already loaded by `load()`. There is no backend
  // search and no URL/localStorage persistence — a refresh resets the
  // filters to "all rows visible". Per AGENTS.md the filter helpers
  // never mutate the loaded data.
  //
  // Selection vs. filter: the row click toggles `selectedHostId` /
  // `selectedProfileId` directly, so a row that is currently filtered
  // out of the visible list still keeps its detail panel open. The
  // panel renders a "currently hidden by filters" notice in that case
  // so the operator is not confused about why the row no longer shows
  // in the list above.
  // ----------------------------------------------------------------

  let hostSearch = $state("");
  let profileSearch = $state("");
  let profileTagFilter = $state("");

  let availableTags = $derived.by<string[]>(() => {
    if (view.kind !== "ready") return [];
    return collectProfileTags(view.profiles);
  });

  // If the active tag is no longer present (e.g. the only profile
  // bearing it was deleted via a future flow, or the load returned a
  // narrower set), drop the filter so the dropdown does not display
  // an orphan selection.
  $effect(() => {
    if (
      profileTagFilter.length > 0 &&
      view.kind === "ready" &&
      !availableTags.includes(profileTagFilter)
    ) {
      profileTagFilter = "";
    }
  });

  let filteredHosts = $derived.by<Host[]>(() => {
    if (view.kind !== "ready") return [];
    return filterHosts(view.hosts, hostSearch);
  });

  let filteredProfiles = $derived.by<ServerProfile[]>(() => {
    if (view.kind !== "ready") return [];
    return filterProfiles(view.profiles, view.hosts, view.identities, {
      query: profileSearch,
      tag: profileTagFilter,
    });
  });

  let hostsAreFiltered = $derived(
    view.kind === "ready" && hostSearch.trim().length > 0,
  );
  let profilesAreFiltered = $derived(
    view.kind === "ready" &&
      (profileSearch.trim().length > 0 || profileTagFilter.length > 0),
  );

  let anyFilterActive = $derived(hostsAreFiltered || profilesAreFiltered);

  let selectedHostHidden = $derived(
    selectedHost !== null &&
      hostsAreFiltered &&
      !filteredHosts.some((h) => h.id === selectedHost?.id),
  );
  let selectedProfileHidden = $derived(
    selectedProfile !== null &&
      profilesAreFiltered &&
      !filteredProfiles.some((p) => p.id === selectedProfile?.id),
  );

  function clearFilters() {
    hostSearch = "";
    profileSearch = "";
    profileTagFilter = "";
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
      Hosts are reachable target definitions. Server profiles bind a
      host to an SSH identity. Run host-key preflight per profile to
      capture and explicitly trust the server's host key, then run
      auth-check to confirm the configured SSH identity authenticates.
      Terminal launch is future work — creating, trusting, or running
      auth-check here does NOT open a terminal, run commands, or install
      the public key on the target.
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
          auth-check happen per-profile (panels appear under each
          profile row after creation).
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
          key on the target server. Run host-key trust and then
          auth-check on the new profile row after it appears.
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
      class="flex flex-col gap-3 rounded-lg border border-zinc-800 bg-zinc-950/40 p-4"
      data-testid="servers-filter-toolbar"
    >
      <header class="flex items-baseline justify-between gap-2">
        <h3 class="text-sm font-semibold text-zinc-100">Filter inventory</h3>
        <span class="text-xs text-zinc-500">
          In-memory only · no backend search
        </span>
      </header>
      <div class="grid gap-3 sm:grid-cols-3">
        <label class="flex flex-col gap-1 text-xs text-zinc-300">
          <span class="uppercase tracking-wide text-zinc-500">
            Search hosts
          </span>
          <input
            type="search"
            class="rounded-md border border-zinc-700 bg-zinc-900 px-2.5 py-1.5 text-sm text-zinc-100 placeholder:text-zinc-600 focus:border-emerald-700 focus:outline-none"
            bind:value={hostSearch}
            placeholder="display name, hostname, port, user"
            autocomplete="off"
            spellcheck="false"
            data-testid="servers-host-search"
          />
        </label>
        <label class="flex flex-col gap-1 text-xs text-zinc-300">
          <span class="uppercase tracking-wide text-zinc-500">
            Search profiles
          </span>
          <input
            type="search"
            class="rounded-md border border-zinc-700 bg-zinc-900 px-2.5 py-1.5 text-sm text-zinc-100 placeholder:text-zinc-600 focus:border-emerald-700 focus:outline-none"
            bind:value={profileSearch}
            placeholder="name, tag, user, host, identity"
            autocomplete="off"
            spellcheck="false"
            data-testid="servers-profile-search"
          />
        </label>
        <label class="flex flex-col gap-1 text-xs text-zinc-300">
          <span class="uppercase tracking-wide text-zinc-500">
            Profile tag
          </span>
          <select
            class="rounded-md border border-zinc-700 bg-zinc-900 px-2.5 py-1.5 text-sm text-zinc-100 focus:border-emerald-700 focus:outline-none disabled:opacity-60"
            bind:value={profileTagFilter}
            disabled={availableTags.length === 0}
            data-testid="servers-profile-tag-filter"
          >
            <option value="">All tags</option>
            {#each availableTags as tag (tag)}
              <option value={tag}>{tag}</option>
            {/each}
          </select>
        </label>
      </div>
      <div class="flex flex-wrap items-center justify-between gap-2 text-xs text-zinc-400">
        <span>
          {availableTags.length === 0
            ? "No profile tags in current inventory."
            : `${availableTags.length} tag${availableTags.length === 1 ? "" : "s"} in current inventory.`}
        </span>
        <button
          type="button"
          class="rounded-md border border-zinc-700 bg-zinc-800 px-2.5 py-1 text-xs text-zinc-200 transition hover:border-zinc-600 hover:bg-zinc-700 disabled:cursor-not-allowed disabled:opacity-50"
          onclick={clearFilters}
          disabled={!anyFilterActive}
          data-testid="servers-clear-filters"
        >
          Clear filters
        </button>
      </div>
    </article>

    <article
      class="flex flex-col gap-3 rounded-lg border border-zinc-800 bg-zinc-950/40 p-6"
    >
      <header class="flex items-baseline justify-between gap-2">
        <h3 class="text-sm font-semibold text-zinc-100">Hosts</h3>
        <span class="text-xs text-zinc-500" data-testid="hosts-count">
          {countFilteredResults(filteredHosts.length, view.hosts.length, "host")}
        </span>
      </header>
      {#if view.hosts.length === 0}
        <p class="text-sm text-zinc-400" data-testid="hosts-empty">
          No hosts yet. Use “Create host” above to add one.
        </p>
      {:else if filteredHosts.length === 0}
        <p
          class="text-sm text-zinc-400"
          data-testid="hosts-filter-empty"
        >
          No hosts match this filter.
        </p>
      {:else}
        <ul
          class="flex flex-col divide-y divide-zinc-800/60"
          data-testid="hosts-list"
        >
          {#each filteredHosts as host (host.id)}
            {@const isSelected = selectedHostId === host.id}
            <li
              class="flex flex-col py-3 first:pt-0 last:pb-0"
              data-testid="host-row"
            >
              <button
                type="button"
                class="flex flex-col gap-1 rounded-md px-2 py-1 text-left transition focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-700/60 {isSelected
                  ? 'bg-emerald-950/30 ring-1 ring-emerald-800/60'
                  : 'hover:bg-zinc-900/40'}"
                onclick={() => selectHost(host.id)}
                aria-expanded={isSelected}
                data-testid="host-row-select"
              >
                <span class="flex items-baseline justify-between gap-3">
                  <span class="text-sm font-medium text-zinc-100">
                    {host.display_name}
                  </span>
                  <span class="font-mono text-xs text-zinc-500">
                    {host.hostname}:{formatPort(host.port)}
                  </span>
                </span>
                <span class="text-xs text-zinc-400">
                  Default user
                  <span class="font-mono text-zinc-300"
                    >{host.default_username}</span
                  >
                </span>
              </button>
            </li>
          {/each}
        </ul>
      {/if}
    </article>

    {#if selectedHost}
      {@const host = selectedHost}
      {@const linkedProfiles = relatedProfilesForHost(host, view.profiles)}
      <article
        class="flex flex-col gap-3 rounded-lg border border-emerald-900/40 bg-emerald-950/10 p-6"
        data-testid="host-detail-panel"
      >
        <header class="flex items-baseline justify-between gap-2">
          <h3 class="text-sm font-semibold text-zinc-100">
            Host detail
            <span class="ml-2 text-xs font-normal text-zinc-500">
              read-only
            </span>
          </h3>
          <button
            type="button"
            class="rounded-md border border-zinc-800 bg-zinc-900 px-2 py-1 text-xs text-zinc-300 transition hover:border-zinc-700 hover:bg-zinc-800"
            onclick={closeHostDetail}
            data-testid="host-detail-close"
          >
            Close
          </button>
        </header>

        {#if selectedHostHidden}
          <p
            class="rounded-md border border-amber-900/40 bg-amber-950/20 px-3 py-2 text-xs text-amber-200/80"
            data-testid="host-detail-hidden-by-filter"
          >
            This host is currently hidden by your filters. Clear the
            host search to bring it back into the list.
          </p>
        {/if}

        <dl class="grid grid-cols-[max-content_1fr] gap-x-4 gap-y-2 text-sm">
          <dt class="text-xs uppercase tracking-wide text-zinc-500">
            Display name
          </dt>
          <dd class="text-zinc-100" data-testid="host-detail-display-name">
            {host.display_name}
          </dd>
          <dt class="text-xs uppercase tracking-wide text-zinc-500">
            Hostname
          </dt>
          <dd
            class="font-mono text-zinc-100"
            data-testid="host-detail-hostname"
          >
            {host.hostname}
          </dd>
          <dt class="text-xs uppercase tracking-wide text-zinc-500">Port</dt>
          <dd class="font-mono text-zinc-100" data-testid="host-detail-port">
            {formatPort(host.port)}
          </dd>
          <dt class="text-xs uppercase tracking-wide text-zinc-500">
            Default user
          </dt>
          <dd
            class="font-mono text-zinc-100"
            data-testid="host-detail-username"
          >
            {host.default_username}
          </dd>
          <dt class="text-xs uppercase tracking-wide text-zinc-500">Created</dt>
          <dd
            class="font-mono text-zinc-300"
            data-testid="host-detail-created-at"
          >
            {safeDisplayValue(host.created_at)}
          </dd>
          <dt class="text-xs uppercase tracking-wide text-zinc-500">Updated</dt>
          <dd
            class="font-mono text-zinc-300"
            data-testid="host-detail-updated-at"
          >
            {safeDisplayValue(host.updated_at)}
          </dd>
          <dt class="text-xs uppercase tracking-wide text-zinc-500">Id</dt>
          <dd class="font-mono text-xs text-zinc-500" data-testid="host-detail-id">
            {shortId(host.id)}
          </dd>
        </dl>

        <section class="flex flex-col gap-2">
          <header class="flex items-baseline justify-between gap-2">
            <h4 class="text-xs uppercase tracking-wide text-zinc-400">
              Server profiles using this host
            </h4>
            <span
              class="text-xs text-zinc-500"
              data-testid="host-detail-profile-count"
            >
              {hostProfileCount(host, view.profiles)}
            </span>
          </header>
          {#if linkedProfiles.length === 0}
            <p
              class="text-xs text-zinc-500"
              data-testid="host-detail-profiles-empty"
            >
              No profiles reference this host yet.
            </p>
          {:else}
            <ul
              class="flex flex-col gap-1"
              data-testid="host-detail-profiles-list"
            >
              {#each linkedProfiles as p (p.id)}
                <li
                  class="flex items-baseline justify-between gap-3 rounded-sm border border-zinc-800/60 bg-zinc-950/60 px-2 py-1.5 text-xs"
                >
                  <span class="text-zinc-200">{p.name}</span>
                  <span class="font-mono text-zinc-500">
                    {safeDisplayValue(p.username_override, "(host default)")}
                  </span>
                </li>
              {/each}
            </ul>
          {/if}
        </section>

        <p
          class="rounded-md border border-amber-900/40 bg-amber-950/20 px-3 py-2 text-xs text-amber-200/80"
          data-testid="host-detail-honesty"
        >
          Host details do not prove reachability. Connection readiness
          depends on a server profile, host-key trust, and an SSH
          auth-check — run those from the profile's row.
        </p>
      </article>
    {/if}

    <article
      class="flex flex-col gap-3 rounded-lg border border-zinc-800 bg-zinc-950/40 p-6"
    >
      <header class="flex items-baseline justify-between gap-2">
        <h3 class="text-sm font-semibold text-zinc-100">Profiles</h3>
        <span class="text-xs text-zinc-500" data-testid="profiles-count">
          {countFilteredResults(
            filteredProfiles.length,
            view.profiles.length,
            "profile",
          )}
        </span>
      </header>
      {#if view.profiles.length === 0}
        <p class="text-sm text-zinc-400" data-testid="profiles-empty">
          No server profiles yet. Use “Create server profile” above to
          add one — at least one host AND one SSH identity must exist
          first.
        </p>
      {:else if filteredProfiles.length === 0}
        <p
          class="text-sm text-zinc-400"
          data-testid="profiles-filter-empty"
        >
          No profiles match this filter.
        </p>
      {:else}
        <ul
          class="flex flex-col divide-y divide-zinc-800/60"
          data-testid="profiles-list"
        >
          {#each filteredProfiles as profile (profile.id)}
            {@const links = resolveProfileLinks(profile, view.hosts)}
            {@const launchState = launchStates[profile.id]}
            {@const isProfileSelected = selectedProfileId === profile.id}
            <li
              class="flex flex-col gap-1.5 py-3 first:pt-0 last:pb-0"
              data-testid="profile-row"
            >
              <button
                type="button"
                class="flex items-baseline justify-between gap-3 rounded-md px-2 py-1 text-left transition focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-700/60 {isProfileSelected
                  ? 'bg-emerald-950/30 ring-1 ring-emerald-800/60'
                  : 'hover:bg-zinc-900/40'}"
                onclick={() => selectProfile(profile.id)}
                aria-expanded={isProfileSelected}
                data-testid="profile-row-select"
              >
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
              </button>
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
              <AuthCheckPanel profileId={profile.id} />
              <div class="flex flex-wrap items-center gap-2">
                <button
                  type="button"
                  class="rounded-md border border-emerald-700/60 bg-emerald-900/20 px-3 py-1 text-xs text-emerald-100 transition hover:border-emerald-600 hover:bg-emerald-900/40 disabled:cursor-not-allowed disabled:opacity-50"
                  onclick={() => void launchProfile(profile)}
                  disabled={launchState?.kind === "submitting"}
                  data-testid="profile-launch-terminal"
                  title="Create a terminal session and open the Terminal workspace. Run host-key trust + auth-check first; the backend will refuse otherwise."
                >
                  {launchState?.kind === "submitting"
                    ? "Launching…"
                    : "Launch terminal"}
                </button>
                <span class="text-[11px] text-zinc-500">
                  Launch is enabled by host-key trust and SSH auth-check —
                  run those above first if the launch is refused.
                </span>
              </div>
              {#if launchState?.kind === "error"}
                <p
                  class="flex items-center justify-between gap-2 rounded-md border border-rose-900/40 bg-rose-950/20 px-3 py-2 text-xs text-rose-200/80"
                  data-testid="profile-launch-error"
                >
                  <span>{launchState.summary}</span>
                  <button
                    type="button"
                    class="rounded-sm border border-rose-900/60 bg-rose-950/40 px-2 py-0.5 text-[11px] text-rose-100 hover:bg-rose-900/40"
                    onclick={() => dismissLaunchError(profile.id)}
                    data-testid="profile-launch-error-dismiss"
                  >
                    Dismiss
                  </button>
                </p>
              {/if}
            </li>
          {/each}
        </ul>
      {/if}
    </article>

    {#if selectedProfile}
      {@const detail = resolveProfileDetail(
        selectedProfile,
        view.hosts,
        view.identities,
      )}
      {@const readiness = describeReadinessFromKnownState(detail)}
      <article
        class="flex flex-col gap-3 rounded-lg border border-emerald-900/40 bg-emerald-950/10 p-6"
        data-testid="profile-detail-panel"
      >
        <header class="flex items-baseline justify-between gap-2">
          <h3 class="text-sm font-semibold text-zinc-100">
            Server profile detail
            <span class="ml-2 text-xs font-normal text-zinc-500">
              read-only
            </span>
          </h3>
          <button
            type="button"
            class="rounded-md border border-zinc-800 bg-zinc-900 px-2 py-1 text-xs text-zinc-300 transition hover:border-zinc-700 hover:bg-zinc-800"
            onclick={closeProfileDetail}
            data-testid="profile-detail-close"
          >
            Close
          </button>
        </header>

        {#if selectedProfileHidden}
          <p
            class="rounded-md border border-amber-900/40 bg-amber-950/20 px-3 py-2 text-xs text-amber-200/80"
            data-testid="profile-detail-hidden-by-filter"
          >
            This profile is currently hidden by your filters. Clear the
            profile search or tag filter to bring it back into the list.
          </p>
        {/if}

        <dl class="grid grid-cols-[max-content_1fr] gap-x-4 gap-y-2 text-sm">
          <dt class="text-xs uppercase tracking-wide text-zinc-500">Name</dt>
          <dd class="text-zinc-100" data-testid="profile-detail-name">
            {detail.profile.name}
          </dd>
          <dt class="text-xs uppercase tracking-wide text-zinc-500">Host</dt>
          <dd data-testid="profile-detail-host">
            {#if detail.links.host}
              <span class="font-mono text-zinc-100">
                {detail.links.host.display_name} —
                {detail.links.host.hostname}:{formatPort(
                  detail.links.host.port,
                )}
              </span>
            {:else}
              <span
                class="text-amber-300/80"
                data-testid="profile-detail-host-missing"
              >
                host not in your inventory
              </span>
            {/if}
          </dd>
          <dt class="text-xs uppercase tracking-wide text-zinc-500">User</dt>
          <dd data-testid="profile-detail-username">
            {#if detail.links.effectiveUsername !== null}
              <span class="font-mono text-zinc-100">
                {detail.links.effectiveUsername}
              </span>
              {#if detail.links.inheritedFromHost}
                <span class="text-xs text-zinc-500">(host default)</span>
              {:else}
                <span class="text-xs text-zinc-500">(override)</span>
              {/if}
            {:else}
              <span class="text-amber-300/80">
                Username unavailable (host link unresolved)
              </span>
            {/if}
          </dd>
          <dt class="text-xs uppercase tracking-wide text-zinc-500">
            SSH identity
          </dt>
          <dd data-testid="profile-detail-identity">
            {#if detail.identity}
              <div class="flex flex-col gap-0.5">
                <span class="text-zinc-100">
                  {detail.identity.name}
                  <span
                    class="ml-1 font-mono text-xs uppercase tracking-wide text-zinc-500"
                  >
                    {detail.identity.key_type}
                  </span>
                </span>
                <span class="font-mono text-xs text-zinc-400">
                  {detail.identity.fingerprint_sha256}
                </span>
              </div>
            {:else}
              <span class="text-zinc-400">
                Identity not in your inventory — metadata available in
                the SSH Identities view.
              </span>
            {/if}
          </dd>
          {#if detail.profile.tags.length > 0}
            <dt class="text-xs uppercase tracking-wide text-zinc-500">Tags</dt>
            <dd>
              <ul
                class="flex flex-wrap gap-1.5"
                data-testid="profile-detail-tags"
              >
                {#each detail.profile.tags as tag (tag)}
                  <li
                    class="rounded border border-zinc-700/80 bg-zinc-900/60 px-1.5 py-0.5 font-mono text-[11px] text-zinc-300"
                  >
                    {tag}
                  </li>
                {/each}
              </ul>
            </dd>
          {/if}
          <dt class="text-xs uppercase tracking-wide text-zinc-500">
            Last connected
          </dt>
          <dd
            class="font-mono text-zinc-300"
            data-testid="profile-detail-last-connected"
          >
            {safeDisplayValue(detail.profile.last_connected_at, "never")}
          </dd>
          <dt class="text-xs uppercase tracking-wide text-zinc-500">Created</dt>
          <dd
            class="font-mono text-zinc-300"
            data-testid="profile-detail-created-at"
          >
            {safeDisplayValue(detail.profile.created_at)}
          </dd>
          <dt class="text-xs uppercase tracking-wide text-zinc-500">Updated</dt>
          <dd
            class="font-mono text-zinc-300"
            data-testid="profile-detail-updated-at"
          >
            {safeDisplayValue(detail.profile.updated_at)}
          </dd>
          <dt class="text-xs uppercase tracking-wide text-zinc-500">Id</dt>
          <dd
            class="font-mono text-xs text-zinc-500"
            data-testid="profile-detail-id"
          >
            {shortId(detail.profile.id)}
          </dd>
        </dl>

        <p
          class="rounded-md border border-amber-900/40 bg-amber-950/20 px-3 py-2 text-xs text-amber-200/80"
          data-testid="profile-detail-readiness"
        >
          {readiness.advisory}
        </p>
      </article>
    {/if}
  {/if}

  <p
    class="rounded-md border border-amber-900/40 bg-amber-950/20 px-3 py-2 text-xs text-amber-200/80"
  >
    <span class="font-mono uppercase tracking-wide">future work</span> ·
    Edit / delete forms land in a later slice. Launch starts a live SSH
    PTY using the xterm baseline renderer; detached sessions survive
    only briefly (~30s) and replay is in-memory only — not durable
    across a backend restart.
  </p>
</section>
