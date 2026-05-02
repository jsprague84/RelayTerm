<script lang="ts">
  /**
   * Settings UI panel for the current user's browser login sessions.
   *
   * Scope rules (load-bearing):
   *  - Current-user only. There is NO admin / cross-user view.
   *  - Cookie-bearing API; the SPA never reads or echoes the cookie.
   *    Token / token_hash / password_hash material is parser-redacted —
   *    it cannot reach the rendered DOM.
   *  - No retry storms. The mount fetches once; the operator presses
   *    "Refresh" to re-fetch.
   *  - No remote_addr / user_agent / device-name surface — backend
   *    does not expose those fields yet, so the UI does not render
   *    placeholders for them.
   *  - Confirm BEFORE revoking the current session OR before revoking
   *    all other sessions. A revoke against the current session is
   *    routed through the parent's `signOut` handler so AppShell's
   *    local cleanup runs (active-launch drop, navigation reset).
   */
  import {
    describeAuthSessionStatus,
    describeAuthSessionsError,
    listAuthSessions,
    revokeAllAuthSessionsExceptCurrent,
    revokeAuthSession,
    type AuthSession,
  } from "../../api/auth.js";

  interface Props {
    /**
     * Invoked AFTER the wire-side current-session revoke succeeds.
     * The parent (AppShell, via the auth gate) is responsible for
     * the local-cleanup contract (active-launch drop, gate flip).
     * Optional so the panel can be unit-mounted without wiring the
     * full shell.
     */
    onCurrentSessionRevoked?: () => void;
  }

  let { onCurrentSessionRevoked }: Props = $props();

  type ListState =
    | { kind: "idle" }
    | { kind: "loading" }
    | { kind: "ready"; sessions: AuthSession[] }
    | { kind: "error"; summary: string };

  type ActionState =
    | { kind: "idle" }
    | { kind: "revoking"; sessionId: string }
    | { kind: "revoking_all" }
    | { kind: "success"; message: string }
    | { kind: "failure"; summary: string };

  let listState = $state<ListState>({ kind: "idle" });
  let action = $state<ActionState>({ kind: "idle" });

  async function load() {
    listState = { kind: "loading" };
    const result = await listAuthSessions();
    if (!result.ok) {
      listState = {
        kind: "error",
        summary: describeAuthSessionsError(result.error),
      };
      return;
    }
    listState = { kind: "ready", sessions: result.sessions };
  }

  // Mount-only fetch. `load()` is called for side-effect; it does not
  // read any reactive `$state` here, so the effect never re-runs. If a
  // future edit to `load()` introduces a reactive read, this becomes
  // a fetch loop — keep load's body free of `$state` reads, or move
  // the call out of `$effect`.
  $effect(() => {
    void load();
  });

  function formatTimestamp(rfc3339: string): string {
    const t = Date.parse(rfc3339);
    if (Number.isNaN(t)) return rfc3339;
    return new Date(t).toLocaleString();
  }

  function shortId(id: string): string {
    // First 8 hex chars of a UUID is enough to disambiguate in a
    // current-user-scoped list while keeping the rendered string short.
    return id.length > 8 ? `${id.slice(0, 8)}…` : id;
  }

  async function handleRevoke(session: AuthSession) {
    if (action.kind === "revoking" || action.kind === "revoking_all") return;
    if (session.status !== "active") return;

    if (session.current) {
      // Revoking the current session is effectively a sign-out from
      // the session-management surface. We confirm explicitly because
      // the UX is destructive (the user loses access from this tab).
      const ok =
        typeof window === "undefined"
          ? true
          : window.confirm(
              "Sign out of this browser? You will be returned to the login screen.",
            );
      if (!ok) return;
    } else {
      const ok =
        typeof window === "undefined"
          ? true
          : window.confirm(
              "Revoke this session? The other browser will be signed out on its next request.",
            );
      if (!ok) return;
    }

    action = { kind: "revoking", sessionId: session.id };
    const result = await revokeAuthSession(session.id, {
      current: session.current,
    });
    if (!result.ok) {
      action = {
        kind: "failure",
        summary: describeAuthSessionsError(result.error),
      };
      return;
    }

    if (result.current) {
      // Backend has cleared the cookie via Set-Cookie. Hand off to the
      // parent so AppShell drops the active-launch pointer and the
      // auth gate flips to the login screen. Do NOT keep rendering
      // the (now stale) session list afterwards.
      action = { kind: "idle" };
      onCurrentSessionRevoked?.();
      return;
    }

    action = {
      kind: "success",
      message: "Session revoked. The other browser will be signed out on its next request.",
    };
    await load();
  }

  async function handleRevokeAllOthers() {
    if (action.kind === "revoking" || action.kind === "revoking_all") return;
    const ok =
      typeof window === "undefined"
        ? true
        : window.confirm(
            "Revoke all other browser sessions? Only this browser will remain signed in.",
          );
    if (!ok) return;

    action = { kind: "revoking_all" };
    const result = await revokeAllAuthSessionsExceptCurrent();
    if (!result.ok) {
      action = {
        kind: "failure",
        summary: describeAuthSessionsError(result.error),
      };
      return;
    }
    const count = result.revoked_count;
    const noun = count === 1 ? "session" : "sessions";
    action = {
      kind: "success",
      message:
        count > 0
          ? `Revoked ${count} other ${noun}.`
          : "No other sessions to revoke.",
    };
    await load();
  }

  function statusToneClass(session: AuthSession): string {
    if (session.status === "revoked") {
      return "border-rose-900/60 bg-rose-950/30 text-rose-200";
    }
    if (session.status === "expired") {
      return "border-amber-900/60 bg-amber-950/20 text-amber-200";
    }
    return "border-emerald-900/60 bg-emerald-950/30 text-emerald-200";
  }

  const otherActiveCount = $derived(
    listState.kind === "ready"
      ? listState.sessions.filter(
          (s) => !s.current && s.status === "active",
        ).length
      : 0,
  );
