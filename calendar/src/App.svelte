<script lang="ts">
  import { onMount } from 'svelte';
  import { request } from './lib/api';
  import { dateKey, eventsOnDay, isoFromInput, localDate, localInput, monthDays, moveDay } from './lib/dates';
  import type { CalendarEvent, CalendarSettings, EventInput } from './lib/events';

  const today = dateKey(new Date());
  const monthLabel = new Intl.DateTimeFormat(undefined, { month: 'long', year: 'numeric' });
  const dayLabel = new Intl.DateTimeFormat(undefined, { weekday: 'long', month: 'long', day: 'numeric' });
  const eventTime = new Intl.DateTimeFormat(undefined, { hour: 'numeric', minute: '2-digit' });
  const weekdays = ['Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun'];
  const timeZone = Intl.DateTimeFormat().resolvedOptions().timeZone;

  let month = $state(new Date(new Date().getFullYear(), new Date().getMonth(), 1));
  let selectedDay = $state(today);
  let token = $state('');
  let enteredToken = $state('');
  let signingIn = $state(false);
  let loginError = $state('');
  let settings = $state<CalendarSettings>();
  let events = $state<CalendarEvent[]>([]);
  let visibleDays = $derived(monthDays(month).map((day) => ({
    key: day,
    inMonth: localDate(day).getMonth() === month.getMonth(),
    events: eventsOnDay(events, day),
  })));
  let agenda = $derived(eventsOnDay(events, selectedDay));
  let feedUrl = $derived(settings ? new URL(settings.feed_url, window.location.origin).href : '');

  let editor = $state<HTMLDialogElement>();
  let subscription = $state<HTMLDialogElement>();
  let feedInput = $state<HTMLInputElement>();
  let editing = $state<CalendarEvent>();
  let form = $state({ title: '', location: '', description: '', allDay: false, start: '', end: '' });
  let saving = $state(false);
  let editorError = $state('');
  let confirmDelete = $state(false);
  let createRequest = $state<{ body: string; key: string }>();
  let feedback = $state('');
  let copied = $state(false);

  onMount(() => {
    const saved = sessionStorage.getItem('diy-calendar-token');
    if (saved) void signIn(saved);
  });

  async function signIn(value: string) {
    signingIn = true;
    loginError = '';
    try {
      const [calendar, result] = await Promise.all([
        request<CalendarSettings>(value, '/settings'),
        request<{ events: CalendarEvent[] }>(value, '/events'),
      ]);
      token = value;
      settings = calendar;
      events = result.events;
      sessionStorage.setItem('diy-calendar-token', value);
      enteredToken = '';
    } catch (error) {
      sessionStorage.removeItem('diy-calendar-token');
      loginError = (error as Error).message;
    } finally {
      signingIn = false;
    }
  }

  function signOut() {
    sessionStorage.removeItem('diy-calendar-token');
    token = '';
    settings = undefined;
    events = [];
    editing = undefined;
    createRequest = undefined;
    feedback = '';
  }

  function select(day: string) {
    const date = localDate(day);
    if (date.getMonth() !== month.getMonth() || date.getFullYear() !== month.getFullYear()) {
      month = new Date(date.getFullYear(), date.getMonth(), 1);
    }
    selectedDay = day;
  }

  function navigate(offset: number) {
    const target = new Date(month.getFullYear(), month.getMonth() + offset, 1);
    select(dateKey(target));
  }

  function eventHours(event: CalendarEvent, day: string) {
    if (event.status === 'cancelled') return 'Cancelled';
    if (event.all_day) return 'All day';
    const start = new Date(event.starts_at);
    const end = new Date(event.ends_at);
    if (dateKey(start) === day && dateKey(end) === day) return eventTime.formatRange(start, end);
    if (dateKey(start) === day) return `From ${eventTime.format(start)}`;
    if (dateKey(end) === day) return `Until ${eventTime.format(end)}`;
    return 'Continues all day';
  }

  function openEditor(event?: CalendarEvent) {
    editing = event;
    form = {
      title: event?.title ?? '',
      description: event?.description ?? '',
      location: event?.location ?? '',
      allDay: event?.all_day ?? false,
      start: event ? event.all_day ? event.starts_at : localInput(new Date(event.starts_at)) : selectedDay + 'T09:00',
      end: event ? event.all_day ? moveDay(event.ends_at, -1) : localInput(new Date(event.ends_at)) : selectedDay + 'T10:00',
    };
    createRequest = undefined;
    editorError = '';
    confirmDelete = false;
    editor?.showModal();
  }

  function setAllDay(checked: boolean) {
    form.allDay = checked;
    if (checked) {
      form.start = form.start.slice(0, 10);
      form.end = form.end.slice(0, 10);
    } else {
      form.start += 'T09:00';
      form.end += 'T10:00';
    }
  }

  async function saveEvent(event: SubmitEvent) {
    event.preventDefault();
    if (saving) return;
    saving = true;
    editorError = '';
    try {
      const input: EventInput = {
        title: form.title,
        description: form.description,
        location: form.location,
        all_day: form.allDay,
        status: editing?.status ?? 'scheduled',
        starts_at: form.allDay ? form.start : isoFromInput(form.start, editing?.all_day === false ? editing.starts_at : undefined),
        ends_at: form.allDay ? moveDay(form.end, 1) : isoFromInput(form.end, editing?.all_day === false ? editing.ends_at : undefined),
      };
      const body = JSON.stringify(input);
      let saved: CalendarEvent;
      if (editing) {
        saved = await request(token, '/events/' + editing.id, {
          method: 'PUT',
          body,
          headers: { 'If-Match': `"${editing.revision}"` },
        });
      } else {
        if (createRequest?.body !== body) createRequest = { body, key: crypto.randomUUID() };
        saved = await request(token, '/events', {
          method: 'POST',
          body,
          headers: { 'Idempotency-Key': createRequest.key },
        });
      }
      events = [...events.filter((current) => current.id !== saved.id), saved];
      select(saved.all_day ? saved.starts_at : dateKey(new Date(saved.starts_at)));
      feedback = editing ? 'Event updated.' : 'Event created.';
      editor?.close();
    } catch (error) {
      editorError = (error as Error).message;
    } finally {
      saving = false;
    }
  }

  async function deleteEvent() {
    if (!editing || saving) return;
    const { id, revision } = editing;
    saving = true;
    editorError = '';
    try {
      await request(token, '/events/' + id, {
        method: 'DELETE',
        headers: { 'If-Match': `"${revision}"` },
      });
      events = events.filter((event) => event.id !== id);
      feedback = 'Event deleted.';
      editor?.close();
    } catch (error) {
      editorError = (error as Error).message;
    } finally {
      saving = false;
    }
  }

  function openSubscription() {
    copied = false;
    subscription?.showModal();
  }

  async function copyFeed(input: HTMLInputElement) {
    input.select();
    try {
      await navigator.clipboard.writeText(feedUrl);
      copied = true;
    } catch {
      // The selected text can still be copied using the browser.
    }
  }
