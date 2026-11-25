<script lang="ts">
  import { admin, copy, type Template } from '../../lib/api';
  import { route } from '../../lib/router.svelte';
  import { isAdmin } from '../../lib/session.svelte';

  let list = $state<any[]>([]);
  let q = $state(route.query.get('q') ?? '');
  let templates = $state<Template[]>([]);
  let openId = $state<string | null>(null);
  let error = $state('');
  let info = $state('');

  // new person
  let newName = $state('');
  let newDiscord = $state('');
  let newEmail = $state('');
  let newZip = $state('');
  // link accounts
  let oldId = $state('');
  let newId = $state('');
  // per person actions
  let addDiscord = $state('');
  let mergeInto = $state('');
  let linkTemplate = $state('');
  let createdLink = $state('');

  admin.get('/templates').then((t) => (templates = t));

  async function load() {
    list = await admin.get(`/persons?q=${encodeURIComponent(q)}`);
  }
  let timer: ReturnType<typeof setTimeout>;
  $effect(() => {
    q;
    clearTimeout(timer);
    timer = setTimeout(load, 200);
  });

  async function run(fn: () => Promise<unknown>, ok?: string) {
    error = '';
    info = '';
    try {
      await fn();
      if (ok) info = ok;
      await load();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  const create = (e: SubmitEvent) => {
    e.preventDefault();
    run(async () => {
      await admin.post('/persons', { name: newName, discord_id: newDiscord || null, email: newEmail || null, zip: newZip || null });
      newName = newDiscord = newEmail = newZip = '';
    }, 'Person created');
  };

  const link = (e: SubmitEvent) => {
    e.preventDefault();
    run(async () => {
      const p = await admin.post('/persons/link-discord', { old_discord_id: oldId, new_discord_id: newId });
      info = `Linked ${newId} to ${p.name}. All documents are kept.`;
      oldId = newId = '';
    });
  };

  // normalized-name duplicates (same rule as the server: case/accents/ue-spellings ignored)
  const norm = (n: string) =>
    n.toLowerCase().replace(/ß/g, 'ss').normalize('NFKD').replace(/[\u0300-\u036f]/g, '')
      .replace(/ae/g, 'a').replace(/oe/g, 'o').replace(/ue/g, 'u').replace(/ss/g, 's')
      .split(/[\s,.]+/).filter(Boolean).join(' ');
  const dupCount = $derived(list.reduce((m, p) => m.set(norm(p.name), (m.get(norm(p.name)) ?? 0) + 1), new Map<string, number>()));
  const isDup = (p: any) => (dupCount.get(norm(p.name)) ?? 0) > 1;

  // sorting (client side; the server returns up to 1000 people)
  type SortKey = 'name' | 'discord' | 'documents';
  let sortKey = $state<SortKey>('name');
  let sortDir = $state<1 | -1>(1);
  function sortBy(k: SortKey) {
    if (sortKey === k) sortDir = sortDir === 1 ? -1 : 1;
    else {
      sortKey = k;
      sortDir = k === 'documents' ? -1 : 1;
    }
  }
  const collator = new Intl.Collator(undefined, { sensitivity: 'base', numeric: true });
  const discordLabel = (p: any) => p.discord_accounts.map((d: any) => d.username || d.discord_id).sort(collator.compare)[0] ?? '';
  const sorted = $derived.by(() => {
    const byName = (a: any, b: any) => collator.compare(a.name, b.name);
    return list.toSorted((a, b) => {
      if (sortKey === 'documents') return sortDir * (a.submission_count - b.submission_count) || byName(a, b);
      if (sortKey === 'discord') {
        const da = discordLabel(a), db = discordLabel(b);
        // people without a Discord account always come last
        if (!da !== !db) return da ? -1 : 1;
        return sortDir * collator.compare(da, db) || byName(a, b);
      }
      return sortDir * byName(a, b);
    });
  });
  const arrow = (k: SortKey) => (sortKey === k ? (sortDir === 1 ? ' ▲' : ' ▼') : '');

  function toggle(p: any) {
    openId = openId === p.id ? null : p.id;
    addDiscord = mergeInto = createdLink = '';
  }
</script>

<h1>People</h1>
{#if error}<div class="error">{error}</div>{/if}
{#if info}<div class="success">{info}</div>{/if}

<div class="grid2">
  <form class="card" onsubmit={create} style="margin:0">
    <h3>Add person</h3>
    <div class="field"><input placeholder="Full name" bind:value={newName} required /></div>
    <div class="grid2">
      <input placeholder="Discord id (optional)" bind:value={newDiscord} />
      <input placeholder="Zip code (optional)" bind:value={newZip} />
    </div>
    <input placeholder="E-mail (optional)" bind:value={newEmail} style="margin-top:10px" /> 
    <button class="primary" type="submit" style="margin-top:10px">Create</button>
  </form>
  <form class="card" onsubmit={link} style="margin:0">
    <h3>Lost Discord account?</h3>
    <p class="small muted">Link a new Discord account to the person of an old one. If the new account already registered itself, both are merged — no documents are lost.</p>
    <div class="grid2">
      <input placeholder="Old Discord id" bind:value={oldId} required />
      <input placeholder="New Discord id" bind:value={newId} required />
    </div>
    <button type="submit" style="margin-top:10px">Link accounts</button>
  </form>
</div>

<div class="card" style="margin-top:16px">
  <input placeholder="Search name, e-mail, Discord id or username" bind:value={q} />
  <table style="margin-top:10px">
    <thead><tr>
      <th aria-sort={sortKey === 'name' ? (sortDir === 1 ? 'ascending' : 'descending') : 'none'}><button class="sort" class:on={sortKey === 'name'} onclick={() => sortBy('name')}>Name{arrow('name')}</button></th>
      <th aria-sort={sortKey === 'discord' ? (sortDir === 1 ? 'ascending' : 'descending') : 'none'}><button class="sort" class:on={sortKey === 'discord'} onclick={() => sortBy('discord')}>Discord{arrow('discord')}</button></th>
      <th aria-sort={sortKey === 'documents' ? (sortDir === 1 ? 'ascending' : 'descending') : 'none'}><button class="sort" class:on={sortKey === 'documents'} onclick={() => sortBy('documents')}>Documents{arrow('documents')}</button></th>
      <th style="text-align:right"><span class="small muted" style="text-transform:none;font-weight:400">{list.length} {list.length === 1 ? 'person' : 'people'}</span></th>
    </tr></thead>
    <tbody>
      {#each sorted as p (p.id)}
        <tr>
          <td><b>{p.name}</b>
            {#if isDup(p)}<span class="badge pending" title="Several people share this name - their zip code tells them apart on general links">same name{p.zip ? '' : ' · no zip!'}</span>{/if}
            <div class="small muted">{[p.zip, p.email].filter(Boolean).join(' · ')}</div></td>
          <td>{#each p.discord_accounts as d}<div><code>{d.discord_id}</code> <span class="small muted">{d.username ?? ''}</span></div>{:else}<span class="muted">—</span>{/each}</td>
          <td><a href="/admin/submissions?q={encodeURIComponent(p.name)}">{p.submission_count}</a></td>
          <td style="text-align:right"><button class="small" onclick={() => toggle(p)}>{openId === p.id ? 'Close' : 'Manage'}</button></td>
        </tr>
        {#if openId === p.id}
          <tr>
            <td colspan="4" style="background:var(--panel-2)">
              <div class="grid2">
                <div>
                  <label for="nm-{p.id}">Name (second factor for signing links)</label>
                  <div class="row">
                    <input id="nm-{p.id}" bind:value={p.name} style="flex:1;width:auto" />
                    <input placeholder="Zip" bind:value={p.zip} style="width:90px" />
                    <button class="small" onclick={() => run(() => admin.put(`/persons/${p.id}`, { name: p.name, email: p.email, zip: p.zip }), 'Saved')}>Save</button>
                  </div>
                  <label for="dc-{p.id}" style="margin-top:10px">Discord accounts</label>
                  {#each p.discord_accounts as d}
                    <div class="row small"><code>{d.discord_id}</code> {d.username ?? ''}
                      <button class="small ghost" onclick={() => confirm('Unlink this Discord account?') && run(() => admin.del(`/discord/${d.discord_id}`))}>unlink</button>
                    </div>
                  {/each}
                  <div class="row" style="margin-top:4px">
                    <input id="dc-{p.id}" placeholder="Discord id" bind:value={addDiscord} style="flex:1;width:auto" />
                    <button class="small" onclick={() => run(() => admin.post(`/persons/${p.id}/discord`, { discord_id: addDiscord }), 'Linked')}>Link</button>
                  </div>
                </div>
                <div>
                  <label for="tl-{p.id}">Create / get signing link</label>
                  <div class="row">
                    <select id="tl-{p.id}" bind:value={linkTemplate} style="flex:1;width:auto">
                      <option value="">— template —</option>
                      {#each templates as t}<option value={t.id}>{t.name}</option>{/each}
                    </select>
                    <button class="small" disabled={!linkTemplate} onclick={() => run(async () => { const s = await admin.post('/submissions', { template_id: linkTemplate, person_id: p.id }); createdLink = s.url; await copy(s.url); }, 'Link copied to clipboard')}>Get link</button>
                  </div>
                  {#if createdLink}<input readonly value={createdLink} style="margin-top:6px" />{/if}
                  <label for="mg-{p.id}" style="margin-top:10px">Merge this person into…</label>
                  <div class="row">
                    <select id="mg-{p.id}" bind:value={mergeInto} style="flex:1;width:auto">
                      <option value="">— person —</option>
                      {#each list.filter((x) => x.id !== p.id) as o}<option value={o.id}>{o.name}</option>{/each}
                    </select>
                    <button class="small" disabled={!mergeInto} onclick={() => confirm(`Move all documents and accounts of ${p.name} to the selected person and delete ${p.name}?`) && run(() => admin.post('/persons/merge', { from: p.id, into: mergeInto }), 'Merged')}>Merge</button>
                  </div>
                  {#if isAdmin()}<button class="small danger" style="margin-top:14px" onclick={() => confirm(`Delete ${p.name} and ALL their documents?`) && run(() => admin.del(`/persons/${p.id}`), 'Deleted')}>Delete person</button>{/if}
                </div>
              </div>
            </td>
          </tr>
        {/if}
      {/each}
    </tbody>
  </table>
</div>