</script>

<article
  class="flex flex-col gap-3 rounded-lg border border-zinc-800 bg-zinc-950/40 p-6"
  data-testid="settings-auth-sessions"
>
  <header class="flex items-center justify-between gap-3">
    <div class="flex flex-col gap-1">
      <h3 class="text-sm font-semibold text-zinc-100">
        Active sessions
      </h3>
      <p class="text-xs text-zinc-500">
        Browser login sessions for your account. The session marked
        <span class="font-medium text-emerald-300">current</span> is
        this browser. Revoking another session signs it out on its
        next request.
      </p>
    </div>
    <div class="flex shrink-0 items-center gap-2">
      <button
        type="button"
        class="rounded-md border border-zinc-700 bg-zinc-900 px-3 py-1.5 text-xs text-zinc-200 transition hover:border-zinc-600 hover:bg-zinc-800 disabled:cursor-not-allowed disabled:opacity-60"
        onclick={() => void load()}
        disabled={listState.kind === "loading"}
        data-testid="settings-auth-sessions-refresh"
      >
        {listState.kind === "loading" ? "Refreshing…" : "Refresh"}
      </button>
      <button
        type="button"
        class="rounded-md border border-rose-900 bg-rose-950/40 px-3 py-1.5 text-xs text-rose-100 transition hover:border-rose-700 hover:bg-rose-900/40 disabled:cursor-not-allowed disabled:opacity-60"
        onclick={() => void handleRevokeAllOthers()}
        disabled={listState.kind !== "ready" ||
          otherActiveCount === 0 ||
          action.kind === "revoking" ||
          action.kind === "revoking_all"}
        data-testid="settings-auth-sessions-revoke-all"
      >
        {action.kind === "revoking_all"
          ? "Revoking…"
          : "Revoke all other sessions"}
      </button>
    </div>
  </header>

  {#if action.kind === "success"}
    <p
      class="rounded-md border border-emerald-900/40 bg-emerald-950/20 px-3 py-2 text-xs text-emerald-200"
      data-testid="settings-auth-sessions-success"
    >
      {action.message}
    </p>
  {:else if action.kind === "failure"}
    <p
      class="rounded-md border border-rose-900/40 bg-rose-950/20 px-3 py-2 text-xs text-rose-200"
      data-testid="settings-auth-sessions-action-error"
    >
      {action.summary}
    </p>
  {/if}

  {#if listState.kind === "idle" || listState.kind === "loading"}
    <p
      class="text-xs text-zinc-500"
      data-testid="settings-auth-sessions-loading"
    >
      Loading sessions…
    </p>
  {:else if listState.kind === "error"}
    <p
      class="rounded-md border border-rose-900/40 bg-rose-950/20 px-3 py-2 text-xs text-rose-200"
      data-testid="settings-auth-sessions-error"
    >
      {listState.summary}
    </p>
  {:else if listState.sessions.length === 0}
    <p
      class="text-xs text-zinc-500"
      data-testid="settings-auth-sessions-empty"
    >
      No sessions found.
    </p>
  {:else}
    <ul
      class="flex flex-col gap-2 text-sm text-zinc-200"
      data-testid="settings-auth-sessions-list"
    >
      {#each listState.sessions as session (session.id)}
        {@const isRevoking =
          action.kind === "revoking" && action.sessionId === session.id}
        <li
          class="flex flex-col gap-2 rounded-md border border-zinc-800 bg-zinc-900/40 px-3 py-2"
          data-testid="settings-auth-sessions-row"
          data-current={session.current ? "true" : "false"}
          data-status={session.status}
        >
          <div class="flex flex-wrap items-center gap-2">
            <span
              class="font-mono text-[11px] uppercase tracking-wide text-zinc-500"
              data-testid="settings-auth-sessions-row-id"
            >
              {shortId(session.id)}
            </span>
            {#if session.current}
              <span
                class="rounded-full border border-emerald-700 bg-emerald-900/40 px-2 py-0.5 text-[10px] font-semibold uppercase tracking-wide text-emerald-200"
                data-testid="settings-auth-sessions-current-badge"
              >
                Current
              </span>
            {/if}
            <span
              class="rounded-full border px-2 py-0.5 text-[10px] font-semibold uppercase tracking-wide {statusToneClass(
                session,
              )}"
              data-testid="settings-auth-sessions-status-badge"
            >
              {describeAuthSessionStatus(session.status)}
            </span>
          </div>
          <dl
            class="grid grid-cols-1 gap-x-4 gap-y-0.5 text-[11px] text-zinc-500 md:grid-cols-2"
          >
            <div class="flex gap-2">
              <dt class="text-zinc-500">Created</dt>
              <dd class="font-mono text-zinc-300">
                {formatTimestamp(session.created_at)}
              </dd>
            </div>
            <div class="flex gap-2">
              <dt class="text-zinc-500">Last seen</dt>
              <dd class="font-mono text-zinc-300">
                {formatTimestamp(session.last_seen_at)}
              </dd>
            </div>
            <div class="flex gap-2">
              <dt class="text-zinc-500">Expires</dt>
              <dd class="font-mono text-zinc-300">
                {formatTimestamp(session.expires_at)}
              </dd>
            </div>
            {#if session.revoked_at}
              <div class="flex gap-2">
                <dt class="text-zinc-500">Revoked</dt>
                <dd class="font-mono text-zinc-300">
                  {formatTimestamp(session.revoked_at)}
                </dd>
              </div>
            {/if}
          </dl>
          {#if session.status === "active"}
            <div class="flex justify-end">
              <button
                type="button"
                class="rounded-md border border-rose-900 bg-rose-950/40 px-2.5 py-1 text-[11px] text-rose-100 transition hover:border-rose-700 hover:bg-rose-900/40 disabled:cursor-not-allowed disabled:opacity-60"
                onclick={() => void handleRevoke(session)}
                disabled={isRevoking ||
                  action.kind === "revoking_all" ||
                  (action.kind === "revoking" &&
                    action.sessionId !== session.id)}
                data-testid={session.current
                  ? "settings-auth-sessions-revoke-current"
                  : "settings-auth-sessions-revoke"}
              >
                {isRevoking
                  ? "Revoking…"
                  : session.current
                    ? "Sign out this browser"
                    : "Revoke"}
              </button>
            </div>
          {/if}
        </li>
      {/each}
    </ul>
  {/if}

  <p
    class="rounded-md border border-amber-900/40 bg-amber-950/20 px-3 py-2 text-[11px] text-amber-200/80"
    data-testid="settings-auth-sessions-future-note"
  >
    <span class="font-mono uppercase tracking-wide">future work</span> ·
    Remote address, user-agent, and per-device names are deliberately
    not displayed — the backend does not expose them yet. Password
    reset, passkeys/WebAuthn, and admin/cross-user session views are
    later slices.
  </p>
</article>
