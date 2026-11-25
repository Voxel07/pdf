<script lang="ts">
  import { request, fmtDate } from '../lib/api';
  import { navigate } from '../lib/router.svelte';

  let { token }: { token: string } = $props();

  let info = $state<{ template_name: string; description: string; label: string; validity_days: number | null } | null>(null);
  let phase = $state<'loading' | 'name' | 'zip' | 'valid' | 'error'>('loading');
  let error = $state('');
  let name = $state('');
  let zip = $state('');
  let zipReason = $state<'duplicate' | 'new'>('new');
  let busy = $state(false);
  let validInfo = $state<{ person_name: string; valid_until: string | null } | null>(null);

  // svelte-ignore state_referenced_locally
  request('GET', `/api/public/p/${token}`)
    .then((i) => {
      info = i;
      phase = 'name';
    })
    .catch((e) => {
      error = e.message;
      phase = 'error';
    });

  async function identify(ev: SubmitEvent) {
    ev.preventDefault();
    busy = true;
    error = '';
    try {
      const r = await request('POST', `/api/public/p/${token}/identify`, { name, zip: phase === 'zip' ? zip : undefined });
      if (r.result === 'need_zip') {
        zipReason = r.reason;
        phase = 'zip';
      } else if (r.result === 'valid') {
        validInfo = r;
        phase = 'valid';
      } else if (r.result === 'sign') {
        sessionStorage.setItem(`pdfsign_session_${r.token}`, r.session);
        sessionStorage.setItem(`pdfsign_return_${r.token}`, location.pathname);
        navigate(`/s/${r.token}`);
      }
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }

  function reset() {
    name = '';
    zip = '';
    error = '';
    validInfo = null;
    phase = 'name';
  }
</script>

<div class="wrap">
  <div class="card">
    {#if phase === 'loading'}
      <p class="muted">Loading…</p>
    {:else if phase === 'error'}
      <h2>Link not available</h2>
      <p class="error">{error}</p>
    {:else if info}
      <div class="muted small">{info.label}</div>
      <h1>{info.template_name}</h1>
      {#if info.description}<p class="muted">{info.description}</p>{/if}

      {#if phase === 'valid' && validInfo}
        <div class="big-ok">✓</div>
        <h2 style="text-align:center">You're all set, {validInfo.person_name}!</h2>
        <p style="text-align:center">You already have a valid signature for this document{validInfo.valid_until ? ` until ${fmtDate(validInfo.valid_until)}` : ''}. Nothing else to do.</p>
        <button class="primary" style="width:100%" onclick={reset}>Next person</button>
      {:else}
        <form onsubmit={identify}>
          <div class="field">
            <label for="nm">Your full name</label>
            <!-- svelte-ignore a11y_autofocus -->
            <input id="nm" bind:value={name} autocomplete="name" placeholder="First and last name" required minlength="3" disabled={phase === 'zip'} autofocus />
          </div>
          {#if phase === 'zip'}
            <div class="field">
              <label for="zip">Zip code</label>
              <!-- svelte-ignore a11y_autofocus -->
              <input id="zip" bind:value={zip} autocomplete="postal-code" inputmode="text" required minlength="3" maxlength="10" autofocus />
              <p class="small muted" style="margin-top:4px">
                {zipReason === 'duplicate'
                  ? 'More than one person with this name is registered. Your zip code tells us which one you are.'
                  : "You're signing for the first time. Your zip code helps us tell people with the same name apart."}
              </p>
            </div>
          {/if}
          {#if error}<div class="error">{error}</div>{/if}
          <div class="row">
            {#if phase === 'zip'}<button type="button" onclick={reset}>Back</button>{/if}
            <button class="primary grow" type="submit" disabled={busy}>{busy ? 'Checking…' : 'Continue'}</button>
          </div>
        </form>
      {/if}
    {/if}
  </div>
</div>

<style>
  .wrap { display: flex; justify-content: center; padding: 48px 16px; }
  .card { width: min(460px, 100%); }
  .big-ok { font-size: 64px; color: var(--ok); text-align: center; line-height: 1; margin: 16px 0 8px; }
</style>
