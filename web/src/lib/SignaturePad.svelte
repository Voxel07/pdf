<script lang="ts">
  let { title = 'Sign here', onsave, oncancel }: { title?: string; onsave: (dataUrl: string) => void; oncancel: () => void } = $props();

  let canvas: HTMLCanvasElement;
  let ctx: CanvasRenderingContext2D;
  let empty = $state(true);
  let drawing = false;
  let last: { x: number; y: number; t: number } | null = null;
  let lastMid: { x: number; y: number } | null = null;
  let lineW = 2.5;
  const strokes: ImageData[] = [];

  $effect(() => {
    const dpr = Math.max(window.devicePixelRatio || 1, 2);
    const rect = canvas.getBoundingClientRect();
    canvas.width = rect.width * dpr;
    canvas.height = rect.height * dpr;
    ctx = canvas.getContext('2d')!;
    ctx.scale(dpr, dpr);
    ctx.lineCap = 'round';
    ctx.lineJoin = 'round';
    ctx.strokeStyle = '#14215a';
    ctx.fillStyle = '#14215a';
  });

  function pos(e: PointerEvent) {
    const r = canvas.getBoundingClientRect();
    return { x: e.clientX - r.left, y: e.clientY - r.top, t: performance.now() };
  }

  function down(e: PointerEvent) {
    e.preventDefault();
    canvas.setPointerCapture(e.pointerId);
    strokes.push(ctx.getImageData(0, 0, canvas.width, canvas.height));
    drawing = true;
    last = pos(e);
    lastMid = { x: last.x, y: last.y };
    lineW = 2.6;
    ctx.beginPath();
    ctx.arc(last.x, last.y, 1.3, 0, Math.PI * 2);
    ctx.fill();
    empty = false;
  }

  function move(e: PointerEvent) {
    if (!drawing || !last) return;
    const events = e.getCoalescedEvents?.() ?? [e];
    for (const ev of events) {
      const p = pos(ev);
      const dist = Math.hypot(p.x - last.x, p.y - last.y);
      if (dist < 0.8) continue;
      const v = dist / Math.max(p.t - last.t, 1);
      const pressure = ev.pointerType === 'pen' && ev.pressure > 0 ? ev.pressure : 0.5;
      const target = Math.max(1.2, Math.min(3.6, 3.6 - v * 1.1)) * (0.6 + pressure * 0.8);
      lineW = lineW * 0.7 + target * 0.3;
      const mid = { x: (last.x + p.x) / 2, y: (last.y + p.y) / 2 };
      ctx.lineWidth = lineW;
      ctx.beginPath();
      ctx.moveTo(lastMid!.x, lastMid!.y);
      ctx.quadraticCurveTo(last.x, last.y, mid.x, mid.y);
      ctx.stroke();
      lastMid = mid;
      last = p;
    }
  }

  function up() {
    drawing = false;
    last = null;
  }

  function clear() {
    ctx.clearRect(0, 0, canvas.width, canvas.height);
    strokes.length = 0;
    empty = true;
  }

  function undo() {
    const prev = strokes.pop();
    if (prev) ctx.putImageData(prev, 0, 0);
    if (strokes.length === 0) clear();
  }

  /** Exports the drawing trimmed to its bounding box. */
  function save() {
    const { width, height } = canvas;
    const data = ctx.getImageData(0, 0, width, height).data;
    let minX = width, minY = height, maxX = -1, maxY = -1;
    for (let y = 0; y < height; y++) {
      for (let x = 0; x < width; x++) {
        if (data[(y * width + x) * 4 + 3] > 8) {
          if (x < minX) minX = x;
          if (x > maxX) maxX = x;
          if (y < minY) minY = y;
          if (y > maxY) maxY = y;
        }
      }
    }
    if (maxX < 0) return;
    const pad = 6;
    minX = Math.max(0, minX - pad); minY = Math.max(0, minY - pad);
    maxX = Math.min(width - 1, maxX + pad); maxY = Math.min(height - 1, maxY + pad);
    const w = maxX - minX + 1, h = maxY - minY + 1;
    const scale = Math.min(1, 900 / w);
    const out = document.createElement('canvas');
    out.width = Math.round(w * scale);
    out.height = Math.round(h * scale);
    out.getContext('2d')!.drawImage(canvas, minX, minY, w, h, 0, 0, out.width, out.height);
    onsave(out.toDataURL('image/png'));
  }
</script>

<div class="modal-backdrop" role="presentation" onclick={(e) => e.target === e.currentTarget && oncancel()}>
  <div class="modal" role="dialog" aria-label={title}>
    <div class="row">
      <h3 class="grow" style="margin:0">{title}</h3>
      <button class="ghost small" onclick={oncancel} aria-label="Close">✕</button>
    </div>
    <p class="muted small">Draw your signature with your finger, pen or mouse.</p>
    <div class="pad">
      <canvas
        bind:this={canvas}
        onpointerdown={down}
        onpointermove={move}
        onpointerup={up}
        onpointercancel={up}
      ></canvas>
      <div class="line"></div>
      {#if empty}<div class="hint">Sign here</div>{/if}
    </div>
    <div class="row" style="margin-top:12px">
      <button onclick={undo} disabled={empty}>Undo</button>
      <button onclick={clear} disabled={empty}>Clear</button>
      <span class="spacer"></span>
      <button onclick={oncancel}>Cancel</button>
      <button class="primary" onclick={save} disabled={empty}>Apply signature</button>
    </div>
  </div>
</div>

<style>
  .pad { position: relative; border: 1px dashed #b8c0cf; border-radius: 10px; background: #fcfcfd; }
  canvas { display: block; width: 100%; height: min(240px, 45vw); touch-action: none; cursor: crosshair; }
  .line { position: absolute; left: 6%; right: 6%; bottom: 26%; border-bottom: 1px solid #c7cdd8; pointer-events: none; }
  .hint { position: absolute; left: 7%; bottom: 28%; color: #b0b7c5; pointer-events: none; font-size: 0.9rem; }
</style>
