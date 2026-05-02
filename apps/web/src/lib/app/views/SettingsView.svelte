<script lang="ts">
  /**
   * Production-safe local terminal preferences UI.
   *
   * Scope rules (load-bearing):
   *  - Local-browser preferences only. NO backend persistence, NO
   *    account settings, NO per-server-profile overrides, NO renderer
   *    selector (production stays xterm baseline).
   *  - Settings are renderer-neutral: every field maps onto
   *    `BaseTerminalRendererOptions` from `@relayterm/terminal-core`.
   *  - "Apply" persists to localStorage and tells the operator that
   *    changes apply to the NEXT terminal session. We deliberately do
   *    NOT walk the live xterm `Terminal.options` object — re-fitting
   *    and atlas-resetting on a live PTY is its own slice.
   */
  import {
    CURSOR_STYLES,
    FONT_FAMILY_MAX_LEN,
    FONT_SIZE_MAX,
    FONT_SIZE_MIN,
    LINE_HEIGHT_MAX,
    LINE_HEIGHT_MIN,
    SCROLLBACK_MAX,
    SCROLLBACK_MIN,
    clampFontSize,
    clampLineHeight,
    clampScrollbackLines,
    defaultTerminalSettings,
    loadTerminalSettings,
    normalizeTerminalSettings,
    resolveTheme,
    saveTerminalSettings,
    type TerminalSettings,
  } from "../settings/terminalSettings.js";
  import {
    DEFAULT_THEME_PRESET_ID,
    TERMINAL_THEME_PRESETS,
    findThemePreset,
  } from "../settings/themePresets.js";
  import { TERMINAL_UX_COPY } from "../terminal/terminalLaunch.js";
  import AuthSessionsPanel from "./AuthSessionsPanel.svelte";
  import RecentActivityPanel from "./RecentActivityPanel.svelte";

  interface Props {
    /**
     * Forwarded to {@link AuthSessionsPanel}. When the user revokes
     * their CURRENT session from the panel, the backend has already
     * cleared the cookie via the revoke route's `Set-Cookie` header;
     * AppShell runs local cleanup + auth-gate flip via this callback
     * so we do not re-POST `/auth/logout`.
     */
    onCurrentSessionRevoked?: () => void;
  }

  let { onCurrentSessionRevoked }: Props = $props();

  type SaveState =
    | { kind: "idle" }
    | { kind: "saved" }
    | { kind: "failed" };

  let draft = $state<TerminalSettings>(loadTerminalSettings());
  let saveState = $state<SaveState>({ kind: "idle" });

  const previewTheme = $derived(resolveTheme(draft));
  const activePreset = $derived(
    findThemePreset(draft.themePresetId) ?? TERMINAL_THEME_PRESETS[0],
  );

  function markDirty() {
    if (saveState.kind !== "idle") {
      saveState = { kind: "idle" };
    }
  }

  function apply() {
    const normalized = normalizeTerminalSettings(draft);
    draft = normalized;
    const ok = saveTerminalSettings(normalized);
    saveState = ok ? { kind: "saved" } : { kind: "failed" };
  }

  function resetToDefaults() {
    draft = defaultTerminalSettings();
    const ok = saveTerminalSettings(draft);
    saveState = ok ? { kind: "saved" } : { kind: "failed" };
  }

  function onFontFamilyInput(event: Event) {
    const value = (event.target as HTMLInputElement).value;
    draft = { ...draft, fontFamily: value };
    markDirty();
  }

  function onFontSizeInput(event: Event) {
    const value = Number((event.target as HTMLInputElement).value);
    draft = { ...draft, fontSize: clampFontSize(value) };
    markDirty();
  }

  function onLineHeightInput(event: Event) {
    const value = Number((event.target as HTMLInputElement).value);
    draft = { ...draft, lineHeight: clampLineHeight(value) };
    markDirty();
  }

  function onScrollbackInput(event: Event) {
    const value = Number((event.target as HTMLInputElement).value);
    draft = { ...draft, scrollbackLines: clampScrollbackLines(value) };
    markDirty();
  }

  function onCursorStyleChange(event: Event) {
    const value = (event.target as HTMLSelectElement).value;
    if (
      value === "block" ||
      value === "underline" ||
      value === "bar"
    ) {
      draft = { ...draft, cursorStyle: value };
      markDirty();
    }
  }

  function onCursorBlinkChange(event: Event) {
    draft = {
      ...draft,
      cursorBlink: (event.target as HTMLInputElement).checked,
    };
    markDirty();
  }

  function onThemeChange(event: Event) {
    const value = (event.target as HTMLSelectElement).value;
    draft = {
      ...draft,
      themePresetId:
        findThemePreset(value)?.id ?? DEFAULT_THEME_PRESET_ID,
    };
    markDirty();
  }
</script>

<section
  class="flex flex-col gap-6"
  data-testid="production-view-settings"
