<script lang="ts">
  import { onDestroy } from 'svelte';
  import { admin, type ActivityEntry } from '../../lib/api';
  import { route } from '../../lib/router.svelte';
  import ActivityList from './ActivityList.svelte';

  const PAGE = 100;
  let entries = $state<ActivityEntry[]>([]);
  let categories = $state<string[]>([]);
  let errors24h = $state(0);
  let level = $state(route.query.get('level') ?? '');
  let category = $state(route.query.get('category') ?? '');
  let q = $state('');
  let auto = $state(true);
  let more = $state(false);
  let error = $state('');

  function params(before?: number) {
    const p = new URLSearchParams({ limit: String(PAGE) });
    if (level) p.set('level', level);
    if (category) p.set('category', category);
    if (q.trim()) p.set('q', q.trim());
    if (before) p.set('before', String(before));
    return p;
  }

  async function load() {
    try {
      const r = await admin.get(`/activity?${params()}`);
      entries = r.entries;
      categories = r.categories;
      errors24h = r.errors_24h;
      more = r.entries.length === PAGE;
      error = '';
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  async function older() {
    const last = entries.at(-1);
    if (!last) return;
    const r = await admin.get(`/activity?${params(last.id)}`);
    entries = [...entries, ...r.entries];
    more = r.entries.length === PAGE;
  }

  let debounce: ReturnType<typeof setTimeout>;
  $effect(() => {
    level;
    category;
    q;
    clearTimeout(debounce);
    debounce = setTimeout(load, 200);
  });

  // refresh the newest page while nobody is paging through older entries
  const refresh = setInterval(() => {
    if (auto && entries.length <= PAGE && document.visibilityState === 'visible') load();
  }, 10_000);
  onDestroy(() => {
    clearInterval(refresh);
    clearTimeout(debounce);
  });
</script>

<div class="row"><h1 class="grow">Activity log</h1>
  {#if errors24h}<span class="badge revoked">{errors24h} error(s) in the last 24 h</span>{/if}
</div>
<div class="card">
  <div class="row">
    <select bind:value={level} style="width:auto" aria-label="Level">
      <option value="">All entries</option>
      <option value="warn">Warnings and errors</option>
      <option value="error">Errors only</option>
    </select>
    <select bind:value={category} style="width:auto" aria-label="Category">
      <option value="">All areas</option>
      {#each categories as c}<option value={c}>{c}</option>{/each}
    </select>
    <input class="grow" style="flex:1;width:auto" placeholder="Search messages and users" bind:value={q} />
    <label class="inline"><input type="checkbox" bind:checked={auto} /> auto-refresh</label>
  </div>
  <p class="small muted" style="margin:8px 0 0">Shows what staff, signers, the bot and background jobs (imports, exports, backups) did, plus every server error. The newest 5000 entries are kept.</p>
</div>
{#if error}<div class="error">{error}</div>{/if}
<div class="card">
  <ActivityList {entries} />
  {#if more}<div class="row" style="margin-top:10px"><button onclick={older}>Load older entries</button></div>{/if}
</div>
