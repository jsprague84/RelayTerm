<script lang="ts">
  import AppShell from "./lib/app/AppShell.svelte";
  import DevTerminalWorkbench from "./lib/dev/DevTerminalWorkbench.svelte";
  import TerminalProtocolLab from "./lib/dev/TerminalProtocolLab.svelte";

  // `import.meta.env.DEV` is statically `true` under `vite dev` / vitest
  // and statically `false` for `vite build` (see vite.config.ts). Vite
  // inlines this constant at build time so dead-code elimination drops
  // the dev branch — and its dev-lab imports — from the production
  // bundle.
  //
  // Caveat: the lab components are imported unconditionally above. JS
  // tree-shaking handles that — `terminal-xterm`'s `sideEffects` field
  // lets Rollup drop xterm entirely from the prod JS — but the CSS
  // side-effect import (`@relayterm/terminal-xterm/styles`) is still
  // included in the prod CSS bundle (≈3KB of xterm.css). Documented
  // compromise; revisit if it ever stops being trivial.
  const isDev = import.meta.env.DEV;
</script>

{#if isDev}
  <AppShell devMode>
    {#snippet devTools()}
      <div class="flex flex-col gap-4">
        <TerminalProtocolLab />
        <DevTerminalWorkbench />
      </div>
    {/snippet}
  </AppShell>
{:else}
  <AppShell />
{/if}
