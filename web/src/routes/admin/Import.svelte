<script lang="ts">
  import { admin } from '../../lib/api';

  let baseUrl = $state('https://api.docuseal.com');
  let apiKey = $state('');
  let templates = $state<any[] | null>(null);
  let picked = $state<Record<number, boolean>>({});
  let includeSubmissions = $state(true);
  let saveOriginals = $state(false);
  let busy = $state(false);
  let error = $state('');
  let report = $state<any>(null);

  async function loadTemplates(e: SubmitEvent) {
    e.preventDefault();
    busy = true;
    error = '';
    try {
      templates = await admin.post('/docuseal/templates', { base_url: baseUrl, api_key: apiKey });
      picked = Object.fromEntries(templates!.map((t) => [t.id, true]));
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
    } finally {
      busy = false;
    }
  }

  async function runImport() {
    busy = true;
    error = '';
    report = null;
    try {
      report = await admin.post('/docuseal/import', {
        base_url: baseUrl,
        api_key: apiKey,
        template_ids: Object.entries(picked).filter(([, v]) => v).map(([k]) => +k),
        include_submissions: includeSubmissions,
        save_originals: saveOriginals,
      });
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
    } finally {
      busy = false;
    }
  }

  async function exportNow() {
    const r = await admin.post('/export');
    alert(`Wrote ${r.written} PDFs to ${r.dir}`);
  }
</script>

<h1>Import from DocuSeal</h1>
<form class="card" onsubmit={loadTemplates}>
  <p class="muted">Imports templates (PDF + fields) and completed submissions (values + signatures). The signed PDFs are then recreated by this app and can be exported as files. Submitters whose <code>external_id</code> or <code>metadata.discord_id</code> is a Discord id are linked automatically; others can be linked under <a href="/admin/persons">People</a>.</p>
  <div class="grid2">
    <div class="field"><label for="bu">API URL</label><input id="bu" bind:value={baseUrl} />
      <div class="small muted">Cloud: <code>https://api.docuseal.com</code> / <code>https://api.docuseal.eu</code>, self-hosted: <code>https://your-host/api</code></div></div>
    <div class="field"><label for="ak">API key (X-Auth-Token)</label><input id="ak" type="password" bind:value={apiKey} required /></div>
  </div>
  <button class="primary" type="submit" disabled={busy}>{busy && !templates ? 'Loading…' : 'Load templates'}</button>
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
      <button class="primary" onclick={runImport} disabled={busy}>{busy ? 'Importing… (may take a while)' : 'Import selected'}</button>
    </div>
  </div>
{/if}

{#if report}
  <div class="card">
    <h2>Import finished</h2>
    <p>Templates created: <b>{report.templates_created}</b>, reused: {report.templates_reused}<br />
      Submissions imported: <b>{report.submissions_imported}</b>, skipped: {report.submissions_skipped}<br />
      People created: {report.persons_created}, original PDFs saved: {report.originals_saved}</p>
    {#if report.warnings.length}
      <details><summary>{report.warnings.length} warnings</summary><ul class="small">{#each report.warnings as w}<li>{w}</li>{/each}</ul></details>
    {/if}
    <div class="row" style="margin-top:10px">
      <a class="btn" href="/admin/templates">Check templates</a>
      <button class="primary" onclick={exportNow}>Recreate & export all PDFs to disk</button>
    </div>
  </div>
{/if}
