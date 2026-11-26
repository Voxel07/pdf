import { expect, test } from 'bun:test';
import { createHmac } from 'node:crypto';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
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
