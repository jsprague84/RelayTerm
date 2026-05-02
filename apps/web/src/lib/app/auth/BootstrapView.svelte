<script lang="ts">
  /**
   * First-time setup form: creates the first user via
   * `POST /api/v1/auth/bootstrap`.
   *
   * Scope (load-bearing):
   *  - Bootstrap creates the user; it does NOT mint a session and does
   *    NOT set a cookie. On success the SPA shows "Account created.
   *    Please sign in." and routes the operator back to the login form.
   *    Auto-login here would silently widen the bootstrap route into a
   *    second unauthenticated session-issuing surface; SPEC.md
   *    "Frontend authentication UI plan" Phase 3 pins the split.
   *  - The bootstrap token and password are NOT persisted to local
   *    storage, NOT logged, and NOT echoed in any error string.
   *  - Password confirmation is a frontend-only typo guard. The
   *    backend does not see the confirmation field.
   */
  import {
    bootstrap as bootstrapApi,
    describeAuthError,
    describeBootstrapFormError,
    validateBootstrapForm,
    type BootstrapFormError,
  } from "../../api/auth.js";

  interface Props {
    /** Switch the unauthenticated screen back to the login form. */
    onRequestLogin: () => void;
  }

  let { onRequestLogin }: Props = $props();

  let bootstrapToken = $state("");
  let email = $state("");
  let displayName = $state("");
  let password = $state("");
  let passwordConfirmation = $state("");

  let submitting = $state(false);
  let succeeded = $state(false);
  let formError = $state<BootstrapFormError | null>(null);
  let wireError = $state<string | null>(null);

  let trimmedEmail = $derived(email.trim());
  let trimmedDisplayName = $derived(displayName.trim());
  let canSubmit = $derived(
    !submitting &&
      !succeeded &&
      bootstrapToken.length > 0 &&
      trimmedEmail.length > 0 &&
      trimmedDisplayName.length > 0 &&
      password.length > 0 &&
      passwordConfirmation.length > 0,
  );

  function clearSecrets() {
    bootstrapToken = "";
    password = "";
    passwordConfirmation = "";
  }

  async function handleSubmit(event: SubmitEvent) {
    event.preventDefault();
    if (submitting) return;

    formError = null;
    wireError = null;

    const validation = validateBootstrapForm({
      bootstrap_token: bootstrapToken,
      email: trimmedEmail,
      display_name: trimmedDisplayName,
      password,
      password_confirmation: passwordConfirmation,
    });
    if (!validation.ok) {
      formError = validation.reason;
      return;
    }

    submitting = true;
    try {
      const result = await bootstrapApi({
        bootstrap_token: bootstrapToken,
        email: trimmedEmail,
        display_name: trimmedDisplayName,
        password,
      });
      if (!result.ok) {
        wireError = describeAuthError("first-time setup", result.error);
        // Drop secret-shaped fields on failure so a retry starts from a
        // fresh state. The operator re-enters the token/password.
        clearSecrets();
        return;
      }
      // Account created. Drop the secrets; the operator now signs in
      // through the normal login route.
      clearSecrets();
      succeeded = true;
    } finally {
      submitting = false;
    }
  }

  function handleBackToLogin() {
    if (submitting) return;
    onRequestLogin();
  }
</script>

<div
  class="flex min-h-screen items-center justify-center bg-zinc-900 px-4 py-10"
  data-testid="auth-bootstrap-screen"
