<script lang="ts">
  import { admin, copy, fmtDateTime } from '../../lib/api';
  import { session, isAdmin } from '../../lib/session.svelte';
  import ExportPanel from './ExportPanel.svelte';

  let keys = $state<any[]>([]);
  let users = $state<any[]>([]);
  let keyName = $state('discord-bot');
  let newKey = $state('');
  let confirmation = $state('');
  let resetting = $state(false);
  let resetError = $state('');

  async function resetData(e: SubmitEvent) {
    e.preventDefault();
    if (confirmation !== 'DELETE ALL DATA' || resetting) return;
    resetting = true;
    resetError = '';
    try {
      await admin.del('/data', { confirmation });
      location.replace('/admin');
    } catch (err) {
      resetError = err instanceof Error ? err.message : String(err);
      resetting = false;
    }
  }

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

<ExportPanel showDir />

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

{#if isAdmin()}
  <form class="card" onsubmit={resetData}>
    <h2>Delete all application data</h2>
    <p>Permanently delete all templates, source PDFs, submissions, signatures, people, links, and activity. This also clears settings and API tokens and signs everyone out.</p>
    <p class="muted small">Backups, exports, and downloaded copies are kept. You can delete backup archives separately under Backups.</p>
    <label for="confirm-reset">Type <b>DELETE ALL DATA</b> to confirm.</label>
    <div class="row">
      <input id="confirm-reset" style="max-width:240px" bind:value={confirmation} autocomplete="off" disabled={resetting} />
      <button type="submit" class="danger" disabled={resetting || confirmation !== 'DELETE ALL DATA'}>{resetting ? 'Deleting…' : 'Delete all application data'}</button>
    </div>
    {#if resetError}<div class="error" role="alert">{resetError}</div>{/if}
  </form>
{/if}
