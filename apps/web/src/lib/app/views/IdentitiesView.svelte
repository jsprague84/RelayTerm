<script lang="ts">
  import {
    listSshIdentities,
    publicKeyPreview,
    type SshIdentity,
  } from "../../api/sshIdentities.js";
  import { describeLoadError } from "../../api/apiErrors.js";

  type LoadState =
    | { kind: "idle" }
    | { kind: "loading" }
    | { kind: "ready"; identities: SshIdentity[] }
    | { kind: "error"; summary: string };

  type CopyState = "idle" | "copied" | "failed";

  let view = $state<LoadState>({ kind: "idle" });
  let copy = $state<Record<string, CopyState>>({});

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
      Read-only inventory of vault-managed SSH keypairs. Public material
      only — the encrypted private key never leaves the backend and is
      never rendered, copied, or logged on the client.
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
  </div>

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
      class="flex flex-col gap-3 rounded-lg border border-zinc-800 bg-zinc-950/40 p-6"
    >
      <header class="flex items-baseline justify-between gap-2">
        <h3 class="text-sm font-semibold text-zinc-100">Identities</h3>
        <span
          class="text-xs text-zinc-500"
          data-testid="identities-count"
        >
          {view.identities.length}
          {view.identities.length === 1 ? "identity" : "identities"}
        </span>
      </header>
      {#if view.identities.length === 0}
        <p class="text-sm text-zinc-400" data-testid="identities-empty">
          No SSH identities yet. Generation UI is not implemented in
          this view — identities are created through the backend API
          today.
        </p>
      {:else}
        <ul
          class="flex flex-col divide-y divide-zinc-800/60"
          data-testid="identities-list"
        >
          {#each view.identities as identity (identity.id)}
            <li
              class="flex flex-col gap-1.5 py-3 first:pt-0 last:pb-0"
              data-testid="identity-row"
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
              <div class="flex items-center gap-2">
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
    Generation UI, deletion, and private-key import are deliberate
    later slices. This view never renders or copies private material.
  </p>
</section>
