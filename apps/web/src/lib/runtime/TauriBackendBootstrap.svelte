<script lang="ts">
  /**
   * Tauri-only bootstrap picker (design § 5).
   *
   * Renders ONLY when `isTauriBootstrapEnabled()` AND no valid
   * `BackendConfig` is in storage. The browser deployment never
   * mounts this component (`App.svelte` gates on
   * `isTauriBootstrapEnabled()`).
   *
   * Responsibilities:
   *  - Single text input + "Connect" button.
   *  - Validates the URL through `validateBackendOrigin`. Rejection
   *    surfaces via a typed enum → `describeBackendUrlError` (no
   *    echoing the URL back to the operator).
   *  - On accept: persist the canonical origin via
   *    `saveBackendConfig`, then call `onSaved(origin)` so the parent
   *    gate triggers `performHandoff` (which calls
   *    `window.location.assign`).
   *  - Includes the Android-localhost caveat per design § 12.
   *
   * Redaction posture: the script never logs; failure messages are a
   * function of the typed reason only; the input value is never
   * placed in any `data-*` attribute, console call, thrown error, or
   * audit-shaped path. There is no probe network call in this slice
   * — design § 5 leaves the probe deferred and the implementation
   * plan explicitly excludes a backend health check.
   */
  import {
    BACKEND_URL_MAX_LEN,
    saveBackendConfig,
    validateBackendOrigin,
    type BackendConfigStorage,
  } from "./backendConfig.js";
  import { describeBackendUrlError } from "./backendUrlError.js";

  interface Props {
    /** Storage to persist the canonical config into. Defaults to
     * `window.localStorage`; tests inject an in-memory shim. */
    storage?: BackendConfigStorage;
    /** Called with the canonical origin once the config has been
     * saved. The parent gate then performs the WebView navigation
     * (we keep the side effect out of this component so unit tests
     * can drive `onSaved` synchronously). */
    onSaved: (origin: string) => void;
    /** Override `Date.now()` for deterministic tests. */
    now?: () => Date;
  }

  const {
    storage = (typeof window !== "undefined"
      ? window.localStorage
      : undefined) as BackendConfigStorage | undefined,
    onSaved,
    now = () => new Date(),
  }: Props = $props();

  let input = $state("");
  let attempted = $state(false);
  let validationReason = $state<string | null>(null);

  let trimmed = $derived(input.trim());
  // Submit is disabled while empty so the operator sees the help
  // copy first; the precise rejection appears after they hit
  // Connect, mirroring the LoginView pattern.
  let canSubmit = $derived(trimmed.length > 0);

  function handleSubmit(event: SubmitEvent) {
    event.preventDefault();
    attempted = true;
    validationReason = null;

    const validation = validateBackendOrigin(trimmed);
    if (!validation.ok) {
      validationReason = describeBackendUrlError(validation.reason);
      return;
    }

    if (storage === undefined) {
      // Defensive — production wiring always supplies storage; this
      // branch only happens in a non-DOM, non-test misuse. Surface a
      // generic message so we never throw an Error containing the
      // URL.
      validationReason =
        "Could not save server URL on this device. Please reopen the app.";
      return;
    }

    saveBackendConfig(storage, {
      version: 1,
      backendOrigin: validation.origin,
      savedAt: now().toISOString(),
    });
    onSaved(validation.origin);
  }
</script>

<div
  class="flex min-h-screen items-center justify-center bg-zinc-900 px-4 py-10"
  data-testid="tauri-bootstrap-screen"
>
  <section
    class="flex w-full max-w-md flex-col gap-6 rounded-lg border border-zinc-800 bg-zinc-950/60 p-6 shadow-2xl"
  >
    <header class="flex flex-col gap-2">
      <h1 class="text-lg font-semibold tracking-tight text-zinc-100">
        Connect to RelayTerm Server
      </h1>
      <p class="text-sm text-zinc-400">
        Enter the URL of your RelayTerm server. The bundled app will
        load that server's web UI; sign-in continues there as it would
        in a browser.
      </p>
    </header>

    <form
      class="flex flex-col gap-4"
      data-testid="tauri-bootstrap-form"
      onsubmit={handleSubmit}
      novalidate
    >
      <label class="flex flex-col gap-1 text-sm text-zinc-300">
        <span>Server URL</span>
        <input
          type="url"
          inputmode="url"
          autocomplete="off"
          autocapitalize="none"
          autocorrect="off"
          spellcheck={false}
          required
          maxlength={BACKEND_URL_MAX_LEN}
          placeholder="https://relayterm.example.com"
          bind:value={input}
          data-testid="tauri-bootstrap-input"
          class="rounded-md border border-zinc-700 bg-zinc-900 px-3 py-2 font-mono text-sm text-zinc-100 outline-none focus:border-zinc-500"
        />
      </label>

      {#if attempted && validationReason !== null}
        <p
          class="rounded-md border border-amber-900/40 bg-amber-950/20 px-3 py-2 text-xs text-amber-200/80"
          data-testid="tauri-bootstrap-error"
        >
          {validationReason}
        </p>
      {/if}

      <button
        type="submit"
        disabled={!canSubmit}
        data-testid="tauri-bootstrap-submit"
        class="rounded-md bg-zinc-100 px-3 py-2 text-sm font-semibold text-zinc-900 transition hover:bg-white disabled:cursor-not-allowed disabled:bg-zinc-700 disabled:text-zinc-400"
      >
        Connect
      </button>
    </form>

    <footer
      class="flex flex-col gap-2 border-t border-zinc-800 pt-4 text-xs text-zinc-500"
    >
      <p data-testid="tauri-bootstrap-public-config-note">
        The server URL is public configuration, not a secret. Login
        and session cookies remain handled by the server after handoff.
      </p>
      <p data-testid="tauri-bootstrap-android-caveat">
        On Android, <code class="font-mono">localhost</code> means the
        phone or emulator itself, not your laptop. Use your computer's
        LAN IP, or <code class="font-mono">10.0.2.2</code> from the
        Android emulator.
      </p>
    </footer>
  </section>
</div>