</script>

{#if !settings}
  <main class="sign-in">
    <div class="login-card">
      <div class="calendar-mark" aria-hidden="true"><span></span></div>
      <h1>Calendar</h1>
      <form onsubmit={(event) => { event.preventDefault(); void signIn(enteredToken.trim()); }}>
        <label for="admin-token">Admin token</label>
        <input id="admin-token" type="password" autocomplete="current-password" bind:value={enteredToken} required />
        {#if loginError}<p class="error" role="alert">{loginError}</p>{/if}
        <button class="primary" type="submit" disabled={signingIn}>{signingIn ? 'Signing in…' : 'Sign in'}</button>
      </form>
    </div>
  </main>
{:else}
  <header class="site-header">
    <div class="header-inner">
      <div class="brand">
        <div class="calendar-mark" aria-hidden="true"><span></span></div>
        <h1>{settings.name}</h1>
      </div>
      <div class="header-actions">
        <button class="secondary" onclick={openSubscription}>Subscribe</button>
        <button class="quiet" onclick={signOut}>Sign out</button>
      </div>
    </div>
  </header>

  <main class="workspace">
    <div class="toolbar">
      <h2>{monthLabel.format(month)}</h2>
      <div class="controls">
        <button class="secondary today-button" onclick={() => select(dateKey(new Date()))}>Today</button>
        <div class="month-nav">
          <button aria-label="Previous month" onclick={() => navigate(-1)}><span class="chevron previous" aria-hidden="true"></span></button>
          <button aria-label="Next month" onclick={() => navigate(1)}><span class="chevron next" aria-hidden="true"></span></button>
        </div>
        <button class="primary" onclick={() => openEditor()}>New event</button>
      </div>
    </div>

    <div class="workspace-grid">
      <section class="month-panel" aria-label={monthLabel.format(month)}>
        <div class="weekdays" aria-hidden="true">
          {#each weekdays as day (day)}<span>{day}</span>{/each}
        </div>
        <div class="month-grid">
          {#each visibleDays as day (day.key)}
            <button
              class="day"
              class:outside={!day.inMonth}
              class:selected={day.key === selectedDay}
              aria-pressed={day.key === selectedDay}
              aria-current={day.key === today ? 'date' : undefined}
              aria-label={`${dayLabel.format(localDate(day.key))}, ${day.events.length} ${day.events.length === 1 ? 'event' : 'events'}`}
              onclick={() => select(day.key)}
            >
              <span class="day-number" class:current={day.key === today}>{Number(day.key.slice(-2))}</span>
              {#each day.events.slice(0, 2) as event (event.id)}
                <span class="event-chip" class:all-day={event.all_day}>
                  <span class="event-dot"></span>{event.title}
                </span>
              {/each}
              {#if day.events.length > 2}<span class="overflow">+{day.events.length - 2} more</span>{/if}
            </button>
          {/each}
        </div>
      </section>

      <aside class="agenda" aria-label="Daily agenda">
        <header>
          <h3>{dayLabel.format(localDate(selectedDay))}</h3>
          <p>{timeZone.replaceAll('_', ' ')}</p>
        </header>
        {#if agenda.length === 0}
          <p class="empty">No events scheduled.</p>
        {:else}
          <ul>
            {#each agenda as event (event.id)}
              <li>
                <button class="agenda-event" onclick={() => openEditor(event)}>
                  <span class="agenda-time">{eventHours(event, selectedDay)}</span>
                  <strong>{event.title}</strong>
                  {#if event.location}<span class="agenda-location">{event.location}</span>{/if}
                </button>
              </li>
            {/each}
          </ul>
        {/if}
      </aside>
    </div>
    <p class="feedback" role="status">{feedback}</p>
  </main>

  <dialog bind:this={editor} aria-labelledby="editor-title" oncancel={(event) => { if (saving) event.preventDefault(); }}>
    <h2 id="editor-title">{editing ? 'Edit event' : 'New event'}</h2>
    <form onsubmit={saveEvent}>
      <label for="event-title">Title</label>
      <input id="event-title" bind:value={form.title} maxlength="160" required />
      <label class="checkbox"><input type="checkbox" checked={form.allDay} onchange={(event) => setAllDay(event.currentTarget.checked)} /> All day</label>
      <div class="date-fields">
        <div>
          <label for="event-start">Starts</label>
          {#if form.allDay}
            <input id="event-start" type="date" bind:value={form.start} required />
          {:else}
            <input id="event-start" type="datetime-local" bind:value={form.start} required />
          {/if}
        </div>
        <div>
          <label for="event-end">Ends</label>
          {#if form.allDay}
            <input id="event-end" type="date" bind:value={form.end} required />
          {:else}
            <input id="event-end" type="datetime-local" bind:value={form.end} required />
          {/if}
        </div>
      </div>
      <label for="event-location">Location</label>
      <input id="event-location" bind:value={form.location} maxlength="250" />
      <label for="event-description">Description</label>
      <textarea id="event-description" bind:value={form.description} maxlength="4000" rows="3"></textarea>
      {#if editorError}<p class="error" role="alert">{editorError}</p>{/if}
      {#if confirmDelete}
        <p class="delete-prompt">Delete this event from the calendar?</p>
        <div class="dialog-actions">
          <button type="button" class="secondary" disabled={saving} onclick={() => { confirmDelete = false; }}>Keep event</button>
          <button type="button" class="danger" disabled={saving} onclick={deleteEvent}>Delete event</button>
        </div>
      {:else}
        <div class="dialog-actions">
          {#if editing}<button type="button" class="quiet delete" disabled={saving} onclick={() => { confirmDelete = true; }}>Delete</button>{/if}
          <button type="button" class="secondary" disabled={saving} onclick={() => editor?.close()}>Cancel</button>
          <button type="submit" class="primary" disabled={saving}>{saving ? 'Saving…' : editing ? 'Save changes' : 'Create event'}</button>
        </div>
      {/if}
    </form>
  </dialog>

  <dialog bind:this={subscription} aria-labelledby="subscribe-title">
    <h2 id="subscribe-title">Subscribe to calendar</h2>
    <p>Add this read-only link in Google Calendar or Apple Calendar. Anyone with the link can view the feed.</p>
    <label for="feed-url">Subscription URL</label>
    <input id="feed-url" type="url" readonly value={feedUrl} bind:this={feedInput} />
    <div class="dialog-actions">
      <button class="secondary" onclick={() => subscription?.close()}>Done</button>
      <button class="primary" onclick={() => { if (feedInput) void copyFeed(feedInput); }}>{copied ? 'Copied' : 'Copy link'}</button>
    </div>
  </dialog>
{/if}
