<script lang="ts">
  import { admin, type ActivityEntry } from '../../lib/api';
  import ActivityList from './ActivityList.svelte';
  import ExportPanel from './ExportPanel.svelte';

  let stats = $state<any>(null);
  let recent = $state<ActivityEntry[]>([]);
  let errors24h = $state(0);

  admin.get('/stats').then((s) => (stats = s));
  admin.get('/activity?limit=8').then((a) => {
    recent = a.entries;
    errors24h = a.errors_24h;
  });
</script>

<h1>Dashboard</h1>
{#if stats}
  <div class="tiles">
    <a class="card tile" href="/admin/templates"><div class="num">{stats.templates}</div><div class="muted">templates</div></a>
    <a class="card tile" href="/admin/persons"><div class="num">{stats.persons}</div><div class="muted">people</div></a>
    <a class="card tile" href="/admin/submissions?status=pending"><div class="num">{stats.pending}</div><div class="muted">open documents</div></a>
    <a class="card tile" href="/admin/submissions?status=completed"><div class="num">{stats.completed}</div><div class="muted">signed documents</div></a>
  </div>
{/if}

<div class="card">
  <div class="row">
    <h2 class="grow" style="margin:0">Recent activity</h2>
    {#if errors24h}<a class="badge revoked" href="/admin/activity?level=error">{errors24h} error(s) in the last 24 h</a>{/if}
    <a class="small" href="/admin/activity">Show all</a>
  </div>
  <ActivityList entries={recent} compact />
</div>

<ExportPanel />

<div class="card">
  <h2>Getting started</h2>
  <ol class="muted">
    <li>Upload a PDF under <a href="/admin/templates">Templates</a> and place text, date, number, select and signature fields on it — or start from a copy of an existing template.</li>
    <li>Use sections to split a form into parts that can be filled later (e.g. one section per equipment hand-out).</li>
    <li>Create an API key under <a href="/admin/settings">API & export</a> and configure the Discord bot with it.</li>
    <li>Or <a href="/admin/import">import</a> existing templates and signed documents from DocuSeal.</li>
  </ol>
</div>

<style>
  .tiles { display: grid; grid-template-columns: repeat(auto-fit, minmax(180px, 1fr)); gap: 14px; margin-bottom: 16px; }
  .tile { text-decoration: none !important; color: inherit; margin: 0 !important; }
  .num { font-size: 2rem; font-weight: 700; }
</style>
