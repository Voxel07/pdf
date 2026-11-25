<script lang="ts">
  import { onDestroy } from 'svelte';
  import { admin } from '../../lib/api';
  import ExportPanel from './ExportPanel.svelte';

  interface ImportReport {
    templates_created: number;
    templates_reused: number;
    submissions_imported: number;
    submissions_skipped: number;
    submissions_existing: number;
    submissions_incomplete: number;
    submissions_failed: number;
    originals_saved: number;
    persons_created: number;
    persons_matched_by_name: number;
    warnings: string[];
  }
  interface ImportProgress {
    running: boolean;
    started_at: string | null;
    finished_at: string | null;
    started_by: string | null;
    phase: string;
    template_index: number;
    template_total: number;
    template_name: string | null;
    done: number;
    total: number;
    report: ImportReport;
    error: string | null;
  }

  let baseUrl = $state('https://api.docuseal.com');
  let apiKey = $state('');
  let templates = $state<any[] | null>(null);
  let picked = $state<Record<number, boolean>>({});
  let includeSubmissions = $state(true);
  let saveOriginals = $state(false);
  let loading = $state(false);
  let error = $state('');
  let progress = $state<ImportProgress | null>(null);

  let timer: ReturnType<typeof setTimeout> | undefined;
  onDestroy(() => clearTimeout(timer));

  async function poll() {
    clearTimeout(timer);
    try {
      progress = await admin.get<ImportProgress>('/docuseal/import');
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
    }
    if (progress?.running) timer = setTimeout(poll, 1000);
  }
  // an import keeps running on the server when this page is closed; show it on return
  poll();

  async function loadTemplates(e: SubmitEvent) {
    e.preventDefault();
    loading = true;
    error = '';
    try {
      templates = await admin.post('/docuseal/templates', { base_url: baseUrl, api_key: apiKey });
      picked = Object.fromEntries(templates!.map((t) => [t.id, true]));
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
    } finally {
      loading = false;
    }
  }

  async function runImport() {
    error = '';
    try {
      progress = await admin.post<ImportProgress>('/docuseal/import', {
        base_url: baseUrl,
        api_key: apiKey,
        template_ids: Object.entries(picked).filter(([, v]) => v).map(([k]) => +k),
        include_submissions: includeSubmissions,
        save_originals: saveOriginals,
      });
      poll();
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
    }
  }

  const running = $derived(!!progress?.running);
  const selectedCount = $derived(Object.values(picked).filter(Boolean).length);
  // overall fraction: finished templates plus the share of the current one
  const fraction = $derived.by(() => {
    const p = progress;
    if (!p || !p.template_total) return 0;
    if (!p.running) return 1;
    const current = p.total ? p.done / p.total : 0;
    return Math.min(1, (Math.max(0, p.template_index - 1) + current) / p.template_total);
  });
  const r = $derived(progress?.report);
  const hasResult = $derived(!!progress && !!progress.started_at);
</script>

