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
    describeReplaceHostKeyError,
    describeTrustHostKeyError,
    hostKeyPreflight,
    replaceHostKey,
    trustHostKey,
    type HostKeyPreflightResponse,
    type HostKeyReplacementReasonCode,
    type ReplaceHostKeyResponse,
    type TrustHostKeyResponse,
  } from "../../api/serverProfiles.js";
  import {
    decideReplaceSubmit,
    fingerprintConfirmationMatches,
    hostKeyStatusDescription,
    hostKeyStatusLabel,
    PREFLIGHT_DISCLAIMER,
    replaceGateForPreflight,
    replacementReasonOptions,
    synthesizePostReplacePreflight,
    trustGateForPreflight,
    TRUST_DISCLAIMER,
    type ReplaceSubmitDecision,
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
    | {
        // Successful replace. The synthesized preflight pins the new
        // trusted fingerprint; the replacement response carries the audit
        // identifiers (revoked + trusted entry ids) but the panel only
        // surfaces the public-side data already on the synthesized
        // preflight — we never render row ids or audit detail.
        kind: "replaced";
        preflight: HostKeyPreflightResponse;
        replacement: ReplaceHostKeyResponse;
      }
    | { kind: "preflight_error"; summary: string }
    | {
        kind: "trust_error";
        preflight: HostKeyPreflightResponse;
        summary: string;
      };

  // Modal state lives separately from `panelState` because the modal can
  // be opened, the user can change inputs, the request can fail, and the
  // user can retry — all without disturbing the underlying preflight
  // result. On success the modal closes AND `panelState` advances to
  // `replaced`.
  type ReplaceModalState =
    | { kind: "closed" }
    | { kind: "open" }
    | { kind: "submitting" }
    | { kind: "error"; summary: string };

  interface Props {
    profileId: string;
    /**
     * When true, the panel renders a disabled-profile notice instead of
     * the preflight / trust controls. Mirrors the backend's launch-time
     * gate — disabled profiles refuse preflight and trust with `409
     * conflict`. Defaulting to `false` keeps the existing call sites
     * (older tests, dev surfaces) green.
     */
    disabled?: boolean;
  }

  let { profileId, disabled = false }: Props = $props();

  let panelState = $state<State>({ kind: "idle" });
  let confirmInput = $state("");

  // Replace-modal local inputs and state. Reset on every `runPreflight`
  // / `submitTrust` to keep the modal closed when the operator re-runs
  // the panel from scratch.
  let replaceModalState = $state<ReplaceModalState>({ kind: "closed" });
  let replaceReasonCode = $state<HostKeyReplacementReasonCode | null>(null);
  let replaceConfirmInput = $state("");
  // Cached decision used by the disabled/visible flags AND by the
  // submit handler — keeping a single derivation point closes a stale-
  // shape race where the displayed fingerprints could disagree with the
  // submitted request body.
  const REPLACE_REASON_OPTIONS = replacementReasonOptions();
  const REPLACE_LEDE =
    "RelayTerm will not silently overwrite a pinned host key. The fingerprint shown below is different from what you trusted previously. Replace it only if you can explain why the host key changed.";
  const REPLACE_DISCLAIMER =
    "After replacement, run auth-check to confirm the configured SSH identity still authenticates against the new host key. Existing live terminal sessions on this profile are not killed by this action.";

  function statusBadgeClass(status: "unknown" | "trusted" | "changed"): string {
    if (status === "trusted") {
      return "border-emerald-800/60 bg-emerald-900/30 text-emerald-200";
    }
    if (status === "changed") {
      return "border-rose-900/60 bg-rose-950/40 text-rose-200";
    }
    return "border-amber-900/60 bg-amber-950/40 text-amber-200";
  }

  function resetReplaceForm() {
    replaceModalState = { kind: "closed" };
    replaceReasonCode = null;
    replaceConfirmInput = "";
  }

  async function runPreflight() {
    if (
      panelState.kind === "preflighting" ||
      panelState.kind === "trusting" ||
      replaceModalState.kind === "submitting"
    ) {
      return;
    }
    panelState = { kind: "preflighting" };
    confirmInput = "";
    resetReplaceForm();
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

  function openReplace() {
    if (replaceModalState.kind !== "closed") return;
    replaceReasonCode = null;
    replaceConfirmInput = "";
    replaceModalState = { kind: "open" };
  }

  function cancelReplace() {
    if (replaceModalState.kind === "submitting") return;
    resetReplaceForm();
  }

  async function submitReplace() {
    if (replaceModalState.kind === "submitting") return;
    const carrier =
      panelState.kind === "ready" || panelState.kind === "trust_error"
        ? panelState.preflight
        : null;
    if (carrier === null) return;
    const decision: ReplaceSubmitDecision = decideReplaceSubmit(
      carrier,
      replaceReasonCode,
      replaceConfirmInput,
    );
    if (decision.kind !== "ready") return;
    replaceModalState = { kind: "submitting" };
    const result = await replaceHostKey(profileId, decision.request);
    if (!result.ok) {
      replaceModalState = {
        kind: "error",
        summary: describeReplaceHostKeyError(result.error),
      };
      return;
    }
    panelState = {
      kind: "replaced",
      preflight: synthesizePostReplacePreflight(carrier, result.replacement),
      replacement: result.replacement,
    };
    resetReplaceForm();
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
    if (panelState.kind === "replaced") {
      // The synthesized preflight already pins the new fingerprint at
      // status `trusted` (see `synthesizePostReplacePreflight`).
      return panelState.preflight;
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

  // Replace-flow derived selectors. `replacementSummary` is sourced
  // from `replaceGateForPreflight(...).kind === "ok"` so the affordance
  // and the submit decision share ONE shape-check — visibility cannot
  // diverge from submit-readiness. R6 ("invisible, not just disabled"
  // for non-changed / missing-active-pin / malformed-fingerprint
  // outcomes) is therefore enforced by a single gate, not by two
  // gates that could drift.
  let replacementSummary = $derived.by<{ old: string; new: string } | null>(
    () => {
      if (preflight === null) return null;
      const gate = replaceGateForPreflight(
        preflight,
        preflight.active_pin_fingerprint,
      );
      if (gate.kind !== "ok") return null;
      return { old: gate.old_fingerprint, new: gate.new_fingerprint };
    },
  );

  let replaceDecision = $derived<ReplaceSubmitDecision | null>(
    preflight
      ? decideReplaceSubmit(
          preflight,
          replaceReasonCode,
          replaceConfirmInput,
        )
      : null,
  );

  let replaceButtonVisible = $derived(
    // Visible only when the gate is `ok` (changed status AND active
    // pin known AND both fingerprint shapes valid). Reason / typed-
    // REPLACE input live INSIDE the modal — they never affect button
    // visibility.
    !disabled &&
      replaceModalState.kind === "closed" &&
      panelState.kind !== "trusted" &&
      panelState.kind !== "replaced" &&
      replacementSummary !== null,
  );

  let replaceSubmitDisabled = $derived(
    replaceModalState.kind === "submitting" ||
      replaceDecision === null ||
      replaceDecision.kind !== "ready",
  );

  let replaceConfirmTouched = $derived(replaceConfirmInput.length > 0);
  let replaceConfirmMismatch = $derived(
    replaceConfirmTouched && replaceConfirmInput !== "REPLACE",
  );

  let preflightButtonLabel = $derived(
    panelState.kind === "preflighting"
      ? "Running preflight…"
      : panelState.kind === "trusted" ||
          panelState.kind === "replaced" ||
          panelState.kind === "ready" ||
          panelState.kind === "preflight_error" ||
          panelState.kind === "trust_error"
        ? "Re-run preflight"
        : "Run host-key preflight",
  );
</script>

<section
  class="flex flex-col gap-2 rounded-md border border-zinc-800/80 bg-zinc-950/30 p-3"
  data-testid="host-key-panel"
  data-profile-id={profileId}
  data-profile-disabled={disabled ? "true" : "false"}
>
  <header class="flex items-center justify-between gap-2">
    <h4 class="text-xs font-semibold uppercase tracking-wide text-zinc-300">
      Host key
    </h4>
    <button
      type="button"
      class="rounded-md border border-zinc-700 bg-zinc-900 px-2 py-1 text-[11px] text-zinc-200 transition hover:border-zinc-600 hover:bg-zinc-800 disabled:cursor-not-allowed disabled:opacity-50"
      onclick={runPreflight}
      disabled={disabled ||
        panelState.kind === "preflighting" ||
        panelState.kind === "trusting" ||
        replaceModalState.kind === "submitting"}
      data-testid="host-key-preflight-button"
      title={disabled
        ? "This profile is disabled — re-enable to run host-key preflight."
        : undefined}
    >
      {preflightButtonLabel}
    </button>
  </header>

  {#if disabled}
    <p
      class="rounded-md border border-amber-900/40 bg-amber-950/20 px-2 py-1.5 text-[11px] text-amber-200/80"
      data-testid="host-key-profile-disabled"
    >
      Profile is disabled. Host-key preflight and trust are blocked until the profile is re-enabled.
    </p>
  {:else}
    <p class="text-[11px] text-zinc-500">{PREFLIGHT_DISCLAIMER}</p>
  {/if}

  {#if disabled}
    <!-- Disabled-profile branch: nothing else to render. The header
         already emits the gated affordance and the inline notice above
         names the gate. -->
  {:else if panelState.kind === "idle"}
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

    {#if panelState.kind === "replaced"}
      <p
        class="rounded-md border border-emerald-900/50 bg-emerald-950/30 px-2 py-1.5 text-[11px] text-emerald-100"
        data-testid="host-key-replaced-success"
      >
        Host key replaced. Run auth-check below to confirm the
        configured SSH identity still authenticates against the new pin.
        Existing live terminal sessions on this profile are not affected.
      </p>
    {:else if panelState.kind === "trusted"}
      <p
        class="rounded-md border border-emerald-900/50 bg-emerald-950/30 px-2 py-1.5 text-[11px] text-emerald-100"
        data-testid="host-key-trusted-success"
      >
        Host key pinned. Re-run preflight to confirm. Run auth-check
        below to verify the configured SSH identity authenticates;
        terminal launch is still future work.
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
      {#if replaceButtonVisible}
        <div class="flex flex-col gap-2">
          <button
            type="button"
            class="self-start rounded-md border border-rose-800/70 bg-rose-950/40 px-3 py-1 text-[11px] text-rose-100 transition hover:border-rose-700 hover:bg-rose-900/40 disabled:cursor-not-allowed disabled:opacity-50"
            onclick={openReplace}
            data-testid="host-key-replace-button"
          >
            Replace trusted host key…
          </button>
          <p class="text-[11px] text-zinc-500">
            Opens a deliberate confirmation dialog. The replace action
            revokes the previously-trusted pin and trusts the
            newly-observed host key in its place — there is no silent
            overwrite.
          </p>
        </div>
      {/if}
      {#if replaceModalState.kind !== "closed" && replacementSummary}
        <div
          class="flex flex-col gap-2 rounded-md border border-rose-900/50 bg-rose-950/30 p-3"
          role="dialog"
          aria-modal="true"
          aria-labelledby="host-key-replace-title"
          data-testid="host-key-replace-modal"
        >
          <h5
            id="host-key-replace-title"
            class="text-xs font-semibold uppercase tracking-wide text-rose-100"
          >
            Replace trusted host key
          </h5>
          <p class="text-[11px] text-rose-100/90">{REPLACE_LEDE}</p>
          <dl class="flex flex-col gap-2 text-[11px]">
            <div class="flex flex-col gap-1">
              <dt class="uppercase tracking-wide text-rose-200/80">
                Profile
              </dt>
              <dd class="text-rose-50">
                {preflight.hostname}:{preflight.port}
              </dd>
            </div>
            <div class="flex flex-col gap-1">
              <dt class="uppercase tracking-wide text-rose-200/80">
                Revoking (old fingerprint)
              </dt>
              <dd>
                <code
                  class="select-all break-all rounded border border-rose-900/60 bg-rose-950/60 px-2 py-1 font-mono text-[11px] text-rose-100"
                  data-testid="host-key-replace-old-fingerprint"
                >
                  {replacementSummary.old}
                </code>
              </dd>
            </div>
            <div class="flex flex-col gap-1">
              <dt class="uppercase tracking-wide text-rose-200/80">
                New fingerprint ({preflight.host_key_type})
              </dt>
              <dd>
                <code
                  class="select-all break-all rounded border border-rose-900/60 bg-rose-950/60 px-2 py-1 font-mono text-[11px] text-rose-100"
                  data-testid="host-key-replace-new-fingerprint"
                >
                  {replacementSummary.new}
                </code>
              </dd>
            </div>
          </dl>
          <label
            class="flex flex-col gap-1 text-[11px] text-rose-100"
          >
            <span class="uppercase tracking-wide text-rose-200/80">
              Reason for replacement
            </span>
            <select
              class="rounded-md border border-rose-900/60 bg-rose-950/60 px-2 py-1 text-[11px] text-rose-50 focus:border-rose-600 focus:outline-none disabled:opacity-50"
              bind:value={replaceReasonCode}
              disabled={replaceModalState.kind === "submitting"}
              data-testid="host-key-replace-reason-select"
            >
              <option value={null} selected={replaceReasonCode === null}>
                Select a reason…
              </option>
              {#each REPLACE_REASON_OPTIONS as opt (opt.code)}
                <option value={opt.code}>{opt.label}</option>
              {/each}
            </select>
          </label>
          <label
            class="flex flex-col gap-1 text-[11px] text-rose-100"
          >
            <span class="uppercase tracking-wide text-rose-200/80">
              Type REPLACE to confirm
            </span>
            <input
              type="text"
              class="rounded-md border border-rose-900/60 bg-rose-950/60 px-2 py-1 font-mono text-[11px] text-rose-50 placeholder:text-rose-300/40 focus:border-rose-600 focus:outline-none disabled:opacity-50"
              bind:value={replaceConfirmInput}
              placeholder="REPLACE"
              disabled={replaceModalState.kind === "submitting"}
              data-testid="host-key-replace-confirm-input"
              autocomplete="off"
              spellcheck="false"
            />
            {#if replaceConfirmMismatch}
              <span
                class="text-[11px] text-amber-300/80"
                data-testid="host-key-replace-confirm-mismatch"
              >
                Type the literal word REPLACE in uppercase to enable
                the action.
              </span>
            {/if}
          </label>
          <p class="text-[11px] text-rose-100/80">{REPLACE_DISCLAIMER}</p>
          <div class="flex flex-wrap items-center gap-2">
            <button
              type="button"
              class="rounded-md border border-rose-700 bg-rose-800 px-3 py-1 text-[11px] text-rose-50 transition hover:border-rose-600 hover:bg-rose-700 disabled:cursor-not-allowed disabled:opacity-50"
              onclick={submitReplace}
              disabled={replaceSubmitDisabled}
              data-testid="host-key-replace-submit"
            >
              {replaceModalState.kind === "submitting"
                ? "Replacing…"
                : "Replace pin"}
            </button>
            <button
              type="button"
              class="rounded-md border border-zinc-700 bg-zinc-900 px-3 py-1 text-[11px] text-zinc-200 transition hover:border-zinc-600 hover:bg-zinc-800 disabled:cursor-not-allowed disabled:opacity-50"
              onclick={cancelReplace}
              disabled={replaceModalState.kind === "submitting"}
              data-testid="host-key-replace-cancel"
            >
              Cancel
            </button>
          </div>
          {#if replaceModalState.kind === "error"}
            <p
              class="rounded-md border border-rose-900/40 bg-rose-950/20 px-2 py-1.5 text-[11px] text-rose-200/80"
              data-testid="host-key-replace-error"
            >
              {replaceModalState.summary}
            </p>
          {/if}
        </div>
      {/if}
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
