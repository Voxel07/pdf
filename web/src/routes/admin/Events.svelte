<script lang="ts">
  import { onMount } from 'svelte';
  import { isoFromInput, localInput, moveDay } from '../../../../calendar/src/lib/dates';
  import type { CalendarEvent } from '../../../../calendar/src/lib/events';
  import { admin, copy, randomId } from '../../lib/api';
  import { navigate } from '../../lib/router.svelte';
  import { isAdmin, isStaff } from '../../lib/session.svelte';

  type Rsvp = 'going' | 'maybe' | 'declined';
  type Phase = 'upcoming' | 'ongoing' | 'ended' | 'cancelled';
  interface EventInfo extends CalendarEvent {
    phase: Phase;
    going: number;
    maybe: number;
    declined: number;
    my_rsvp?: Rsvp;
  }

  let { id, action }: { id?: string; action: string | null } = $props();
  const base = '/admin/events';
  const editing = $derived(action === 'create' || action === 'edit');
  const cancelling = $derived(action === 'cancel');
  const endpoint = $derived('/events/' + id);
  const zone = Intl.DateTimeFormat().resolvedOptions().timeZone;
  const dates = new Intl.DateTimeFormat(undefined, { dateStyle: 'medium', timeStyle: 'short' });
  const days = new Intl.DateTimeFormat(undefined, { dateStyle: 'medium', timeZone: 'UTC' });
  const phases: Record<Phase, string> = { upcoming: 'Upcoming', ongoing: 'In progress', ended: 'Ended', cancelled: 'Cancelled' };
  const responses: { id: Rsvp; label: string }[] = [
    { id: 'going', label: 'Going' }, { id: 'maybe', label: 'Maybe' }, { id: 'declined', label: 'Can’t go' },
  ];
  let event = $state<EventInfo>();
  let events = $state<EventInfo[]>([]);
  let nextOffset = $state<number | null>(null);
  let filter = $state('');
  let loading = $state(true);
  let busy = $state(false);
  let error = $state('');
  let copied = $state(false);
  let title = $state('');
  let description = $state('');
  let location = $state('');
  let start = $state('');
  let end = $state('');
  let allDay = $state(false);
  let status = $state<CalendarEvent['status']>('scheduled');
  let requestNumber = 0;
  let lastCreateRequest: { body: string; key: string } | undefined;

  function toggleAllDay(checked: boolean) {
    allDay = checked;
    start = checked ? start.slice(0, 10) : start && start + 'T09:00';
    end = checked ? end.slice(0, 10) : end && end + 'T10:00';
  }

  async function loadList(offset = 0) {
    const request = ++requestNumber;
    loading = true;
    error = '';
    const query = new URLSearchParams({ offset: String(offset) });
    if (filter) query.set('phase', filter);
    try {
      const page = await admin.get<{ events: EventInfo[]; next_offset: number | null }>('/events?' + query);
      if (request !== requestNumber) return;
      const loadedIds = new Set(events.map((item) => item.id));
      events = offset ? [...events, ...page.events.filter((item) => !loadedIds.has(item.id))] : page.events;
      nextOffset = page.next_offset;
    } catch (err) {
      if (request === requestNumber) error = (err as Error).message;
    } finally {
      if (request === requestNumber) loading = false;
    }
  }

  async function loadDetail() {
    try {
      event = await admin.get<EventInfo>(endpoint);
      title = event.title;
      description = event.description;
      location = event.location;
      allDay = event.all_day;
      start = allDay ? event.starts_at : localInput(new Date(event.starts_at));
      end = allDay ? moveDay(event.ends_at, -1) : localInput(new Date(event.ends_at));
      status = event.status;
    } catch (err) {
      error = (err as Error).message;
    } finally {
      loading = false;
    }
  }

  onMount(() => {
    if (id) void loadDetail();
    else if (action === 'create') loading = false;
    else void loadList();
  });

  async function save(e?: SubmitEvent) {
    e?.preventDefault();
    busy = true;
    error = '';
    try {
      const body = {
        title, description, location,
        starts_at: allDay ? start : isoFromInput(start, event?.all_day === false ? event.starts_at : undefined),
        ends_at: allDay ? moveDay(end, 1) : isoFromInput(end, event?.all_day === false ? event.ends_at : undefined),
        ...(allDay ? { all_day: true } : {}),
        status: cancelling ? 'cancelled' : status,
      };
      if (Date.parse(body.ends_at) <= Date.parse(body.starts_at)) throw new Error('End time must be after the start time.');
      let saved: EventInfo;
      if (event) {
        saved = await admin.put<EventInfo>(endpoint, { ...body, revision: event.revision });
      } else {
        const serialized = JSON.stringify(body);
        if (lastCreateRequest?.body !== serialized) lastCreateRequest = { body: serialized, key: randomId(32) };
        saved = await admin.post<EventInfo>('/events', body, { 'Idempotency-Key': lastCreateRequest.key });
      }
      navigate(base + '/' + saved.id);
    } catch (err) {
      error = (err as Error).message;
    } finally {
      busy = false;
    }
  }

  async function respond(response: Rsvp) {
    busy = true;
    error = '';
    try {
      event = await admin.put<EventInfo>(endpoint + '/rsvp', { response });
    } catch (err) {
      error = (err as Error).message;
    } finally {
      busy = false;
    }
  }

  async function copyEventLink() {
    try {
      await copy(new URL(base + '/' + id, window.location.origin).href);
      copied = true;
    } catch (err) {
      error = (err as Error).message;
    }
  }
