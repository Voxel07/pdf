<script lang="ts">
  import { fmtDateTime, type ActivityEntry } from '../../lib/api';

  let { entries, compact = false }: { entries: ActivityEntry[]; compact?: boolean } = $props();
</script>

{#if entries.length === 0}
  <p class="muted" style="margin:10px 0 0">Nothing logged yet.</p>
{:else}
  <table class="activity" class:compact>
    <tbody>
      {#each entries as e (e.id)}
        <tr class={e.level}>
          <td class="when small muted">{fmtDateTime(e.created_at)}</td>
          <td class="lvl"><span class="badge {e.level === 'error' ? 'revoked' : e.level === 'warn' ? 'pending' : ''}">{e.level}</span></td>
          {#if !compact}<td class="small muted">{e.category}</td>{/if}
          <td class="msg">{e.message}{#if e.actor}<span class="small muted"> — {e.actor}</span>{/if}</td>
        </tr>
      {/each}
    </tbody>
  </table>
{/if}

<style>
  .activity td { vertical-align: top; }
  .when { white-space: nowrap; width: 1%; }
  .lvl { width: 1%; }
  .msg { word-break: break-word; }
  .compact td { padding: 5px 8px; }
  tr.error .msg { color: var(--danger); }
</style>
