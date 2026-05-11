<script lang="ts">
  import {
    createSshIdentity,
    describeCreateSshIdentityError,
    listSshIdentities,
    publicKeyPreview,
    SUPPORTED_GENERATION_KEY_TYPES,
    type SshIdentity,
    type SshKeyType,
  } from "../../api/sshIdentities.js";
  import { describeLoadError } from "../../api/apiErrors.js";
  import {
    identityPublicDetail,
    publicKeyCopyValue,
    safeDisplayValue,
    shortId,
  } from "../inventory/inventoryDetails.js";
  import {
    countFilteredResults,
    filterIdentities,
  } from "../inventory/inventoryFilters.js";

  type LoadState =
    | { kind: "idle" }
    | { kind: "loading" }
    | { kind: "ready"; identities: SshIdentity[] }
    | { kind: "error"; summary: string };

  type CopyState = "idle" | "copied" | "failed";

  type GenerateState =
    | { kind: "idle" }
    | { kind: "submitting" }
    | { kind: "success"; identity: SshIdentity }
    | { kind: "error"; summary: string };

  let view = $state<LoadState>({ kind: "idle" });
  let copy = $state<Record<string, CopyState>>({});

  /**
   * Currently-selected identity row. A re-click on the same row closes
   * the panel; selecting a different row swaps the panel content. The
   * generation panel is independent — selection does not close it.
   */
  let selectedIdentityId = $state<string | null>(null);

  let panelOpen = $state(false);
  let formName = $state("");
  let formKeyType = $state<SshKeyType>(SUPPORTED_GENERATION_KEY_TYPES[0]);
  let generate = $state<GenerateState>({ kind: "idle" });

  async function load() {
    view = { kind: "loading" };
    const result = await listSshIdentities();
    if (!result.ok) {
      view = {
        kind: "error",
        summary: describeLoadError("SSH identities", result.error),
      };
      return;
    }
    view = { kind: "ready", identities: result.data };
  }

  // One-shot mount load: the body reads no reactive state, so Svelte
  // runs it once on mount. The explicit reload path is the "Refresh"
  // button below.
  $effect(() => {
    void load();
  });

  async function copyPublicKey(identity: SshIdentity) {
    // Copy ONLY the public key string — never the fingerprint, never
    // any other field. The button label must stay clear about this.
    try {
      const clipboard = navigator?.clipboard;
      if (!clipboard) {
        copy = { ...copy, [identity.id]: "failed" };
        return;
      }
      await clipboard.writeText(identity.public_key);
      copy = { ...copy, [identity.id]: "copied" };
      // Reset after a short delay; safe — the value never echoes
      // outside this state record.
      setTimeout(() => {
        if (copy[identity.id] === "copied") {
          copy = { ...copy, [identity.id]: "idle" };
        }
      }, 1500);
    } catch {
      // Swallow the error — clipboard failure detail is not useful in
      // the UI and might smuggle origin/permission detail into a label.
      copy = { ...copy, [identity.id]: "failed" };
    }
  }

  function copyLabel(s: CopyState | undefined): string {
    if (s === "copied") return "Copied!";
    if (s === "failed") return "Copy failed";
    return "Copy public key";
  }

  function selectIdentity(id: string) {
    selectedIdentityId = selectedIdentityId === id ? null : id;
  }

  function closeIdentityDetail() {
    selectedIdentityId = null;
  }

  let selectedIdentity = $derived.by<SshIdentity | null>(() => {
    if (view.kind !== "ready" || selectedIdentityId === null) return null;
    return view.identities.find((i) => i.id === selectedIdentityId) ?? null;
  });

  // ----------------------------------------------------------------
  // Client-side search & filter state.
  //
  // In-memory only over `view.identities` already loaded by `load()`.
  // No backend search, no URL/localStorage persistence — a refresh
  // resets the filters. The OpenSSH `public_key` body is deliberately
  // NOT a searchable field; a search string can only match on name,
  // fingerprint, and key type (per the helper's haystack).
  //
  // The key-type select is only rendered when more than one key type
  // is present in the loaded list — there is no useful "filter to the
  // only key type" affordance for a single-type inventory.
  // ----------------------------------------------------------------

  let identitySearch = $state("");
  let keyTypeFilter = $state<SshKeyType | "">("");

  let availableKeyTypes = $derived.by<SshKeyType[]>(() => {
    if (view.kind !== "ready") return [];
    const seen = new Set<SshKeyType>();
    for (const identity of view.identities) seen.add(identity.key_type);
    return Array.from(seen).sort();
  });

  // Drop a stale key-type filter if the only identity bearing it
  // disappears from the loaded list (defends against a future delete
  // flow without changing the helper contract).
  $effect(() => {
    if (
      keyTypeFilter !== "" &&
      view.kind === "ready" &&
      !availableKeyTypes.includes(keyTypeFilter)
    ) {
      keyTypeFilter = "";
    }
  });

  let filteredIdentities = $derived.by<SshIdentity[]>(() => {
    if (view.kind !== "ready") return [];
    return filterIdentities(view.identities, {
      query: identitySearch,
      keyType: keyTypeFilter === "" ? null : keyTypeFilter,
    });
  });

  let identitiesAreFiltered = $derived(
    view.kind === "ready" &&
      (identitySearch.trim().length > 0 || keyTypeFilter !== ""),
  );

  let selectedIdentityHidden = $derived(
    selectedIdentity !== null &&
      identitiesAreFiltered &&
      !filteredIdentities.some((i) => i.id === selectedIdentity?.id),
  );

  function clearIdentityFilters() {
    identitySearch = "";
    keyTypeFilter = "";
  }

  function openPanel() {
    panelOpen = true;
    if (generate.kind !== "submitting") {
      generate = { kind: "idle" };
    }
  }

  function closePanel() {
    if (generate.kind === "submitting") return;
    panelOpen = false;
    formName = "";
    formKeyType = SUPPORTED_GENERATION_KEY_TYPES[0];
    generate = { kind: "idle" };
  }

  async function submitGenerate(event: Event) {
    event.preventDefault();
    if (generate.kind === "submitting") return;
    generate = { kind: "submitting" };
    const result = await createSshIdentity({
      name: formName,
      key_type: formKeyType,
    });
    if (!result.ok) {
      // describeCreateSshIdentityError is the only redaction-safe
      // formatter — never echo `result.error.message` directly.
      generate = {
        kind: "error",
        summary: describeCreateSshIdentityError(result.error),
      };
      return;
    }
    // The parser already dropped any private_key / encrypted_private_key
    // field that might have been on the wire. Appending the parsed DTO
    // directly is safe and avoids a second list round-trip.
    if (view.kind === "ready") {
      const exists = view.identities.some((i) => i.id === result.identity.id);
      view = exists
        ? view
        : { kind: "ready", identities: [result.identity, ...view.identities] };
    } else {
      // List was loading or errored — refetch so the list catches up.
      void load();
    }
    generate = { kind: "success", identity: result.identity };
    formName = "";
    // Leave the panel open so the success card (with the public-key copy
    // action) is visible. The user closes it deliberately.
  }
