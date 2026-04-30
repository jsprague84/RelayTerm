<script lang="ts">
  import PlaceholderView from "./PlaceholderView.svelte";
</script>

<PlaceholderView
  testid="production-view-servers"
  title="Server profiles"
  summary="Saved SSH connection profiles — host, user, port, tags, and an associated SSH identity — will be managed here. The backend repository and HTTP API are in place; the CRUD UI is not."
  bullets={[
    "Profiles are scoped to the calling user; cross-user reads return a genuine 404.",
    "Host-key trust is captured separately from profile creation.",
    "An identity is selected per profile; identities are stored in the vault.",
  ]}
  futureWork="Create / edit / delete forms, host-key trust prompts, and last-connected metadata land alongside the production terminal workspace."
/>
