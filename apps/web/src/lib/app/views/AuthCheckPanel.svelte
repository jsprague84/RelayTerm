<script lang="ts">
  /**
   * Per-profile SSH auth-check action area.
   *
   * Architectural rules (load-bearing):
   *  - Auth-check is a credential check ONLY. It does NOT open a PTY,
   *    does NOT run commands, does NOT install the public key, and does
   *    NOT create a terminal session. Success copy must explicitly
   *    disclaim that scope so the operator does not mistake "credentials
   *    work" for "shell is ready".
   *  - Trusted host key is a precondition. `host_key_unknown` and
   *    `host_key_changed` outcomes are surfaced as "trust the host key
   *    first" rather than as internal errors.
   *  - The panel holds local Svelte state ONLY. No global stores, no
   *    router, no polling, no auto-retry.
   *  - The panel never logs raw response bodies. Error display goes
   *    through `describeAuthCheckError`, which is a pure function of
   *    `kind` + `status` + `code`.
   *  - No private-key material is ever rendered or implied. The wire
   *    DTO carries no key fields and the parser builds field-by-field.
   */

  import {
    authCheckServerProfile,
    describeAuthCheckError,
    type AuthCheckResponse,
  } from "../../api/serverProfiles.js";
  import {
    AUTH_CHECK_DISCLAIMER,
    AUTH_CHECK_SUCCESS_FOOTNOTE,
    authCheckStatusDescription,
    authCheckStatusLabel,
    authCheckStatusTone,
  } from "../authCheckState.js";

  type State =
    | { kind: "idle" }
    | { kind: "checking" }
    | { kind: "ready"; check: AuthCheckResponse }
    | { kind: "error"; summary: string };

  interface Props {
    profileId: string;
  }

  let { profileId }: Props = $props();

  let panelState = $state<State>({ kind: "idle" });

  function toneClass(tone: "ok" | "warn" | "blocked" | "error"): string {
    switch (tone) {
      case "ok":
        return "border-emerald-800/60 bg-emerald-900/30 text-emerald-200";
      case "warn":
        return "border-amber-900/60 bg-amber-950/40 text-amber-200";
      case "blocked":
        return "border-rose-900/60 bg-rose-950/40 text-rose-200";
      case "error":
        return "border-rose-900/60 bg-rose-950/40 text-rose-200";
    }
  }

  async function runAuthCheck() {
    if (panelState.kind === "checking") return;
    panelState = { kind: "checking" };
    const result = await authCheckServerProfile(profileId);
    if (!result.ok) {
      panelState = {
        kind: "error",
        summary: describeAuthCheckError(result.error),
      };
      return;
    }
    panelState = { kind: "ready", check: result.check };
  }

  let buttonLabel = $derived(
    panelState.kind === "checking"
      ? "Running auth-check…"
      : panelState.kind === "ready" || panelState.kind === "error"
        ? "Re-run auth-check"
        : "Run auth-check",
  );
</script>

<section
  class="flex flex-col gap-2 rounded-md border border-zinc-800/80 bg-zinc-950/30 p-3"
  data-testid="auth-check-panel"
  data-profile-id={profileId}
>
  <header class="flex items-center justify-between gap-2">
    <h4 class="text-xs font-semibold uppercase tracking-wide text-zinc-300">
      Auth-check
    </h4>
    <button
      type="button"
      class="rounded-md border border-zinc-700 bg-zinc-900 px-2 py-1 text-[11px] text-zinc-200 transition hover:border-zinc-600 hover:bg-zinc-800 disabled:cursor-not-allowed disabled:opacity-50"
      onclick={runAuthCheck}
      disabled={panelState.kind === "checking"}
      data-testid="auth-check-run-button"
    >
      {buttonLabel}
    </button>
  </header>

  <p class="text-[11px] text-zinc-500">{AUTH_CHECK_DISCLAIMER}</p>

  {#if panelState.kind === "idle"}
    <p
      class="text-[11px] text-zinc-500"
      data-testid="auth-check-idle"
    >
      Run auth-check to confirm the configured SSH identity authenticates
      to this server. Trust the host key above first.
    </p>
  {:else if panelState.kind === "checking"}
    <p
      class="text-xs text-zinc-400"
      data-testid="auth-check-checking"
    >
      Attempting SSH public-key authentication (no PTY, no command)…
    </p>
  {:else if panelState.kind === "error"}
    <p
      class="rounded-md border border-rose-900/40 bg-rose-950/20 px-2 py-1.5 text-[11px] text-rose-200/80"
      data-testid="auth-check-error"
    >
      {panelState.summary}
    </p>
  {:else if panelState.kind === "ready"}
    {@const check = panelState.check}
    {@const tone = authCheckStatusTone(check.status)}
    <div class="flex flex-col gap-1.5">
      <div class="flex items-center gap-2">
        <span
          class="rounded border px-1.5 py-0.5 text-[11px] font-medium {toneClass(
            tone,
          )}"
          data-testid="auth-check-status-badge"
          data-status={check.status}
          data-tone={tone}
        >
          {authCheckStatusLabel(check.status)}
        </span>
        <time
          class="text-[11px] uppercase tracking-wide text-zinc-500"
          datetime={check.checked_at}
          data-testid="auth-check-checked-at"
        >
          {check.checked_at}
        </time>
      </div>
      <p
        class="text-[11px] text-zinc-400"
        data-testid="auth-check-status-description"
      >
        {authCheckStatusDescription(check.status)}
      </p>
      {#if check.status === "authentication_succeeded"}
        <p
          class="rounded-md border border-emerald-900/50 bg-emerald-950/30 px-2 py-1.5 text-[11px] text-emerald-100"
          data-testid="auth-check-success-footnote"
        >
          {AUTH_CHECK_SUCCESS_FOOTNOTE}
        </p>
      {/if}
    </div>
  {/if}
</section>
