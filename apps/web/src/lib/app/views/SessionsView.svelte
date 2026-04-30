<script lang="ts">
  import PlaceholderView from "./PlaceholderView.svelte";
</script>

<PlaceholderView
  testid="production-view-sessions"
  title="Terminal sessions"
  summary="A list of live and detached terminal sessions will land here. The backend already owns session lifecycle, sequence numbers, and a 30-second detached reconnect TTL; this view will read that state and let you reattach."
  bullets={[
    "Sessions persist on the backend; the client can disappear and come back.",
    "Reconnect replays from the sequence-numbered ring buffer — never client-buffered state.",
    "Closed sessions stay visible until the orchestrator drops them.",
  ]}
  futureWork="Listing, filtering, and reattach actions wait on the production terminal workspace and the backend list endpoint."
/>
