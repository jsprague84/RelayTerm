<script lang="ts">
  /**
   * Settings UI panel for the current user's password rotation.
   *
   * Scope rules (load-bearing):
   *  - Current-user only. There is NO admin password reset, NO
   *    forgot-password / email-recovery flow, and NO password history.
   *  - Verifies the current password server-side; the new password is
   *    enforced against the same length policy as bootstrap / login.
   *  - On success, every OTHER session for the caller is revoked
   *    server-side; the current session stays active. The parent does
   *    NOT need to flip the auth gate — a successful rotation is NOT a
   *    sign-out from this tab.
   *  - On failure, the form clears every password input. We do not
   *    keep partially-entered secrets in memory once a request has
   *    been answered.
   *  - No password values are passed to console / toast / status / log
   *    surfaces. The two formatters in `lib/api/auth.ts` (success +
   *    error) are sentinel-tested and the only strings the UI renders.
   */
  import {
    PASSWORD_MAX_LEN,
    PASSWORD_MIN_LEN,
    changePassword,
    describeChangePasswordError,
    describeChangePasswordFormError,
    describeChangePasswordSuccess,
    validateChangePasswordForm,
  } from "../../api/auth.js";

  type PanelState =
    | { kind: "idle" }
    | { kind: "submitting" }
    | { kind: "success"; message: string }
    | { kind: "failure"; summary: string };

  let currentPassword = $state("");
  let newPassword = $state("");
  let newPasswordConfirmation = $state("");
  let panelState = $state<PanelState>({ kind: "idle" });

  /**
   * Wipe every password field. Called after a request is answered —
   * success OR failure — so a partially-entered or just-rotated value
   * does not linger in the input element across navigations.
   */
  function clearPasswords() {
    currentPassword = "";
    newPassword = "";
    newPasswordConfirmation = "";
  }

  function markDirty() {
    if (
      panelState.kind === "success" ||
      panelState.kind === "failure"
    ) {
      panelState = { kind: "idle" };
    }
  }

  async function submit(event: Event) {
    event.preventDefault();
    if (panelState.kind === "submitting") return;

    const validation = validateChangePasswordForm({
      current_password: currentPassword,
      new_password: newPassword,
      new_password_confirmation: newPasswordConfirmation,
    });
    if (!validation.ok) {
      panelState = {
        kind: "failure",
        summary: describeChangePasswordFormError(validation.reason),
      };
      return;
    }

    panelState = { kind: "submitting" };
    const result = await changePassword({
      current_password: currentPassword,
      new_password: newPassword,
    });

    if (!result.ok) {
      // Clear every password input on the failure path. The exact
      // shape (HTTP / transport / malformed) is irrelevant to the UI:
      // a failed change MUST not leave secrets in the form for a
      // future render or autofill cycle to capture.
      clearPasswords();
      panelState = {
        kind: "failure",
        summary: describeChangePasswordError(result.error),
      };
      return;
    }

    clearPasswords();
    panelState = {
      kind: "success",
      message: describeChangePasswordSuccess(result.response),
    };
  }
</script>

<article
  class="flex flex-col gap-4 rounded-lg border border-zinc-800 bg-zinc-950/40 p-6"
  data-testid="settings-password-panel"
>
  <header class="flex flex-col gap-1">
    <h3 class="text-sm font-semibold text-zinc-100">Password</h3>
    <p class="text-xs text-zinc-500">
      Change the password you sign in with. You'll need to enter your
      current password to confirm it's you. After a successful change,
      every other browser session signed in to this account will be
      signed out — this tab stays signed in.
    </p>
  </header>

  <form class="flex flex-col gap-3" onsubmit={submit}>
    <label class="flex flex-col gap-1 text-sm text-zinc-200">
      <span class="text-xs uppercase tracking-wide text-zinc-400">
        Current password
      </span>
      <input
        type="password"
        autocomplete="current-password"
        spellcheck="false"
        class="rounded-md border border-zinc-700 bg-zinc-900 px-3 py-1.5 text-sm text-zinc-100 placeholder:text-zinc-600 focus:border-emerald-700 focus:outline-none"
        bind:value={currentPassword}
        oninput={markDirty}
        maxlength={PASSWORD_MAX_LEN}
        required
        data-testid="settings-password-current"
      />
    </label>

    <label class="flex flex-col gap-1 text-sm text-zinc-200">
      <span class="text-xs uppercase tracking-wide text-zinc-400">
        New password
      </span>
      <input
        type="password"
        autocomplete="new-password"
        spellcheck="false"
        class="rounded-md border border-zinc-700 bg-zinc-900 px-3 py-1.5 text-sm text-zinc-100 placeholder:text-zinc-600 focus:border-emerald-700 focus:outline-none"
        bind:value={newPassword}
        oninput={markDirty}
        minlength={PASSWORD_MIN_LEN}
        maxlength={PASSWORD_MAX_LEN}
        required
        data-testid="settings-password-new"
      />
      <span class="text-[11px] text-zinc-500">
        At least {PASSWORD_MIN_LEN} characters. The backend rejects passwords
        outside the {PASSWORD_MIN_LEN}–{PASSWORD_MAX_LEN}-character range.
      </span>
    </label>

    <label class="flex flex-col gap-1 text-sm text-zinc-200">
      <span class="text-xs uppercase tracking-wide text-zinc-400">
        Confirm new password
      </span>
      <input
        type="password"
        autocomplete="new-password"
        spellcheck="false"
        class="rounded-md border border-zinc-700 bg-zinc-900 px-3 py-1.5 text-sm text-zinc-100 placeholder:text-zinc-600 focus:border-emerald-700 focus:outline-none"
        bind:value={newPasswordConfirmation}
        oninput={markDirty}
        minlength={PASSWORD_MIN_LEN}
        maxlength={PASSWORD_MAX_LEN}
        required
        data-testid="settings-password-confirm"
      />
    </label>

    <div class="flex flex-wrap items-center gap-2">
      <button
        type="submit"
        class="rounded-md border border-emerald-700 bg-emerald-800 px-3 py-1.5 text-sm text-emerald-50 transition hover:border-emerald-600 hover:bg-emerald-700 disabled:cursor-not-allowed disabled:opacity-60"
        disabled={panelState.kind === "submitting"}
        data-testid="settings-password-submit"
      >
        {panelState.kind === "submitting" ? "Updating…" : "Update password"}
      </button>
      {#if panelState.kind === "success"}
        <span
          class="text-xs text-emerald-300"
          data-testid="settings-password-status-success"
        >
          {panelState.message}
        </span>
      {:else if panelState.kind === "failure"}
        <span
          class="text-xs text-rose-300"
          data-testid="settings-password-status-failure"
        >
          {panelState.summary}
        </span>
      {/if}
    </div>
  </form>

  <p
    class="rounded-md border border-amber-900/40 bg-amber-950/20 px-3 py-2 text-xs text-amber-200/80"
  >
    <span class="font-mono uppercase tracking-wide">future work</span> ·
    Forgot-password / email recovery, passkeys / WebAuthn, and admin-
    initiated password reset are deliberate later slices. Today this
    surface is the only way to rotate a password.
  </p>
</article>
