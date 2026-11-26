<script lang="ts">
  import { pageSize } from './paged-list.svelte';
  let { page, count, loading, change }: {
    page: number; count: number; loading: boolean; change: (page: number) => void;
  } = $props();
  const pages = $derived(Math.max(1, Math.ceil(count / pageSize)));
</script>

<div class="row pager" aria-label="Pagination">
  <span class="small muted grow" role="status">
    {#if count}{(page - 1) * pageSize + 1}–{Math.min(page * pageSize, count)} of {count}{loading ? '+' : ''}{/if}
    {#if loading}{count ? ' · Loading more…' : 'Loading…'}{/if}
  </span>
  {#if pages > 1 || page > 1}
    <button class="small" disabled={page <= 1} onclick={() => change(page - 1)}>Previous</button>
    <button class="small" disabled={page >= pages} onclick={() => change(page + 1)}>Next</button>
  {/if}
</div>

<style>.pager { margin-top: 12px; }</style>