>
  <header class="flex flex-col gap-1">
    <h2 class="text-lg font-semibold tracking-tight text-zinc-100">
      Settings
    </h2>
    <p class="text-sm text-zinc-400">
      Local terminal preferences for this browser. Stored in localStorage
      only — there is no backend / account settings yet, and these
      preferences do not sync to other devices. Changes apply to the
      next terminal session you launch.
    </p>
  </header>

  <article
    class="flex flex-col gap-5 rounded-lg border border-zinc-800 bg-zinc-950/40 p-6"
    data-testid="settings-terminal-appearance"
  >
    <header class="flex flex-col gap-1">
      <h3 class="text-sm font-semibold text-zinc-100">
        Terminal appearance
      </h3>
      <p class="text-xs text-zinc-500">
        These options map onto the renderer-neutral
        <code class="font-mono">BaseTerminalRendererOptions</code> shape, so
        a future production renderer would honour the same values without
        a per-renderer migration.
      </p>
    </header>

    <div class="grid grid-cols-1 gap-4 md:grid-cols-2">
      <label class="flex flex-col gap-1 text-sm text-zinc-200">
        <span class="text-xs uppercase tracking-wide text-zinc-400">
          Font family
        </span>
        <input
          type="text"
          class="rounded-md border border-zinc-700 bg-zinc-900 px-3 py-1.5 text-sm text-zinc-100 placeholder:text-zinc-600 focus:border-emerald-700 focus:outline-none"
          value={draft.fontFamily}
          oninput={onFontFamilyInput}
          maxlength={FONT_FAMILY_MAX_LEN}
          autocomplete="off"
          spellcheck="false"
          data-testid="settings-font-family"
        />
        <span class="text-[11px] text-zinc-500">
          A CSS <code class="font-mono">font-family</code> value. Quoted
          family names like <code class="font-mono">"JetBrains Mono"</code>
          are allowed; control characters are stripped.
        </span>
      </label>

      <label class="flex flex-col gap-1 text-sm text-zinc-200">
        <span class="text-xs uppercase tracking-wide text-zinc-400">
          Font size (px)
        </span>
        <input
          type="number"
          min={FONT_SIZE_MIN}
          max={FONT_SIZE_MAX}
          step="1"
          class="rounded-md border border-zinc-700 bg-zinc-900 px-3 py-1.5 text-sm text-zinc-100 focus:border-emerald-700 focus:outline-none"
          value={draft.fontSize}
          oninput={onFontSizeInput}
          data-testid="settings-font-size"
        />
        <span class="text-[11px] text-zinc-500">
          {FONT_SIZE_MIN}–{FONT_SIZE_MAX}. Out-of-range values are clamped.
        </span>
      </label>

      <label class="flex flex-col gap-1 text-sm text-zinc-200">
        <span class="text-xs uppercase tracking-wide text-zinc-400">
          Line height
        </span>
        <input
          type="number"
          min={LINE_HEIGHT_MIN}
          max={LINE_HEIGHT_MAX}
          step="0.05"
          class="rounded-md border border-zinc-700 bg-zinc-900 px-3 py-1.5 text-sm text-zinc-100 focus:border-emerald-700 focus:outline-none"
          value={draft.lineHeight}
          oninput={onLineHeightInput}
          data-testid="settings-line-height"
        />
        <span class="text-[11px] text-zinc-500">
          Multiplier on the line box ({LINE_HEIGHT_MIN.toFixed(2)}–{LINE_HEIGHT_MAX.toFixed(
            2,
          )}). 1.0 keeps the renderer default.
        </span>
      </label>

      <label class="flex flex-col gap-1 text-sm text-zinc-200">
        <span class="text-xs uppercase tracking-wide text-zinc-400">
          Scrollback lines
        </span>
        <input
          type="number"
          min={SCROLLBACK_MIN}
          max={SCROLLBACK_MAX}
          step="100"
          class="rounded-md border border-zinc-700 bg-zinc-900 px-3 py-1.5 text-sm text-zinc-100 focus:border-emerald-700 focus:outline-none"
          value={draft.scrollbackLines}
          oninput={onScrollbackInput}
          data-testid="settings-scrollback-lines"
        />
        <span class="text-[11px] text-zinc-500">
          Visible scrollback only ({SCROLLBACK_MIN.toLocaleString()}–{SCROLLBACK_MAX.toLocaleString()}).
          Backend replay buffer size is unrelated and not configured here.
        </span>
      </label>

      <label class="flex flex-col gap-1 text-sm text-zinc-200">
        <span class="text-xs uppercase tracking-wide text-zinc-400">
          Cursor style
        </span>
        <select
          class="rounded-md border border-zinc-700 bg-zinc-900 px-3 py-1.5 text-sm text-zinc-100 focus:border-emerald-700 focus:outline-none"
          value={draft.cursorStyle}
          onchange={onCursorStyleChange}
          data-testid="settings-cursor-style"
        >
          {#each CURSOR_STYLES as style (style)}
            <option value={style}>{style}</option>
          {/each}
        </select>
      </label>

      <label class="flex flex-col gap-1 text-sm text-zinc-200">
        <span class="text-xs uppercase tracking-wide text-zinc-400">
          Cursor blink
        </span>
        <span class="flex items-center gap-2 pt-1">
          <input
            type="checkbox"
            class="size-4 accent-emerald-600"
            checked={draft.cursorBlink}
            onchange={onCursorBlinkChange}
            data-testid="settings-cursor-blink"
          />
          <span class="text-xs text-zinc-400">
            Blink the cursor when the terminal has focus.
          </span>
        </span>
      </label>

      <label class="flex flex-col gap-1 text-sm text-zinc-200 md:col-span-2">
        <span class="text-xs uppercase tracking-wide text-zinc-400">
          Theme preset
        </span>
        <select
          class="rounded-md border border-zinc-700 bg-zinc-900 px-3 py-1.5 text-sm text-zinc-100 focus:border-emerald-700 focus:outline-none"
          value={draft.themePresetId}
          onchange={onThemeChange}
          data-testid="settings-theme-preset"
        >
          {#each TERMINAL_THEME_PRESETS as preset (preset.id)}
            <option value={preset.id}>{preset.label}</option>
          {/each}
        </select>
        <span class="text-[11px] text-zinc-500">
          {activePreset.description}
        </span>
      </label>
    </div>

    <article
      class="flex flex-col gap-2 rounded-md border border-zinc-800 p-4 font-mono text-xs"
      style="background-color: {previewTheme.background ??
        '#0a0a0a'}; color: {previewTheme.foreground ?? '#e4e4e7'}; font-family: {draft.fontFamily}; font-size: {draft.fontSize}px; line-height: {draft.lineHeight};"
      data-testid="settings-preview"
    >
      <span class="opacity-70">{activePreset.label} preview</span>
      <pre
        class="whitespace-pre"><code>$ ssh ops@example.internal
Last login: Mon May  1 14:02:51
<span style="color: {previewTheme.green ?? previewTheme.foreground ?? '#7fbf7f'};"
          >ops@example</span
        >:<span style="color: {previewTheme.blue ?? previewTheme.foreground ?? '#7faaff'};"
          >~/projects</span
        >$ rg --pretty TODO src/
<span style="color: {previewTheme.yellow ?? previewTheme.foreground ?? '#ffd56b'};"
          >src/main.rs</span
        >:42: // TODO(j): wire up cancellation
<span style="color: {previewTheme.brightBlack ?? '#888'};"
          >└─ exit 0</span
        ></code></pre>
    </article>
  </article>

  <div class="flex flex-wrap items-center gap-2">
    <button
      type="button"
      class="rounded-md border border-emerald-700 bg-emerald-800 px-3 py-1.5 text-sm text-emerald-50 transition hover:border-emerald-600 hover:bg-emerald-700"
      onclick={apply}
      data-testid="settings-apply"
    >
      Save changes
    </button>
    <button
      type="button"
      class="rounded-md border border-zinc-700 bg-zinc-900 px-3 py-1.5 text-sm text-zinc-200 transition hover:border-zinc-600 hover:bg-zinc-800"
      onclick={resetToDefaults}
      data-testid="settings-reset"
    >
      Reset to defaults
    </button>
    {#if saveState.kind === "saved"}
      <span
        class="text-xs text-emerald-300"
        data-testid="settings-status-saved"
      >
        Saved locally. Applies to the next terminal session.
      </span>
    {:else if saveState.kind === "failed"}
      <span
        class="text-xs text-rose-300"
        data-testid="settings-status-failed"
      >
        Couldn't save to local storage. Settings stayed in memory only.
      </span>
    {/if}
  </div>

  <div class="grid grid-cols-1 gap-2 text-[11px] text-zinc-500 md:grid-cols-2">
    <p
      class="rounded-md border border-zinc-800 bg-zinc-950/40 px-3 py-2"
      data-testid="settings-apply-note"
    >
      <span class="font-medium text-zinc-400">When changes apply.</span>
      {TERMINAL_UX_COPY.settingsApplyNote}
    </p>
    <p
      class="rounded-md border border-zinc-800 bg-zinc-950/40 px-3 py-2"
      data-testid="settings-copy-paste-note"
    >
      <span class="font-medium text-zinc-400">Copy &amp; paste.</span>
      {TERMINAL_UX_COPY.copyPasteNote}
    </p>
  </div>

  <p
    class="rounded-md border border-amber-900/40 bg-amber-950/20 px-3 py-2 text-xs text-amber-200/80"
  >
    <span class="font-mono uppercase tracking-wide">future work</span> ·
    Per-server-profile preferences, custom palettes, keybinding editor,
    copy/paste policy editor, production renderer selection, and
    mobile/Tauri settings are deliberate later slices. Today's settings
    are stored locally in this browser only.
  </p>

  <AuthSessionsPanel {onCurrentSessionRevoked} />

  <RecentActivityPanel />
</section>
