<script lang="ts">
  import { admin, type Template, fmtDate } from '../../lib/api';
  import { navigate } from '../../lib/router.svelte';

  let list = $state<Template[]>([]);
  let showArchived = $state(false);
  let file = $state<FileList | null>(null);
  let name = $state('');
  let error = $state('');
  let uploading = $state(false);
  // new template based on an existing one
  let all = $state<Template[]>([]);
  let baseId = $state('');
  let copyName = $state('');
  let copying = $state(false);
  let copyError = $state('');
  admin.get<Template[]>('/templates?archived=true').then((t) => (all = t));

  async function copyTemplate(e: SubmitEvent) {
    e.preventDefault();
    if (!baseId) return;
    copying = true;
    copyError = '';
    try {
      const t = await admin.post<Template>(`/templates/${baseId}/duplicate`, { name: copyName || null });
      navigate(`/admin/templates/${t.id}`);
    } catch (err) {
      copyError = err instanceof Error ? err.message : String(err);
    } finally {
      copying = false;
    }
  }

  async function load() {
    list = await admin.get(`/templates?archived=${showArchived}`);
  }
  $effect(() => {
    showArchived;
    load();
  });

  async function upload(e: SubmitEvent) {
    e.preventDefault();
    if (!file?.[0]) return;
    uploading = true;
    error = '';
    try {
      const fd = new FormData();
      fd.append('name', name);
      fd.append('file', file[0]);
      const t = await admin.post<Template>('/templates', fd);
      navigate(`/admin/templates/${t.id}`);
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
    } finally {
      uploading = false;
    }
  }
</script>

<div class="row"><h1 class="grow">Templates</h1><label class="inline"><input type="checkbox" bind:checked={showArchived} /> show archived</label></div>

<form class="card" onsubmit={upload}>
  <h2>New template from PDF</h2>
  <div class="row">
    <input type="file" accept="application/pdf" bind:files={file} style="max-width:320px" required />
    <input class="grow" placeholder="Name (optional, defaults to file name)" bind:value={name} style="width:auto;flex:1" />
    <button class="primary" type="submit" disabled={uploading || !file?.length}>{uploading ? 'Uploading…' : 'Upload & edit'}</button>
  </div>
  {#if error}<div class="error">{error}</div>{/if}
</form>

<form class="card" onsubmit={copyTemplate}>
  <h2>New template based on an existing one</h2>
  <p class="small muted">Copies the PDF, all fields, sections, description and validity — for example to start next year's form. Documents and general links are not copied.</p>
  <div class="row">
    <select bind:value={baseId} style="width:auto;max-width:320px" required aria-label="Template to copy">
      <option value="">— template to copy —</option>
      {#each all as t (t.id)}<option value={t.id}>{t.name}{t.archived ? ' (archived)' : ''}</option>{/each}
    </select>
    <input class="grow" placeholder={baseId ? `${all.find((t) => t.id === baseId)?.name ?? ''} (copy)` : 'Name of the new template'} bind:value={copyName} style="width:auto;flex:1" />
    <button class="primary" type="submit" disabled={copying || !baseId}>{copying ? 'Copying…' : 'Copy & edit'}</button>
  </div>
  {#if copyError}<div class="error">{copyError}</div>{/if}
</form>

<div class="card">
  {#if list.length === 0}
    <p class="muted">No templates yet.</p>
  {:else}
    <table>
      <thead><tr><th>Name</th><th>Slug (API)</th><th>Fields</th><th>Sections</th><th>Valid for</th><th>Documents</th><th>Updated</th></tr></thead>
      <tbody>
        {#each list as t (t.id)}
          <tr class="clickable" onclick={() => navigate(`/admin/templates/${t.id}`)}>
            <td><b>{t.name}</b> {#if t.archived}<span class="badge">archived</span>{/if} {#if t.source}<span class="badge accent">imported</span>{/if}</td>
            <td><code>{t.slug}</code></td>
            <td>{t.fields.length}</td>
            <td>{t.sections.length}</td>
            <td>{t.validity_days ? `${t.validity_days} days` : 'forever'}</td>
            <td><a href="/admin/submissions?template_id={t.id}" onclick={(e) => e.stopPropagation()}>{t.submission_count ?? 0}</a></td>
            <td class="muted">{fmtDate(t.updated_at)}</td>
          </tr>
        {/each}
      </tbody>
    </table>
  {/if}
</div>
