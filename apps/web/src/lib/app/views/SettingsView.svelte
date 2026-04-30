<script lang="ts">
  import PlaceholderView from "./PlaceholderView.svelte";
</script>

<PlaceholderView
  testid="production-view-settings"
  title="Settings"
  summary="Renderer preference, theme, scrollback, and account settings will live here. The renderer-neutral option types in @relayterm/terminal-core already model the shared shape; the production surface to edit them is not built."
  bullets={[
    "Renderer selection is dev-lab-only today; the production default is xterm.",
    "Renderer-specific knobs stay behind each adapter's escape hatch.",
    "Auth UI (passkey enrollment, sessions) is a separate later slice.",
  ]}
  futureWork="Persistence of renderer preference, theme overrides, and authenticated account settings ship after the production terminal workspace."
/>
