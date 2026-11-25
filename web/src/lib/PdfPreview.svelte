<script lang="ts">
  import { onDestroy } from 'svelte';
  import { downloadBlob } from './api';
  import { openPdfBlob, type PdfDoc } from './pdf';
  import PdfViewer from './PdfViewer.svelte';

  /** Shows a PDF in the page before it is downloaded. `load` fetches it (with whatever
   * credentials the caller needs) and returns the file name for the download. */
  let { title = 'Preview', load, onclose }: { title?: string; load: () => Promise<{ blob: Blob; filename: string }>; onclose: () => void } = $props();

  let doc = $state<PdfDoc | null>(null);
  let file = $state<{ blob: Blob; filename: string } | null>(null);
  let error = $state('');
  let blobUrl = '';
  let task: Awaited<ReturnType<typeof openPdfBlob>> | null = null;
  let closed = false;

  (async () => {
    try {
      file = await load();
      task = await openPdfBlob(file.blob);
      if (closed) return task.destroy();
      doc = await task.promise;
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  })();

  function openTab() {
    if (!file) return;
    blobUrl ||= URL.createObjectURL(file.blob);
    window.open(blobUrl, '_blank');
  }

  onDestroy(() => {
    closed = true;
    task?.destroy();
    if (blobUrl) setTimeout(() => URL.revokeObjectURL(blobUrl), 60_000);
  });

  function onkeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') onclose();
  }
</script>

<svelte:window {onkeydown} />

<div class="modal-backdrop" role="presentation" onclick={(e) => e.target === e.currentTarget && onclose()}>
  <div class="modal preview" role="dialog" aria-modal="true" aria-label={title}>
    <div class="row head">
      <h3 class="grow" style="margin:0">{title}</h3>
      {#if file}<span class="small muted">{doc ? `${doc.numPages} page${doc.numPages === 1 ? '' : 's'} · ` : ''}{(file.blob.size / 1024).toFixed(0)} KB</span>{/if}
      <button class="small" onclick={openTab} disabled={!file}>Open in new tab</button>
      <button class="small primary" onclick={() => file && downloadBlob(file.blob, file.filename)} disabled={!file}>⬇ Download</button>
      <button class="small ghost" onclick={onclose} aria-label="Close preview">✕</button>
    </div>
    <div class="body">
      {#if error}
        <div class="error">{error}</div>
      {:else if doc}
        <PdfViewer {doc} />
      {:else}
        <p class="muted" style="padding:20px">Loading PDF…</p>
      {/if}
    </div>
  </div>
</div>

<style>
  .preview { width: min(980px, 100%); height: min(92vh, 1400px); display: flex; flex-direction: column; padding: 12px; }
  .head { padding-bottom: 10px; border-bottom: 1px solid var(--border); }
  .body { flex: 1; overflow: auto; padding: 14px; background: var(--bg); border-radius: 0 0 8px 8px; }
</style>
