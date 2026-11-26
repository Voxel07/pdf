<script lang="ts">
  import { request, ApiError, downloadBlob, type Field, type Section, fmtDate, fmtDateTime } from '../lib/api';
  import { loadPdf, type PdfDoc } from '../lib/pdf';
  import PdfPreview from '../lib/PdfPreview.svelte';
  import PdfViewer from '../lib/PdfViewer.svelte';
  import SignaturePad from '../lib/SignaturePad.svelte';
  import { navigate } from '../lib/router.svelte';
  import { signerPrefill } from '../lib/signer-prefill';

  let { token }: { token: string } = $props();

  interface FormData {
    template: { name: string; description: string; page_count: number; fields: Field[]; sections: Section[] };
    person_name: string;
    status: string;
    completed_at: string | null;
    valid_until: string | null;
    values: Record<string, { value: string; display?: string | null; image?: string | null; round_id: number }>;
    sections: Record<string, { done: boolean; at: string | null }>;
  }

  const base = $derived(`/api/public/s/${token}`);
  const sessionKey = $derived(`pdfsign_session_${token}`);

  let phase = $state<'loading' | 'gate' | 'form' | 'error'>('loading');
  let error = $state('');
  let templateName = $state('');
  let nameInput = $state('');
  let unlocking = $state(false);
  let session = $state(sessionStorage.getItem(`pdfsign_session_${location.pathname.split('/')[2]}`) ?? '');
  let form = $state<FormData | null>(null);
  let doc = $state<PdfDoc | null>(null);
  let input = $state<Record<string, string>>({});
  let active = $state<Record<number, boolean>>({});
  let showOptional = $state(3);
  let padFor = $state<Field | null>(null);
  let submitting = $state(false);
  let submitError = $state('');
  let justSubmitted = $state(false);

  const today = new Date().toISOString().slice(0, 10);
  // set when the document was opened through a general (QR) link
  const returnTo = sessionStorage.getItem(`pdfsign_return_${location.pathname.split('/')[2]}`);

  function nextPerson() {
    sessionStorage.removeItem(sessionKey);
    sessionStorage.removeItem(`pdfsign_zip_${token}`);
    sessionStorage.removeItem(`pdfsign_return_${token}`);
    navigate(returnTo ?? '/');
  }

  async function init() {
    try {
      const info = await request('GET', base);
      templateName = info.template_name;
      if (info.revoked) throw new ApiError(410, 'revoked', 'This link has been revoked.');
      if (info.locked) throw new ApiError(423, 'locked', 'This link is locked after too many wrong attempts. Please ask for a new link.');
      if (session) {
        try {
          await loadForm(await request('GET', `${base}/form`, undefined, { 'x-session': session }));
          return;
        } catch (e) {
          if (!(e instanceof ApiError && e.status === 401)) throw e;
          sessionStorage.removeItem(sessionKey);
          session = '';
        }
      }
      phase = 'gate';
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
      phase = 'error';
    }
  }

  async function unlock(ev: SubmitEvent) {
    ev.preventDefault();
    unlocking = true;
    error = '';
    try {
      const r = await request('POST', `${base}/unlock`, { name: nameInput });
      session = r.session;
      sessionStorage.setItem(sessionKey, session);
      await loadForm(r.form);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
      if (e instanceof ApiError && e.status === 423) phase = 'error';
    } finally {
      unlocking = false;
    }
  }

  async function loadForm(f: FormData) {
    form = f;
    const act: Record<number, boolean> = {};
    for (const s of f.template.sections) {
      if (!f.sections[s.id]?.done && s.required) act[s.id] = true;
    }
    // if all required sections are done, pre-select the first open optional one
    if (Object.keys(act).length === 0) {
      const first = f.template.sections.find((s) => !f.sections[s.id]?.done);
      if (first) act[first.id] = true;
    }
    active = act;
    const editable = f.template.fields.filter((field) => !f.values[field.id] && !f.sections[field.section]?.done);
    input = signerPrefill(editable, f.person_name, sessionStorage.getItem(`pdfsign_zip_${token}`) ?? '');
    if (!doc) doc = await loadPdf(`${base}/template.pdf`, { 'x-session': session });
    phase = 'form';
  }

  const sections = $derived(form?.template.sections ?? []);
  const doneSections = $derived(sections.filter((s) => form?.sections[s.id]?.done));
  const openSections = $derived(sections.filter((s) => !form?.sections[s.id]?.done));
  const openRequired = $derived(openSections.filter((s) => s.required));
  const openOptional = $derived(openSections.filter((s) => !s.required));
  const visibleOptional = $derived(openOptional.slice(0, Math.max(showOptional, openOptional.findLastIndex((s) => active[s.id]) + 1)));
  const fieldsOf = (sid: number) => (form?.template.fields ?? []).filter((f) => f.section === sid);
  const isEditable = (f: Field) => !!active[f.section] && !form?.sections[f.section]?.done && !f.autofill;

  function autofillPreview(f: Field) {
    if (f.autofill === 'person_name') return form?.person_name ?? '';
    if (f.autofill === 'today') return fmtDate(today);
    if (f.autofill === 'discord_id') return 'Discord ID';
    return '';
  }

  function displayDate(iso: string, fmt?: string) {
    const [y, m, d] = iso.split('-');
    if (!y || !m || !d) return iso;
    return (fmt ?? 'DD.MM.YYYY').replace('YYYY', y).replace('MM', m).replace('DD', d);
  }

  function focusField(f: Field) {
    if (!isEditable(f)) {
      if (!form?.sections[f.section]?.done && !f.autofill) {
        active[f.section] = true;
      } else return;
    }
    if (f.type === 'signature') {
      padFor = f;
      return;
    }
    setTimeout(() => {
      const el = document.getElementById(`in-${f.id}`) as HTMLInputElement | null;
      el?.scrollIntoView({ behavior: 'smooth', block: 'center' });
      el?.focus();
    }, 30);
  }

  function missingRequired(): Field[] {
    const out: Field[] = [];
    for (const s of sections) {
      if (!active[s.id] || form?.sections[s.id]?.done) continue;
      for (const f of fieldsOf(s.id)) if (f.required && !f.autofill && !input[f.id]?.trim()) out.push(f);
    }
    return out;
  }

  const activeCount = $derived(openSections.filter((s) => active[s.id]).length);

  async function submit() {
    submitError = '';
    const missing = missingRequired();
    if (missing.length) {
      submitError = `Please fill in: ${missing.map((f) => f.name).join(', ')}`;
      focusField(missing[0]);
      return;
    }
    const values: Record<string, string> = {};
    for (const [k, v] of Object.entries(input)) {
      const f = form!.template.fields.find((x) => x.id === k);
      if (f && active[f.section] && v?.trim()) values[k] = v;
    }
    if (Object.keys(values).length === 0) {
      submitError = 'Please fill in at least one field.';
      return;
    }
    submitting = true;
    try {
      const f = await request('POST', `${base}/submit`, { values }, { 'x-session': session });
      await loadForm(f);
      justSubmitted = true;
      window.scrollTo({ top: 0, behavior: 'smooth' });
    } catch (e) {
      if (e instanceof ApiError && e.status === 401) {
        sessionStorage.removeItem(sessionKey);
        session = '';
        phase = 'gate';
        error = 'Your session expired. Please enter your name again; your entries are kept.';
      } else submitError = e instanceof Error ? e.message : String(e);
    } finally {
      submitting = false;
    }
  }

  function boxStyle(f: Field) {
    return `left:${f.x * 100}%;top:${f.y * 100}%;width:${f.w * 100}%;height:${f.h * 100}%`;
  }

  init();

  /** The session goes in a header, never in a URL (history, logs, referrers). */
  async function fetchSigned(): Promise<{ blob: Blob; filename: string }> {
    const res = await fetch(`${base}/signed.pdf`, { headers: { 'x-session': session } });
    if (!res.ok) throw new Error(res.status === 401 ? 'Your session expired. Please enter your name again.' : `Download failed (${res.status})`);
    const m = /filename="([^"]+)"/.exec(res.headers.get('content-disposition') ?? '');
    return { blob: await res.blob(), filename: m?.[1] ?? 'signed.pdf' };
  }

  async function downloadSigned() {
    error = '';
    try {
      const f = await fetchSigned();
      await downloadBlob(f.blob, f.filename);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  let previewing = $state(false);
</script>

<div class="sign-page">
  <header>
    <div class="brand">✍️ {templateName || 'Document'}</div>
  </header>

  {#if phase === 'loading'}
    <div class="center muted">Loading…</div>
  {:else if phase === 'error'}
    <div class="center"><div class="card narrow"><h2>Cannot open document</h2><p class="error">{error}</p></div></div>
  {:else if phase === 'gate'}
    <div class="center">
      <form class="card narrow" onsubmit={unlock}>
        <h2>Confirm your identity</h2>
        <p class="muted">To open <b>{templateName}</b>, please enter your full name exactly as you registered it.</p>
        <div class="field">
          <label for="nm">Full name</label>
          <!-- svelte-ignore a11y_autofocus -->
          <input id="nm" bind:value={nameInput} autocomplete="name" autofocus required minlength="3" />
        </div>
        {#if error}<div class="error">{error}</div>{/if}
        <button class="primary" type="submit" disabled={unlocking || nameInput.trim().length < 3}>{unlocking ? 'Checking…' : 'Continue'}</button>
      </form>
    </div>
  {:else if form && doc}
    <div class="layout">
      <main>
        <PdfViewer {doc}>
          {#snippet overlay(page)}
            {#each form!.template.fields.filter((f) => f.page === page.index) as f (f.id)}
              {@const stored = form!.values[f.id]}
              {@const fs = ((f.font_size ?? 10) / page.widthPt) * 100}
              <button
                type="button"
                class="box"
                class:filled={!!stored}
                class:editable={isEditable(f)}
                class:openable={!stored && !f.autofill && !isEditable(f)}
                class:required={f.required}
                style={boxStyle(f)}
                style:font-size="{fs}cqw"
                title={f.name}
                onclick={() => focusField(f)}
              >
                {#if stored}
                  {#if stored.image}<img src={stored.image} alt="signature" />{:else}<span>{stored.display ?? stored.value}</span>{/if}
                {:else if f.autofill && active[f.section]}
                  <span class="auto">{autofillPreview(f)}</span>
                {:else if isEditable(f) && input[f.id]}
                  {#if f.type === 'signature'}<img src={input[f.id]} alt="signature" />
                  {:else if f.type === 'date'}<span>{displayDate(input[f.id], f.format)}</span>
                  {:else}<span>{input[f.id]}</span>{/if}
                {:else if isEditable(f)}
                  <span class="ph">{f.type === 'signature' ? '✍ Sign' : f.name}</span>
                {/if}
              </button>
            {/each}
          {/snippet}
        </PdfViewer>
      </main>

      <aside>
        <div class="card">
          <h2>{form.template.name}</h2>
          {#if form.template.description}<p class="muted">{form.template.description}</p>{/if}
          <div class="row small">
            <span class="badge {form.status}">{form.status}</span>
            {#if form.completed_at}<span class="muted">signed {fmtDate(form.completed_at)}</span>{/if}
            {#if form.valid_until}<span class="muted">· valid until {fmtDate(form.valid_until)}</span>{/if}
          </div>
          {#if justSubmitted}
            <div class="success">Thank you, {form.person_name}! Your entries were saved.</div>
          {/if}
          {#if returnTo && form.status !== 'pending'}
            <button class="primary" style="width:100%;margin-top:8px" onclick={nextPerson}>Done — next person</button>
          {/if}
          {#if form.status !== 'pending'}
            <div class="row" style="margin-top:8px">
              <button class="small" onclick={() => (previewing = true)}>👁 View signed PDF</button>
              <button class="small" onclick={downloadSigned}>⬇ Download</button>
            </div>
          {/if}
        </div>

        {#if doneSections.length}
          <div class="card">
            <h3>Completed</h3>
            {#each doneSections as s (s.id)}
              <div class="sec done">✓ {s.name} <span class="muted small">{fmtDateTime(form.sections[s.id]?.at)}</span></div>
            {/each}
          </div>
        {/if}

        {#if form.status === 'expired'}
          <div class="card"><p class="error">This document has expired. Please request a new link.</p></div>
        {:else if openSections.length}
          {#each [...openRequired, ...visibleOptional] as s (s.id)}
            <div class="card sec-card" class:inactive={!active[s.id]}>
              <div class="row">
                <h3 class="grow" style="margin:0">{s.name}</h3>
                {#if s.required}
                  <span class="badge pending">required</span>
                {:else}
                  <label class="inline"><input type="checkbox" bind:checked={active[s.id]} /> fill now</label>
                {/if}
              </div>
              {#if active[s.id]}
                <div class="fields">
                  {#each fieldsOf(s.id) as f (f.id)}
                    <div class="field">
                      <label for="in-{f.id}">{f.name}{#if f.required && !f.autofill}<span class="req"> *</span>{/if}</label>
                      {#if f.autofill}
                        <div class="auto-val">{autofillPreview(f)} <span class="muted small">(filled automatically)</span></div>
                      {:else if f.type === 'signature'}
                        {#if input[f.id]}
                          <div class="sig-preview"><img src={input[f.id]} alt="signature" /><button class="small" onclick={() => (padFor = f)}>Redo</button><button class="small ghost" onclick={() => delete input[f.id]}>Remove</button></div>
                        {:else}
                          <button id="in-{f.id}" class="sign-btn" onclick={() => (padFor = f)}>✍ Click to sign</button>
                        {/if}
                      {:else if f.type === 'date'}
                        <div class="row"><input id="in-{f.id}" class="grow" type="date" bind:value={input[f.id]} style="flex:1;width:auto" /><button class="small" onclick={() => (input[f.id] = today)}>Today</button></div>
                      {:else if f.type === 'number'}
                        <input id="in-{f.id}" type="number" step="any" min={f.min} max={f.max} placeholder={f.placeholder} value={input[f.id] ?? ''} oninput={(e) => (input[f.id] = e.currentTarget.value)} />
                      {:else if f.type === 'select'}
                        <select id="in-{f.id}" bind:value={input[f.id]}>
                          <option value="">— select —</option>
                          {#each f.options ?? [] as o}<option value={o}>{o}</option>{/each}
                        </select>
                      {:else}
                        <input id="in-{f.id}" type="text" placeholder={f.placeholder} bind:value={input[f.id]} />
                      {/if}
                    </div>
                  {/each}
                </div>
              {/if}
            </div>
          {/each}
          {#if openOptional.length > visibleOptional.length}
            <button class="ghost" onclick={() => (showOptional += 3)}>+ Show more sections ({openOptional.length - visibleOptional.length} left)</button>
          {/if}
          <div class="card submit-card">
            {#if submitError}<div class="error">{submitError}</div>{/if}
            <p class="small muted">By submitting, you confirm the entries above and agree that your hand-drawn signature is legally binding. Submitted sections cannot be changed afterwards.</p>
            <button class="primary" style="width:100%" onclick={submit} disabled={submitting || activeCount === 0}>
              {submitting ? 'Submitting…' : form.status === 'pending' ? 'Sign & submit' : 'Submit additions'}
            </button>
          </div>
        {:else}
          <div class="card"><p class="muted">All sections of this document are complete.</p></div>
        {/if}
      </aside>
    </div>
  {/if}
</div>

{#if padFor}
  <SignaturePad
    title={padFor.name}
    oncancel={() => (padFor = null)}
    onsave={(d) => {
      input[padFor!.id] = d;
      padFor = null;
    }}
  />
{/if}


{#if previewing}
  <PdfPreview title="Signed document — {templateName || 'Document'}" load={fetchSigned} onclose={() => (previewing = false)} />
{/if}

<style>
  .sign-page { min-height: 100vh; }
  header { background: var(--panel); border-bottom: 1px solid var(--border); padding: 12px 20px; }
  .brand { font-weight: 600; }
  .center { display: flex; justify-content: center; padding: 60px 16px; }
  .narrow { width: min(440px, 100%); }
  .layout { display: grid; grid-template-columns: minmax(0, 1fr) 380px; gap: 20px; padding: 20px; max-width: 1400px; margin: 0 auto; align-items: start; }
  aside { position: sticky; top: 16px; max-height: calc(100vh - 32px); overflow: auto; padding-bottom: 8px; }
  @media (max-width: 900px) {
    .layout { grid-template-columns: 1fr; padding: 10px; }
    aside { position: static; max-height: none; }
  }
  .box {
    position: absolute; padding: 0 2px; border: 0; border-radius: 2px; background: transparent;
    display: flex; align-items: center; overflow: hidden; color: #111; line-height: 1.1;
    font-family: Helvetica, Arial, sans-serif; font-weight: 400; cursor: default; justify-content: flex-start;
  }
  .box:hover { background: transparent; }
  .box.editable { background: rgba(47, 91, 211, 0.12); outline: 1.5px solid rgba(47, 91, 211, 0.7); cursor: pointer; }
  .box.editable.required { outline-color: rgba(214, 120, 0, 0.85); background: rgba(255, 170, 0, 0.12); }
  .box.openable { background: rgba(120, 130, 150, 0.08); outline: 1px dashed rgba(120, 130, 150, 0.5); cursor: pointer; }
  .box img { max-width: 100%; max-height: 100%; margin: auto; display: block; }
  .box span { white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .box .ph { color: rgba(47, 91, 211, 0.85); font-size: 0.8em; }
  .box .auto { color: #444; }
  .sec { padding: 4px 0; }
  .sec.done { color: var(--ok); }
  .sec-card.inactive { opacity: 0.8; }
  .fields { margin-top: 12px; }
  .req { color: var(--warn); }
  .sign-btn { width: 100%; height: 64px; border-style: dashed; color: var(--accent-text); }
  .sig-preview { display: flex; align-items: center; gap: 8px; border: 1px solid var(--border); border-radius: var(--radius); padding: 6px; }
  .sig-preview img { height: 48px; flex: 1; object-fit: contain; object-position: left; }
  .auto-val { padding: 6px 0; }
  .submit-card { position: sticky; bottom: 0; }
</style>