<h1>Import from DocuSeal</h1>
<form class="card" onsubmit={loadTemplates}>
  <p class="muted">Imports templates (PDF + fields) and completed submissions (values + signatures). The signed PDFs are then recreated by this app and can be exported as files. Submitters are matched to existing people by Discord id (<code>external_id</code> or <code>metadata.discord_id</code>), then e-mail, then a unique name; others are created and can be linked under <a href="/admin/persons">People</a>. Running the import again only adds what is missing.</p>
  <div class="grid2">
    <div class="field"><label for="bu">API URL</label><input id="bu" bind:value={baseUrl} />
      <div class="small muted">Cloud: <code>https://api.docuseal.com</code> / <code>https://api.docuseal.eu</code>, self-hosted: <code>https://your-host/api</code></div></div>
    <div class="field"><label for="ak">API key (X-Auth-Token)</label><input id="ak" type="password" bind:value={apiKey} required /></div>
  </div>
  <button class="primary" type="submit" disabled={loading}>{loading ? 'Loading…' : 'Load templates'}</button>
  {#if error}<div class="error">{error}</div>{/if}
</form>

{#if templates}
  <div class="card">
    <div class="row"><h2 class="grow">Templates ({templates.length})</h2>
      <button class="small" onclick={() => (picked = Object.fromEntries(templates!.map((t) => [t.id, true])))}>All</button>
      <button class="small" onclick={() => (picked = {})}>None</button>
    </div>
    <table>
      <thead><tr><th></th><th>Name</th><th>Folder</th><th>Fields</th><th>Documents</th></tr></thead>
      <tbody>
        {#each templates as t (t.id)}
          <tr><td><input type="checkbox" bind:checked={picked[t.id]} /></td><td>{t.name}</td><td class="muted">{t.folder ?? ''}</td><td>{t.fields}</td><td>{t.documents}</td></tr>
        {/each}
      </tbody>
    </table>
    <div class="row" style="margin-top:12px">
      <label class="inline"><input type="checkbox" bind:checked={includeSubmissions} /> import completed submissions</label>
      <label class="inline"><input type="checkbox" bind:checked={saveOriginals} /> also save DocuSeal's original signed PDFs to the export folder</label>
      <span class="spacer"></span>
      <button class="primary" onclick={runImport} disabled={running || selectedCount === 0}>{running ? 'Import running…' : `Import ${selectedCount} selected`}</button>
    </div>
  </div>
{/if}

{#if hasResult && progress && r}
  <div class="card">
    <div class="row">
      <h2 class="grow">{progress.running ? 'Importing…' : progress.error ? 'Import stopped' : 'Import finished'}</h2>
      <a class="small" href="/admin/activity?category=import">Activity log</a>
    </div>
    <div class="progress" role="progressbar" aria-valuemin="0" aria-valuemax="100" aria-valuenow={Math.round(fraction * 100)} class:done={!progress.running} class:failed={!!progress.error || r.submissions_failed > 0}>
      <div class="bar" style:width="{(fraction * 100).toFixed(1)}%"></div>
    </div>
    <p class="small muted" style="margin-top:6px">
      {#if progress.running}
        Template {progress.template_index} of {progress.template_total}{progress.template_name ? ` — ${progress.template_name}` : ''}
        {#if progress.total}· submission {progress.done} of {progress.total}{/if}
        · {progress.phase}
      {:else}
        Started by {progress.started_by ?? '—'}, finished {progress.finished_at ? new Date(progress.finished_at).toLocaleString() : '—'}
      {/if}
    </p>
    {#if progress.error}<div class="error">{progress.error}</div>{/if}
    <p>Templates created: <b>{r.templates_created}</b>, reused: {r.templates_reused}<br />
      Submissions imported: <b>{r.submissions_imported}</b>, already imported: {r.submissions_existing}, not completed yet: {r.submissions_incomplete},
      failed: <b class:err={r.submissions_failed > 0}>{r.submissions_failed}</b><br />
      People created: {r.persons_created}, matched by name: {r.persons_matched_by_name}, original PDFs saved: {r.originals_saved}</p>
    {#if r.submissions_failed > 0 && !progress.running}
      <div class="error">{r.submissions_failed} submission(s) could not be imported. The reasons are listed below and in the activity log; run the import again to retry them.</div>
    {/if}
    {#if r.warnings.length}
      <details open={r.submissions_failed > 0}><summary>{r.warnings.length} warning(s) and error(s)</summary><ul class="small">{#each r.warnings as w}<li>{w}</li>{/each}</ul></details>
    {/if}
    {#if !progress.running}
      <div class="row" style="margin-top:10px">
        <a class="btn" href="/admin/templates">Check templates</a>
      </div>
    {/if}
  </div>
  {#if !progress.running}<ExportPanel />{/if}
{/if}

<style>
  .progress { margin-top: 4px; }
  .progress.done .bar { background: var(--ok); }
  .progress.done.failed .bar { background: var(--warn); }
  .err { color: var(--danger); }
</style>
