import { expect, test } from 'bun:test';
import { createHmac } from 'node:crypto';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { Database } from 'bun:sqlite';
import { openCalendar, parseEvent, SHARED } from './events';
import { createCalendarServer } from './index';

test('event writes update the subscription and persist webhook retries across restart', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'diy-calendar-test-'));
  const secret = 'test-webhook-secret';
  const messages: Array<{ headers: Headers; body: string }> = [];
  const receiver = Bun.serve({
    port: 0,
    hostname: '127.0.0.1',
    async fetch(request) {
      messages.push({ headers: request.headers, body: await request.text() });
      return new Response(null, { status: messages.length === 1 ? 503 : 204 });
    },
  });
  const config = {
    adminToken: 'test-admin-token',
    feedToken: 'test-feed-token',
    name: 'Integration test',
    webhook: { url: receiver.url, bearer: 'test-receiver-token', secret },
  };
  const start = () => createCalendarServer(config, join(dir, 'test.sqlite'), dir, 0, '127.0.0.1');
  let app = start();
  const api = (path: string, options: RequestInit = {}) => {
    const headers = new Headers(options.headers);
    headers.set('Authorization', 'Bearer ' + config.adminToken);
    if (options.body) headers.set('Content-Type', 'application/json');
    return fetch(new URL('/api/v1' + path, app.server.url), { ...options, headers });
  };
  const rawFeed = () => fetch(new URL('/feeds/' + config.feedToken + '.ics', app.server.url)).then((r) => r.text());
  const waitFor = async (count: number) => {
    const deadline = Date.now() + 10_000;
    while (messages.length < count && Date.now() < deadline) await Bun.sleep(45);
    expect(messages.length).toBeGreaterThanOrEqual(count);
  };
  try {
    const unauthenticated = await fetch(new URL('/api/v1/events', app.server.url));
    expect(unauthenticated.status).toBe(401);
    expect((await fetch(new URL('/feeds/missing.ics', app.server.url))).status).toBe(404);
    expect((await apiInternalHealth(app.server.url, config.adminToken)).status).toBe(404);

    const body = JSON.stringify({
      title: 'Planning, team',
      starts_at: '2027-03-28',
      ends_at: '2027-03-29',
      all_day: true,
    });
    const create = () => api('/events', { method: 'POST', body, headers: { 'Idempotency-Key': 'create-once' } });
    const created = await create();
    expect(created.status).toBe(201);
    const { id } = await created.json();
    const replay = await create();
    expect(replay.status).toBe(200);
    expect((await replay.json()).id).toBe(id);
    expect((await (await api('/events')).json()).events).toHaveLength(1);
    const originalFeed = await rawFeed();
    expect(originalFeed).toContain('DTSTART;VALUE=DATE:20270328');
    expect(originalFeed).toContain('DTEND;VALUE=DATE:20270329');
    expect(originalFeed).toContain(`UID:${id}@diy-calendar`);
    expect(originalFeed).toContain('SUMMARY:Planning\\, team');

    await waitFor(1);
    await app.stop();
    app = start();
    expect((await api('/events/' + id)).status).toBe(200);
    await waitFor(2);

    const first = messages[0];
    const retry = messages[1];
    expect(retry.body).toBe(first.body);
    expect(retry.headers.get('X-Calendar-Delivery')).toBe(first.headers.get('X-Calendar-Delivery'));
    expect(retry.headers.get('Authorization')).toBe('Bearer test-receiver-token');
    const stamp = retry.headers.get('X-Calendar-Timestamp');
    const signature = createHmac('sha256', secret).update(`${stamp}.${retry.body}`).digest('hex');
    expect(retry.headers.get('X-Calendar-Signature')).toBe('sha256=' + signature);

    const patch = (revision: string) => api('/events/' + id, {
      method: 'PATCH',
      headers: { 'If-Match': revision },
      body: JSON.stringify({ title: 'Updated planning' }),
    });
    const updated = await patch('"0"');
    expect(updated.headers.get('ETag')).toBe('"1"');
    expect((await patch('"0"')).status).toBe(412);
    expect((await patch('"1"')).headers.get('ETag')).toBe('"1"');
    const updatedFeed = await rawFeed();
    expect(updatedFeed).toContain(`UID:${id}@diy-calendar`);
    expect(updatedFeed).toContain('SEQUENCE:1');
    expect(updatedFeed).toContain('SUMMARY:Updated planning');

    expect((await api('/events/' + id, { method: 'DELETE' })).status).toBe(204);
    expect((await api('/events/' + id)).status).toBe(404);
    const deletedFeed = await rawFeed();
    expect(deletedFeed).toContain(`UID:${id}@diy-calendar`);
    expect(deletedFeed).toContain('STATUS:CANCELLED');
    expect(deletedFeed).toContain('SEQUENCE:2');

    await waitFor(4);
    expect(messages.map(({ body }) => JSON.parse(body).type)).toEqual([
      'event.created', 'event.created', 'event.updated', 'event.deleted',
    ]);
  } finally {
    await app.stop();
    await receiver.stop();
    rmSync(dir, { recursive: true, force: true });
  }
}, 20_000);

function apiInternalHealth(url: URL, token: string) {
  return fetch(new URL('/internal/pdfsign/health', url), { headers: { Authorization: 'Bearer ' + token } });
}

test('existing events move to the shared calendar and pdfsign never sees personal events', () => {
  const dir = mkdtempSync(join(tmpdir(), 'diy-calendar-upgrade-'));
  const file = join(dir, 'calendar.sqlite');
  try {
    // The events table as created before per-user calendars.
    const old = new Database(file);
    old.exec(`CREATE TABLE events (id TEXT PRIMARY KEY, title TEXT NOT NULL, description TEXT NOT NULL, location TEXT NOT NULL,
      starts_at TEXT NOT NULL, ends_at TEXT NOT NULL, all_day INTEGER NOT NULL, status TEXT NOT NULL DEFAULT 'scheduled',
      revision INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL, updated_at TEXT NOT NULL, deleted_at TEXT)`);
    old.query(`INSERT INTO events VALUES ('00000000-0000-4000-8000-000000000001', 'Old', '', '', '2099-01-01', '2099-01-02', 1,
      'scheduled', 0, '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z', NULL)`).run();
    old.close();

    const store = openCalendar(file, true);
    try {
      expect(store.list().map((event) => event.title)).toEqual(['Old']);
      store.recordLogin('user', 'User');
      const [personal] = store.ownCalendars('user');
      const input = parseEvent({ title: 'Private', starts_at: '2099-01-01', ends_at: '2099-01-02', all_day: true });
      const { event } = store.create(input, null, personal.id);
      expect(store.page(null, 10, 0).events.map((row) => row.title)).toEqual(['Old']);
      expect(() => store.getDetail(event.id)).toThrow('Event not found.');
      expect(() => store.edit(event.id, SHARED, { ...input, revision: 0 }, false, '"0"', true)).toThrow('Event not found.');
      expect(store.renderFeed('pdfsign events', 'https://sign.example.com')).not.toContain('Private');
      // only the shared calendar feeds webhooks
      expect(store.nextDelivery(Date.now() + 1000)).toBeNull();
      store.snapshot(join(dir, 'copy.sqlite'));
    } finally {
      store.close();
    }
    const copy = new Database(join(dir, 'copy.sqlite'));
    expect(copy.query('SELECT count(*) AS n FROM ui_sessions').get()).toEqual({ n: 0 });
    copy.close();
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
