<script lang="ts">
  import { onMount } from 'svelte';
  import { request, SignInRequired } from './lib/api';
  import { dateKey, eventsOnDay, isoFromInput, localDate, localInput, monthDays, moveDay } from './lib/dates';
  import type { CalendarEvent, CalendarInfo, EventInput, UiSessionInfo } from './lib/events';
  import Snackbar from './lib/Snackbar.svelte';
  import { nextTheme, theme } from './lib/theme.svelte';

  const SELECTED_KEY = 'diy-calendar-selected';
  const today = dateKey(new Date());
  const monthLabel = new Intl.DateTimeFormat(undefined, { month: 'long', year: 'numeric' });
  const dayLabel = new Intl.DateTimeFormat(undefined, { weekday: 'long', month: 'long', day: 'numeric' });
  const shortDay = new Intl.DateTimeFormat(undefined, { month: 'short', day: 'numeric' });
  const eventTime = new Intl.DateTimeFormat(undefined, { hour: 'numeric', minute: '2-digit' });
  const weekdays = ['Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun'];
  const timeZone = Intl.DateTimeFormat().resolvedOptions().timeZone;

  let phase = $state<'loading' | 'signed-out' | 'unavailable' | 'ready'>('loading');
  let session = $state<UiSessionInfo>();
  let calendarId = $state('');
  let current = $derived<CalendarInfo | undefined>(session?.calendars.find((calendar) => calendar.id === calendarId));
  let writable = $derived(Boolean(current?.writable));
  let month = $state(new Date(new Date().getFullYear(), new Date().getMonth(), 1));
  let selectedDay = $state(today);
  let events = $state<CalendarEvent[]>([]);
  let loadError = $state('');
  let visibleDays = $derived(monthDays(month).map((day) => ({
    key: day,
    inMonth: localDate(day).getMonth() === month.getMonth(),
    events: eventsOnDay(events, day),
  })));
  let agenda = $derived(eventsOnDay(events, selectedDay));
  let feedUrl = $derived(current ? new URL(current.feed_url, window.location.origin).href : '');

  // Pressing on a day and dragging across others selects a range for a new multi-day event.
  let grid = $state<HTMLDivElement>();
  let drag = $state<{ pointerId: number; anchor: string; current: string; touch: boolean; x: number; y: number }>();
  let range = $derived(drag && !drag.touch
    ? (drag.anchor <= drag.current ? [drag.anchor, drag.current] : [drag.current, drag.anchor])
    : undefined);

  let editor = $state<HTMLDialogElement>();
  let subscription = $state<HTMLDialogElement>();
  let manager = $state<HTMLDialogElement>();
  let feedInput = $state<HTMLInputElement>();
  let editing = $state<CalendarEvent>();
  let form = $state({ title: '', location: '', description: '', allDay: false, start: '', end: '' });
  let saving = $state(false);
  let editorError = $state('');
  let confirmDelete = $state(false);
  let createRequest = $state<{ body: string; key: string; calendar: string }>();
  let copied = $state(false);
  let toast = $state<{ id: number; message: string }>();
  let toastId = 0;

  let newCalendarName = $state('');
  let renaming = $state<{ id: string; name: string }>();
  let removing = $state<string>();
  let managerError = $state('');
  let managerBusy = $state(false);

  onMount(() => { void loadSession(); });

  // Pointer-only shortcut: keyboard users reach the same actions through the day and New event buttons.
  $effect(() => {
    const element = grid;
    if (!element) return;
    const cancel = () => { drag = undefined; };
    element.addEventListener('pointerdown', pointerDown);
    element.addEventListener('pointermove', pointerMove);
    element.addEventListener('pointerup', pointerUp);
    element.addEventListener('pointercancel', cancel);
    return () => {
      element.removeEventListener('pointerdown', pointerDown);
      element.removeEventListener('pointermove', pointerMove);
      element.removeEventListener('pointerup', pointerUp);
      element.removeEventListener('pointercancel', cancel);
    };
  });

  function notify(message: string) {
    toast = { id: ++toastId, message };
  }

  function handle(error: unknown) {
    if (error instanceof SignInRequired) {
      phase = error.available ? 'signed-out' : 'unavailable';
      session = undefined;
      editor?.close();
      subscription?.close();
      manager?.close();
      return true;
    }
    return false;
  }

  async function loadSession() {
    try {
      session = await request<UiSessionInfo>('/session');
      phase = 'ready';
      const saved = localStorage.getItem(SELECTED_KEY);
      const fallback = session.calendars.find((calendar) => calendar.personal) ?? session.calendars[0];
      await openCalendar(session.calendars.some((calendar) => calendar.id === saved) ? saved! : fallback.id);
    } catch (error) {
      if (!handle(error)) {
        phase = 'signed-out';
        loadError = (error as Error).message;
      }
    }
  }

  async function openCalendar(id: string) {
    calendarId = id;
    localStorage.setItem(SELECTED_KEY, id);
    loadError = '';
    try {
      const result = await request<{ events: CalendarEvent[] }>('/events?calendar=' + encodeURIComponent(id));
      if (calendarId === id) events = result.events;
    } catch (error) {
      if (!handle(error) && calendarId === id) {
        events = [];
        loadError = (error as Error).message;
      }
    }
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
    if (event.all_day) {
      const last = moveDay(event.ends_at, -1);
      return last === event.starts_at ? 'All day' : `All day · ${shortDay.format(localDate(event.starts_at))} – ${shortDay.format(localDate(last))}`;
    }
    const start = new Date(event.starts_at);
    const end = new Date(event.ends_at);
    if (dateKey(start) === day && dateKey(end) === day) return eventTime.formatRange(start, end);
    if (dateKey(start) === day) return `From ${eventTime.format(start)}`;
    if (dateKey(end) === day) return `Until ${eventTime.format(end)}`;
    return 'Continues all day';
  }

  function dayAt(x: number, y: number) {
    return document.elementFromPoint(x, y)?.closest<HTMLElement>('[data-day]')?.dataset.day;
  }

  function pointerDown(event: PointerEvent) {
    if (drag || event.button !== 0) return;
    const target = event.target as HTMLElement;
    if (target.closest('.event-chip, .overflow')) return;
    const day = target.closest<HTMLElement>('[data-day]')?.dataset.day;
    if (!day) return;
    const touch = event.pointerType === 'touch';
    drag = { pointerId: event.pointerId, anchor: day, current: day, touch, x: event.clientX, y: event.clientY };
    if (!touch) {
      // Keeps the drag from selecting text and follows the pointer outside the grid.
      event.preventDefault();
      grid?.setPointerCapture(event.pointerId);
    }
  }

  function pointerMove(event: PointerEvent) {
    if (!drag || event.pointerId !== drag.pointerId || drag.touch) return;
    const day = dayAt(event.clientX, event.clientY);
    if (day && day !== drag.current) drag.current = day;
  }

  function pointerUp(event: PointerEvent) {
    if (!drag || event.pointerId !== drag.pointerId) return;
    const { anchor, touch, x, y } = drag;
    const end = touch ? anchor : drag.current;
    drag = undefined;
    if (touch) {
      // A tap selects the day; tapping the selected day again starts a new event there.
      if (Math.hypot(event.clientX - x, event.clientY - y) > 10) return;
      const again = anchor === selectedDay;
      select(anchor);
      if (again && writable) openEditor(undefined, { start: anchor, end: anchor, allDay: false });
      return;
    }
    const [first, last] = anchor <= end ? [anchor, end] : [end, anchor];
    select(first);
    if (!writable) return;
    openEditor(undefined, { start: first, end: last, allDay: first !== last });
  }

  function openEditor(event?: CalendarEvent, preset?: { start: string; end: string; allDay: boolean }) {
    editing = event;
    if (event) {
      form = {
        title: event.title,
        description: event.description,
        location: event.location,
        allDay: event.all_day,
        start: event.all_day ? event.starts_at : localInput(new Date(event.starts_at)),
        end: event.all_day ? moveDay(event.ends_at, -1) : localInput(new Date(event.ends_at)),
      };
    } else {
      const start = preset?.start ?? selectedDay;
      const end = preset?.end ?? start;
      const allDay = preset?.allDay ?? false;
      form = {
        title: '',
        description: '',
        location: '',
        allDay,
        start: allDay ? start : start + 'T09:00',
        end: allDay ? end : end + 'T10:00',
      };
    }
    createRequest = undefined;
    editorError = '';
    confirmDelete = false;
    editor?.showModal();
    if (!event) queueMicrotask(() => document.getElementById('event-title')?.focus());
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
    if (saving || !writable) return;
    saving = true;
    editorError = '';
    const calendar = calendarId;
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
        saved = await request('/events/' + editing.id, {
          method: 'PUT',
          body,
          headers: { 'If-Match': `"${editing.revision}"` },
        });
      } else {
        if (createRequest?.body !== body || createRequest.calendar !== calendar) {
          createRequest = { body, key: crypto.randomUUID(), calendar };
        }
        saved = await request('/events?calendar=' + encodeURIComponent(calendar), {
          method: 'POST',
          body,
          headers: { 'Idempotency-Key': createRequest.key },
        });
      }
      if (calendarId === calendar) events = [...events.filter((current) => current.id !== saved.id), saved];
      select(saved.all_day ? saved.starts_at : dateKey(new Date(saved.starts_at)));
      notify(editing ? 'Event updated' : 'Event created');
      editor?.close();
    } catch (error) {
      if (!handle(error)) editorError = (error as Error).message;
    } finally {
      saving = false;
    }
  }

  async function deleteEvent() {
    if (!editing || saving) return;
    const { id, revision, title } = editing;
    saving = true;
    editorError = '';
    try {
      await request('/events/' + id, {
        method: 'DELETE',
        headers: { 'If-Match': `"${revision}"` },
      });
      events = events.filter((event) => event.id !== id);
      notify(`“${title}” deleted`);
      editor?.close();
    } catch (error) {
      if (!handle(error)) editorError = (error as Error).message;
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

  function openManager() {
    newCalendarName = '';
    renaming = undefined;
    removing = undefined;
    managerError = '';
    manager?.showModal();
  }

  async function manage<T>(action: () => Promise<T>, done: (result: T) => void | Promise<void>) {
    if (managerBusy) return;
    managerBusy = true;
    managerError = '';
    try {
      await done(await action());
    } catch (error) {
      if (!handle(error)) managerError = (error as Error).message;
    } finally {
      managerBusy = false;
    }
  }

  function createCalendar(event: SubmitEvent) {
    event.preventDefault();
    const name = newCalendarName.trim();
    if (!name) return;
    void manage(
      () => request<CalendarInfo>('/calendars', { method: 'POST', body: JSON.stringify({ name }) }),
      async (created) => {
        session!.calendars = [...session!.calendars, created];
        newCalendarName = '';
        await openCalendar(created.id);
        notify(`Calendar “${created.name}” created`);
      },
    );
  }

  function renameCalendar(event: SubmitEvent) {
    event.preventDefault();
    if (!renaming) return;
    const { id, name } = renaming;
    void manage(
      () => request<CalendarInfo>('/calendars/' + id, { method: 'PATCH', body: JSON.stringify({ name }) }),
      (updated) => {
        session!.calendars = session!.calendars.map((calendar) => calendar.id === id ? updated : calendar);
        renaming = undefined;
        notify('Calendar renamed');
      },
    );
  }

  function deleteCalendar(id: string) {
    const name = session?.calendars.find((calendar) => calendar.id === id)?.name ?? 'Calendar';
    void manage(
      () => request<void>('/calendars/' + id, { method: 'DELETE' }),
      async () => {
        session!.calendars = session!.calendars.filter((calendar) => calendar.id !== id);
        removing = undefined;
        if (calendarId === id) await openCalendar(session!.calendars[0].id);
        notify(`Calendar “${name}” deleted`);
      },
    );
  }
</script>

{#if phase !== 'ready' || !session}
  <main class="sign-in">
    <div class="login-card">
      <div class="calendar-mark" aria-hidden="true"><span></span></div>
      <h1>Calendar</h1>
      {#if phase === 'loading'}
        <p class="muted">Loading…</p>
      {:else if phase === 'unavailable'}
        <p class="error" role="alert">Sign-in is not configured for this calendar. Ask the administrator to set up authentik.</p>
      {:else}
        <p class="muted">Sign in with your authentik account to manage events.</p>
        {#if loadError}<p class="error" role="alert">{loadError}</p>{/if}
        <a class="btn primary block" href="/auth/login">Sign in with authentik</a>
      {/if}
    </div>
  </main>
{:else}
  <header class="site-header">
    <div class="header-inner">
      <div class="brand">
        <div class="calendar-mark" aria-hidden="true"><span></span></div>
        <label class="visually-hidden" for="calendar-switcher">Calendar</label>
        <select id="calendar-switcher" class="switcher" value={calendarId} onchange={(event) => void openCalendar(event.currentTarget.value)}>
          <optgroup label="Shared">
            {#each session.calendars.filter((calendar) => !calendar.personal) as calendar (calendar.id)}
              <option value={calendar.id}>{calendar.name}</option>
            {/each}
          </optgroup>
          {#if session.calendars.some((calendar) => calendar.personal)}
            <optgroup label="My calendars">
              {#each session.calendars.filter((calendar) => calendar.personal) as calendar (calendar.id)}
                <option value={calendar.id}>{calendar.name}</option>
              {/each}
            </optgroup>
          {/if}
        </select>
        {#if !writable}<span class="badge">View only</span>{/if}
      </div>
      <div class="header-actions">
        <button class="small" onclick={openManager}>Calendars</button>
        <button class="small" onclick={openSubscription}>Subscribe</button>
        <span class="small muted user">{session.user.name}</span>
        <button class="small ghost" onclick={nextTheme} title="Theme: {theme.choice} (click to change)" aria-label="Change theme, currently {theme.choice}">{theme.choice === 'dark' ? '🌙' : theme.choice === 'light' ? '☀️' : '🖥️'}</button>
        <form method="post" action="/auth/logout"><button class="small" type="submit">Sign out</button></form>
      </div>
    </div>
  </header>

  <main class="workspace">
    <div class="toolbar">
      <h2>{monthLabel.format(month)}</h2>
      <div class="controls">
        <button class="today-button" onclick={() => select(dateKey(new Date()))}>Today</button>
        <div class="month-nav">
          <button aria-label="Previous month" onclick={() => navigate(-1)}><span class="chevron previous" aria-hidden="true"></span></button>
          <button aria-label="Next month" onclick={() => navigate(1)}><span class="chevron next" aria-hidden="true"></span></button>
        </div>
        {#if writable}<button class="primary" onclick={() => openEditor()}>New event</button>{/if}
      </div>
    </div>
    {#if loadError}<p class="error" role="alert">{loadError}</p>{/if}

    <div class="workspace-grid">
      <section class="month-panel" aria-label={monthLabel.format(month)}>
        <div class="weekdays" aria-hidden="true">
          {#each weekdays as day (day)}<span>{day}</span>{/each}
        </div>
        <div
          class="month-grid"
          class:dragging={range}
          class:creatable={writable}
          bind:this={grid}
        >
          {#each visibleDays as day (day.key)}
            <div
              class="day"
              data-day={day.key}
              class:outside={!day.inMonth}
              class:selected={day.key === selectedDay}
              class:in-range={range && day.key >= range[0] && day.key <= range[1]}
            >
              <button
                class="day-number"
                class:current={day.key === today}
                aria-pressed={day.key === selectedDay}
                aria-current={day.key === today ? 'date' : undefined}
                aria-label={`${dayLabel.format(localDate(day.key))}, ${day.events.length} ${day.events.length === 1 ? 'event' : 'events'}`}
                onclick={() => select(day.key)}
              >{Number(day.key.slice(-2))}</button>
              {#each day.events.slice(0, 2) as event (event.id)}
                <button
                  class="event-chip"
                  class:all-day={event.all_day}
                  class:cancelled={event.status === 'cancelled'}
                  title={event.title}
                  onclick={() => { select(day.key); openEditor(event); }}
                >
                  <span class="event-dot" aria-hidden="true"></span>{event.title}
                </button>
              {/each}
              {#if day.events.length > 2}
                <button class="overflow" onclick={() => select(day.key)}>+{day.events.length - 2} more</button>
              {/if}
            </div>
          {/each}
        </div>
      </section>

      <aside class="agenda" aria-label="Daily agenda">
        <header>
          <div>
            <h3>{dayLabel.format(localDate(selectedDay))}</h3>
            <p>{timeZone.replaceAll('_', ' ')}</p>
          </div>
          {#if writable}
            <button class="small" aria-label="Add an event on this day" onclick={() => openEditor()}>Add</button>
          {/if}
        </header>
        {#if agenda.length === 0}
          <p class="empty">No events scheduled.</p>
        {:else}
          <ul>
            {#each agenda as event (event.id)}
              <li>
                <button class="agenda-event" class:cancelled={event.status === 'cancelled'} onclick={() => openEditor(event)}>
                  <span class="agenda-time">{eventHours(event, selectedDay)}</span>
                  <strong>{event.title}</strong>
                  {#if event.location}<span class="agenda-location">{event.location}</span>{/if}
                </button>
              </li>
            {/each}
          </ul>
        {/if}
        {#if writable}
          <p class="hint">Click a day to add an event, or drag across days for a multi-day event.</p>
        {/if}
      </aside>
    </div>
  </main>

  <dialog bind:this={editor} aria-labelledby="editor-title" oncancel={(event) => { if (saving) event.preventDefault(); }}>
    <h2 id="editor-title">{!writable ? 'Event' : editing ? 'Edit event' : 'New event'}</h2>
    <p class="dialog-calendar">{current?.name}</p>
    <form onsubmit={saveEvent}>
      <fieldset disabled={!writable}>
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
              <input id="event-end" type="date" bind:value={form.end} min={form.start} required />
            {:else}
              <input id="event-end" type="datetime-local" bind:value={form.end} required />
            {/if}
          </div>
        </div>
        <label for="event-location">Location</label>
        <input id="event-location" bind:value={form.location} maxlength="250" />
        <label for="event-description">Description</label>
        <textarea id="event-description" bind:value={form.description} maxlength="4000" rows="3"></textarea>
      </fieldset>
      {#if editorError}<p class="error" role="alert">{editorError}</p>{/if}
      {#if !writable}
        <div class="dialog-actions">
          <button type="button" onclick={() => editor?.close()}>Close</button>
        </div>
      {:else if confirmDelete}
        <p class="delete-prompt">Delete this event from the calendar?</p>
        <div class="dialog-actions">
          <button type="button" disabled={saving} onclick={() => { confirmDelete = false; }}>Keep event</button>
          <button type="button" class="danger" disabled={saving} onclick={deleteEvent}>Delete event</button>
        </div>
      {:else}
        <div class="dialog-actions">
          {#if editing}<button type="button" class="ghost delete" disabled={saving} onclick={() => { confirmDelete = true; }}>Delete</button>{/if}
          <button type="button" disabled={saving} onclick={() => editor?.close()}>Cancel</button>
          <button type="submit" class="primary" disabled={saving}>{saving ? 'Saving…' : editing ? 'Save changes' : 'Create event'}</button>
        </div>
      {/if}
    </form>
  </dialog>

  <dialog bind:this={subscription} aria-labelledby="subscribe-title">
    <h2 id="subscribe-title">Subscribe to {current?.name}</h2>
    <p>Add this read-only link in Google Calendar or Apple Calendar. Anyone with the link can view the feed.</p>
    <label for="feed-url">Subscription URL</label>
    <input id="feed-url" type="url" readonly value={feedUrl} bind:this={feedInput} />
    <div class="dialog-actions">
      <button onclick={() => subscription?.close()}>Done</button>
      <button class="primary" onclick={() => { if (feedInput) void copyFeed(feedInput); }}>{copied ? 'Copied' : 'Copy link'}</button>
    </div>
  </dialog>

  <dialog bind:this={manager} aria-labelledby="manager-title" class="manager">
    <h2 id="manager-title">Calendars</h2>
    <p>Only you can see your own calendars. The shared calendar is visible to everyone who signs in.</p>
    <ul class="calendar-list">
      {#each session.calendars as calendar (calendar.id)}
        <li>
          {#if renaming?.id === calendar.id}
            <form class="rename" onsubmit={renameCalendar}>
              <label class="visually-hidden" for="rename-{calendar.id}">Calendar name</label>
              <input id="rename-{calendar.id}" bind:value={renaming.name} maxlength="80" required />
              <button type="submit" class="small primary" disabled={managerBusy}>Save</button>
              <button type="button" class="small" disabled={managerBusy} onclick={() => { renaming = undefined; }}>Cancel</button>
            </form>
          {:else if removing === calendar.id}
            <span class="calendar-name">Delete “{calendar.name}” and all its events?</span>
            <button class="small" disabled={managerBusy} onclick={() => { removing = undefined; }}>Keep</button>
            <button class="small danger" disabled={managerBusy} onclick={() => deleteCalendar(calendar.id)}>Delete</button>
          {:else}
            <button class="calendar-name link" onclick={() => { void openCalendar(calendar.id); manager?.close(); }}>
              {calendar.name}
              {#if calendar.id === calendarId}<span class="badge accent">Open</span>{/if}
            </button>
            {#if calendar.personal}
              <button class="small ghost" disabled={managerBusy} onclick={() => { renaming = { id: calendar.id, name: calendar.name }; removing = undefined; }}>Rename</button>
              <button class="small ghost delete" disabled={managerBusy} onclick={() => { removing = calendar.id; renaming = undefined; }}>Delete</button>
            {:else}
              <span class="badge">{calendar.writable ? 'Shared' : 'Shared · view only'}</span>
            {/if}
          {/if}
        </li>
      {/each}
    </ul>
    <form class="new-calendar" onsubmit={createCalendar}>
      <label for="new-calendar">New calendar</label>
      <div class="row">
        <input id="new-calendar" bind:value={newCalendarName} maxlength="80" placeholder="e.g. Work" required />
        <button type="submit" class="primary" disabled={managerBusy}>Create</button>
      </div>
    </form>
    {#if managerError}<p class="error" role="alert">{managerError}</p>{/if}
    <div class="dialog-actions">
      <button onclick={() => manager?.close()}>Done</button>
    </div>
  </dialog>
{/if}

{#if toast}
  {#key toast.id}
    <Snackbar message={toast.message} onclose={() => { toast = undefined; }} />
  {/key}
{/if}
