<script lang="ts">
  /**
   * Per-profile host-key preflight + trust action area.
   *
   * Architectural rules (load-bearing):
   *  - Trust is NEVER auto-issued. The "Trust" button only appears
   *    after a successful preflight returns `unknown`, AND the operator
   *    has explicitly confirmed the captured fingerprint via a
   *    confirmation checkbox.
   *  - `changed` and `revoked` outcomes never enable trust. `revoked`
   *    is not a wire status; it surfaces only as a deferred 409 from
   *    the trust route, which the formatter collapses to a conservative
   *    "trust refused" message.
   *  - The component holds local Svelte state ONLY. No global stores,
   *    no router, no polling, no auto-retry.
   *  - The component never logs raw response bodies. Error display
   *    goes through `describePreflightError` / `describeTrustHostKeyError`
   *    which are pure functions of `kind` + `status` + `code`.
   *  - Host-key fingerprints are public-ish security metadata; safe to
   *    display deliberately. No private-key material is ever rendered.
   */

  import {
    describePreflightError,
    describeTrustHostKeyError,
    hostKeyPreflight,
    trustHostKey,
    type HostKeyPreflightResponse,
    type TrustHostKeyResponse,
  } from "../../api/serverProfiles.js";
  import {
    fingerprintConfirmationMatches,
    hostKeyStatusDescription,
    hostKeyStatusLabel,
    PREFLIGHT_DISCLAIMER,
    trustGateForPreflight,
    TRUST_DISCLAIMER,
  } from "../hostKeyTrustState.js";

  type State =
    | { kind: "idle" }
    | { kind: "preflighting" }
    | { kind: "ready"; preflight: HostKeyPreflightResponse }
    | { kind: "trusting"; preflight: HostKeyPreflightResponse }
    | {
        // Carry the original preflight alongside the trust response so the
        // panel can keep rendering the captured fingerprint + key type
        // after success. The displayed status is forced to `trusted` via
        // the derived selector below.
        kind: "trusted";
        preflight: HostKeyPreflightResponse;
        trust: TrustHostKeyResponse;
      }
    | { kind: "preflight_error"; summary: string }
    | {
        kind: "trust_error";
        preflight: HostKeyPreflightResponse;
        summary: string;
      };

  interface Props {
    profileId: string;
  }

  let { profileId }: Props = $props();

  let panelState = $state<State>({ kind: "idle" });
  let confirmInput = $state("");

  function statusBadgeClass(status: "unknown" | "trusted" | "changed"): string {
    if (status === "trusted") {
      return "border-emerald-800/60 bg-emerald-900/30 text-emerald-200";
    }
    if (status === "changed") {
      return "border-rose-900/60 bg-rose-950/40 text-rose-200";
    }
    return "border-amber-900/60 bg-amber-950/40 text-amber-200";
  }

  async function runPreflight() {
    if (panelState.kind === "preflighting" || panelState.kind === "trusting") return;
    panelState = { kind: "preflighting" };
    confirmInput = "";
    const result = await hostKeyPreflight(profileId);
    if (!result.ok) {
      panelState = {
        kind: "preflight_error",
        summary: describePreflightError(result.error),
      };
      return;
    }
    panelState = { kind: "ready", preflight: result.preflight };
  }

  async function submitTrust() {
    if (panelState.kind !== "ready") return;
    const preflight = panelState.preflight;
    const gate = trustGateForPreflight(preflight);
    if (gate.kind !== "ok") return;
    if (
      !fingerprintConfirmationMatches(
        preflight.host_key_fingerprint,
        confirmInput,
      )
    ) {
      return;
    }
    panelState = { kind: "trusting", preflight };
    const result = await trustHostKey(
      profileId,
      preflight.host_key_fingerprint,
    );
    if (!result.ok) {
      panelState = {
        kind: "trust_error",
        preflight,
        summary: describeTrustHostKeyError(result.error),
      };
      return;
    }
    confirmInput = "";
    panelState = { kind: "trusted", preflight, trust: result.trust };
  }

  let preflight = $derived.by<HostKeyPreflightResponse | null>(() => {
    if (panelState.kind === "ready") return panelState.preflight;
    if (panelState.kind === "trusting") return panelState.preflight;
    if (panelState.kind === "trust_error") return panelState.preflight;
    if (panelState.kind === "trusted") {
      // Force the badge to `trusted` after a successful trust action —
      // the captured fingerprint is now pinned, so showing the original
      // `unknown` status would be misleading.
      return { ...panelState.preflight, host_key_status: "trusted" };
    }
    return null;
  });

  let trustGate = $derived(
    preflight ? trustGateForPreflight(preflight) : null,
  );

  let confirmationMatches = $derived(
    preflight
      ? fingerprintConfirmationMatches(
          preflight.host_key_fingerprint,
          confirmInput,
        )
      : false,
  );

  let trustSubmitDisabled = $derived(
    panelState.kind === "trusting" ||
      trustGate?.kind !== "ok" ||
      !confirmationMatches,
  );

  let preflightButtonLabel = $derived(
    panelState.kind === "preflighting"
      ? "Running preflight…"
      : panelState.kind === "trusted" || panelState.kind === "ready" ||
          panelState.kind === "preflight_error" || panelState.kind === "trust_error"
        ? "Re-run preflight"
        : "Run host-key preflight",
  );
</script>

<section
  class="flex flex-col gap-2 rounded-md border border-zinc-800/80 bg-zinc-950/30 p-3"
  data-testid="host-key-panel"
  data-profile-id={profileId}
