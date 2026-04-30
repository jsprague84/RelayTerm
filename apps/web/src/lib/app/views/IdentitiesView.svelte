<script lang="ts">
  import PlaceholderView from "./PlaceholderView.svelte";
</script>

<PlaceholderView
  testid="production-view-identities"
  title="SSH identities"
  summary="Generated SSH keypairs are stored in the encrypted vault. This view will list the public-key half (for pasting into authorized_keys) and the SHA-256 fingerprint. The encrypted private key is never surfaced to the client."
  bullets={[
    "Private key material stays server-side; the API never returns the encrypted PEM.",
    "Each identity exposes its public key, comment, and SHA-256 fingerprint.",
    "Generation defaults to ed25519; RSA/ECDSA generators are not wired up.",
  ]}
  futureWork="Listing, generation, deletion, and copy-to-clipboard for the public key arrive with the production terminal workspace. Private-key import is a separate later slice."
/>
