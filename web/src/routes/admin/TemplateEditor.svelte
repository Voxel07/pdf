<script lang="ts">
  import { admin, openBlob, randomId, type Field, type FieldKind, type Section, type Template } from '../../lib/api';
  import { loadPdf, type PdfDoc } from '../../lib/pdf';
  import { navigate } from '../../lib/router.svelte';
  import PdfViewer from '../../lib/PdfViewer.svelte';
  import GeneralLinks from './GeneralLinks.svelte';

  let { id }: { id: string } = $props();

  let t = $state<Template | null>(null);
  let doc = $state<PdfDoc | null>(null);
  let selected = $state<string | null>(null);
  let tool = $state<FieldKind>('text');
  let currentSection = $state(0);
  let dirty = $state(false);
  let saving = $state(false);
  let msg = $state('');
  let error = $state('');
  let repeatCount = $state<Record<number, number>>({});
  let repeatOffset = $state<Record<number, number>>({});

  const KINDS: { kind: FieldKind; label: string; icon: string; w: number; h: number }[] = [
    { kind: 'text', label: 'Text', icon: 'T', w: 0.25, h: 0.025 },
    { kind: 'date', label: 'Date', icon: '📅', w: 0.15, h: 0.025 },
    { kind: 'number', label: 'Number', icon: '#', w: 0.1, h: 0.025 },
    { kind: 'select', label: 'Select', icon: '▾', w: 0.2, h: 0.025 },
    { kind: 'signature', label: 'Signature', icon: '✍', w: 0.22, h: 0.06 },
  ];
  const kindInfo = (k: FieldKind) => KINDS.find((x) => x.kind === k)!;

  async function load() {
    t = await admin.get<Template>(`/templates/${id}`);
    currentSection = t.sections[0]?.id ?? 0;
    doc = await loadPdf(`/api/admin/templates/${id}/pdf`, admin.headers());
  }
  load().catch((e) => (error = String(e)));

  const sel = $derived(t?.fields.find((f) => f.id === selected) ?? null);

  function touch() {
    dirty = true;
    msg = '';
  }

  function uniqueName(base: string, except?: string) {
    const names = new Set(t!.fields.filter((f) => f.id !== except).map((f) => f.name.toLowerCase()));
    if (!names.has(base.toLowerCase())) return base;
    const m = /^(.*?)(\d+)$/.exec(base);
    let stem = m ? m[1] : base + ' ';
    let n = m ? parseInt(m[2]) : 1;
    while (names.has(`${stem}${n}`.toLowerCase())) n++;
    return `${stem}${n}`;
  }

  // -------------------------------------------------------------------------------------
  // Drag & drop

  type Drag = { mode: 'create' | 'move' | 'resize'; fid: string; el: HTMLElement; sx: number; sy: number; orig: { x: number; y: number; w: number; h: number } };
  let drag: Drag | null = null;

  function rel(e: PointerEvent, el: HTMLElement) {
    const r = el.getBoundingClientRect();
    return { x: (e.clientX - r.left) / r.width, y: (e.clientY - r.top) / r.height };
  }
  const clamp = (v: number, lo = 0, hi = 1) => Math.max(lo, Math.min(hi, v));

  function layerDown(e: PointerEvent, page: number) {
    if (e.button !== 0 || !t) return;
    const layer = e.currentTarget as HTMLElement;
    const p = rel(e, layer);
    const info = kindInfo(tool);
    const f: Field = {
      id: randomId(),
      name: uniqueName(`${info.label} 1`),
      type: tool,
      required: true,
      section: currentSection,
      page,
      x: p.x,
      y: p.y,
      w: 0,
      h: 0,
      ...(tool === 'select' ? { options: ['Option A', 'Option B'] } : {}),
    };
    t.fields.push(f);
    selected = f.id;
    drag = { mode: 'create', fid: f.id, el: layer, sx: p.x, sy: p.y, orig: { x: p.x, y: p.y, w: 0, h: 0 } };
    layer.setPointerCapture(e.pointerId);
    touch();
  }

  function fieldDown(e: PointerEvent, f: Field, mode: 'move' | 'resize') {
    if (e.button !== 0) return;
    e.stopPropagation();
    const layer = (e.currentTarget as HTMLElement).closest('.layer') as HTMLElement;
    const p = rel(e, layer);
    selected = f.id;
    drag = { mode, fid: f.id, el: layer, sx: p.x, sy: p.y, orig: { x: f.x, y: f.y, w: f.w, h: f.h } };
    layer.setPointerCapture(e.pointerId);
  }

  function layerMove(e: PointerEvent) {
    if (!drag || !t) return;
    const f = t.fields.find((x) => x.id === drag!.fid);
    if (!f) return;
    const p = rel(e, drag.el);
    const dx = p.x - drag.sx, dy = p.y - drag.sy;
    if (drag.mode === 'create') {
      f.x = clamp(Math.min(drag.sx, p.x));
      f.y = clamp(Math.min(drag.sy, p.y));
      f.w = Math.abs(dx);
      f.h = Math.abs(dy);
    } else if (drag.mode === 'move') {
      f.x = clamp(drag.orig.x + dx, 0, 1 - f.w);
      f.y = clamp(drag.orig.y + dy, 0, 1 - f.h);
      if (dx !== 0 || dy !== 0) touch();
    } else {
      f.w = clamp(drag.orig.w + dx, 0.01, 1 - f.x);
      f.h = clamp(drag.orig.h + dy, 0.008, 1 - f.y);
      touch();
    }
  }

  function layerUp() {
    if (drag?.mode === 'create' && t) {
      const f = t.fields.find((x) => x.id === drag!.fid);
      if (f && (f.w < 0.01 || f.h < 0.006)) {
        const info = kindInfo(f.type);
        f.w = Math.min(info.w, 1 - f.x);
        f.h = Math.min(info.h, 1 - f.y);
        f.y = Math.max(0, f.y - f.h / 2);
      }
    }
    drag = null;
  }

  // -------------------------------------------------------------------------------------
  // Field / section operations

  function removeField(fid: string) {
    if (!t) return;
    t.fields = t.fields.filter((f) => f.id !== fid);
    if (selected === fid) selected = null;
    touch();
  }

  function duplicateField(f: Field) {
    const c: Field = { ...$state.snapshot(f), id: randomId(), name: uniqueName(f.name), y: clamp(f.y + f.h + 0.005, 0, 1 - f.h) };
    t!.fields.push(c);
    selected = c.id;
    touch();
  }

  function addSection() {
    const nid = Math.max(-1, ...t!.sections.map((s) => s.id)) + 1;
    t!.sections.push({ id: nid, name: `Section ${nid + 1}`, required: false });
    currentSection = nid;
    touch();
  }

  function removeSection(s: Section) {
    const n = t!.fields.filter((f) => f.section === s.id).length;
    if (t!.sections.length <= 1) return alert('A template needs at least one section.');
    if (n && !confirm(`Delete section "${s.name}" and its ${n} field(s)?`)) return;
    t!.fields = t!.fields.filter((f) => f.section !== s.id);
    t!.sections = t!.sections.filter((x) => x.id !== s.id);
    if (currentSection === s.id) currentSection = t!.sections[0].id;
    touch();
  }

  function bump(name: string, by: number) {
    const m = /^(.*?)(\d+)(\D*)$/.exec(name);
    return m ? `${m[1]}${parseInt(m[2]) + by}${m[3]}` : `${name} ${by + 1}`;
  }

  function sectionHeight(sid: number) {
    const fs = t!.fields.filter((f) => f.section === sid);
    if (!fs.length) return 0;
    return Math.max(...fs.map((f) => f.y + f.h)) - Math.min(...fs.map((f) => f.y));
  }

  /** Copies a section's fields n times, shifted down — e.g. rows of an equipment list. */
  function repeatSection(s: Section) {
    const fs = t!.fields.filter((f) => f.section === s.id);
    if (!fs.length) return alert('Add fields to this section first.');
    const n = repeatCount[s.id] ?? 1;
    const off = (repeatOffset[s.id] ?? +(sectionHeight(s.id) * 100 + 0.3).toFixed(2)) / 100;
    // only copies that fit on the page entirely are made; clamping them to the bottom edge
    // would stack rows on top of each other
    const fits = (i: number) => fs.every((f) => f.y + off * i + f.h <= 1 + 1e-6);
    let made = 0;
    while (made < n && fits(made + 1)) made++;
    let nextId = Math.max(...t!.sections.map((x) => x.id)) + 1;
    for (let i = 1; i <= made; i++) {
      const sid = nextId++;
      t!.sections.push({ id: sid, name: bump(s.name, i), required: false });
      for (const f of fs) {
        t!.fields.push({ ...$state.snapshot(f), id: randomId(), section: sid, y: clamp(f.y + off * i, 0, 1 - f.h), name: uniqueName(bump(f.name, i)) });
      }
    }
    if (made < n) alert(made ? `Only ${made} of ${n} copies fit on the page.` : 'No copy fits below this section on the page.');
    if (made) touch();
  }

  async function save() {
    if (!t) return;
    saving = true;
    error = '';
    try {
      t = await admin.put<Template>(`/templates/${id}`, {
        name: t.name,
        slug: t.slug,
        description: t.description,
        validity_days: t.validity_days || null,
        sections: t.sections,
        fields: t.fields,
      });
      dirty = false;
      msg = 'Saved';
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      saving = false;
    }
  }

  async function preview() {
    if (dirty) await save();
    if (!error) openBlob(await admin.blob(`/templates/${id}/preview.pdf`));
  }

  async function archive() {
    if (!confirm('Delete this template? (Templates with documents are archived instead.)')) return;
    await admin.del(`/templates/${id}`);
    navigate('/admin/templates');
  }

  function onKey(e: KeyboardEvent) {
    const tag = (e.target as HTMLElement).tagName;
    if ((e.ctrlKey || e.metaKey) && e.key === 's') {
      e.preventDefault();
      save();
      return;
    }
    if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT' || !sel) return;
    const step = e.shiftKey ? 0.01 : 0.002;
    if (e.key === 'Delete' || e.key === 'Backspace') removeField(sel.id);
    else if (e.key === 'ArrowLeft') sel.x = clamp(sel.x - step);
    else if (e.key === 'ArrowRight') sel.x = clamp(sel.x + step, 0, 1 - sel.w);
    else if (e.key === 'ArrowUp') sel.y = clamp(sel.y - step);
    else if (e.key === 'ArrowDown') sel.y = clamp(sel.y + step, 0, 1 - sel.h);
    else if ((e.ctrlKey || e.metaKey) && e.key === 'd') duplicateField(sel);
    else if (e.key === 'Escape') selected = null;
    else return;
    e.preventDefault();
    touch();
  }

  const sectionColor = (sid: number) => `hsl(${(sid * 67 + 215) % 360} 70% 45%)`;
