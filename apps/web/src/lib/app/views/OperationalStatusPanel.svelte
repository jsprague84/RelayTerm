<script lang="ts">
  /**
   * Operational Status panel — single self-hosted RelayTerm operator
   * "is it healthy?" view that lives inside the Settings page.
   *
   * Scope rules (load-bearing):
   *  - Local browser + already-available API data only. NO new backend
   *    endpoints. NO secret / env / config value display. NO retry
   *    storms — manual refresh button only.
   *  - Production xterm baseline is honestly named. The panel does
   *    NOT promote experimental renderers and does NOT change the
   *    default; flipping a renderer setting still happens in the
   *    "Experimental renderer evaluation" card above.
   *  - Readiness section points to operator runbooks for B2 (production
   *    smoke) and B3 (mobile portrait sanity). It NEVER claims either
   *    is done — those are operator-walked artefacts that live in the
   *    deployment log, not in the SPA.
   *  - Failure paths render `Unavailable` with the typed safe summary
   *    from the api-layer formatter. We do NOT echo the wire `message`
   *    or transport detail in any rendered string.
   *
   * Mobile posture: card grid collapses to a single column under the
   * `sm:` breakpoint; the manual refresh control has a 40 px tap
   * target (`min-h-10`); no hover-only affordances.
   */
  import {
    describeAuthGateError,
    listAuthSessions,
    type AuthError,
    type AuthSession,
    type CurrentUser,
  } from "../../api/auth.js";
  import { checkHealth, type HealthStatus } from "../../api/health.js";
  import {
    loadSessionPolicy,
    type SessionPolicy,
  } from "../../api/sessionPolicy.js";
  import {
    listTerminalSessions,
    type TerminalSession,
  } from "../../api/terminalSessions.js";
  import type { LoadResult } from "../../api/apiErrors.js";
  import {
    loadTerminalSettings,
    type TerminalSettings,
  } from "../settings/terminalSettings.js";
  import {
    buildSessionPolicySummary,
    describeAuthSessions,
    describeAutofit,
    describeDetachedTtlIndicator,
    describeEffectiveRenderer,
    describeExperimentalGate,
    describeQuotaIndicator,
    describeTerminalSessions,
    summarizeAccount,
    summarizeAuthSessions,
    summarizeHealth,
    summarizeTerminalDefaults,
    summarizeTerminalSessions,
    TERMINAL_SESSION_STATUS_DISPLAY_ORDER,
    terminalSessionStatusLabel,
    toneClass,
    type IndicatorState,
  } from "../settings/operationalStatus.js";

  interface Props {
    /**
     * Authenticated caller, threaded down from `AppShell.svelte`. The
     * panel renders without a user (some unit-test mounts skip the
     * auth gate) but in production the account section is always
     * populated.
     */
    user?: CurrentUser | null;
  }

  let { user = null }: Props = $props();

  type AuthSessionsLoad =
    | { ok: true; sessions: AuthSession[] }
    | { ok: false; error: AuthError }
    | null;

  let health = $state<HealthStatus>("unknown");
  let authSessionsResult = $state<AuthSessionsLoad>(null);
  let terminalSessionsResult = $state<LoadResult<TerminalSession[]> | null>(
    null,
  );
  let sessionPolicy = $state<SessionPolicy | null>(null);
  let terminalSettings = $state<TerminalSettings>(loadTerminalSettings());
  let isRefreshing = $state(false);
  let lastRefreshedAt = $state<string | null>(null);

  const account = $derived(summarizeAccount(user));
  const healthIndicator = $derived(summarizeHealth(health));
  const authSessionsSummary = $derived(
    summarizeAuthSessions(authSessionsResult),
  );
  const authSessionsIndicator = $derived(
    describeAuthSessions(authSessionsSummary),
  );
  const terminalSessionsSummary = $derived(
    summarizeTerminalSessions(terminalSessionsResult),
  );
  const terminalSessionsIndicator = $derived(
    describeTerminalSessions(terminalSessionsSummary),
  );
  const defaults = $derived(summarizeTerminalDefaults(terminalSettings));
  const policySummary = $derived(buildSessionPolicySummary(sessionPolicy));
  const ttlIndicator = $derived(describeDetachedTtlIndicator(policySummary));
  const quotaIndicator = $derived(describeQuotaIndicator(policySummary));
  const experimentalIndicator = $derived(describeExperimentalGate(defaults));
  const autofitIndicator = $derived(describeAutofit(defaults));

  /**
   * Always-safe core fetch. WRITES `$state` only — never READS any
   * reactive value before the first `await`. The mount-time `$effect`
   * calls this directly so the effect's synchronous frame collects
   * ZERO dependencies and the effect runs exactly once at mount.
   *
   * If a future edit introduces a `$state` read inside this function
   * BEFORE the first `await`, the mount-time effect will subscribe to
   * that state and re-run on every write made after the await — i.e.
   * an infinite refresh loop on mount. Mirror the AuthSessionsPanel /
   * DashboardView pattern: writes only, then await, then writes.
   */
  async function runRefresh() {
    // Re-read local settings synchronously — the operator may have
    // hit "Save changes" in the appearance card just above and we
    // want the indicator to reflect that without a navigation. This
    // is a WRITE (`terminalSettings = …`); `loadTerminalSettings`
    // itself only reads localStorage, not any `$state`.
    terminalSettings = loadTerminalSettings();
    const [healthResult, authResult, sessionsResult, policy] =
      await Promise.all([
        checkHealth(),
        listAuthSessions(),
        listTerminalSessions(),
        loadSessionPolicy(),
      ]);
    health = healthResult;
    authSessionsResult = authResult;
    terminalSessionsResult = sessionsResult;
    sessionPolicy = policy;
    lastRefreshedAt = new Date().toISOString();
  }

  /**
   * Operator-triggered refresh handler. Owns the in-flight guard so a
   * double-tap on the Refresh button does not fire two concurrent
   * fetches. The mount-time `$effect` does NOT route through this
   * function — it calls {@link runRefresh} directly — so the
   * `isRefreshing` read here cannot subscribe an effect and cannot
   * create a refresh loop.
   */
  async function refresh() {
    if (isRefreshing) return;
    isRefreshing = true;
    try {
      await runRefresh();
    } finally {
      isRefreshing = false;
    }
  }

  // Mount-only fetch. See {@link runRefresh} for the no-reads-before-
  // await invariant that keeps this effect a single-run.
  $effect(() => {
    void runRefresh();
  });

  function formatTimestamp(rfc3339: string | null | undefined): string {
    if (rfc3339 == null) return "—";
    const t = Date.parse(rfc3339);
    if (Number.isNaN(t)) return "—";
    return new Date(t).toLocaleString();
  }

  function describeAuthGateAvailability(state: AuthSessionsLoad): string {
    // The per-helper `describeAuthSessionsError` is the safe formatter
    // for the session-list call itself. This auxiliary helper renders a
    // shorter availability label next to the account block: it never
    // echoes the wire `message` (uses `describeAuthGateError`, which
    // stays a function of kind + status only).
    if (state === null) return "Loading…";
    if (state.ok) return "Authenticated";
    return describeAuthGateError(state.error);
  }
