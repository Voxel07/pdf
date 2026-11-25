<script lang="ts">
  import { admin, copy, fmtDate, type SubmissionSummary, type Template } from '../../lib/api';
  import { route, navigate } from '../../lib/router.svelte';

  let list = $state<SubmissionSummary[]>([]);
  let templates = $state<Template[]>([]);
  let templateId = $state(route.query.get('template_id') ?? '');
  let status = $state(route.query.get('status') ?? '');
  let q = $state('');
  let loading = $state(false);

  admin.get('/templates?archived=true').then((t) => (templates = t));

  let timer: ReturnType<typeof setTimeout>;
  $effect(() => {
    const params = new URLSearchParams();
    if (templateId) params.set('template_id', templateId);
    if (status) params.set('status', status);
    if (q.trim()) params.set('q', q.trim());
    clearTimeout(timer);
    timer = setTimeout(async () => {
      loading = true;
      list = await admin.get(`/submissions?${params}`);
      loading = false;
    }, 200);
  });
</script>

<h1>Documents</h1>
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
    {#if loading}<span class="muted small">loading…</span>{/if}
  </div>
</div>
<div class="card">
  {#if list.length === 0}
    <p class="muted">No documents found.</p>
  {:else}
    <table>
      <thead><tr><th>Person</th><th>Template</th><th>Status</th><th>Signed</th><th>Valid until</th><th>Updated</th><th></th></tr></thead>
      <tbody>
        {#each list as s (s.id)}
          <tr class="clickable" onclick={() => navigate(`/admin/submissions/${s.id}`)}>
            <td><b>{s.person_name}</b></td>
            <td>{s.template_name}</td>
            <td><span class="badge {s.status}">{s.status}</span> {#if s.locked}<span class="badge locked">locked</span>{/if}</td>
            <td>{fmtDate(s.completed_at)}</td>
            <td>{s.valid_until ? fmtDate(s.valid_until) : s.status === 'completed' ? '∞' : '—'}</td>
            <td class="muted">{fmtDate(s.updated_at)}</td>
            <td><button class="small" onclick={(e) => { e.stopPropagation(); copy(s.url); }}>Copy link</button></td>
          </tr>
        {/each}
      </tbody>
    </table>
  {/if}
</div>
