<script lang="ts">
  import { route } from './lib/router.svelte';
  import Sign from './routes/Sign.svelte';
  import AdminApp from './routes/admin/AdminApp.svelte';
  import PublicLink from './routes/PublicLink.svelte';

  const signToken = $derived(/^\/s\/([A-Za-z0-9]+)\/?$/.exec(route.path)?.[1]);
  const linkToken = $derived(/^\/p\/([A-Za-z0-9]+)\/?$/.exec(route.path)?.[1]);
</script>

{#if signToken}
  {#key signToken}<Sign token={signToken} />{/key}
{:else if linkToken}
  {#key linkToken}<PublicLink token={linkToken} />{/key}
{:else if route.path.startsWith('/admin')}
  <AdminApp />
{:else}
  <div style="max-width:560px;margin:80px auto;padding:0 16px">
    <div class="card">
      <h1>Not found</h1>
      <p class="muted">If you received a signing link, please open it exactly as sent to you.</p>
    </div>
  </div>
{/if}
