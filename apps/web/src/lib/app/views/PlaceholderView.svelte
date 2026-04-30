<script lang="ts">
  interface Props {
    title: string;
    summary: string;
    bullets?: readonly string[];
    futureWork?: string;
    testid?: string;
  }

  let { title, summary, bullets = [], futureWork, testid }: Props = $props();
</script>

<section
  class="flex flex-col gap-4 rounded-lg border border-zinc-800 bg-zinc-950/40 p-6"
  data-testid={testid}
>
  <header class="flex flex-col gap-1">
    <h2 class="text-lg font-semibold tracking-tight text-zinc-100">{title}</h2>
    <p class="text-sm text-zinc-400">{summary}</p>
  </header>
  {#if bullets.length > 0}
    <ul class="flex flex-col gap-2 text-sm text-zinc-300">
      {#each bullets as item (item)}
        <li class="flex items-start gap-2">
          <span class="mt-1 h-1.5 w-1.5 shrink-0 rounded-full bg-zinc-600"
          ></span>
          <span>{item}</span>
        </li>
      {/each}
    </ul>
  {/if}
  {#if futureWork}
    <p
      class="rounded-md border border-amber-900/40 bg-amber-950/20 px-3 py-2 text-xs text-amber-200/80"
    >
      <span class="font-mono uppercase tracking-wide">future work</span> ·
      {futureWork}
    </p>
  {/if}
</section>
