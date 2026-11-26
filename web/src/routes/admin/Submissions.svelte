<script lang="ts">
  import { admin, adminFetch, copy, fmtDate, type SubmissionSummary, type Template } from '../../lib/api';
  import PdfPreview from '../../lib/PdfPreview.svelte';
  import ListPagination from '../../lib/ListPagination.svelte';
  import { pagedList, pageSize } from '../../lib/paged-list.svelte';
  import { route, navigate } from '../../lib/router.svelte';

  const list = pagedList<SubmissionSummary>((s) => s.updated_at);
  const personId = $derived(route.query.get('person_id') ?? '');
  const personName = $derived(route.query.get('person_name') || list.items[0]?.person_name);
  let templates = $state<Template[]>([]);
  let templateId = $state(route.query.get('template_id') ?? '');
  let status = $state(route.query.get('status') ?? '');
  let q = $state(route.query.get('q') ?? '');
  let retry = $state(0);
  let preview = $state<SubmissionSummary | null>(null);

  admin.get('/templates?archived=true').then((t) => (templates = t));

  $effect(() => {
    retry;
    const params = new URLSearchParams();
    if (personId) params.set('person_id', personId);
    if (templateId) params.set('template_id', templateId);
    if (status) params.set('status', status);
    if (q.trim()) params.set('q', q.trim());
    const timer = setTimeout(() => list.load(`/submissions?${params}`), q ? 150 : 0);
    return () => { clearTimeout(timer); list.cancel(); };
  });
</script>

<div class="row"><h1 class="grow">{personId && personName ? `Documents for ${personName}` : 'Documents'}</h1>
  {#if personId}<a href="/admin/submissions">All documents</a>{/if}
</div>
<div class="card">
  <div class="row">
    <select bind:value={templateId} style="width:auto">
      <option value="">All templates</option>
      {#each templates as t}<option value={t.id}>{t.name}</option>{/each}
    </select>
    <select bind:value={status} style="width:auto">
      <option value="">Any status</option>
      <option value="pending">Pending</option>
      <option value="completed">Signed</option>
      <option value="expired">Expired</option>
      <option value="revoked">Revoked</option>
    </select>
    <input class="grow" style="flex:1;width:auto" placeholder="Search name, Discord id or username" bind:value={q} />
  </div>
</div>
<div class="card">
  {#if list.error}<div class="error">{list.error} <button class="small" onclick={() => retry++}>Retry</button></div>{/if}
  {#if list.items.length === 0 && !list.loading && !list.error}
    <p class="muted">No documents found.</p>
  {:else if list.items.length}
    <table>
      <thead><tr><th>Person</th><th>Template</th><th>Status</th><th>Signed</th><th>Valid until</th><th>Updated</th><th></th></tr></thead>
      <tbody>
        {#each list.items.slice((list.page - 1) * pageSize, list.page * pageSize) as s (s.id)}
          <tr class="clickable" onclick={(e) => { if (!(e.target as Element).closest('a')) navigate(`/admin/submissions/${s.id}`); }}>
            <td><a href="/admin/submissions?person_id={s.person_id}&person_name={encodeURIComponent(s.person_name)}">{s.person_name}</a></td>
            <td>{s.template_name}</td>
            <td><span class="badge {s.status}">{s.status}</span> {#if s.locked}<span class="badge locked">locked</span>{/if}</td>
            <td>{fmtDate(s.completed_at)}</td>
            <td>{s.valid_until ? fmtDate(s.valid_until) : s.status === 'completed' ? '∞' : '—'}</td>
            <td class="muted">{fmtDate(s.updated_at)}</td>
            <td style="text-align:right;white-space:nowrap">
              <button class="small" title="Preview the PDF" onclick={(e) => { e.stopPropagation(); preview = s; }}>Preview</button>
              <button class="small" onclick={(e) => { e.stopPropagation(); copy(s.url); }}>Copy link</button>
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  {/if}
  <ListPagination page={list.page} count={list.items.length} loading={list.loading} change={(p) => (list.page = p)} />
</div>

{#if preview}
  {@const p = preview}
  <PdfPreview title="{p.template_name} — {p.person_name}" load={() => adminFetch(`/submissions/${p.id}/pdf`, 'document.pdf')} onclose={() => (preview = null)} />
{/if}
