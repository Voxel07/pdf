<script lang="ts">
  import { admin, adminDownload, fmtDateTime } from '../../lib/api';

  interface Config { enabled: boolean; dir: string; start_time: string; interval_hours: number; keep: number }

  let data = $state<any>(null);
  let cfg = $state<Config | null>(null);
  let unit = $state<'hours' | 'days'>('days');
  let every = $state(1);
  let saving = $state(false);
  let running = $state(false);
  let msg = $state('');
  let error = $state('');

  async function load() {
    data = await admin.get('/backup');
    cfg = { ...data.config };
    if (cfg!.interval_hours % 24 === 0) {
      unit = 'days';
      every = cfg!.interval_hours / 24;
    } else {
      unit = 'hours';
      every = cfg!.interval_hours;
    }
  }
  load();

  async function save(e: SubmitEvent) {
    e.preventDefault();
    saving = true;
    error = msg = '';
    try {
      cfg!.interval_hours = unit === 'days' ? every * 24 : every;
      await admin.put('/backup/config', cfg);
      msg = 'Saved';
      await load();
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
    } finally {
      saving = false;
    }
  }

  async function runNow() {
    running = true;
    error = msg = '';
    try {
      const s = await admin.post('/backup/run');
      if (s.last_ok) msg = `Backup written: ${s.last_file} (app paused ${s.last_pause_ms} ms)`;
      else error = s.last_error;
      await load();
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
    } finally {
      running = false;
    }
  }

  async function remove(name: string) {
    if (!confirm(`Delete ${name}?`)) return;
    await admin.del(`/backup/files/${encodeURIComponent(name)}`);
    await load();
  }

  const size = (n?: number) => (n == null ? '—' : n > 1e6 ? `${(n / 1e6).toFixed(1)} MB` : `${Math.ceil(n / 1e3)} kB`);
</script>

<h1>Backups</h1>
{#if data && cfg}
  <form class="card" onsubmit={save}>
    <h2>Schedule</h2>
    <p class="muted">
      Each backup briefly pauses signing writes while it snapshots the signing database and template PDFs. Login and signing sessions are removed from the snapshot.
      {#if data.includes_calendar}The archive also includes the calendar database.{/if}
    </p>
    <label class="inline" style="margin-bottom:12px"><input type="checkbox" bind:checked={cfg.enabled} /> Automatic backups enabled</label>
    <div class="field">
      <label for="dir">Backup folder</label>
      <input id="dir" bind:value={cfg.dir} required />
      <div class="small muted" style="margin-top:3px">
        A path inside the container. With Docker, mount the host folder there, e.g. <code>BACKUP_HOST_DIR=/srv/pdfsign-backups</code> in <code>.env</code> is mounted at <code>{data.default_dir}</code>.
      </div>
    </div>
    <div class="grid3">
      <div class="field"><label for="st">First run at ({data.timezone})</label><input id="st" type="time" bind:value={cfg.start_time} required /></div>
      <div class="field">
        <label for="ev">Repeat every</label>
        <div class="row" style="flex-wrap:nowrap">
          <input id="ev" type="number" min="1" max={unit === 'days' ? 31 : 744} bind:value={every} style="width:80px" />
          <select bind:value={unit} style="width:auto"><option value="hours">hours</option><option value="days">days</option></select>
        </div>
      </div>
      <div class="field"><label for="keep">Keep last (0 = all)</label><input id="keep" type="number" min="0" max="10000" bind:value={cfg.keep} /></div>
    </div>
    <div class="row">
      <button class="primary" type="submit" disabled={saving}>{saving ? 'Saving…' : 'Save'}</button>
      <button type="button" onclick={runNow} disabled={running || data.running}>{running || data.running ? 'Backing up…' : 'Back up now'}</button>
      {#if data.status.next_run_at && cfg.enabled}<span class="muted small">Next run: {fmtDateTime(data.status.next_run_at)}</span>{/if}
    </div>
    {#if msg}<div class="success">{msg}</div>{/if}
    {#if error}<div class="error">{error}</div>{/if}
  </form>

  <div class="card">
    <h2>Last run</h2>
    {#if data.status.last_run_at}
      <p>
        <span class="badge {data.status.last_ok ? 'completed' : 'revoked'}">{data.status.last_ok ? 'ok' : 'failed'}</span>
        {fmtDateTime(data.status.last_run_at)} · {data.status.last_trigger}
        {#if data.status.last_ok}· {size(data.status.last_size)} · paused {data.status.last_pause_ms} ms · took {data.status.last_duration_ms} ms{/if}
      </p>
      {#if data.status.last_error}<div class="error">{data.status.last_error}</div>{/if}
    {:else}
      <p class="muted">No backup has run yet.</p>
    {/if}
  </div>

  <div class="card">
    <h2>Backups in {cfg.dir} ({data.files.length})</h2>
    <table>
      <thead><tr><th>File</th><th>Size</th><th>Created</th><th></th></tr></thead>
      <tbody>
        {#each data.files as f (f.name)}
          <tr>
            <td><code>{f.name}</code></td><td>{size(f.size)}</td><td>{fmtDateTime(f.modified)}</td>
            <td style="text-align:right">
              <button class="small" onclick={() => adminDownload(`/backup/files/${encodeURIComponent(f.name)}`, f.name)}>Download</button>
              <button class="small ghost" onclick={() => remove(f.name)}>🗑</button>
            </td>
          </tr>
        {:else}
          <tr><td colspan="4" class="muted">No backups in this folder yet.</td></tr>
        {/each}
      </tbody>
    </table>
    <h3 style="margin-top:16px">Restore</h3>
    <p>Restoring signs everyone out and invalidates old signing links and API tokens. A combined restore also invalidates scoped calendar tokens and feeds.</p>
    {#if data.includes_calendar}
      <p>Stop both services and mount the calendar volume for the restore. Replace the default volume name if you configured another one.</p>
      <pre class="small">docker compose --profile calendar stop pdfsign calendar
docker compose run --rm -v calendar_calendar-data:/calendar-data pdfsign pdfsign restore /backups/backup-file.zip --force --calendar-data-dir /calendar-data
docker compose --profile calendar start calendar pdfsign</pre>
    {:else}
    <pre class="small">docker compose stop pdfsign
docker compose run --rm pdfsign pdfsign restore /backups/backup-file.zip --force
docker compose start pdfsign</pre>
    {/if}
  </div>
{:else}
  <p class="muted">Loading…</p>
{/if}

<style>
  .grid3 { display: grid; grid-template-columns: repeat(3, 1fr); gap: 10px; }
  @media (max-width: 700px) { .grid3 { grid-template-columns: 1fr; } }
</style>
