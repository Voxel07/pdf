import { Database } from 'bun:sqlite';
import ical, { ICalEventStatus } from 'ical-generator';
import type { CalendarEvent, EventInput } from '../src/lib/events';

export class HttpError extends Error {
  constructor(public status: number, message: string, public code?: string) {
    super(message);
  }
}

function field(value: unknown, name: string, limit: number) {
  if (typeof value !== 'string' || [...value].length > limit || /[\x00-\x08\x0B\x0C\x0E-\x1F]/.test(value)) {
    throw new HttpError(400, `${name} must be text of at most ${limit} characters.`);
  }
  return value.trim().replace(/\r\n?/g, '\n');
}

function validDate(value: string) {
  const date = new Date(value + 'T00:00:00Z');
  return /^\d{4}-\d{2}-\d{2}$/.test(value) && !isNaN(+date) && date.toISOString().startsWith(value);
}

function eventDate(value: unknown, allDay: boolean, name: string) {
  if (typeof value === 'string') {
    if (allDay && validDate(value)) return value;
    const timestamp = /^\d{4}-\d{2}-\d{2}T(?:[01]\d|2[0-3]):[0-5]\d:[0-5]\d(?:\.\d{1,9})?(?:Z|[+-](?:[01]\d|2[0-3]):[0-5]\d)$/i;
    if (!allDay && timestamp.test(value) && validDate(value.slice(0, 10))) {
      const utc = new Date(value).toISOString();
      if (utc.length === 24) return utc;
    }
  }
  throw new HttpError(400, `${name} must be ${allDay ? 'a date (YYYY-MM-DD)' : 'an ISO timestamp with seconds and a time zone'}.`);
}

export function parseEvent(raw: unknown, defaults?: Partial<EventInput>): Required<EventInput> {
  if (!raw || typeof raw !== 'object' || Array.isArray(raw)) {
    throw new HttpError(400, 'Expected an event object.');
  }
  const input = { all_day: false, description: '', location: '', status: 'scheduled', ...defaults, ...raw } as Record<string, unknown>;
  const title = field(input.title, 'Title', 160);
  if (!title) throw new HttpError(400, 'Title is required.');
  if (typeof input.all_day !== 'boolean') throw new HttpError(400, 'all_day must be a boolean.');
  if (input.status !== 'scheduled' && input.status !== 'cancelled') throw new HttpError(400, 'Invalid event status.');
  const startsAt = eventDate(input.starts_at, input.all_day, 'Start');
  const endsAt = eventDate(input.ends_at, input.all_day, 'End');
  if (endsAt <= startsAt) throw new HttpError(400, 'End must be after start.');
  return {
    title,
    description: field(input.description, 'Description', 4000),
    location: field(input.location, 'Location', 250),
    starts_at: startsAt,
    ends_at: endsAt,
    all_day: input.all_day,
    status: input.status,
  };
}

type StoredEvent = Omit<CalendarEvent, 'all_day'> & { all_day: number; deleted_at: string | null };
type Phase = 'upcoming' | 'ongoing' | 'ended' | 'cancelled';
type Rsvp = 'going' | 'maybe' | 'declined';
export type Scope = 'read' | 'write' | 'calendar';
type EventDetail = StoredEvent & { phase: Phase; going: number; maybe: number; declined: number; my_rsvp: Rsvp | null };
type WebhookType = 'event.created' | 'event.updated' | 'event.deleted';

export interface Delivery {
  id: string;
  type: WebhookType;
  body: string;
  attempts: number;
}

function publicEvent(row: StoredEvent): CalendarEvent {
  return {
    id: row.id,
    title: row.title,
    description: row.description,
    location: row.location,
    starts_at: row.starts_at,
    ends_at: row.ends_at,
    all_day: Boolean(row.all_day),
    status: row.status,
    revision: row.revision,
    created_at: row.created_at,
    updated_at: row.updated_at,
  };
}

function pdfsignEvent(row: EventDetail) {
  return {
    ...publicEvent(row),
    starts_at: row.starts_at.replace(/\.000Z$/, 'Z'),
    ends_at: row.ends_at.replace(/\.000Z$/, 'Z'),
    phase: row.phase,
    going: row.going,
    maybe: row.maybe,
    declined: row.declined,
    my_rsvp: row.my_rsvp ?? undefined,
  };
}