>
  <section
    class="flex w-full max-w-md flex-col gap-6 rounded-lg border border-zinc-800 bg-zinc-950/60 p-6 shadow-2xl"
  >
    <header class="flex flex-col gap-1">
      <h1
        class="text-lg font-semibold tracking-tight text-zinc-100"
        data-testid="auth-bootstrap-heading"
      >
        First-time setup
      </h1>
      <p class="text-sm text-zinc-400">
        Create the first RelayTerm account using the bootstrap token your
        operator configured.
      </p>
    </header>

    {#if succeeded}
      <div
        class="flex flex-col gap-3 rounded-md border border-emerald-900/40 bg-emerald-950/20 px-4 py-3 text-sm text-emerald-200/90"
        data-testid="auth-bootstrap-success"
      >
        <p>Account created. Please sign in.</p>
        <button
          type="button"
          data-testid="auth-bootstrap-back-to-login"
          class="self-start rounded-md bg-emerald-900/60 px-3 py-1.5 text-xs font-semibold text-emerald-50 hover:bg-emerald-900"
          onclick={handleBackToLogin}
        >
          Back to sign in
        </button>
      </div>
    {:else}
      <form
        class="flex flex-col gap-4"
        data-testid="auth-bootstrap-form"
        onsubmit={handleSubmit}
        novalidate
      >
        <label class="flex flex-col gap-1 text-sm text-zinc-300">
          <span>Bootstrap token</span>
          <input
            type="password"
            autocomplete="off"
            required
            bind:value={bootstrapToken}
            disabled={submitting}
            data-testid="auth-bootstrap-token"
            class="rounded-md border border-zinc-700 bg-zinc-900 px-3 py-2 text-zinc-100 outline-none focus:border-zinc-500"
          />
        </label>

        <label class="flex flex-col gap-1 text-sm text-zinc-300">
          <span>Email</span>
          <input
            type="email"
            autocomplete="username"
            required
            bind:value={email}
            disabled={submitting}
            data-testid="auth-bootstrap-email"
            class="rounded-md border border-zinc-700 bg-zinc-900 px-3 py-2 text-zinc-100 outline-none focus:border-zinc-500"
          />
        </label>

        <label class="flex flex-col gap-1 text-sm text-zinc-300">
          <span>Display name</span>
          <input
            type="text"
            autocomplete="name"
            required
            bind:value={displayName}
            disabled={submitting}
            data-testid="auth-bootstrap-display-name"
            class="rounded-md border border-zinc-700 bg-zinc-900 px-3 py-2 text-zinc-100 outline-none focus:border-zinc-500"
          />
        </label>

        <label class="flex flex-col gap-1 text-sm text-zinc-300">
          <span>Password</span>
          <input
            type="password"
            autocomplete="new-password"
            required
            bind:value={password}
            disabled={submitting}
            data-testid="auth-bootstrap-password"
            class="rounded-md border border-zinc-700 bg-zinc-900 px-3 py-2 text-zinc-100 outline-none focus:border-zinc-500"
          />
        </label>

        <label class="flex flex-col gap-1 text-sm text-zinc-300">
          <span>Confirm password</span>
          <input
            type="password"
            autocomplete="new-password"
            required
            bind:value={passwordConfirmation}
            disabled={submitting}
            data-testid="auth-bootstrap-password-confirm"
            class="rounded-md border border-zinc-700 bg-zinc-900 px-3 py-2 text-zinc-100 outline-none focus:border-zinc-500"
          />
        </label>

        {#if formError}
          <p
            class="rounded-md border border-amber-900/40 bg-amber-950/20 px-3 py-2 text-xs text-amber-200/80"
            data-testid="auth-bootstrap-form-error"
          >
            {describeBootstrapFormError(formError)}
          </p>
        {/if}

        {#if wireError}
          <p
            class="rounded-md border border-red-900/40 bg-red-950/20 px-3 py-2 text-xs text-red-200/80"
            data-testid="auth-bootstrap-error"
          >
            {wireError}
          </p>
        {/if}

        <button
          type="submit"
          disabled={!canSubmit}
          data-testid="auth-bootstrap-submit"
          class="rounded-md bg-zinc-100 px-3 py-2 text-sm font-semibold text-zinc-900 transition hover:bg-white disabled:cursor-not-allowed disabled:bg-zinc-700 disabled:text-zinc-400"
        >
          {submitting ? "Creating account…" : "Create account"}
        </button>
      </form>

      <footer class="flex flex-col gap-2 border-t border-zinc-800 pt-4">
        <button
          type="button"
          disabled={submitting}
          data-testid="auth-bootstrap-cancel"
          class="self-start text-xs font-medium text-zinc-300 underline underline-offset-2 hover:text-zinc-100 disabled:cursor-not-allowed disabled:text-zinc-600"
          onclick={handleBackToLogin}
        >
          Back to sign in
        </button>
      </footer>
    {/if}
  </section>
</div>
