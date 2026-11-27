<script lang="ts">
  import type { RenderTask } from 'pdfjs-dist';
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
  let loadFailed = $state(false);
  let pageFailures = $state<Record<number, boolean>>({});
  let container: HTMLDivElement;
  let width = $state(0);

  $effect(() => {
    const d = doc;
    let active = true;
    pages = [];
    loadFailed = false;
    pageFailures = {};
    (async () => {
      const list: PageInfo[] = [];
      for (let i = 0; i < d.numPages; i++) {
        const p = await d.getPage(i + 1);
        if (!active) return;
        const vp = p.getViewport({ scale: 1 });
        list.push({ index: i, widthPt: vp.width, heightPt: vp.height });
      }
      pages = list;
    })().catch(() => {
      if (active) loadFailed = true;
    });
    return () => { active = false; };
  });

  $effect(() => {
    const ro = new ResizeObserver((entries) => {
      const w = Math.round(entries[0].contentRect.width);
      if (Math.abs(w - width) > 4) width = w;
    });
    ro.observe(container);
    return () => ro.disconnect();
  });

  $effect(() => {
    const d = doc;
    const renderWidth = width;
    const renderPages = pages;
    if (!renderWidth || renderPages.length === 0) return;
    let active = true;
    let task: RenderTask | undefined;
    const t = setTimeout(async () => {
      const dpr = Math.min(window.devicePixelRatio || 1, 2.5);
      for (const info of renderPages) {
        if (!active) return;
        const canvas = canvases[info.index];
        if (!canvas) continue;
        try {
          const page = await d.getPage(info.index + 1);
          if (!active) return;
          const scale = (renderWidth / info.widthPt) * dpr;
          const vp = page.getViewport({ scale });
          canvas.width = Math.floor(vp.width);
          canvas.height = Math.floor(vp.height);
          task = page.render({ canvas, viewport: vp });
          await task.promise;
          if (active) delete pageFailures[info.index];
        } catch {
          if (!active) return;
          pageFailures[info.index] = true;
          // Release the canvas if PDF.js failed while initializing the render.
          task?.cancel();
        } finally {
          task = undefined;
        }
      }
    }, 80);
    return () => {
      active = false;
      clearTimeout(t);
      task?.cancel();
    };
  });
</script>

<div class="viewer" bind:this={container}>
  {#if loadFailed}<p class="error" role="alert">The PDF could not be displayed.</p>{/if}
  {#each pages as info (info.index)}
    <div class="page" style:aspect-ratio="{info.widthPt} / {info.heightPt}" data-page={info.index}>
      <canvas bind:this={canvases[info.index]}></canvas>
      {#if pageFailures[info.index]}
        <p class="error page-error" role="alert">Page {info.index + 1} could not be displayed.</p>
      {:else}
        <div class="overlay">
          {@render overlay?.(info)}
        </div>
      {/if}
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
  .page-error { position: absolute; inset: 0; display: grid; place-items: center; margin: 0; }
</style>
