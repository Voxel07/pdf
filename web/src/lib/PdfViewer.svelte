<script lang="ts">
  import type { Snippet } from 'svelte';
  import type { PdfDoc } from './pdf';

  interface PageInfo {
    index: number;
    widthPt: number;
    heightPt: number;
  }

  let { doc, overlay }: { doc: PdfDoc; overlay?: Snippet<[PageInfo]> } = $props();

  let pages = $state<PageInfo[]>([]);
  let canvases: HTMLCanvasElement[] = $state([]);
  let container: HTMLDivElement;
  let width = $state(0);

  $effect(() => {
    const d = doc;
    (async () => {
      const list: PageInfo[] = [];
      for (let i = 0; i < d.numPages; i++) {
        const p = await d.getPage(i + 1);
        const vp = p.getViewport({ scale: 1 });
        list.push({ index: i, widthPt: vp.width, heightPt: vp.height });
      }
      pages = list;
    })();
  });

  $effect(() => {
    const ro = new ResizeObserver((entries) => {
      const w = Math.round(entries[0].contentRect.width);
      if (Math.abs(w - width) > 4) width = w;
    });
    ro.observe(container);
    return () => ro.disconnect();
  });

  let renderGen = 0;
  $effect(() => {
    if (!width || pages.length === 0) return;
    const gen = ++renderGen;
    const t = setTimeout(async () => {
      const dpr = Math.min(window.devicePixelRatio || 1, 2.5);
      for (const info of pages) {
        if (gen !== renderGen) return;
        const canvas = canvases[info.index];
        if (!canvas) continue;
        const page = await doc.getPage(info.index + 1);
        const scale = (width / info.widthPt) * dpr;
        const vp = page.getViewport({ scale });
        canvas.width = Math.floor(vp.width);
        canvas.height = Math.floor(vp.height);
        await page.render({ canvas, viewport: vp }).promise.catch(() => {});
      }
    }, 80);
    return () => clearTimeout(t);
  });
</script>

<div class="viewer" bind:this={container}>
  {#each pages as info (info.index)}
    <div class="page" style:aspect-ratio="{info.widthPt} / {info.heightPt}" data-page={info.index}>
      <canvas bind:this={canvases[info.index]}></canvas>
      <div class="overlay">
        {@render overlay?.(info)}
      </div>
    </div>
  {/each}
</div>

<style>
  .viewer { display: flex; flex-direction: column; gap: 16px; width: 100%; }
  .page {
    position: relative; width: 100%; background: white;
    box-shadow: 0 1px 4px rgba(0, 0, 0, 0.15); container-type: inline-size;
  }
  canvas { position: absolute; inset: 0; width: 100%; height: 100%; }
  .overlay { position: absolute; inset: 0; }
</style>