</script>

{#if id || editing}
  <p><a href={base}>← Events</a></p>
{/if}

{#if editing}
  <h1>{id ? 'Edit event' : 'New event'}</h1>
{:else if event}
  <div class="row">
    <h1 class="grow">{event.title}</h1>
    <span class="badge accent">{phases[event.phase]}</span>
  </div>
{:else if !id}
  <div class="row page-heading">
    <h1 class="grow">Events</h1>
    {#if isAdmin()}<a class="btn" href={base + '/credentials'}>Calendars &amp; API</a>{/if}
    {#if isStaff()}<a class="btn primary" href={base + '/new'}>New event</a>{/if}
  </div>
{/if}

{#if error}<p class="error" role="alert">{error}</p>{/if}
{#if loading && (id || events.length === 0)}
  <p class="muted" role="status">Loading events…</p>
{/if}

{#if editing && (!id || event)}
  <form class="card" onsubmit={save}>
    <div class="field">
      <label for="event-title">Title</label>
      <input id="event-title" maxlength="160" bind:value={title} required />
    </div>
    <label><input type="checkbox" checked={allDay} onchange={(e) => toggleAllDay(e.currentTarget.checked)} /> All day</label>
    <div class="row date-fields">
      <div class="field grow">
        <label for="event-start">Starts</label>
        {#if allDay}
          <input id="event-start" type="date" bind:value={start} required />
        {:else}
          <input id="event-start" type="datetime-local" bind:value={start} required />
        {/if}
      </div>
      <div class="field grow">
        <label for="event-end">Ends</label>
        {#if allDay}
          <input id="event-end" type="date" bind:value={end} required />
        {:else}
          <input id="event-end" type="datetime-local" bind:value={end} required />
        {/if}
      </div>
    </div>
    {#if !allDay}<p class="small muted">Time zone · {zone}</p>{/if}
    <div class="field">
      <label for="event-location">Location</label>
      <input id="event-location" maxlength="250" bind:value={location} />
    </div>
    <div class="field">
      <label for="event-description">Description</label>
      <textarea id="event-description" rows="4" maxlength="4000" bind:value={description}></textarea>
    </div>
    {#if event?.status === 'cancelled'}
      <label for="event-status">Status</label>
      <select id="event-status" class="field" bind:value={status}>
        <option value="cancelled">Cancelled</option>
        <option value="scheduled">Scheduled</option>
      </select>
    {/if}
    <div class="row">
      <button class="primary" type="submit" disabled={busy}>{busy ? 'Saving…' : id ? 'Save changes' : 'Create event'}</button>
      <a class="btn" href={id ? base + '/' + id : base}>Back</a>
    </div>
  </form>
{:else if event}
  <div class="card">
    <p>
      {#if event.all_day}
        <time datetime={event.starts_at}>{days.format(new Date(event.starts_at))}</time>
        {#if event.starts_at !== moveDay(event.ends_at, -1)}
          {@const lastDay = moveDay(event.ends_at, -1)}
          to <time datetime={lastDay}>{days.format(new Date(lastDay))}</time>
        {/if}
        <span class="small muted"> · All day</span>
      {:else}
        <time datetime={event.starts_at}>{dates.format(new Date(event.starts_at))}</time>
        to <time datetime={event.ends_at}>{dates.format(new Date(event.ends_at))}</time>
        <span class="small muted"> · {zone}</span>
      {/if}
    </p>
    {#if event.location}<p>{event.location}</p>{/if}
    {#if event.description}<p class="description">{event.description}</p>{/if}
    {#if cancelling && event.status !== 'cancelled'}
      <p>Cancel this event? Calendar subscribers will receive the updated status on their next refresh.</p>
      <div class="row">
        <button class="danger" onclick={() => save()} disabled={busy}>Cancel event</button>
        <a class="btn" href={base + '/' + id}>Keep event</a>
      </div>
    {:else}
      <div class="row">
        {#if isStaff()}
          <a class="btn" href={base + '/' + id + '?action=edit'}>Edit</a>
          {#if event.status !== 'cancelled'}
            <a class="btn" href={base + '/' + id + '?action=cancel'}>Cancel event</a>
          {/if}
        {/if}
        <button onclick={copyEventLink}>{copied ? 'Link copied' : 'Copy event link'}</button>
      </div>
    {/if}
  </div>
  {#if !cancelling}
    <div class="card">
      <h2>RSVP</h2>
      <div class="row">
        {#each responses as response (response.id)}
          <button class:primary={event.my_rsvp === response.id} aria-pressed={event.my_rsvp === response.id} disabled={busy || event.phase === 'cancelled' || event.phase === 'ended'} onclick={() => respond(response.id)}>
            {response.label} · {event[response.id]}
          </button>
        {/each}
      </div>
    </div>
  {/if}
{:else if !id}
  <section class="card" aria-label="Event list">
    <div class="row filters">
      <label for="event-filter">Status</label>
      <select id="event-filter" bind:value={filter} onchange={() => { events = []; nextOffset = null; void loadList(); }}>
        <option value="">All events</option>
        {#each Object.entries(phases) as [value, label] (value)}
          <option {value}>{label}</option>
        {/each}
      </select>
    </div>
    {#if !loading && !error && events.length === 0}
      <p class="muted">No events to show.</p>
    {/if}
    {#each events as item (item.id)}
      <article class="event-row">
        <div>
          <h2><a href={base + '/' + item.id}>{item.title}</a></h2>
          <div class="small muted">
            <time datetime={item.starts_at}>{(item.all_day ? days : dates).format(new Date(item.starts_at))}</time>
            {#if item.location} · {item.location}{/if}
          </div>
        </div>
        <span class="badge">{phases[item.phase]}</span>
      </article>
    {/each}
    {#if nextOffset !== null}
      <button class="small" disabled={loading} onclick={() => { if (nextOffset !== null) void loadList(nextOffset); }}>Load more</button>
    {/if}
  </section>
{/if}

<style>
  .page-heading { margin-bottom: 16px; }
  .row h1 { margin-bottom: 0; overflow-wrap: anywhere; }
  .row + .card { margin-top: 12px; }
  .date-fields .field { min-width: min(100%, 220px); }
  .filters select { width: auto; }
  .description { white-space: pre-wrap; overflow-wrap: anywhere; }
  .event-row { display: flex; align-items: flex-start; gap: 12px; justify-content: space-between; padding: 14px 0; border-bottom: 1px solid var(--border); }
  .event-row h2 { font-size: 1rem; margin-bottom: 4px; overflow-wrap: anywhere; }
  .event-row .badge { flex-shrink: 0; }
  .event-row + button { margin-top: 14px; }
</style>
