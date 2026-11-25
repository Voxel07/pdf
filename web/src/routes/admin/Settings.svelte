<script lang="ts">
  import { admin, copy, downloadExportZip, fmtDateTime } from '../../lib/api';
  import { session, isAdmin } from '../../lib/session.svelte';

  let keys = $state<any[]>([]);
  let users = $state<any[]>([]);
  let keyName = $state('discord-bot');
  let newKey = $state('');
  let exportDir = $state('');
  let exportMsg = $state('');

  async function load() {
    if (!isAdmin()) return;
    keys = await admin.get('/api-keys');
    users = await admin.get('/users');
  }
  load();

  async function create(e: SubmitEvent) {
    e.preventDefault();
    const r = await admin.post('/api-keys', { name: keyName });
    newKey = r.key;
    await load();
  }

  async function exportDisk() {
    exportMsg = 'Exporting…';
    const r = await admin.post('/export', { dir: exportDir || null });
    exportMsg = `Wrote ${r.written} PDFs to ${r.dir}` + (r.failed.length ? `; failed: ${r.failed.join(', ')}` : '');
  }
</script>

<h1>API & export</h1>

{#if isAdmin()}
  <div class="card">
    <h2>API tokens</h2>
    <p class="muted">Bots and integrations authenticate with <code>Authorization: Bearer &lt;token&gt;</code> against <code>{session.publicUrl}/api/v1</code>. Tokens are stored hashed and shown only once.</p>
    <form class="row" onsubmit={create}>
      <input class="grow" style="flex:1;width:auto" bind:value={keyName} placeholder="Token name" required />
      <button class="primary" type="submit">Generate token</button>
    </form>
    {#if newKey}
      <div class="success">New token (shown only once): <code>{newKey}</code> <button class="small" onclick={() => copy(newKey)}>Copy</button></div>
    {/if}
    <table style="margin-top:10px">
      <thead><tr><th>Name</th><th>Created by</th><th>Created</th><th>Last used</th><th></th></tr></thead>
      <tbody>
        {#each keys as k (k.id)}
          <tr><td>{k.name}</td><td>{k.created_by ?? '—'}</td><td>{fmtDateTime(k.created_at)}</td><td>{fmtDateTime(k.last_used_at)}</td>
            <td style="text-align:right"><button class="small danger" onclick={async () => { if (confirm('Revoke this token?')) { await admin.del(`/api-keys/${k.id}`); load(); } }}>Revoke</button></td></tr>
        {:else}
          <tr><td colspan="5" class="muted">No tokens yet.</td></tr>
        {/each}
      </tbody>
    </table>
  </div>
{/if}

<div class="card">
  <h2>Export</h2>
  <p class="muted">Writes every signed document as <code>&lt;dir&gt;/&lt;template&gt;/&lt;date&gt;_&lt;name&gt;_&lt;id&gt;.pdf</code> plus <code>index.csv</code>. Re-running updates the files of continued documents. CLI: <code>docker compose exec pdfsign pdfsign export</code>.</p>
  <div class="row">
    {#if isAdmin()}
      <input class="grow" style="flex:1;width:auto" bind:value={exportDir} placeholder={session.exportDir} />
      <button class="primary" onclick={exportDisk}>Export to disk</button>
    {/if}
    <button onclick={async () => (exportMsg = await downloadExportZip())}>Download ZIP</button>
  </div>
  {#if exportMsg}<div class="success">{exportMsg}</div>{/if}
</div>

{#if isAdmin()}
  <div class="card">
    <h2>Users</h2>
    <p class="muted">Users log in through authentik. Their role comes from authentik group membership and is updated on every login.</p>
    <table>
      <thead><tr><th>User</th><th>E-mail</th><th>Role</th><th>Last login</th></tr></thead>
      <tbody>
        {#each users as u (u.id)}
          <tr><td><b>{u.name}</b> <span class="muted small">{u.username}</span></td><td>{u.email ?? '—'}</td><td><span class="badge accent">{u.role}</span></td><td>{fmtDateTime(u.last_login_at)}</td></tr>
        {/each}
      </tbody>
    </table>
  </div>
{/if}

<div class="card">
  <h2>Bot API quick reference</h2>
  <pre class="small">GET  /api/v1/templates
GET  /api/v1/persons/:discord_id/status?template=:slug   → {'{'} registered, valid, valid_until, submission {'}'}
PUT  /api/v1/persons/:discord_id   {'{'} name, username {'}'}
POST /api/v1/links   {'{'} template, discord_id, name?, username?, force_new? {'}'}  → {'{'} url, status, valid, created {'}'}
POST /api/v1/persons/link   {'{'} old_discord_id, new_discord_id {'}'}
GET  /api/v1/submissions/:id/pdf</pre>
</div>
