<script lang="ts">
  import { admin, adminDownload, adminFetch, copy, fmtDate, fmtDateTime } from '../../lib/api';
  import PdfPreview from '../../lib/PdfPreview.svelte';
  import { navigate } from '../../lib/router.svelte';
  import { isAdmin } from '../../lib/session.svelte';

  let { id }: { id: string } = $props();
  let s = $state<any>(null);
  let error = $state('');
  let previewing = $state(false);

  async function load() {
    try {
      s = await admin.get(`/submissions/${id}`);
    } catch (e) {
      error = String(e);
    }
  }
  load();

  async function act(path: string, confirmMsg?: string) {
    if (confirmMsg && !confirm(confirmMsg)) return;
    await admin.post(`/submissions/${id}/${path}`);
    await load();
  }

  async function remove() {
    if (!confirm('Permanently delete this document and all its signatures?')) return;
    await admin.del(`/submissions/${id}`);
    navigate('/admin/submissions');
  }

  const sectionName = (sid: number) => s?.sections.find((x: any) => x.id === sid)?.name ?? `#${sid}`;
</script>

{#if error}<div class="error">{error}</div>{/if}
{#if s}
  <a href="/admin/submissions" class="small">← Documents</a>
  <div class="row" style="margin-top:8px"><h1 class="grow">{s.template_name} — {s.person_name}</h1></div>
  <div class="card">
    <div class="grid2">
      <div>
        <div><span class="badge {s.status}">{s.status}</span> {#if s.locked}<span class="badge locked">locked (wrong names)</span>{/if}</div>
        <p class="small" style="margin-top:8px">
          Created {fmtDateTime(s.created_at)}<br />
          Signed {fmtDateTime(s.completed_at)}<br />
          Valid until {s.valid_until ? fmtDate(s.valid_until) : s.status === 'completed' ? 'forever' : '—'}<br />
          {#if s.source}Imported from <code>{s.source}</code><br />{/if}
          Discord: {#each s.discord_accounts as d}<code>{d.discord_id}</code>{d.username ? ` (${d.username})` : ''} {:else}—{/each}
        </p>
      </div>
      <div>
        <label for="url">Signing link</label>
        <div class="row"><input id="url" readonly value={s.url} style="flex:1;width:auto" /><button class="small" onclick={() => copy(s.url)}>Copy</button></div>
        <p class="small muted">The signer has to enter their name (<b>{s.person_name}</b>) to open the link.</p>
      </div>
    </div>
    <div class="row">
      <button class="primary" onclick={() => (previewing = true)}>👁 Preview PDF</button>
      <button onclick={() => adminDownload(`/submissions/${id}/pdf`, 'document.pdf')}>⬇ Download PDF</button>
      <a class="btn" href="/admin/persons?q={encodeURIComponent(s.person_name)}">Person</a>
      <span class="spacer"></span>
      {#if s.locked}<button onclick={() => act('reset-lock')}>Unlock link</button>{/if}
      <button onclick={() => act('rotate-token', 'Generate a new link? The old link stops working.')}>New link</button>
      {#if s.status !== 'revoked'}<button class="danger" onclick={() => act('revoke', 'Revoke this document? It will no longer count as valid.')}>Revoke</button>{/if}
      {#if isAdmin()}<button class="danger" onclick={remove}>Delete</button>{/if}
    </div>
  </div>

  {#if previewing}
    <PdfPreview title="{s.template_name} — {s.person_name}" load={() => adminFetch(`/submissions/${id}/pdf`, 'document.pdf')} onclose={() => (previewing = false)} />
  {/if}

  <div class="card">
    <h2>Values</h2>
    {#if s.values.length === 0}<p class="muted">Nothing filled in yet.</p>{/if}
    <table>
      <thead><tr><th>Section</th><th>Field</th><th>Value</th></tr></thead>
      <tbody>
        {#each s.values.toSorted((a: any, b: any) => a.section - b.section) as v}
          <tr>
            <td class="muted">{sectionName(v.section)}</td>
            <td>{v.field_name ?? v.field_id}</td>
            <td>{#if v.image}<img src={v.image} alt="signature" style="max-height:50px" />{:else}{v.value}{/if}</td>
          </tr>
        {/each}
      </tbody>
    </table>
  </div>

  <div class="card">
    <h2>Audit trail</h2>
    <table>
      <thead><tr><th>When</th><th>Sections</th><th>Name</th><th>IP / client</th></tr></thead>
      <tbody>
        {#each s.rounds as r}
          <tr>
            <td>{fmtDateTime(r.created_at)}</td>
            <td>{JSON.parse(r.sections).map(sectionName).join(', ')}</td>
            <td>{r.signer_name}</td>
            <td class="small muted">{r.ip} {r.user_agent}</td>
          </tr>
        {/each}
      </tbody>
    </table>
  </div>
{/if}
