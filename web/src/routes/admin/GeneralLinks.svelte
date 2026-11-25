<script lang="ts">
  import { admin, copy, downloadBlob, fmtDate } from '../../lib/api';

  let { templateId, templateName }: { templateId: string; templateName: string } = $props();

  interface Link { id: string; token: string; label: string; active: boolean; expires_at: string | null; uses: number; url: string; created_by: string | null }

  let links = $state<Link[]>([]);
  let label = $state('');
  let expires = $state('');
  let error = $state('');
  let qrFor = $state<Link | null>(null);

  const load = async () => (links = await admin.get(`/templates/${templateId}/links`));
  load();

  async function create(e: SubmitEvent) {
    e.preventDefault();
    error = '';
    try {
      const l = await admin.post<Link>(`/templates/${templateId}/links`, { label, expires_at: expires });
      label = expires = '';
      await load();
      qrFor = l;
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
    }
  }

  async function toggle(l: Link) {
    await admin.put(`/links/${l.id}`, { active: !l.active });
    await load();
  }

  async function remove(l: Link) {
    if (!confirm('Delete this link? Printed QR codes stop working. Signed documents are kept.')) return;
    await admin.del(`/links/${l.id}`);
    await load();
  }

  const qrUrl = (l: Link) => `/api/admin/links/${l.id}/qr.svg`;
  const fileBase = (l: Link) => `qr-${templateName}-${l.label || l.token}`.replace(/[^\w-]+/g, '_');

  async function downloadSvg(l: Link) {
    downloadBlob(await admin.blob(`/links/${l.id}/qr.svg`), `${fileBase(l)}.svg`);
  }

  async function downloadPng(l: Link) {
    const svg = await (await admin.blob(`/links/${l.id}/qr.svg`)).text();
    const img = new Image();
    img.src = 'data:image/svg+xml;base64,' + btoa(svg);
    await img.decode();
    const c = document.createElement('canvas');
    c.width = c.height = 1200;
    const ctx = c.getContext('2d')!;
    ctx.imageSmoothingEnabled = false;
    ctx.drawImage(img, 0, 0, 1200, 1200);
    c.toBlob((b) => b && downloadBlob(b, `${fileBase(l)}.png`), 'image/png');
  }

  async function printPoster(l: Link) {
    const svg = await (await admin.blob(`/links/${l.id}/qr.svg`)).text();
    const w = window.open('', '_blank');
    if (!w) return;
    const esc = (s: string) => s.replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' })[c]!);
    w.document.write(`<!doctype html><meta charset="utf-8"><title>${esc(templateName)}</title>
      <style>body{font-family:system-ui,sans-serif;text-align:center;margin:40px}h1{font-size:34px;margin:0 0 8px}
      p{font-size:18px;color:#444}.qr svg{width:420px;height:420px}.url{font-family:monospace;font-size:14px;color:#666;word-break:break-all}</style>
      <h1>${esc(templateName)}</h1>${l.label ? `<p>${esc(l.label)}</p>` : ''}
      <p>Scan the code, enter your name and sign on your phone.</p>
      <div class="qr">${svg}</div><div class="url">${esc(l.url)}</div>
      <script>setTimeout(() => print(), 300)<\/script>`);
    w.document.close();
  }
</script>

<div class="card">
  <h3>General links & QR codes</h3>
  <p class="small muted">One link for everyone, for example printed at the entrance. People enter their name and sign on the spot, or see that they already have a valid signature. The zip code is only asked for when several people share a name, or for first-time signers.</p>
  <form onsubmit={create}>
    <div class="grid2">
      <input placeholder="Label (e.g. Summer camp 2026)" bind:value={label} />
      <input type="date" bind:value={expires} title="Expires (optional)" />
    </div>
    <button class="small primary" type="submit" style="margin-top:8px">+ Create link</button>
  </form>
  {#if error}<div class="error">{error}</div>{/if}
  {#each links as l (l.id)}
    <div class="link" class:off={!l.active}>
      <div class="row">
        <b class="grow">{l.label || 'Untitled link'}</b>
        <span class="badge {l.active ? 'completed' : 'revoked'}">{l.active ? 'active' : 'off'}</span>
      </div>
      <div class="small muted">{l.uses} uses{l.expires_at ? ` · expires ${fmtDate(l.expires_at)}` : ''}{l.created_by ? ` · by ${l.created_by}` : ''}</div>
      <div class="row" style="margin-top:6px;gap:4px">
        <button class="small" onclick={() => (qrFor = l)}>QR</button>
        <button class="small" onclick={() => copy(l.url)}>Copy</button>
        <button class="small" onclick={() => toggle(l)}>{l.active ? 'Disable' : 'Enable'}</button>
        <button class="small ghost" onclick={() => remove(l)}>🗑</button>
      </div>
    </div>
  {/each}
</div>

{#if qrFor}
  <div class="modal-backdrop" role="presentation" onclick={(e) => e.target === e.currentTarget && (qrFor = null)}>
    <div class="modal" style="width:min(460px,100%);text-align:center">
      <div class="row"><h3 class="grow" style="margin:0;text-align:left">{qrFor.label || templateName}</h3><button class="ghost small" onclick={() => (qrFor = null)}>✕</button></div>
      <img src={qrUrl(qrFor)} alt="QR code" style="width:300px;height:300px;margin:12px auto;display:block" />
      <input readonly value={qrFor.url} />
      <div class="row" style="margin-top:12px;justify-content:center">
        <button class="primary" onclick={() => printPoster(qrFor!)}>Print poster</button>
        <button onclick={() => downloadPng(qrFor!)}>PNG</button>
        <button onclick={() => downloadSvg(qrFor!)}>SVG</button>
        <button onclick={() => copy(qrFor!.url)}>Copy link</button>
      </div>
    </div>
  </div>
{/if}

<style>
  .link { border-top: 1px solid var(--border); padding: 10px 0 4px; margin-top: 10px; }
  .link.off { opacity: 0.6; }
</style>
