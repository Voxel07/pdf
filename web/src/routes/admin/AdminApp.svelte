<script lang="ts">
  import { admin } from '../../lib/api';
  import { route } from '../../lib/router.svelte';
  import { session, isAdmin } from '../../lib/session.svelte';
  import Dashboard from './Dashboard.svelte';
  import Templates from './Templates.svelte';
  import TemplateEditor from './TemplateEditor.svelte';
  import Submissions from './Submissions.svelte';
  import SubmissionDetail from './SubmissionDetail.svelte';
  import Persons from './Persons.svelte';
  import Import from './Import.svelte';
  import Settings from './Settings.svelte';
  import Backups from './Backups.svelte';

  let error = $state('');

  admin
    .get('/me')
    .then((m) => {
      session.user = m.user;
      session.exportDir = m.export_dir;
      session.publicUrl = m.public_url;
      session.version = m.version;
    })
    .catch((e) => (error = e instanceof Error ? e.message : String(e)));

  const nav = [
    ['/admin', 'Dashboard'],
    ['/admin/templates', 'Templates'],
    ['/admin/submissions', 'Documents'],
    ['/admin/persons', 'People'],
    ['/admin/import', 'DocuSeal import', 'admin'],
    ['/admin/settings', 'API & export'],
    ['/admin/backups', 'Backups', 'admin'],
  ];
  const sub = $derived(route.path.replace(/\/$/, ''));
  const editorId = $derived(/^\/admin\/templates\/([^/]+)$/.exec(sub)?.[1]);
  const submissionId = $derived(/^\/admin\/submissions\/([^/]+)$/.exec(sub)?.[1]);
</script>

{#if !session.user}
  <p class="muted" style="padding:40px">{error || 'Loading…'}</p>
{:else}
  <div class="shell" class:wide={!!editorId}>
    <nav>
      <div class="logo" title="version {session.version}">✍️ pdfsign <span class="ver">{session.version}</span></div>
      {#each nav.filter((n) => n[2] !== 'admin' || isAdmin()) as [href, label]}
        <a {href} class:active={href === '/admin' ? sub === '/admin' : sub.startsWith(href)}>{label}</a>
      {/each}
      <span class="spacer"></span>
      <span class="small muted">{session.user.name || session.user.username} · <span class="badge accent">{session.user.role}</span></span>
      <a class="btn small" href="/auth/logout" data-external>Log out</a>
    </nav>
    <div class="content">
      {#if editorId}
        {#key editorId}<TemplateEditor id={editorId} />{/key}
      {:else if submissionId}
        {#key submissionId}<SubmissionDetail id={submissionId} />{/key}
      {:else if sub === '/admin/templates'}
        <Templates />
      {:else if sub === '/admin/submissions'}
        <Submissions />
      {:else if sub === '/admin/persons'}
        <Persons />
      {:else if sub === '/admin/import' && isAdmin()}
        <Import />
      {:else if sub === '/admin/backups' && isAdmin()}
        <Backups />
      {:else if sub === '/admin/settings'}
        <Settings />
      {:else}
        <Dashboard />
      {/if}
    </div>
  </div>
{/if}

<style>
  nav { display: flex; align-items: center; gap: 4px; background: white; border-bottom: 1px solid var(--border); padding: 8px 18px; flex-wrap: wrap; position: sticky; top: 0; z-index: 20; }
  .logo { font-weight: 700; margin-right: 18px; }
  .ver { font-weight: 400; font-size: 0.75rem; color: var(--muted); }
  nav a { padding: 6px 12px; border-radius: 6px; color: var(--text); font-weight: 500; font-size: 0.93rem; }
  nav a:hover { background: #f0f2f6; text-decoration: none; }
  nav a.active { background: var(--accent-soft); color: var(--accent); }
  .content { max-width: 1150px; margin: 0 auto; padding: 22px 18px 60px; }
  .wide .content { max-width: none; padding: 14px; }
</style>
