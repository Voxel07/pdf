<script lang="ts">
  // A Material Design snackbar: bottom-left on wide screens, full width on phones. It hides
  // itself after `duration` ms, pausing while the pointer rests on it or it has focus.
  import { onDestroy } from 'svelte';
  import { scale } from 'svelte/transition';
  import { cubicOut } from 'svelte/easing';

  let { message, duration = 4000, onclose }: { message: string; duration?: number; onclose: () => void } = $props();

  let timer: ReturnType<typeof setTimeout> | undefined;
  const reducedMotion = typeof matchMedia === 'function' && matchMedia('(prefers-reduced-motion: reduce)').matches;

  function start() {
    stop();
    timer = setTimeout(onclose, duration);
  }
  function stop() {
    if (timer) clearTimeout(timer);
    timer = undefined;
  }
  start();
  onDestroy(stop);
</script>

<div
  class="snackbar"
  role="status"
  aria-live="polite"
  onmouseenter={stop}
  onmouseleave={start}
  onfocusin={stop}
  onfocusout={start}
  in:scale={{ start: 0.8, duration: reducedMotion ? 0 : 225, easing: cubicOut }}
  out:scale={{ start: 0.8, duration: reducedMotion ? 0 : 195 }}
>
  <span class="message">{message}</span>
  <button class="close" aria-label="Dismiss" onclick={onclose}>
    <svg width="18" height="18" viewBox="0 0 24 24" aria-hidden="true"><path fill="currentColor" d="M19 6.41 17.59 5 12 10.59 6.41 5 5 6.41 10.59 12 5 17.59 6.41 19 12 13.41 17.59 19 19 17.59 13.41 12z" /></svg>
  </button>
</div>

<style>
  .snackbar {
    position: fixed;
    left: 24px;
    bottom: 24px;
    z-index: 1400;
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 288px;
    max-width: 560px;
    padding: 6px 8px 6px 16px;
    border-radius: 4px;
    background: var(--snackbar-bg);
    color: var(--snackbar-text);
    font-size: 0.875rem;
    line-height: 1.43;
    letter-spacing: 0.01071em;
    box-shadow: 0 3px 5px -1px rgba(0, 0, 0, 0.2), 0 6px 10px 0 rgba(0, 0, 0, 0.14), 0 1px 18px 0 rgba(0, 0, 0, 0.12);
    transform-origin: center;
  }
  .message { flex: 1; padding: 8px 0; }
  .close {
    display: grid;
    place-items: center;
    width: 34px;
    height: 34px;
    padding: 0;
    border: 0;
    border-radius: 50%;
    background: transparent;
    color: inherit;
    opacity: 0.85;
  }
  .close:hover:not(:disabled) { background: color-mix(in srgb, currentColor 14%, transparent); opacity: 1; }
  @media (max-width: 36rem) {
    .snackbar { left: 8px; right: 8px; bottom: 8px; min-width: 0; max-width: none; }
  }
</style>