</script>

<section
  class="flex flex-col gap-6"
  data-testid="production-view-identities"
>
  <header class="flex flex-col gap-1">
    <h2 class="text-lg font-semibold tracking-tight text-zinc-100">
      SSH identities
    </h2>
    <p class="text-sm text-zinc-400">
      Vault-managed SSH keypairs. The private key is generated and
      encrypted on the backend and is never rendered, copied, or logged
      on the client. Public material only.
    </p>
  </header>

  <div class="flex items-center gap-2">
    <button
      type="button"
      class="rounded-md border border-zinc-700 bg-zinc-800 px-3 py-1.5 text-sm text-zinc-100 transition hover:border-zinc-600 hover:bg-zinc-700 disabled:opacity-50"
      onclick={load}
      disabled={view.kind === "loading"}
      data-testid="identities-refresh-button"
    >
      {view.kind === "loading" ? "Loading…" : "Refresh"}
    </button>
    {#if !panelOpen}
      <button
        type="button"
        class="rounded-md border border-emerald-800/60 bg-emerald-900/20 px-3 py-1.5 text-sm text-emerald-100 transition hover:border-emerald-700 hover:bg-emerald-900/40"
        onclick={openPanel}
        data-testid="identities-generate-open"
      >
        Generate SSH identity
      </button>
    {/if}
  </div>

  {#if panelOpen}
    <article
      class="flex flex-col gap-4 rounded-lg border border-emerald-900/40 bg-emerald-950/10 p-6"
      data-testid="identities-generate-panel"
    >
      <header class="flex items-baseline justify-between gap-2">
        <h3 class="text-sm font-semibold text-zinc-100">
          Generate a new SSH identity
        </h3>
        <button
          type="button"
          class="rounded-md border border-zinc-800 bg-zinc-900 px-2 py-1 text-xs text-zinc-300 transition hover:border-zinc-700 hover:bg-zinc-800 disabled:opacity-50"
          onclick={closePanel}
          disabled={generate.kind === "submitting"}
          data-testid="identities-generate-close"
        >
          Close
        </button>
      </header>

      <ul class="flex flex-col gap-1 text-xs text-zinc-400">
        <li>
          RelayTerm generates the keypair on the backend inside the
          vault. The private key is encrypted at rest with the master
          key and never reaches the browser.
        </li>
        <li>
          After generation, copy the public key and append it to the
          target server's <code class="font-mono text-zinc-300"
            >~/.ssh/authorized_keys</code
          > manually. Password bootstrap and <code
            class="font-mono text-zinc-300">ssh-copy-id</code
          > automation are deliberate later slices.
        </li>
        <li>
          The private key cannot be exported or recovered through the
          UI today.
        </li>
      </ul>

      <form
        class="flex flex-col gap-3"
        onsubmit={submitGenerate}
        data-testid="identities-generate-form"
      >
        <label class="flex flex-col gap-1 text-sm text-zinc-200">
          <span class="text-xs uppercase tracking-wide text-zinc-400">
            Name
          </span>
          <input
            type="text"
            class="rounded-md border border-zinc-700 bg-zinc-900 px-3 py-1.5 text-sm text-zinc-100 placeholder:text-zinc-600 focus:border-emerald-700 focus:outline-none disabled:opacity-50"
            bind:value={formName}
            placeholder="e.g. workstation-primary"
            maxlength="64"
            disabled={generate.kind === "submitting"}
            data-testid="identities-generate-name"
            autocomplete="off"
            autocapitalize="none"
            autocorrect="off"
            spellcheck="false"
            inputmode="text"
            required
          />
        </label>

        <label class="flex flex-col gap-1 text-sm text-zinc-200">
          <span class="text-xs uppercase tracking-wide text-zinc-400">
            Key type
          </span>
          <select
            class="rounded-md border border-zinc-700 bg-zinc-900 px-3 py-1.5 text-sm text-zinc-100 focus:border-emerald-700 focus:outline-none disabled:opacity-50"
            bind:value={formKeyType}
            disabled={generate.kind === "submitting"}
            data-testid="identities-generate-key-type"
          >
            {#each SUPPORTED_GENERATION_KEY_TYPES as keyType (keyType)}
              <option value={keyType}>{keyType}</option>
            {/each}
          </select>
          <span class="text-[11px] text-zinc-500">
            Ed25519 is the only key type the vault can generate today.
          </span>
        </label>

        <div class="flex items-center gap-2">
          <button
            type="submit"
            class="rounded-md border border-emerald-700 bg-emerald-800 px-3 py-1.5 text-sm text-emerald-50 transition hover:border-emerald-600 hover:bg-emerald-700 disabled:opacity-50"
            disabled={generate.kind === "submitting" ||
              formName.trim().length === 0}
            data-testid="identities-generate-submit"
          >
            {generate.kind === "submitting"
              ? "Generating…"
              : "Generate identity"}
          </button>
          {#if generate.kind === "submitting"}
            <span class="text-xs text-zinc-400">
              Generating keypair on the backend…
            </span>
          {/if}
        </div>
      </form>

      {#if generate.kind === "error"}
        <p
          class="rounded-md border border-rose-900/40 bg-rose-950/20 px-3 py-2 text-xs text-rose-200/80"
          data-testid="identities-generate-error"
        >
          {generate.summary}
        </p>
      {:else if generate.kind === "success"}
        {@const generated = generate.identity}
        <article
          class="flex flex-col gap-2 rounded-md border border-emerald-900/50 bg-emerald-950/30 p-4 text-sm text-emerald-50"
          data-testid="identities-generate-success"
        >
          <header class="flex items-baseline justify-between gap-2">
            <span class="text-sm font-semibold">
              Generated <span data-testid="identities-generate-success-name"
                >{generated.name}</span
              >
            </span>
            <span
              class="font-mono text-xs uppercase tracking-wide text-emerald-200/80"
              data-testid="identities-generate-success-key-type"
            >
              {generated.key_type}
            </span>
          </header>
          <span
            class="font-mono text-xs text-emerald-100/80"
            data-testid="identities-generate-success-fingerprint"
          >
            {generated.fingerprint_sha256}
          </span>
          <span class="text-xs text-emerald-200/70">
            Created
            <time class="font-mono">{generated.created_at}</time>
          </span>
          <div class="flex flex-col gap-1">
            <span class="text-xs uppercase tracking-wide text-emerald-200/70">
              Public key
            </span>
            <pre
              class="overflow-x-auto rounded-md border border-emerald-900/40 bg-zinc-950/60 p-3 font-mono text-[11px] text-emerald-50/90"
              data-testid="identities-generate-success-public-key"><code
                >{generated.public_key}</code
              ></pre>
            <div class="flex items-center gap-2">
              <button
                type="button"
                class="rounded-md border border-emerald-700 bg-emerald-800 px-2.5 py-1 text-xs text-emerald-50 transition hover:border-emerald-600 hover:bg-emerald-700"
                onclick={() => copyPublicKey(generated)}
                data-testid="identities-generate-success-copy"
              >
                {copyLabel(copy[generated.id])}
              </button>
              <span class="text-[11px] text-emerald-200/60">
                Append to the target server's
                <code class="font-mono">~/.ssh/authorized_keys</code>.
              </span>
            </div>
          </div>
        </article>
      {/if}
    </article>
  {/if}

  {#if view.kind === "loading" || view.kind === "idle"}
    <p
      class="rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-6 text-sm text-zinc-400"
      data-testid="identities-loading"
    >
      Loading identities…
    </p>
  {:else if view.kind === "error"}
    <p
      class="rounded-md border border-rose-900/40 bg-rose-950/20 px-4 py-3 text-sm text-rose-200/80"
      data-testid="identities-error"
    >
      {view.summary}
    </p>
  {:else}
    <article
      class="flex flex-col gap-3 rounded-lg border border-zinc-800 bg-zinc-950/40 p-4"
      data-testid="identities-filter-toolbar"
    >
      <header class="flex items-baseline justify-between gap-2">
        <h3 class="text-sm font-semibold text-zinc-100">Filter identities</h3>
        <span class="text-xs text-zinc-500">
          In-memory only · public metadata only
        </span>
      </header>
      <div class="grid gap-3 sm:grid-cols-2">
        <label class="flex flex-col gap-1 text-xs text-zinc-300">
          <span class="uppercase tracking-wide text-zinc-500">
            Search identities
          </span>
          <input
            type="search"
            class="rounded-md border border-zinc-700 bg-zinc-900 px-2.5 py-1.5 text-sm text-zinc-100 placeholder:text-zinc-600 focus:border-emerald-700 focus:outline-none"
            bind:value={identitySearch}
            placeholder="name, fingerprint, key type"
            autocomplete="off"
            spellcheck="false"
            data-testid="identities-search"
          />
        </label>
        {#if availableKeyTypes.length > 1}
          <label class="flex flex-col gap-1 text-xs text-zinc-300">
            <span class="uppercase tracking-wide text-zinc-500">
              Key type
            </span>
            <select
              class="rounded-md border border-zinc-700 bg-zinc-900 px-2.5 py-1.5 text-sm text-zinc-100 focus:border-emerald-700 focus:outline-none"
              bind:value={keyTypeFilter}
              data-testid="identities-key-type-filter"
            >
              <option value="">All key types</option>
              {#each availableKeyTypes as kt (kt)}
                <option value={kt}>{kt}</option>
              {/each}
            </select>
          </label>
        {/if}
      </div>
      <div class="flex flex-wrap items-center justify-end gap-2 text-xs text-zinc-400">
        <button
          type="button"
          class="rounded-md border border-zinc-700 bg-zinc-800 px-2.5 py-1 text-xs text-zinc-200 transition hover:border-zinc-600 hover:bg-zinc-700 disabled:cursor-not-allowed disabled:opacity-50"
          onclick={clearIdentityFilters}
          disabled={!identitiesAreFiltered}
          data-testid="identities-clear-filters"
        >
          Clear filters
        </button>
      </div>
    </article>

    <article
      class="flex flex-col gap-3 rounded-lg border border-zinc-800 bg-zinc-950/40 p-6"
    >
      <header class="flex items-baseline justify-between gap-2">
        <h3 class="text-sm font-semibold text-zinc-100">Identities</h3>
        <span
          class="text-xs text-zinc-500"
          data-testid="identities-count"
        >
          {countFilteredResults(
            filteredIdentities.length,
            view.identities.length,
            "identity",
            "identities",
          )}
        </span>
      </header>
      {#if view.identities.length === 0}
        <p class="text-sm text-zinc-400" data-testid="identities-empty">
          No SSH identities yet. Use “Generate SSH identity” above to
          create one.
        </p>
      {:else if filteredIdentities.length === 0}
        <p
          class="text-sm text-zinc-400"
          data-testid="identities-filter-empty"
        >
          No identities match this filter.
        </p>
      {:else}
        <ul
          class="flex flex-col divide-y divide-zinc-800/60"
          data-testid="identities-list"
        >
          {#each filteredIdentities as identity (identity.id)}
            {@const isSelected = selectedIdentityId === identity.id}
            <li
              class="flex flex-col py-3 first:pt-0 last:pb-0"
              data-testid="identity-row"
            >
              <div
                class="flex flex-col gap-1.5 rounded-md px-2 py-1 transition {isSelected
                  ? 'bg-emerald-950/30 ring-1 ring-emerald-800/60'
                  : ''}"
              >
                <div class="flex items-baseline justify-between gap-3">
                  <span class="text-sm font-medium text-zinc-100">
                    {identity.name}
                  </span>
                  <span
                    class="font-mono text-xs uppercase tracking-wide text-zinc-500"
                  >
                    {identity.key_type}
                  </span>
                </div>
                <span
                  class="font-mono text-xs text-zinc-300"
                  data-testid="identity-fingerprint"
                >
                  {identity.fingerprint_sha256}
                </span>
                <span
                  class="truncate font-mono text-[11px] text-zinc-500"
                  data-testid="identity-public-key-preview"
                >
                  {publicKeyPreview(identity.public_key)}
                </span>
                <div
                  class="flex flex-wrap items-center gap-x-4 gap-y-1 text-xs text-zinc-400"
                >
                  <span>
                    Created
                    <time class="font-mono text-zinc-300"
                      >{identity.created_at}</time
                    >
                  </span>
                  {#if identity.last_used_at}
                    <span>
                      Last used
                      <time class="font-mono text-zinc-300"
                        >{identity.last_used_at}</time
                      >
                    </span>
                  {:else}
                    <span class="text-zinc-500">Never used</span>
                  {/if}
                </div>
                <div class="flex flex-wrap items-center gap-2">
                  <button
                    type="button"
                    class="rounded-md border px-2.5 py-1 text-xs transition focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-700/60 {isSelected
                      ? 'border-emerald-700 bg-emerald-900/40 text-emerald-100 hover:border-emerald-600 hover:bg-emerald-900/60'
                      : 'border-zinc-700 bg-zinc-800 text-zinc-200 hover:border-zinc-600 hover:bg-zinc-700'}"
                    onclick={() => selectIdentity(identity.id)}
                    aria-expanded={isSelected}
                    data-testid="identity-row-select"
                  >
                    {isSelected ? "Hide details" : "View details"}
                  </button>
                  <button
                    type="button"
                    class="rounded-md border border-zinc-700 bg-zinc-800 px-2.5 py-1 text-xs text-zinc-200 transition hover:border-zinc-600 hover:bg-zinc-700"
                    onclick={() => copyPublicKey(identity)}
                    data-testid="identity-copy-public-key"
                  >
                    {copyLabel(copy[identity.id])}
                  </button>
                  <span class="text-[11px] text-zinc-500">
                    Copies the OpenSSH public key only.
                  </span>
                </div>
              </div>
            </li>
          {/each}
        </ul>
      {/if}
    </article>

    {#if selectedIdentity}
      {@const detail = identityPublicDetail(selectedIdentity, publicKeyPreview)}
      {@const fullKey = publicKeyCopyValue(selectedIdentity)}
      <article
        class="flex flex-col gap-3 rounded-lg border border-emerald-900/40 bg-emerald-950/10 p-6"
        data-testid="identity-detail-panel"
      >
        <header class="flex items-baseline justify-between gap-2">
          <h3 class="text-sm font-semibold text-zinc-100">
            SSH identity detail
            <span class="ml-2 text-xs font-normal text-zinc-500">
              read-only
            </span>
          </h3>
          <button
            type="button"
            class="rounded-md border border-zinc-800 bg-zinc-900 px-2 py-1 text-xs text-zinc-300 transition hover:border-zinc-700 hover:bg-zinc-800"
            onclick={closeIdentityDetail}
            data-testid="identity-detail-close"
          >
            Close
          </button>
        </header>

        {#if selectedIdentityHidden}
          <p
            class="rounded-md border border-amber-900/40 bg-amber-950/20 px-3 py-2 text-xs text-amber-200/80"
            data-testid="identity-detail-hidden-by-filter"
          >
            This identity is currently hidden by your filters. Clear
            the search or key-type filter to bring it back into the
            list.
          </p>
        {/if}

        <dl class="grid grid-cols-[max-content_1fr] gap-x-4 gap-y-2 text-sm">
          <dt class="text-xs uppercase tracking-wide text-zinc-500">Name</dt>
          <dd class="text-zinc-100" data-testid="identity-detail-name">
            {detail.name}
          </dd>
          <dt class="text-xs uppercase tracking-wide text-zinc-500">
            Key type
          </dt>
          <dd
            class="font-mono uppercase tracking-wide text-zinc-100"
            data-testid="identity-detail-key-type"
          >
            {detail.key_type}
          </dd>
          <dt class="text-xs uppercase tracking-wide text-zinc-500">
            Fingerprint
          </dt>
          <dd
            class="font-mono text-zinc-100"
            data-testid="identity-detail-fingerprint"
          >
            {detail.fingerprint_sha256}
          </dd>
          <dt class="text-xs uppercase tracking-wide text-zinc-500">
            Public key preview
          </dt>
          <dd
            class="truncate font-mono text-xs text-zinc-300"
            data-testid="identity-detail-public-key-preview"
          >
            {detail.publicKeyPreview}
          </dd>
          <dt class="text-xs uppercase tracking-wide text-zinc-500">Created</dt>
          <dd
            class="font-mono text-zinc-300"
            data-testid="identity-detail-created-at"
          >
            {safeDisplayValue(detail.created_at)}
          </dd>
          <dt class="text-xs uppercase tracking-wide text-zinc-500">
            Last used
          </dt>
          <dd
            class="font-mono text-zinc-300"
            data-testid="identity-detail-last-used-at"
          >
            {safeDisplayValue(detail.last_used_at, "never")}
          </dd>
          <dt class="text-xs uppercase tracking-wide text-zinc-500">Id</dt>
          <dd
            class="font-mono text-xs text-zinc-500"
            data-testid="identity-detail-id"
          >
            {shortId(detail.id)}
          </dd>
        </dl>

        <div class="flex flex-col gap-1">
          <span class="text-xs uppercase tracking-wide text-zinc-500">
            Public key
          </span>
          <pre
            class="overflow-x-auto rounded-md border border-emerald-900/40 bg-zinc-950/60 p-3 font-mono text-[11px] text-zinc-200"
            data-testid="identity-detail-public-key"><code>{fullKey}</code></pre>
          <div class="flex items-center gap-2">
            <button
              type="button"
              class="rounded-md border border-emerald-700 bg-emerald-800 px-2.5 py-1 text-xs text-emerald-50 transition hover:border-emerald-600 hover:bg-emerald-700"
              onclick={() => copyPublicKey(selectedIdentity)}
              data-testid="identity-detail-copy-public-key"
            >
              {copyLabel(copy[selectedIdentity.id])}
            </button>
            <span class="text-[11px] text-zinc-500">
              Copies the OpenSSH public key only — never any private
              material.
            </span>
          </div>
        </div>

        <p
          class="rounded-md border border-amber-900/40 bg-amber-950/20 px-3 py-2 text-xs text-amber-200/80"
          data-testid="identity-detail-honesty"
        >
          The private key is encrypted at rest in the backend vault and
          never reaches the browser. There is no UI to export, recover,
          or otherwise reveal private material.
        </p>
      </article>
    {/if}
  {/if}

  <p
    class="rounded-md border border-amber-900/40 bg-amber-950/20 px-3 py-2 text-xs text-amber-200/80"
  >
    <span class="font-mono uppercase tracking-wide">future work</span> ·
    Deletion, rename, private-key import, and password bootstrap /
    <code class="font-mono">ssh-copy-id</code> automation are deliberate
    later slices. This view never renders or copies private material.
  </p>
</section>
