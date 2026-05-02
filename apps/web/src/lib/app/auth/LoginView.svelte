<script lang="ts">
  /**
   * Production sign-in form.
   *
   * Scope (load-bearing):
   *  - Email + password fields. No "remember me", no SSO, no passkeys —
   *    those are deliberate later slices (see SPEC.md "Frontend
   *    authentication UI plan").
   *  - The 401 from the wire collapses to a single "invalid credentials"
   *    string. The form NEVER reveals whether the offered email belongs
   *    to a known account; the audit log is the only place the
   *    distinction lives.
   *  - Plaintext password and the offered email are not logged, are not
   *    echoed in any error string, and never reach `console.*`.
   *  - First-time setup is reachable from this view via the "First-time
   *    setup" link. The link does NOT auto-discover whether bootstrap
   *    is currently allowed — operators run the bootstrap flow knowing
   *    whether the backend has a token configured. A 503 from the
   *    bootstrap route is the wire-side rejection.
   */
  import {
    describeAuthError,
    describeLoginFormError,
    login as loginApi,
    validateLoginForm,
    type CurrentUser,
    type LoginFormError,
  } from "../../api/auth.js";

  interface Props {
    /** Called after a successful sign-in with the parsed current
     * user. The shell uses this to swap to the authenticated view
     * tree. */
    onSignedIn: (user: CurrentUser) => void;
    /** Switch the unauthenticated screen to the bootstrap form. */
    onRequestBootstrap: () => void;
  }

  let { onSignedIn, onRequestBootstrap }: Props = $props();

  let email = $state("");
  let password = $state("");
  let submitting = $state(false);
  let formError = $state<LoginFormError | null>(null);
  let wireError = $state<string | null>(null);

  let trimmedEmail = $derived(email.trim());
  // The submit button is disabled while empty / in flight; the full
  // form validator runs on submit so the operator sees the precise
  // reason rather than a silently-disabled button.
  let canSubmit = $derived(
    !submitting && trimmedEmail.length > 0 && password.length > 0,
  );

  async function handleSubmit(event: SubmitEvent) {
    event.preventDefault();
    if (submitting) return;

    formError = null;
    wireError = null;

    const validation = validateLoginForm({ email: trimmedEmail, password });
    if (!validation.ok) {
      formError = validation.reason;
      return;
    }

    submitting = true;
    try {
      const result = await loginApi({ email: trimmedEmail, password });
      if (!result.ok) {
        wireError = describeAuthError("sign in", result.error);
        // Clear the password on any failure so a retry starts from a
        // fresh field. Email is preserved (operator just typed it).
        password = "";
        return;
      }
      // Successful sign-in: hand the user up. The shell takes over
      // from here. Reset local state so a future re-render of this
      // view (e.g. session expiry) starts clean.
      onSignedIn(result.user);
      password = "";
    } finally {
      submitting = false;
    }
  }

  function handleRequestBootstrap() {
    if (submitting) return;
    onRequestBootstrap();
  }
</script>

<div
  class="flex min-h-screen items-center justify-center bg-zinc-900 px-4 py-10"
  data-testid="auth-login-screen"
>
  <section
    class="flex w-full max-w-sm flex-col gap-6 rounded-lg border border-zinc-800 bg-zinc-950/60 p-6 shadow-2xl"
  >
    <header class="flex flex-col gap-1">
      <h1
        class="text-lg font-semibold tracking-tight text-zinc-100"
        data-testid="auth-login-heading"
      >
        Sign in to RelayTerm
      </h1>
      <p class="text-sm text-zinc-400">
        Use your account email and password.
      </p>
    </header>

    <form
      class="flex flex-col gap-4"
      data-testid="auth-login-form"
      onsubmit={handleSubmit}
      novalidate
    >
      <label class="flex flex-col gap-1 text-sm text-zinc-300">
        <span>Email</span>
        <input
          type="email"
          autocomplete="username"
          required
          bind:value={email}
          disabled={submitting}
          data-testid="auth-login-email"
          class="rounded-md border border-zinc-700 bg-zinc-900 px-3 py-2 text-zinc-100 outline-none focus:border-zinc-500"
        />
      </label>

      <label class="flex flex-col gap-1 text-sm text-zinc-300">
        <span>Password</span>
        <input
          type="password"
          autocomplete="current-password"
          required
          bind:value={password}
          disabled={submitting}
          data-testid="auth-login-password"
          class="rounded-md border border-zinc-700 bg-zinc-900 px-3 py-2 text-zinc-100 outline-none focus:border-zinc-500"
        />
      </label>

      {#if formError}
        <p
          class="rounded-md border border-amber-900/40 bg-amber-950/20 px-3 py-2 text-xs text-amber-200/80"
          data-testid="auth-login-form-error"
        >
          {describeLoginFormError(formError)}
        </p>
      {/if}

      {#if wireError}
        <p
          class="rounded-md border border-red-900/40 bg-red-950/20 px-3 py-2 text-xs text-red-200/80"
          data-testid="auth-login-error"
        >
          {wireError}
        </p>
      {/if}

      <button
        type="submit"
        disabled={!canSubmit}
        data-testid="auth-login-submit"
        class="rounded-md bg-zinc-100 px-3 py-2 text-sm font-semibold text-zinc-900 transition hover:bg-white disabled:cursor-not-allowed disabled:bg-zinc-700 disabled:text-zinc-400"
      >
        {submitting ? "Signing in…" : "Sign in"}
      </button>
    </form>

    <footer class="flex flex-col gap-2 border-t border-zinc-800 pt-4">
      <p class="text-xs text-zinc-500">
        First time on this server? Use the bootstrap token your operator
        configured to create the first account.
      </p>
      <button
        type="button"
        disabled={submitting}
        data-testid="auth-login-bootstrap-link"
        class="self-start text-xs font-medium text-zinc-300 underline underline-offset-2 hover:text-zinc-100 disabled:cursor-not-allowed disabled:text-zinc-600"
        onclick={handleRequestBootstrap}
      >
        First-time setup
      </button>
    </footer>
  </section>
</div>
