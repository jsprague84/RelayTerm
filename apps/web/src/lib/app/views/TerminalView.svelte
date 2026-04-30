<script lang="ts">
  import PlaceholderView from "./PlaceholderView.svelte";
</script>

<PlaceholderView
  testid="production-view-terminal"
  title="Terminal workspace"
  summary="The production terminal workspace is not implemented yet. The renderer-neutral protocol, sequenced replay, and reconnect TTL are already in place on the backend; the production UI to drive them lands in a later slice."
  bullets={[
    "Will surface live and detached sessions with reconnect-by-replay.",
    "Will use a single neutral renderer; experimental renderers stay in the dev lab.",
    "Backend protocol stays RelayTerm-shaped — never reshaped to fit a renderer.",
  ]}
  futureWork="A renderer selector and renderer-preference persistence will land alongside the production terminal UI."
/>
