<script lang="ts">
  import type { HealthStatus } from "../api/health.js";

  interface Props {
    status: HealthStatus;
  }

  let { status }: Props = $props();

  let label = $derived(
    status === "ok" ? "online" : status === "down" ? "offline" : "unknown",
  );
  let dotClass = $derived(
    status === "ok"
      ? "bg-emerald-400"
      : status === "down"
        ? "bg-rose-500"
        : "bg-zinc-500",
  );
  let textClass = $derived(
    status === "ok"
      ? "text-emerald-300"
      : status === "down"
        ? "text-rose-300"
        : "text-zinc-400",
  );
</script>

<span
  class="inline-flex items-center gap-2 rounded-full border border-zinc-800 bg-zinc-900/60 px-2.5 py-1 text-xs font-medium {textClass}"
  data-testid="status-badge"
  data-status={status}
>
  <span class="h-2 w-2 rounded-full {dotClass}" aria-hidden="true"></span>
  {label}
</span>