>
  <header class="flex items-center justify-between gap-2">
    <h4 class="text-xs font-semibold uppercase tracking-wide text-zinc-300">
      Host key
    </h4>
    <button
      type="button"
      class="rounded-md border border-zinc-700 bg-zinc-900 px-2 py-1 text-[11px] text-zinc-200 transition hover:border-zinc-600 hover:bg-zinc-800 disabled:cursor-not-allowed disabled:opacity-50"
      onclick={runPreflight}
      disabled={panelState.kind === "preflighting" || panelState.kind === "trusting"}
      data-testid="host-key-preflight-button"
    >
      {preflightButtonLabel}
    </button>
  </header>

  <p class="text-[11px] text-zinc-500">{PREFLIGHT_DISCLAIMER}</p>

  {#if panelState.kind === "idle"}
    <p
      class="text-[11px] text-zinc-500"
      data-testid="host-key-idle"
    >
      Run preflight to capture and classify the server's host key.
    </p>
  {:else if panelState.kind === "preflighting"}
    <p
      class="text-xs text-zinc-400"
      data-testid="host-key-preflighting"
    >
      Capturing host key during SSH key exchange…
    </p>
  {:else if panelState.kind === "preflight_error"}
    <p
      class="rounded-md border border-rose-900/40 bg-rose-950/20 px-2 py-1.5 text-[11px] text-rose-200/80"
      data-testid="host-key-preflight-error"
    >
      {panelState.summary}
    </p>
  {:else if preflight}
    {@const status = preflight.host_key_status}
    <div class="flex flex-col gap-1.5">
      <div class="flex items-center gap-2">
        <span
          class="rounded border px-1.5 py-0.5 text-[11px] font-medium {statusBadgeClass(
            status,
          )}"
          data-testid="host-key-status-badge"
          data-status={status}
        >
          {hostKeyStatusLabel(status)}
        </span>
        <span class="text-[11px] uppercase tracking-wide text-zinc-500">
          {preflight.host_key_type}
        </span>
      </div>
      <p
        class="text-[11px] text-zinc-400"
        data-testid="host-key-status-description"
      >
        {hostKeyStatusDescription(status)}
      </p>
      <code
        class="select-all break-all rounded border border-zinc-800 bg-zinc-900/60 px-2 py-1 font-mono text-[11px] text-zinc-200"
        data-testid="host-key-fingerprint"
      >
        {preflight.host_key_fingerprint}
      </code>
    </div>

    {#if panelState.kind === "trusted"}
      <p
        class="rounded-md border border-emerald-900/50 bg-emerald-950/30 px-2 py-1.5 text-[11px] text-emerald-100"
        data-testid="host-key-trusted-success"
      >
        Host key pinned. Re-run preflight to confirm. SSH authentication
        and terminal launch are still future work.
      </p>
    {:else if trustGate?.kind === "already_trusted"}
      <p
        class="text-[11px] text-emerald-200/80"
        data-testid="host-key-already-trusted"
      >
        This host key is already pinned for the host. Nothing to do here.
      </p>
    {:else if trustGate?.kind === "changed_refused"}
      <p
        class="rounded-md border border-rose-900/40 bg-rose-950/20 px-2 py-1.5 text-[11px] text-rose-200"
        data-testid="host-key-changed-refused"
      >
        RelayTerm refuses to overwrite a pinned host key automatically.
        Investigate before retrying — server reinstallation, key rotation,
        or a man-in-the-middle are all possible explanations.
      </p>
    {:else if trustGate?.kind === "ok"}
      <div class="flex flex-col gap-2 border-t border-zinc-800 pt-2">
        <p class="text-[11px] text-zinc-400">{TRUST_DISCLAIMER}</p>
        <label
          class="flex flex-col gap-1 text-[11px] text-zinc-300"
        >
          <span class="uppercase tracking-wide text-zinc-500">
            Confirm fingerprint to trust
          </span>
          <input
            type="text"
            class="rounded-md border border-zinc-700 bg-zinc-900 px-2 py-1 font-mono text-[11px] text-zinc-100 placeholder:text-zinc-600 focus:border-emerald-700 focus:outline-none disabled:opacity-50"
            bind:value={confirmInput}
            placeholder="Paste the SHA256:… fingerprint shown above"
            disabled={panelState.kind === "trusting"}
            data-testid="host-key-confirm-input"
            autocomplete="off"
            spellcheck="false"
          />
          {#if confirmInput.length > 0 && !confirmationMatches}
            <span
              class="text-[11px] text-amber-300/80"
              data-testid="host-key-confirm-mismatch"
            >
              Confirmation does not match the captured fingerprint.
            </span>
          {/if}
        </label>
        <div class="flex items-center gap-2">
          <button
            type="button"
            class="rounded-md border border-emerald-700 bg-emerald-800 px-3 py-1 text-[11px] text-emerald-50 transition hover:border-emerald-600 hover:bg-emerald-700 disabled:cursor-not-allowed disabled:opacity-50"
            onclick={submitTrust}
            disabled={trustSubmitDisabled}
            data-testid="host-key-trust-button"
          >
            {panelState.kind === "trusting"
              ? "Trusting…"
              : "Trust this host key"}
          </button>
        </div>
      </div>
      {#if panelState.kind === "trust_error"}
        <p
          class="rounded-md border border-rose-900/40 bg-rose-950/20 px-2 py-1.5 text-[11px] text-rose-200/80"
          data-testid="host-key-trust-error"
        >
          {panelState.summary}
        </p>
      {/if}
    {/if}
  {/if}
</section>