export function openCalendar(filename: string, webhookEnabled: boolean) {
  const db = new Database(filename, { strict: true });
  db.exec(`
    PRAGMA journal_mode = WAL;
    PRAGMA busy_timeout = 5000;
    PRAGMA foreign_keys = ON;
    CREATE TABLE IF NOT EXISTS events (
      id TEXT PRIMARY KEY,
      title TEXT NOT NULL,
      description TEXT NOT NULL,
      location TEXT NOT NULL,
      starts_at TEXT NOT NULL,
      ends_at TEXT NOT NULL,
      all_day INTEGER NOT NULL CHECK (all_day IN (0, 1)),
      status TEXT NOT NULL DEFAULT 'scheduled' CHECK (status IN ('scheduled', 'cancelled')),
      revision INTEGER NOT NULL DEFAULT 0,
      created_at TEXT NOT NULL,
      updated_at TEXT NOT NULL,
      deleted_at TEXT
    );
    CREATE INDEX IF NOT EXISTS events_starts ON events(starts_at, id);
    CREATE TABLE IF NOT EXISTS create_requests (
      key TEXT PRIMARY KEY,
      request TEXT NOT NULL,
      response TEXT NOT NULL
    );
    CREATE TABLE IF NOT EXISTS webhook_outbox (
      id TEXT PRIMARY KEY,
      type TEXT NOT NULL,
      body TEXT NOT NULL,
      attempts INTEGER NOT NULL DEFAULT 0,
      next_attempt INTEGER
    );
    CREATE INDEX IF NOT EXISTS webhook_due ON webhook_outbox(next_attempt);
  `);
  db.transaction(() => {
    db.exec(`
      CREATE TABLE IF NOT EXISTS event_rsvps (
        event_id TEXT NOT NULL REFERENCES events(id),
        user_id TEXT NOT NULL,
        response TEXT NOT NULL CHECK (response IN ('going', 'maybe', 'declined')),
        PRIMARY KEY (event_id, user_id)
      );
      CREATE TABLE IF NOT EXISTS event_credentials (
        id TEXT PRIMARY KEY NOT NULL,
        name TEXT NOT NULL,
        scope TEXT NOT NULL CHECK (scope IN ('read', 'write', 'calendar')),
        token_hash TEXT NOT NULL UNIQUE,
        created_at TEXT NOT NULL,
        created_by TEXT NOT NULL
      );
      CREATE TABLE IF NOT EXISTS pdfsign_create_requests (
        actor TEXT NOT NULL,
        key TEXT NOT NULL,
        fingerprint TEXT NOT NULL,
        event_id TEXT NOT NULL REFERENCES events(id),
        PRIMARY KEY (actor, key)
      );
      CREATE VIEW IF NOT EXISTS event_details AS
      SELECT events.*,
        CASE WHEN status = 'cancelled' THEN 'cancelled'
          WHEN julianday(ends_at) <= julianday('now') THEN 'ended'
          WHEN julianday(starts_at) <= julianday('now') THEN 'ongoing'
          ELSE 'upcoming' END AS phase,
        (SELECT count(*) FROM event_rsvps WHERE event_id = events.id AND response = 'going') AS going,
        (SELECT count(*) FROM event_rsvps WHERE event_id = events.id AND response = 'maybe') AS maybe,
        (SELECT count(*) FROM event_rsvps WHERE event_id = events.id AND response = 'declined') AS declined
      FROM events;
    `);
  }).immediate();

  const find = db.query<StoredEvent, [string]>('SELECT * FROM events WHERE id = ? AND deleted_at IS NULL');
  const list = db.query<StoredEvent, []>('SELECT * FROM events WHERE deleted_at IS NULL ORDER BY starts_at, id');
  const feed = db.query<StoredEvent, [string]>('SELECT * FROM events WHERE deleted_at IS NULL OR deleted_at >= ? ORDER BY starts_at, id');
  const enqueue = db.query('INSERT INTO webhook_outbox (id, type, body, next_attempt) VALUES (?, ?, ?, ?)');
  const nextDelivery = db.query<Delivery, [number]>(`
    SELECT id, type, body, attempts FROM webhook_outbox
    WHERE next_attempt <= ? ORDER BY next_attempt, rowid LIMIT 1
  `);
  const delivered = db.query('DELETE FROM webhook_outbox WHERE id = ?');
  const retry = db.query('UPDATE webhook_outbox SET attempts = ?, next_attempt = ? WHERE id = ?');
  const findRequest = db.query<{ request: string; response: string }, [string]>(
    'SELECT request, response FROM create_requests WHERE key = ?',
  );
  const saveRequest = db.query('INSERT INTO create_requests (key, request, response) VALUES (?, ?, ?)');
  const insert = db.query(`
    INSERT INTO events (id, title, description, location, starts_at, ends_at, all_day, status, created_at, updated_at)
    VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
  `);
  const update = db.query(`
    UPDATE events SET title = ?, description = ?, location = ?, starts_at = ?, ends_at = ?, all_day = ?,
      status = ?, revision = ?, updated_at = ? WHERE id = ?
  `);
  const remove = db.query('UPDATE events SET deleted_at = ?, updated_at = ?, revision = ? WHERE id = ?');

  function get(id: string) {
    const event = find.get(id);
    if (!event) throw new HttpError(404, 'Event not found.');
    return publicEvent(event);
  }

  function requireRevision(event: CalendarEvent, expected: string | null) {
    if (expected !== null && expected !== `"${event.revision}"`) {
      throw new HttpError(412, 'This event has changed. Reload it before saving.');
    }
  }

  function recordChange(type: WebhookType, event: CalendarEvent) {
    if (!webhookEnabled) return;
    const id = crypto.randomUUID();
    const body = JSON.stringify({ id, type, occurred_at: event.updated_at, event });
    enqueue.run(id, type, body, Date.now());
  }

  const detail = db.query<EventDetail, [string | null, string]>(`
    SELECT event_details.*, (SELECT response FROM event_rsvps WHERE event_id = id AND user_id = ?1) AS my_rsvp
    FROM event_details WHERE id = ?2 AND deleted_at IS NULL
  `);
  function getDetail(id: string, user: string | null = null) {
    const event = detail.get(user, id);
    if (!event) throw new HttpError(404, 'Event not found.', 'not_found');
    return pdfsignEvent(event);
  }

  const findPdfsignRequest = db.query<{ fingerprint: string; event_id: string }, [string, string]>(
    'SELECT fingerprint, event_id FROM pdfsign_create_requests WHERE actor = ? AND key = ?',
  );
  const savePdfsignRequest = db.query('INSERT INTO pdfsign_create_requests VALUES (?, ?, ?, ?)');

  const create = db.transaction((input: ReturnType<typeof parseEvent>, key: string | null, actor?: string, fingerprint?: string) => {
    if (key && actor) {
      const previous = findPdfsignRequest.get(actor, key);
      if (previous) {
        if (previous.fingerprint !== fingerprint) {
          throw new HttpError(409, 'This idempotency key already belongs to another request.', 'idempotency_conflict');
        }
        return { event: get(previous.event_id), replay: true };
      }
    } else if (key) {
      const previous = findRequest.get(key);
      if (previous) {
        const previousInput = parseEvent(JSON.parse(previous.request));
        if (JSON.stringify(previousInput) !== JSON.stringify(input)) {
          throw new HttpError(409, 'This Idempotency-Key was already used for a different event.');
        }
        return { event: { status: 'scheduled', ...JSON.parse(previous.response) } as CalendarEvent, replay: true };
      }
    }
    const id = crypto.randomUUID();
    const now = new Date().toISOString();
    insert.run(id, input.title, input.description, input.location, input.starts_at, input.ends_at,
      Number(input.all_day), input.status, now, now);
    const event = get(id);
    if (key && actor) savePdfsignRequest.run(actor, key, fingerprint!, id);
    else if (key) saveRequest.run(key, JSON.stringify(input), JSON.stringify(event));
    recordChange('event.created', event);
    return { event, replay: false };
  });

  const edit = db.transaction((id: string, raw: unknown, partial: boolean, expected: string | null, pdfsign = false) => {
    const current = get(id);
    if (pdfsign && expected !== `"${current.revision}"`) {
      throw new HttpError(409, 'This event was updated. Reload the event and try again.', 'stale_revision');
    }
    requireRevision(current, expected);
    const input = parseEvent(raw, partial ? current : pdfsign ? undefined : { status: current.status });
    const fields = Object.keys(input) as Array<keyof EventInput>;
    if (!pdfsign && fields.every((name) => input[name] === current[name])) return current;
    update.run(input.title, input.description, input.location, input.starts_at, input.ends_at,
      Number(input.all_day), input.status, current.revision + 1, new Date().toISOString(), id);
    const event = get(id);
    recordChange('event.updated', event);
    return event;
  });

  const erase = db.transaction((id: string, expected: string | null) => {
    const current = get(id);
    requireRevision(current, expected);
    const event = { ...current, revision: current.revision + 1, updated_at: new Date().toISOString() };
    remove.run(event.updated_at, event.updated_at, event.revision, id);
    recordChange('event.deleted', event);
  });

  const respond = db.transaction((id: string, user: string, response: Rsvp) => {
    const event = getDetail(id);
    if (event.phase !== 'upcoming' && event.phase !== 'ongoing') {
      throw new HttpError(409, 'RSVPs are closed for this event.', 'rsvp_closed');
    }
    db.query(`
      INSERT INTO event_rsvps (event_id, user_id, response) VALUES (?, ?, ?)
      ON CONFLICT (event_id, user_id) DO UPDATE SET response = excluded.response
    `).run(id, user, response);
    return getDetail(id, user);
  });

  return {
    list: () => list.all().map(publicEvent),
    get,
    create: create.immediate,
    edit: edit.immediate,
    erase: erase.immediate,
    getDetail,
    page(phase: string | null, limit: number, offset: number) {
      const rows = db.query<EventDetail, [string | null, number, number]>(`
        SELECT *, NULL AS my_rsvp FROM event_details WHERE deleted_at IS NULL AND (?1 IS NULL OR phase = ?1)
        ORDER BY CASE WHEN phase IN ('upcoming', 'ongoing') THEN 0 ELSE 1 END,
          CASE WHEN phase IN ('upcoming', 'ongoing') THEN starts_at END, starts_at DESC, id LIMIT ?2 OFFSET ?3
      `).all(phase, limit + 1, offset);
      return { events: rows.slice(0, limit).map(pdfsignEvent), next_offset: rows.length > limit ? offset + limit : null };
    },
    respond: respond.immediate,
    findCredential: (hash: string) => db.query<{ id: string; scope: Scope }, [string]>(
      'SELECT id, scope FROM event_credentials WHERE token_hash = ?',
    ).get(hash),
    listCredentials: () => db.query('SELECT id, name, scope, created_at FROM event_credentials ORDER BY created_at DESC, id').all(),
    saveCredential: (id: string, name: string, scope: Scope, hash: string, createdAt: string, createdBy: string) =>
      db.query('INSERT INTO event_credentials VALUES (?, ?, ?, ?, ?, ?)').run(id, name, scope, hash, createdAt, createdBy),
    revokeCredential: (id: string) => db.query('DELETE FROM event_credentials WHERE id = ?').run(id).changes,
    renderFeed(name: string, pdfsignUrl?: string) {
      const calendar = ical({ name, prodId: pdfsignUrl ? '-//pdfsign//Events//EN' : '-//DIY Calendar//Calendar//EN' });
      const retainedSince = new Date(Date.now() - 90 * 86_400_000).toISOString();
      for (const row of feed.all(retainedSince)) {
        if (pdfsignUrl && Date.parse(row.ends_at) < Date.parse(retainedSince) && !row.deleted_at) continue;
        calendar.createEvent({
          id: `${row.id}@${pdfsignUrl ? 'pdfsign' : 'diy-calendar'}`,
          start: new Date(row.starts_at),
          end: new Date(row.ends_at),
          allDay: Boolean(row.all_day),
          summary: row.title,
          description: row.description || undefined,
          location: row.location || undefined,
          stamp: new Date(row.updated_at),
          lastModified: new Date(row.updated_at),
          sequence: row.revision,
          status: row.deleted_at || row.status === 'cancelled' ? ICalEventStatus.CANCELLED : ICalEventStatus.CONFIRMED,
          url: pdfsignUrl ? `${pdfsignUrl}/admin/events/${row.id}` : undefined,
        });
      }
      return calendar.toString();
    },
    nextDelivery: (now: number) => nextDelivery.get(now),
    delivered: (id: string) => delivered.run(id),
    retry: (id: string, attempts: number, when: number | null) => retry.run(attempts, when, id),
    snapshot: (path: string) => db.query('VACUUM INTO ?').run(path),
    close: () => db.close(),
  };
}

export type CalendarStore = ReturnType<typeof openCalendar>;