</script>

<article
  class="flex flex-col gap-4 rounded-lg border border-zinc-800 bg-zinc-950/40 p-6"
  data-testid="settings-operational-status"
  aria-labelledby="settings-operational-status-heading"
>
  <header
    class="flex flex-col gap-2 sm:flex-row sm:items-start sm:justify-between"
  >
    <div class="flex flex-col gap-1">
      <h3
        id="settings-operational-status-heading"
        class="text-sm font-semibold text-zinc-100"
      >
        Operational status
      </h3>
      <p class="text-xs text-zinc-500">
        A quick "is it healthy?" view for this RelayTerm deployment.
        Uses already-loaded API data; nothing here calls a new backend
        endpoint and nothing is shown that could leak secrets or
        deployment configuration values.
      </p>
    </div>
    <button
      type="button"
      class="inline-flex min-h-10 items-center justify-center self-start rounded-md border border-zinc-700 bg-zinc-900 px-3 py-1.5 text-sm text-zinc-200 transition hover:border-zinc-600 hover:bg-zinc-800 disabled:cursor-not-allowed disabled:opacity-60"
      onclick={refresh}
      disabled={isRefreshing}
      data-testid="settings-operational-status-refresh"
      aria-label="Refresh operational status"
    >
      {isRefreshing ? "Refreshing…" : "Refresh"}
    </button>
  </header>

  {#if lastRefreshedAt !== null}
    <p
      class="text-[11px] text-zinc-500"
      data-testid="settings-operational-status-last-refreshed"
    >
      Last refreshed {formatTimestamp(lastRefreshedAt)}
    </p>
  {/if}

  <section
    class="flex flex-col gap-2 rounded-md border border-zinc-800 bg-zinc-950/60 p-4"
    data-testid="settings-operational-status-account"
  >
    <h4 class="text-xs uppercase tracking-wide text-zinc-400">Account</h4>
    {#if account !== null}
      <dl
        class="grid grid-cols-1 gap-x-4 gap-y-1 text-xs text-zinc-300 sm:grid-cols-[max-content_1fr]"
      >
        <dt class="text-zinc-500">Email</dt>
        <dd
          class="break-words font-mono text-zinc-200"
          data-testid="settings-operational-status-account-email"
        >
          {account.email}
        </dd>
        <dt class="text-zinc-500">Display name</dt>
        <dd
          class="break-words text-zinc-200"
          data-testid="settings-operational-status-account-display-name"
        >
          {account.display_name}
        </dd>
        <dt class="text-zinc-500">Account created</dt>
        <dd class="text-zinc-200">
          {formatTimestamp(account.account_created_at)}
        </dd>
        <dt class="text-zinc-500">Last sign-in</dt>
        <dd class="text-zinc-200">
          {formatTimestamp(account.last_login_at)}
        </dd>
      </dl>
    {:else}
      <p class="text-xs text-zinc-500">
        Account details are unavailable until the auth gate resolves.
      </p>
    {/if}
    <p
      class="text-[11px] text-zinc-500"
      data-testid="settings-operational-status-account-availability"
    >
      Auth API: {describeAuthGateAvailability(authSessionsResult)}
    </p>
  </section>

  <section
    class="grid grid-cols-1 gap-3 sm:grid-cols-2"
    data-testid="settings-operational-status-indicators"
  >
    {@render indicator(
      "Backend reachability",
      healthIndicator,
      "settings-operational-status-health",
    )}
    {@render indicator(
      "Browser sessions",
      authSessionsIndicator,
      "settings-operational-status-auth-sessions",
    )}
    {@render indicator(
      "Terminal sessions",
      terminalSessionsIndicator,
      "settings-operational-status-terminal-sessions",
    )}
    {@render indicator(
      "Detached PTY window",
      ttlIndicator,
      "settings-operational-status-detached-ttl",
    )}
    {@render indicator(
      "Per-user quotas",
      quotaIndicator,
      "settings-operational-status-quotas",
    )}
    {@render indicator(
      "Experimental gate",
      experimentalIndicator,
      "settings-operational-status-experimental-gate",
    )}
    {@render indicator(
      "Autofit",
      autofitIndicator,
      "settings-operational-status-autofit",
    )}
  </section>

  <section
    class="flex flex-col gap-2 rounded-md border border-zinc-800 bg-zinc-950/60 p-4"
    data-testid="settings-operational-status-terminal-breakdown"
  >
    <h4 class="text-xs uppercase tracking-wide text-zinc-400">
      Terminal sessions by status
    </h4>
    {#if terminalSessionsSummary.kind === "loading"}
      <p class="text-xs text-zinc-500">Loading…</p>
    {:else if terminalSessionsSummary.kind === "unavailable"}
      <p class="text-xs text-amber-200">
        Terminal session counts are unavailable.
      </p>
    {:else}
      <ul
        class="grid grid-cols-2 gap-1 text-xs text-zinc-300 sm:grid-cols-4"
      >
        {#each TERMINAL_SESSION_STATUS_DISPLAY_ORDER as status (status)}
          <li
            class="flex items-baseline justify-between rounded border border-zinc-800 bg-zinc-950/60 px-2 py-1"
            data-testid={`settings-operational-status-terminal-count-${status}`}
          >
            <span class="text-zinc-400">
              {terminalSessionStatusLabel(status)}
            </span>
            <span class="font-mono text-zinc-100">
              {terminalSessionsSummary.counts[status]}
            </span>
          </li>
        {/each}
      </ul>
    {/if}
  </section>

  <section
    class="flex flex-col gap-2 rounded-md border border-zinc-800 bg-zinc-950/60 p-4"
    data-testid="settings-operational-status-defaults"
  >
    <h4 class="text-xs uppercase tracking-wide text-zinc-400">
      Terminal defaults for this browser
    </h4>
    <p
      class="text-xs text-zinc-300"
      data-testid="settings-operational-status-effective-renderer"
    >
      {describeEffectiveRenderer(defaults)}
    </p>
    <p class="text-[11px] text-zinc-500">
      The "Terminal appearance" and "Experimental renderer evaluation"
      cards above own the controls that change these values; this row
      is read-only so the operator can confirm what the next session
      will mount.
    </p>
  </section>

  <section
    class="flex flex-col gap-2 rounded-md border border-zinc-800 bg-zinc-950/60 p-4"
    data-testid="settings-operational-status-diagnostics"
  >
    <h4 class="text-xs uppercase tracking-wide text-zinc-400">
      Launch diagnostics
    </h4>
    <p class="text-xs text-zinc-400">
      Per-launch timing diagnostics (POST → WebSocket open → attach)
      are surfaced inside the terminal workspace while a session is
      open. The Operational Status panel intentionally does not cache
      or replay those values — open a terminal session to read them
      live.
    </p>
  </section>

  <section
    class="flex flex-col gap-2 rounded-md border border-amber-900/40 bg-amber-950/10 p-4"
    data-testid="settings-operational-status-readiness"
  >
    <h4 class="text-xs uppercase tracking-wide text-amber-200/80">
      Production readiness reminders
    </h4>
    <ul class="flex flex-col gap-1 text-xs text-amber-100/80">
      <li>
        Take a <span class="font-mono">pg_dump</span> before every upgrade
        and keep it off-host — procedure in
        <span class="font-mono">docs/deployment/backup-restore-runbook.md</span>.
      </li>
      <li>
        A production-walked end-to-end smoke (B2) and a mobile portrait
        sanity walk (B3) are operator-recorded steps — this panel does
        not substitute for them. Walk
        <span class="font-mono">docs/v1-release-checklist.md</span> before
        cutting the tag.
      </li>
      <li>
        xterm is the v1 production default renderer. Experimental
        renderers stay opt-in via the gate above and are not promoted
        in v1.
      </li>
    </ul>
  </section>
</article>

{#snippet indicator(label: string, state: IndicatorState, testid: string)}
  <div
    class="flex flex-col gap-1 rounded-md border border-zinc-800 bg-zinc-950/60 p-3 text-xs"
    data-testid={testid}
  >
    <span class="text-zinc-500">{label}</span>
    {#if state.kind === "loading"}
      <span class="text-zinc-400" data-testid={`${testid}-loading`}>
        Loading…
      </span>
    {:else if state.kind === "unavailable"}
      <span
        class="inline-flex items-center self-start rounded border border-rose-900/60 bg-rose-950/30 px-2 py-0.5 text-rose-200"
        data-testid={`${testid}-unavailable`}
      >
        Unavailable
      </span>
      <span class="text-[11px] text-zinc-500">{state.summary}</span>
    {:else}
      <span
        class={`inline-flex items-center self-start rounded border px-2 py-0.5 ${toneClass(state.tone)}`}
        data-testid={`${testid}-value`}
      >
        {state.value}
      </span>
    {/if}
  </div>
{/snippet}