</script>

<svelte:window onkeydown={onKey} onbeforeunload={(e) => { if (dirty) e.preventDefault(); }} />

{#if error && !t}<div class="error">{error}</div>{/if}
{#if t}
  <div class="toolbar card">
    <a href="/admin/templates" class="btn small">← Back</a>
    <b class="title">{t.name}</b>
    <span class="muted small">Draw:</span>
    {#each KINDS as k}
      <button class="small" class:on={tool === k.kind} onclick={() => (tool = k.kind)} title="Drag on the page to add a {k.label} field">{k.icon} {k.label}</button>
    {/each}
    <span class="muted small">into</span>
    <select bind:value={currentSection} style="width:auto">
      {#each t.sections as s}<option value={s.id}>{s.name}</option>{/each}
    </select>
    <span class="spacer"></span>
    {#if msg}<span class="badge completed">{msg}</span>{/if}
    {#if dirty}<span class="badge pending">unsaved</span>{/if}
    <button class="small" onclick={preview}>Preview PDF</button>
    <button class="primary small" onclick={save} disabled={saving}>{saving ? 'Saving…' : 'Save'}</button>
  </div>
  {#if error}<div class="error">{error}</div>{/if}

  <div class="editor">
    <div class="canvas-col">
      {#if doc}
        <PdfViewer {doc}>
          {#snippet overlay(page)}
            <div
              class="layer"
              role="presentation"
              onpointerdown={(e) => layerDown(e, page.index)}
              onpointermove={layerMove}
              onpointerup={layerUp}
              onpointercancel={layerUp}
            >
              {#each t!.fields.filter((f) => f.page === page.index) as f (f.id)}
                <div
                  class="efield"
                  class:selected={selected === f.id}
                  role="button"
                  tabindex="-1"
                  style="left:{f.x * 100}%;top:{f.y * 100}%;width:{f.w * 100}%;height:{f.h * 100}%;--c:{sectionColor(f.section)}"
                  onpointerdown={(e) => fieldDown(e, f, 'move')}
                >
                  <span class="lbl">{kindInfo(f.type).icon} {f.name}{f.required ? ' *' : ''}</span>
                  <div class="handle" role="presentation" onpointerdown={(e) => fieldDown(e, f, 'resize')}></div>
                </div>
              {/each}
            </div>
          {/snippet}
        </PdfViewer>
      {:else}
        <p class="muted">Loading PDF…</p>
      {/if}
    </div>

    <div class="side">
      {#if sel}
        <div class="card">
          <div class="row"><h3 class="grow" style="margin:0">Field</h3><button class="small ghost" onclick={() => (selected = null)}>✕</button></div>
          <div class="field" style="margin-top:10px"><label for="fn">Name (shown to signer, unique)</label><input id="fn" bind:value={sel.name} oninput={touch} /></div>
          <div class="grid2">
            <div class="field"><label for="ft">Type</label>
              <select id="ft" bind:value={sel.type} onchange={touch}>{#each KINDS as k}<option value={k.kind}>{k.label}</option>{/each}</select>
            </div>
            <div class="field"><label for="fs">Section</label>
              <select id="fs" bind:value={sel.section} onchange={touch}>{#each t.sections as s}<option value={s.id}>{s.name}</option>{/each}</select>
            </div>
          </div>
          <div class="row" style="margin-bottom:10px">
            <label class="inline"><input type="checkbox" bind:checked={sel.required} onchange={touch} /> required</label>
          </div>
          <div class="field"><label for="fa">Autofill (not editable by signer)</label>
            <select id="fa" value={sel.autofill ?? ''} onchange={(e) => { sel!.autofill = (e.target as HTMLSelectElement).value || undefined; touch(); }}>
              <option value="">— none —</option>
              <option value="person_name">Registered name</option>
              <option value="today">Date of signing</option>
              <option value="discord_id">Discord ID</option>
            </select>
          </div>
          {#if sel.type === 'select'}
            <div class="field"><label for="fo">Options (one per line)</label>
              <textarea id="fo" rows="6" value={(sel.options ?? []).join('\n')} oninput={(e) => { sel!.options = (e.target as HTMLTextAreaElement).value.split('\n').map((s) => s.trim()).filter(Boolean); touch(); }}></textarea>
            </div>
          {/if}
          {#if sel.type === 'date'}
            <div class="field"><label for="fd">Date format in PDF</label>
              <select id="fd" value={sel.format ?? 'DD.MM.YYYY'} onchange={(e) => { sel!.format = (e.target as HTMLSelectElement).value; touch(); }}>
                {#each ['DD.MM.YYYY', 'YYYY-MM-DD', 'MM/DD/YYYY', 'DD/MM/YYYY'] as f}<option>{f}</option>{/each}
              </select>
            </div>
          {/if}
          {#if sel.type === 'number'}
            <div class="grid2">
              <div class="field"><label for="fmin">Min</label><input id="fmin" type="number" value={sel.min ?? ''} oninput={(e) => { const v = (e.target as HTMLInputElement).value; sel!.min = v === '' ? undefined : +v; touch(); }} /></div>
              <div class="field"><label for="fmax">Max</label><input id="fmax" type="number" value={sel.max ?? ''} oninput={(e) => { const v = (e.target as HTMLInputElement).value; sel!.max = v === '' ? undefined : +v; touch(); }} /></div>
            </div>
          {/if}
          {#if sel.type !== 'signature'}
            <div class="grid2">
              <div class="field"><label for="ffs">Font size (pt, auto if empty)</label><input id="ffs" type="number" min="4" max="40" value={sel.font_size ?? ''} oninput={(e) => { const v = (e.target as HTMLInputElement).value; sel!.font_size = v === '' ? undefined : +v; touch(); }} /></div>
              <div class="field"><label for="fph">Placeholder</label><input id="fph" value={sel.placeholder ?? ''} oninput={(e) => { sel!.placeholder = (e.target as HTMLInputElement).value || undefined; touch(); }} /></div>
            </div>
          {/if}
          <div class="row">
            <button class="small" onclick={() => duplicateField(sel!)}>Duplicate</button>
            <button class="small danger" onclick={() => removeField(sel!.id)}>Delete</button>
          </div>
          <p class="small muted" style="margin-top:8px">Arrow keys move (Shift = faster), Del removes, Ctrl+D duplicates.</p>
        </div>
      {/if}

      <div class="card">
        <h3>Template</h3>
        <div class="field"><label for="tn">Name</label><input id="tn" bind:value={t.name} oninput={touch} /></div>
        <div class="field"><label for="tsl">Slug (used by the API / bot)</label><input id="tsl" bind:value={t.slug} oninput={touch} /></div>
        <div class="field"><label for="td">Description (shown to signer)</label><textarea id="td" rows="2" bind:value={t.description} oninput={touch}></textarea></div>
        <div class="field"><label for="tv">Valid for (days, empty = forever)</label>
          <input id="tv" type="number" min="1" value={t.validity_days ?? ''} oninput={(e) => { const v = (e.target as HTMLInputElement).value; t!.validity_days = v === '' ? null : +v; touch(); }} />
        </div>
        <button class="small danger" onclick={archive}>Delete / archive template</button>
      </div>

      <GeneralLinks templateId={t.id} templateName={t.name} />

      <div class="card">
        <div class="row"><h3 class="grow" style="margin:0">Sections</h3><button class="small" onclick={addSection}>+ Add</button></div>
        <p class="small muted">Each section is signed and locked on its own. Required sections must be done on the first signing; optional ones can be filled later by opening the same link again.</p>
        {#each t.sections as s (s.id)}
          <div class="section" style="--c:{sectionColor(s.id)}">
            <div class="row">
              <input class="grow" bind:value={s.name} oninput={touch} style="flex:1;width:auto" />
              <label class="inline small"><input type="checkbox" bind:checked={s.required} onchange={touch} /> req.</label>
              <button class="small ghost" title="Delete section" onclick={() => removeSection(s)}>🗑</button>
            </div>
            <div class="small muted">{t.fields.filter((f) => f.section === s.id).length} fields</div>
            <details>
              <summary class="small">Repeat below…</summary>
              <div class="row small" style="margin-top:6px">
                <input type="number" min="1" max="100" style="width:64px" value={repeatCount[s.id] ?? 1} oninput={(e) => (repeatCount[s.id] = +(e.target as HTMLInputElement).value)} /> copies,
                offset
                <input type="number" step="0.05" style="width:80px" value={repeatOffset[s.id] ?? +(sectionHeight(s.id) * 100 + 0.3).toFixed(2)} oninput={(e) => (repeatOffset[s.id] = +(e.target as HTMLInputElement).value)} /> % page
                <button class="small" onclick={() => repeatSection(s)}>Repeat</button>
              </div>
            </details>
          </div>
        {/each}
      </div>

      <div class="card">
        <h3>Fields ({t.fields.length})</h3>
        {#each t.sections as s (s.id)}
          {@const fs = t.fields.filter((f) => f.section === s.id)}
          {#if fs.length}
            <div class="small muted" style="margin-top:6px">{s.name}</div>
            {#each fs as f (f.id)}
              <button class="flist" class:on={selected === f.id} onclick={() => { selected = f.id; document.querySelector(`[data-page="${f.page}"]`)?.scrollIntoView({ behavior: 'smooth', block: 'start' }); }}>
                {kindInfo(f.type).icon} {f.name} <span class="muted small">p.{f.page + 1}</span>
              </button>
            {/each}
          {/if}
        {/each}
      </div>
    </div>
  </div>
{/if}

<style>
  .toolbar { display: flex; gap: 6px; align-items: center; flex-wrap: wrap; padding: 8px 12px; position: sticky; top: 52px; z-index: 10; margin-bottom: 12px; }
  .toolbar .title { margin: 0 10px; }
  .toolbar button.on { background: var(--accent-soft); border-color: var(--accent); color: var(--accent); }
  .editor { display: grid; grid-template-columns: minmax(0, 1fr) 360px; gap: 14px; align-items: start; }
  .canvas-col { max-width: 1000px; width: 100%; margin: 0 auto; }
  .side { position: sticky; top: 112px; max-height: calc(100vh - 124px); overflow: auto; }
  .side .card { padding: 14px; }
  .layer { position: absolute; inset: 0; cursor: crosshair; touch-action: none; }
  .efield {
    position: absolute; border: 1.5px solid var(--c); background: color-mix(in srgb, var(--c) 14%, transparent);
    cursor: move; border-radius: 2px; overflow: visible;
  }
  .efield.selected { outline: 2px solid var(--c); outline-offset: 1px; background: color-mix(in srgb, var(--c) 26%, transparent); z-index: 2; }
  .lbl { position: absolute; left: 2px; top: 50%; transform: translateY(-50%); font-size: 11px; color: var(--c); white-space: nowrap; pointer-events: none; font-weight: 600; text-shadow: 0 0 2px white, 0 0 2px white; overflow: hidden; max-width: calc(100% - 4px); }
  .handle { position: absolute; right: -5px; bottom: -5px; width: 10px; height: 10px; background: white; border: 2px solid var(--c); border-radius: 2px; cursor: nwse-resize; }
  .section { border-left: 3px solid var(--c); padding: 6px 0 6px 10px; margin: 8px 0; }
  .flist { display: block; width: 100%; text-align: left; border: 0; padding: 4px 6px; background: transparent; font-weight: 400; justify-content: flex-start; }
  .flist.on { background: var(--accent-soft); }
  @media (max-width: 1000px) { .editor { grid-template-columns: 1fr; } .side { position: static; max-height: none; } }
</style>
