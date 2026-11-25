<script lang="ts">
  import { admin, downloadExportZip } from '../../lib/api';
  import { session, isAdmin } from '../../lib/session.svelte';

  let stats = $state<any>(null);
  let exportMsg = $state('');
  let busy = $state(false);

  admin.get('/stats').then((s) => (stats = s));

  async function exportDisk() {
    busy = true;
    exportMsg = '';
    try {
      const r = await admin.post('/export');
      exportMsg = `Wrote ${r.written} PDFs to ${r.dir}` + (r.failed.length ? ` (${r.failed.length} failed)` : '');
    } catch (e) {
      exportMsg = String(e);
    } finally {
      busy = false;
    }
  }
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
  <h2>Export signed documents</h2>
  <p class="muted">Recreates every signed document as a PDF (with an audit page) and stores it as a file, grouped by template, together with an <code>index.csv</code>.</p>
  <div class="row">
    {#if isAdmin()}<button class="primary" onclick={exportDisk} disabled={busy}>{busy ? 'Exporting…' : `Export to server disk (${session.exportDir})`}</button>{/if}
    <button onclick={async () => (exportMsg = await downloadExportZip())}>Download all as ZIP</button>
  </div>
  {#if exportMsg}<div class="success">{exportMsg}</div>{/if}
</div>

<div class="card">
  <h2>Getting started</h2>
  <ol class="muted">
    <li>Upload a PDF under <a href="/admin/templates">Templates</a> and place text, date, number, select and signature fields on it.</li>
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
