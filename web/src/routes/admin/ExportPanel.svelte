<script lang="ts">
  import { onDestroy } from 'svelte';
  import { admin, adminFetch, downloadBlob, type Template } from '../../lib/api';
  import { session, isAdmin } from '../../lib/session.svelte';

  let { showDir = false }: { showDir?: boolean } = $props();

  interface ExportJob {
    id: string;
    kind: 'disk' | 'zip';
    running: boolean;
    done: number;
    total: number;
    phase: 'rendering' | 'packing' | 'finished' | 'failed';
    template_name: string | null;
    report: { dir: string; written: number; failed: string[] } | null;
    error: string | null;
    ready: boolean;
    filename: string | null;
  }

  let templates = $state<Template[]>([]);
  let templateId = $state('');
  let dir = $state('');
  let job = $state<ExportJob | null>(null);
  let downloading = $state(false);
  let msg = $state('');
  let error = $state('');

  admin.get<Template[]>('/templates?archived=true').then((t) => (templates = t));

  let timer: ReturnType<typeof setTimeout> | undefined;
  onDestroy(() => clearTimeout(timer));

  const busy = $derived(!!job?.running || downloading);

  async function start(kind: 'disk' | 'zip') {
    clearTimeout(timer);
    msg = error = '';
    try {
      job = await admin.post<ExportJob>('/export/jobs', { kind, dir: kind === 'disk' ? dir || null : null, template_id: templateId || null });
      poll();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  async function poll() {
    if (!job) return;
    try {
      job = await admin.get<ExportJob>(`/export/jobs/${job.id}`);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
      return;
    }
    if (job.running) {
      timer = setTimeout(poll, 500);
      return;
    }
    if (job.error) {
      error = job.error;
      return;
    }
    const r = job.report!;
    if (r.failed.length) error = `${r.failed.length} document(s) could not be rendered: ${r.failed.join(', ')}`;
    if (job.kind === 'disk') {
      msg = `Wrote ${r.written} PDFs to ${r.dir}`;
    } else if (job.ready) {
      downloading = true;
      try {
        const f = await adminFetch(`/export/jobs/${job.id}/download`, job.filename ?? 'signatures.zip');
        await downloadBlob(f.blob, f.filename);
        msg = `Downloaded ${r.written} PDFs as ${f.filename}` + (r.failed.length ? ' (the missing ones are listed in FAILED.txt)' : '');
      } catch (e) {
        error = e instanceof Error ? e.message : String(e);
      } finally {
        downloading = false;
      }
    }
  }

  const fraction = $derived(!job ? 0 : !job.running ? 1 : job.total ? job.done / job.total : 0);
  const status = $derived.by(() => {
    if (!job) return '';
    if (downloading) return 'Downloading ZIP…';
    if (job.phase === 'packing') return `Packing ZIP (${job.total} documents)…`;
    if (job.running) return job.total ? `Rendering document ${Math.min(job.done + 1, job.total)} of ${job.total}…` : 'Preparing…';
    return '';
  });
</script>

<div class="card">
  <h2>Export signed documents</h2>
  <p class="muted">Recreates the signed documents as PDFs (with an audit page) in <code>&lt;dir&gt;/&lt;template&gt;/&lt;date&gt;_&lt;name&gt;_&lt;id&gt;.pdf</code> plus an <code>index.csv</code>. Re-running updates the files of continued documents. CLI: <code>pdfsign export [--template &lt;slug&gt;]</code>.</p>
  <div class="row">
    <select bind:value={templateId} style="width:auto;max-width:100%" aria-label="Template to export" disabled={busy}>
      <option value="">All templates</option>
      {#each templates as t (t.id)}<option value={t.id}>{t.name}{t.archived ? ' (archived)' : ''}</option>{/each}
    </select>
    {#if isAdmin() && showDir}
      <input class="grow" style="flex:1;width:auto;min-width:180px" bind:value={dir} placeholder={session.exportDir} aria-label="Export folder" disabled={busy} />
    {/if}
    {#if isAdmin()}<button class="primary" onclick={() => start('disk')} disabled={busy}>{job?.running && job.kind === 'disk' ? 'Exporting…' : 'Export to server disk'}</button>{/if}
    <button onclick={() => start('zip')} disabled={busy}>{busy && job?.kind === 'zip' ? 'Preparing ZIP…' : 'Download ZIP'}</button>
  </div>
  {#if isAdmin() && !showDir}<div class="small muted" style="margin-top:6px">Server folder: <code>{session.exportDir}</code></div>{/if}
  {#if job && (busy || status)}
    <div class="progress" style="margin-top:12px" role="progressbar" aria-label="Export progress" aria-valuemin="0" aria-valuemax="100" aria-valuenow={Math.round(fraction * 100)}>
      <div class="bar" class:indeterminate={job.phase === 'packing' || downloading} style:width="{(fraction * 100).toFixed(1)}%"></div>
    </div>
    <div class="small muted" style="margin-top:4px">{status}{job.template_name ? ` · ${job.template_name}` : ''}</div>
  {/if}
  {#if msg}<div class="success">{msg}</div>{/if}
  {#if error}<div class="error">{error}</div>{/if}
</div>
