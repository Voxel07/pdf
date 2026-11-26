<script lang="ts">
  import { admin, type PersonGroup, type Template } from '../../lib/api';

  interface Requirement {
    id: string;
    name: string;
    archived: boolean;
    missing: { id: string; name: string; zip: string | null; status: keyof typeof statusLabels }[];
  }
  const statusLabels = { unissued: 'No document issued', pending: 'Awaiting signature', expired: 'Expired', revoked: 'Revoked' };

  let groups = $state<PersonGroup[]>([]);
  let templates = $state<Template[]>([]);
  let requirements = $state<Requirement[]>([]);
  let groupId = $state('');
  let templateId = $state('');
  let name = $state('');
  let error = $state('');
  let reportError = $state('');
  let busy = $state(false);
  let loading = $state(true);
  let revision = $state(0);

  async function init() {
    try {
      [groups, templates] = await Promise.all([
        admin.get<PersonGroup[]>('/groups'), admin.get<Template[]>('/templates'),
      ]);
      groupId = groups[0]?.id ?? '';
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      loading = false;
    }
  }
  void init();

  $effect(() => {
    const id = groupId;
    revision;
    let current = true;
    requirements = [];
    templateId = '';
    reportError = '';
    if (id) {
      loading = true;
      admin.get<Requirement[]>(`/groups/${id}/requirements`).then((rows) => {
        if (current) requirements = rows;
      }).catch((e) => { if (current) reportError = e.message; })
        .finally(() => { if (current) loading = false; });
    }
    return () => { current = false; };
  });

  async function change(action: () => Promise<void>) {
    busy = true;
    error = '';
    try { await action(); }
    catch (e) { error = e instanceof Error ? e.message : String(e); }
    finally { busy = false; }
  }

  function create(e: SubmitEvent) {
    e.preventDefault();
    void change(async () => {
      const group = await admin.post<PersonGroup>('/groups', { name });
      groups = [...groups, group].sort((a, b) => a.name.localeCompare(b.name));
      groupId = group.id;
      name = '';
    });
  }
  function updateRequirement(id: string, add: boolean) {
    const path = `/groups/${groupId}/requirements/${id}`;
    void change(async () => {
      if (add) await admin.put(path, {});
      else await admin.del(path);
      revision++;
    });
  }
</script>

<div class="row"><h1 class="grow">Groups &amp; requirements</h1><a href="/admin/persons">People</a></div>
{#if error}<div class="error">{error}</div>{/if}
<form class="card" onsubmit={create}>
  <label for="new-group">New group</label>
  <div class="row"><input class="grow" style="width:auto" id="new-group" bind:value={name} maxlength="100" placeholder="Group name" required />
    <button disabled={busy || !name.trim()}>Create group</button></div>
</form>
{#if groups.length}
  <div class="card">
    <label for="group">Group</label>
    <select id="group" bind:value={groupId} disabled={busy}>{#each groups as g}<option value={g.id}>{g.name}</option>{/each}</select>
  </div>
  <div class="card">
    <h2>Required templates</h2>
    <div class="row">
      <select bind:value={templateId} aria-label="Template to require" class="grow" style="width:auto" disabled={loading || busy || !!reportError}>
        <option value="">Choose a template</option>
        {#each templates.filter((t) => !requirements.some((r) => r.id === t.id)) as t}<option value={t.id}>{t.name}</option>{/each}
      </select>
      <button disabled={!templateId || busy || loading} onclick={() => updateRequirement(templateId, true)}>Require template</button>
    </div>
    {#if reportError}<p class="error">{reportError} <button class="small" onclick={() => revision++}>Retry</button></p>
    {:else if loading}<p class="muted">Loading requirements…</p>
    {:else if requirements.length}
      <p class="small muted">Open a template to see group members who still need a valid signature.</p>
      {#each requirements as r (r.id)}
        <div class="requirement">
          <details>
            <summary><strong>{r.name}</strong> {r.archived ? '(archived)' : ''} · {r.missing.length} outstanding</summary>
            {#each r.missing as p (p.id)}
              <div class="member"><a href="/admin/submissions?person_id={p.id}&person_name={encodeURIComponent(p.name)}&template_id={r.id}">{p.name}</a> <span class="small muted">{p.zip ?? ''}</span>
                <span class="badge {p.status}">{statusLabels[p.status]}</span>
              </div>
            {:else}<p class="muted small">No outstanding signatures.</p>{/each}
          </details>
          <button class="small ghost" disabled={busy} aria-label="Remove requirement for {r.name}" onclick={() => updateRequirement(r.id, false)}>Remove</button>
        </div>
      {/each}
    {:else}<p class="muted">This group has no required templates.</p>{/if}
  </div>
{:else if !loading && !error}<p class="muted">Create a group, then assign members under People → Manage.</p>{/if}

<style>
  .requirement { display:flex; gap:12px; align-items:flex-start; border-top:1px solid var(--border); padding:12px 0; }
  details { flex:1; }
  summary { cursor:pointer; }
  .member { padding:6px 0 0 16px; }
</style>
