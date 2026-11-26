<script lang="ts">
  import { onMount } from 'svelte';
  import { admin, copy } from '../../lib/api';

  type Scope = 'calendar' | 'read' | 'write';
  interface Credential {
    id: string;
    name: string;
    scope: Scope;
    created_at: string;
  }

  const endpoint = '/events/credentials';
  const names: Record<Scope, string> = { calendar: 'Calendar subscription', read: 'Read-only API token', write: 'Read/write API token' };
  let credentials = $state<Credential[]>([]);
  let issued = $state<(Credential & { secret: string })>();
  let name = $state('');
  let scope = $state<Scope>('calendar');
  let busy = $state(false);
  let loading = $state(true);
  let error = $state('');
  let copied = $state(false);

  onMount(async () => {
    try {
      credentials = await admin.get<Credential[]>(endpoint);
    } catch (err) {
      error = (err as Error).message;
    } finally {
      loading = false;
    }
  });

  async function issue(event: SubmitEvent) {
    event.preventDefault();
    busy = true;
    error = '';
    try {
      issued = await admin.post<Credential & { secret: string }>(endpoint, { name, scope });
      credentials = [issued, ...credentials];
      name = '';
      copied = false;
    } catch (err) {
      error = (err as Error).message;
    } finally {
      busy = false;
    }
  }

  async function revoke(credential: Credential) {
    if (!window.confirm('Revoke access for “' + credential.name + '”?')) return;
    busy = true;
    error = '';
    try {
      await admin.del(endpoint + '/' + credential.id);
      credentials = credentials.filter((item) => item.id !== credential.id);
      if (issued?.id === credential.id) issued = undefined;
    } catch (err) {
      error = (err as Error).message;
    } finally {
      busy = false;
    }
  }

  async function copySecret(secret: string) {
    try {
      await copy(secret);
      copied = true;
    } catch (err) {
      error = (err as Error).message;
    }
  }
</script>

<p><a href="/admin/events">← Events</a></p>
<h1>Calendars &amp; API</h1>
{#if error}<p class="error" role="alert">{error}</p>{/if}

<form class="card" onsubmit={issue}>
  <h2>Create a subscription or API token</h2>
  <div class="row fields">
    <div class="grow">
      <label for="credential-name">Name</label>
      <input id="credential-name" maxlength="120" bind:value={name} required />
    </div>
    <div class="grow">
      <label for="credential-scope">Access</label>
      <select id="credential-scope" bind:value={scope}>
        {#each Object.entries(names) as [value, label] (value)}
          <option {value}>{label}</option>
        {/each}
      </select>
    </div>
    <button class="primary" disabled={busy || loading} type="submit">Generate</button>
  </div>
</form>

{#if issued}
  <section class="card" aria-label="New credential">
    <label for="credential-secret">{names[issued.scope]}</label>
    <div class="row">
      <input id="credential-secret" class="grow" value={issued.secret} readonly />
      <button onclick={() => copySecret(issued?.secret ?? '')}>{copied ? 'Copied' : 'Copy'}</button>
    </div>
    {#if issued.scope === 'calendar'}
      <p class="small muted">Use “Subscribe by URL” or “Add by URL” in your calendar app. Anyone with this URL can read the feed.</p>
    {:else}
      <p class="small muted">Copy this token now. The app shows it only when issued.</p>
    {/if}
  </section>
{/if}

<section class="card">
  <h2>Active access</h2>
  {#if loading}
    <p class="muted" role="status">Loading…</p>
  {:else if !credentials.length && !error}
    <p class="muted">No calendar links or API tokens.</p>
  {/if}
  {#each credentials as credential (credential.id)}
    <div class="row credential">
      <div class="grow">
        <strong>{credential.name}</strong>
        <div class="small muted">{names[credential.scope]}</div>
      </div>
      <button class="small danger" disabled={busy} onclick={() => revoke(credential)}>Revoke</button>
    </div>
  {/each}
</section>

<style>
  .fields { align-items: end; }
  .fields > .grow { min-width: min(100%, 220px); }
  .credential { padding: 8px 0; border-bottom: 1px solid var(--border); overflow-wrap: anywhere; }
  .card p:last-child { margin: 8px 0 0; }
</style>
