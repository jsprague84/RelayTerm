<script lang="ts">
  import {
    createSshIdentity,
    deleteSshIdentity,
    describeCreateSshIdentityError,
    describeDeleteSshIdentityError,
    describeImportSshIdentityError,
    describeUpdateSshIdentityError,
    importSshIdentity,
    listSshIdentities,
    publicKeyPreview,
    updateSshIdentity,
    MAX_IDENTITY_NAME_LEN,
    MAX_PRIVATE_KEY_OPENSSH_BYTES,
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

  /**
   * Which create-style panel is currently open. The "Generate" and
   * "Import" panels are mutually exclusive — opening one closes the
   * other so there is at most one form-with-secret-material on screen.
   */
  type CreatePanel = "none" | "generate" | "import";
  let activePanel = $state<CreatePanel>("none");
  let panelOpen = $derived(activePanel === "generate");
  let importPanelOpen = $derived(activePanel === "import");

  let formName = $state("");
  let formKeyType = $state<SshKeyType>(SUPPORTED_GENERATION_KEY_TYPES[0]);
  let generate = $state<GenerateState>({ kind: "idle" });

  // ----------------------------------------------------------------
  // Import-panel state.
  //
  // The pasted private-key text is held in `pendingImportPrivateKey`
  // and is cleared:
  //   - on successful import (BEFORE the success card is shown);
  //   - on every failure branch (validation / HTTP / transport / parse);
  //   - on panel close.
  //
  // The string is bound to a `<textarea>`. It is NEVER persisted to
  // localStorage / sessionStorage / `data-*` / a store — the only
  // durable form is the encrypted blob the backend produces.
  // ----------------------------------------------------------------
  type ImportState =
    | { kind: "idle" }
    | { kind: "submitting" }
    | { kind: "success"; identity: SshIdentity }
    | { kind: "error"; summary: string };

  let importFormName = $state("");
  let pendingImportPrivateKey = $state("");
  let importState = $state<ImportState>({ kind: "idle" });

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
  // Rename + delete state. Mirrors the host / profile flow in
  // `ServersView.svelte`: one row at a time, an inline form for
  // rename, a deliberate-confirmation step for delete that requires
  // the operator to echo the identity name verbatim. Private-key
  // material is never touched by either action — the rename SQL only
  // writes `name`, and delete removes the row outright AFTER the
  // backend pre-checks for referencing profiles.
  // ----------------------------------------------------------------

  type RenameIdentityState =
    | { kind: "idle" }
    | { kind: "open"; identityId: string; name: string }
    | { kind: "submitting"; identityId: string }
    | { kind: "error"; identityId: string; summary: string };

  type DeleteIdentityState =
    | { kind: "idle" }
    | { kind: "confirming"; identityId: string; typed: string }
    | { kind: "submitting"; identityId: string }
    | { kind: "error"; identityId: string; summary: string };

  let renameIdentityState = $state<RenameIdentityState>({ kind: "idle" });
  let deleteIdentityState = $state<DeleteIdentityState>({ kind: "idle" });

  function replaceIdentityInView(updated: SshIdentity) {
    if (view.kind !== "ready") return;
    view = {
      kind: "ready",
      identities: view.identities.map((i) =>
        i.id === updated.id ? updated : i,
      ),
    };
  }

  function removeIdentityFromView(id: string) {
    if (view.kind !== "ready") return;
    view = {
      kind: "ready",
      identities: view.identities.filter((i) => i.id !== id),
    };
  }

  function openRenameIdentity(identity: SshIdentity) {
    if (renameIdentityState.kind === "submitting") return;
    renameIdentityState = {
      kind: "open",
      identityId: identity.id,
      name: identity.name,
    };
  }

  function cancelRenameIdentity() {
    if (renameIdentityState.kind === "submitting") return;
    renameIdentityState = { kind: "idle" };
  }

  function setRenameIdentityName(value: string) {
    if (renameIdentityState.kind !== "open") return;
    renameIdentityState = { ...renameIdentityState, name: value };
  }

  async function submitRenameIdentity(event: Event) {
    event.preventDefault();
    if (renameIdentityState.kind !== "open") return;
    const { identityId, name } = renameIdentityState;
    renameIdentityState = { kind: "submitting", identityId };
    const result = await updateSshIdentity(identityId, { name });
    if (!result.ok) {
      renameIdentityState = {
        kind: "error",
        identityId,
        summary: describeUpdateSshIdentityError(result.error),
      };
      return;
    }
    replaceIdentityInView(result.identity);
    renameIdentityState = { kind: "idle" };
  }

  function openDeleteIdentity(identity: SshIdentity) {
    if (deleteIdentityState.kind === "submitting") return;
    deleteIdentityState = {
      kind: "confirming",
      identityId: identity.id,
      typed: "",
    };
  }

  function cancelDeleteIdentity() {
    if (deleteIdentityState.kind === "submitting") return;
    deleteIdentityState = { kind: "idle" };
  }

  function setDeleteIdentityInput(value: string) {
    if (deleteIdentityState.kind !== "confirming") return;
    deleteIdentityState = { ...deleteIdentityState, typed: value };
  }

  async function submitDeleteIdentity(identity: SshIdentity) {
    if (
      deleteIdentityState.kind !== "confirming" ||
      deleteIdentityState.identityId !== identity.id
    ) {
      return;
    }
    if (deleteIdentityState.typed !== identity.name) return;
    deleteIdentityState = { kind: "submitting", identityId: identity.id };
    const result = await deleteSshIdentity(identity.id);
    if (!result.ok) {
      deleteIdentityState = {
        kind: "error",
        identityId: identity.id,
        summary: describeDeleteSshIdentityError(result.error),
      };
      return;
    }
    removeIdentityFromView(identity.id);
    selectedIdentityId = null;
    deleteIdentityState = { kind: "idle" };
  }

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
    if (importState.kind === "submitting") return;
    activePanel = "generate";
    if (generate.kind !== "submitting") {
      generate = { kind: "idle" };
    }
  }

  function closePanel() {
    if (generate.kind === "submitting") return;
    activePanel = "none";
    formName = "";
    formKeyType = SUPPORTED_GENERATION_KEY_TYPES[0];
    generate = { kind: "idle" };
  }

  function openImportPanel() {
    if (generate.kind === "submitting") return;
    activePanel = "import";
    if (importState.kind !== "submitting") {
      importState = { kind: "idle" };
    }
  }

  function closeImportPanel() {
    if (importState.kind === "submitting") return;
    activePanel = "none";
    importFormName = "";
    // Always wipe the PEM string when leaving the panel — it must NOT
    // outlive the form scope.
    pendingImportPrivateKey = "";
    importState = { kind: "idle" };
  }

  function appendImportedToView(identity: SshIdentity) {
    if (view.kind === "ready") {
      const exists = view.identities.some((i) => i.id === identity.id);
      view = exists
        ? view
        : { kind: "ready", identities: [identity, ...view.identities] };
    } else {
      void load();
    }
  }

  async function submitImport(event: Event) {
    event.preventDefault();
    if (importState.kind === "submitting") return;
    // Snapshot the pasted PEM into a local variable, then immediately
    // clear the bound state. The remainder of this function references
    // only the local snapshot — the textarea is empty for the rest of
    // the request lifecycle. Mirrors the redaction discipline the
    // backend keeps for the in-memory plaintext.
    const pem = pendingImportPrivateKey;
    const name = importFormName;
    pendingImportPrivateKey = "";
    importState = { kind: "submitting" };
    const result = await importSshIdentity({
      name,
      private_key_openssh: pem,
    });
    if (!result.ok) {
      // describeImportSshIdentityError is the only redaction-safe
      // formatter — never echo `result.error.message` directly. The
      // textarea is already cleared above; the formatter output never
      // contains PEM bytes either.
      importState = {
        kind: "error",
        summary: describeImportSshIdentityError(result.error),
      };
      return;
    }
    appendImportedToView(result.identity);
    // Clear the name too on success — the operator typically wants a
    // fresh entry next time. (Generate keeps the name for retry; import
    // is a one-shot per imported key, so wiping is the right default.)
    importFormName = "";
    importState = { kind: "success", identity: result.identity };
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
    {#if !importPanelOpen}
      <button
        type="button"
        class="rounded-md border border-sky-800/60 bg-sky-900/20 px-3 py-1.5 text-sm text-sky-100 transition hover:border-sky-700 hover:bg-sky-900/40"
        onclick={openImportPanel}
        data-testid="identities-import-open"
      >
        <span class="hidden sm:inline">Import SSH identity</span>
        <span class="sm:hidden">Import</span>
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

  {#if importPanelOpen}
    <article
      class="flex flex-col gap-4 rounded-lg border border-sky-900/40 bg-sky-950/10 p-6"
      data-testid="identities-import-panel"
    >
      <header class="flex items-baseline justify-between gap-2">
        <h3 class="text-sm font-semibold text-zinc-100">
          Import an existing SSH identity
        </h3>
        <button
          type="button"
          class="rounded-md border border-zinc-800 bg-zinc-900 px-2 py-1 text-xs text-zinc-300 transition hover:border-zinc-700 hover:bg-zinc-800 disabled:opacity-50"
          onclick={closeImportPanel}
          disabled={importState.kind === "submitting"}
          data-testid="identities-import-close"
        >
          Close
        </button>
      </header>

      <p
        class="rounded-md border border-sky-900/40 bg-sky-950/20 px-3 py-2 text-xs text-sky-200/80"
        data-testid="identities-import-honesty"
      >
        The private key is sent to your RelayTerm server over HTTPS and
        encrypted into the server-side vault. It is not stored in the
        browser. The textarea is cleared on success and on every
        failure.
      </p>

      <ul class="flex flex-col gap-1 text-xs text-zinc-400">
        <li>
          Only OpenSSH-format Ed25519 private keys without a passphrase
          are supported in this release.
        </li>
        <li>
          Encrypted (passphrase-protected) keys, RSA / ECDSA, file
          pickers, and <code class="font-mono text-zinc-300">ssh-copy-id</code>
          automation are explicit later slices.
        </li>
        <li>
          The imported public key adopts the supplied name as its
          OpenSSH comment so the
          <code class="font-mono text-zinc-300">authorized_keys</code>
          line on the target host stays self-identifying.
        </li>
      </ul>

      <form
        class="flex flex-col gap-3"
        onsubmit={submitImport}
        data-testid="identities-import-form"
      >
        <label class="flex flex-col gap-1 text-sm text-zinc-200">
          <span class="text-xs uppercase tracking-wide text-zinc-400">
            Name
          </span>
          <input
            type="text"
            class="rounded-md border border-zinc-700 bg-zinc-900 px-3 py-1.5 text-sm text-zinc-100 placeholder:text-zinc-600 focus:border-sky-700 focus:outline-none disabled:opacity-50"
            bind:value={importFormName}
            placeholder="e.g. workstation-imported"
            maxlength={MAX_IDENTITY_NAME_LEN}
            disabled={importState.kind === "submitting"}
            data-testid="identities-import-name"
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
            OpenSSH private key (PEM)
          </span>
          <textarea
            class="min-h-[10rem] w-full rounded-md border border-zinc-700 bg-zinc-900 px-3 py-2 font-mono text-xs text-zinc-100 placeholder:text-zinc-600 focus:border-sky-700 focus:outline-none disabled:opacity-50"
            bind:value={pendingImportPrivateKey}
            rows="10"
            placeholder={"-----BEGIN OPENSSH PRIVATE KEY-----\n...\n-----END OPENSSH PRIVATE KEY-----"}
            maxlength={MAX_PRIVATE_KEY_OPENSSH_BYTES}
            disabled={importState.kind === "submitting"}
            data-testid="identities-import-private-key"
            autocomplete="off"
            autocapitalize="none"
            spellcheck="false"
            inputmode="text"
            required
          ></textarea>
          <span class="text-[11px] text-zinc-500">
            Paste the full file contents, including the
            <code class="font-mono">BEGIN</code> and
            <code class="font-mono">END</code> markers.
          </span>
        </label>

        <div class="flex items-center gap-2">
          <button
            type="submit"
            class="rounded-md border border-sky-700 bg-sky-800 px-3 py-1.5 text-sm text-sky-50 transition hover:border-sky-600 hover:bg-sky-700 disabled:opacity-50"
            disabled={importState.kind === "submitting" ||
              importFormName.trim().length === 0 ||
              pendingImportPrivateKey.length === 0}
            data-testid="identities-import-submit"
          >
            {#if importState.kind === "submitting"}
              Importing…
            {:else}
              <span class="hidden sm:inline">Import SSH identity</span>
              <span class="sm:hidden">Import</span>
            {/if}
          </button>
          {#if importState.kind === "submitting"}
            <span class="text-xs text-zinc-400">
              Encrypting into the backend vault…
            </span>
          {/if}
        </div>
      </form>

      {#if importState.kind === "error"}
        <p
          class="rounded-md border border-rose-900/40 bg-rose-950/20 px-3 py-2 text-xs text-rose-200/80"
          data-testid="identities-import-error"
        >
          {importState.summary}
        </p>
      {:else if importState.kind === "success"}
        {@const imported = importState.identity}
        <article
          class="flex flex-col gap-2 rounded-md border border-sky-900/50 bg-sky-950/30 p-4 text-sm text-sky-50"
          data-testid="identities-import-success"
        >
          <header class="flex items-baseline justify-between gap-2">
            <span class="text-sm font-semibold">
              Imported <span data-testid="identities-import-success-name"
                >{imported.name}</span
              >
            </span>
            <span
              class="font-mono text-xs uppercase tracking-wide text-sky-200/80"
              data-testid="identities-import-success-key-type"
            >
              {imported.key_type}
            </span>
          </header>
          <span
            class="font-mono text-xs text-sky-100/80"
            data-testid="identities-import-success-fingerprint"
          >
            {imported.fingerprint_sha256}
          </span>
          <span class="text-xs text-sky-200/70">
            Created
            <time class="font-mono">{imported.created_at}</time>
          </span>
          <div class="flex flex-col gap-1">
            <span class="text-xs uppercase tracking-wide text-sky-200/70">
              Public key
            </span>
            <pre
              class="overflow-x-auto rounded-md border border-sky-900/40 bg-zinc-950/60 p-3 font-mono text-[11px] text-sky-50/90"
              data-testid="identities-import-success-public-key"><code
                >{imported.public_key}</code
              ></pre>
            <div class="flex items-center gap-2">
              <button
                type="button"
                class="rounded-md border border-sky-700 bg-sky-800 px-2.5 py-1 text-xs text-sky-50 transition hover:border-sky-600 hover:bg-sky-700"
                onclick={() => copyPublicKey(imported)}
                data-testid="identities-import-success-copy"
              >
                {copyLabel(copy[imported.id])}
              </button>
              <span class="text-[11px] text-sky-200/60">
                The private key never reaches the browser. Only the
                public key is renderable here.
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

        <section
          class="flex flex-wrap items-center gap-2 border-t border-emerald-900/30 pt-3"
          data-testid="identity-detail-actions"
        >
          {#if renameIdentityState.kind !== "open" || renameIdentityState.identityId !== selectedIdentity.id}
            <button
              type="button"
              class="rounded-md border border-zinc-700 bg-zinc-800 px-2.5 py-1 text-xs text-zinc-100 transition hover:border-zinc-600 hover:bg-zinc-700 disabled:opacity-50"
              onclick={() => openRenameIdentity(selectedIdentity)}
              disabled={renameIdentityState.kind === "submitting" ||
                deleteIdentityState.kind === "submitting"}
              data-testid="identity-detail-rename-open"
            >
              Rename identity
            </button>
          {/if}
          {#if deleteIdentityState.kind !== "confirming" || deleteIdentityState.identityId !== selectedIdentity.id}
            <button
              type="button"
              class="rounded-md border border-red-800/60 bg-red-950/40 px-2.5 py-1 text-xs text-red-200 transition hover:border-red-700 hover:bg-red-900/40 disabled:opacity-50"
              onclick={() => openDeleteIdentity(selectedIdentity)}
              disabled={renameIdentityState.kind === "submitting" ||
                deleteIdentityState.kind === "submitting"}
              data-testid="identity-detail-delete-open"
            >
              Delete identity
            </button>
          {/if}
        </section>

        {#if renameIdentityState.kind === "open" && renameIdentityState.identityId === selectedIdentity.id}
          <form
            class="flex flex-col gap-2 rounded-md border border-zinc-800 bg-zinc-950/60 p-3"
            onsubmit={submitRenameIdentity}
            data-testid="identity-detail-rename-form"
          >
            <label class="flex flex-col gap-1 text-xs">
              <span class="text-zinc-400">Name</span>
              <input
                type="text"
                class="rounded border border-zinc-700 bg-zinc-900 px-2 py-1 text-sm text-zinc-100"
                value={renameIdentityState.name}
                oninput={(e) =>
                  setRenameIdentityName(
                    (e.currentTarget as HTMLInputElement).value,
                  )}
                data-testid="identity-detail-rename-input"
              />
            </label>
            <div class="flex items-center gap-2">
              <button
                type="submit"
                class="rounded-md border border-emerald-700 bg-emerald-800 px-2.5 py-1 text-xs text-emerald-50 transition hover:border-emerald-600 hover:bg-emerald-700"
                data-testid="identity-detail-rename-save"
              >
                Save
              </button>
              <button
                type="button"
                class="rounded-md border border-zinc-800 bg-zinc-900 px-2.5 py-1 text-xs text-zinc-300 transition hover:border-zinc-700 hover:bg-zinc-800"
                onclick={cancelRenameIdentity}
                data-testid="identity-detail-rename-cancel"
              >
                Cancel
              </button>
            </div>
          </form>
        {/if}

        {#if renameIdentityState.kind === "submitting" && renameIdentityState.identityId === selectedIdentity.id}
          <p
            class="text-xs text-zinc-400"
            data-testid="identity-detail-rename-submitting"
          >
            Saving…
          </p>
        {/if}

        {#if renameIdentityState.kind === "error" && renameIdentityState.identityId === selectedIdentity.id}
          <p
            class="rounded-md border border-red-900/60 bg-red-950/40 px-3 py-2 text-xs text-red-200"
            data-testid="identity-detail-rename-error"
          >
            {renameIdentityState.summary}
          </p>
        {/if}

        {#if deleteIdentityState.kind === "confirming" && deleteIdentityState.identityId === selectedIdentity.id}
          <div
            class="flex flex-col gap-2 rounded-md border border-red-900/60 bg-red-950/30 p-3 text-xs text-red-200"
            data-testid="identity-detail-delete-confirm"
          >
            <p>
              Deleting <span class="font-mono">{selectedIdentity.name}</span>
              is permanent. The encrypted private key and public-key
              metadata are removed. Server profiles that still reference
              this identity will block the delete — re-bind or remove
              them first.
            </p>
            <label class="flex flex-col gap-1">
              <span class="text-zinc-300">
                Type the identity name to confirm
              </span>
              <input
                type="text"
                class="rounded border border-red-900/60 bg-zinc-950 px-2 py-1 font-mono text-sm text-zinc-100"
                value={deleteIdentityState.typed}
                oninput={(e) =>
                  setDeleteIdentityInput(
                    (e.currentTarget as HTMLInputElement).value,
                  )}
                data-testid="identity-detail-delete-confirm-input"
              />
            </label>
            <div class="flex items-center gap-2">
              <button
                type="button"
                class="rounded-md border border-red-700 bg-red-800 px-2.5 py-1 text-xs text-red-50 transition hover:border-red-600 hover:bg-red-700 disabled:opacity-50"
                onclick={() => submitDeleteIdentity(selectedIdentity)}
                disabled={deleteIdentityState.typed !== selectedIdentity.name}
                data-testid="identity-detail-delete-confirm-submit"
              >
                Delete identity
              </button>
              <button
                type="button"
                class="rounded-md border border-zinc-800 bg-zinc-900 px-2.5 py-1 text-xs text-zinc-300 transition hover:border-zinc-700 hover:bg-zinc-800"
                onclick={cancelDeleteIdentity}
                data-testid="identity-detail-delete-cancel"
              >
                Cancel
              </button>
            </div>
          </div>
        {/if}

        {#if deleteIdentityState.kind === "submitting" && deleteIdentityState.identityId === selectedIdentity.id}
          <p
            class="text-xs text-zinc-400"
            data-testid="identity-detail-delete-submitting"
          >
            Deleting…
          </p>
        {/if}

        {#if deleteIdentityState.kind === "error" && deleteIdentityState.identityId === selectedIdentity.id}
          <p
            class="rounded-md border border-red-900/60 bg-red-950/40 px-3 py-2 text-xs text-red-200"
            data-testid="identity-detail-delete-error"
          >
            {deleteIdentityState.summary}
          </p>
        {/if}

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
    Passphrase-protected key import, RSA / ECDSA, file pickers, and
    password bootstrap / <code class="font-mono">ssh-copy-id</code>
    automation are deliberate later slices. This view never renders or
    copies private material.
  </p>
</section>
