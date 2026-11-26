import { afterAll, beforeAll, expect, test } from 'bun:test';
import { createHash } from 'node:crypto';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createCalendarServer } from './index';

// A minimal OpenID provider: /authorize is skipped by the test, which calls the callback itself.
const grants = new Map<string, { nonce: string; challenge: string; user: Record<string, unknown> }>();
const provider = Bun.serve({
  port: 0,
  hostname: '127.0.0.1',
  async fetch(request): Promise<Response> {
    const url = new URL(request.url);
    const issuer: string = `${provider.url.origin}/o/calendar/`;
    if (url.pathname === '/o/calendar/.well-known/openid-configuration') {
      return Response.json({
        issuer,
        authorization_endpoint: `${provider.url.origin}/o/authorize`,
        token_endpoint: `${provider.url.origin}/o/token`,
        userinfo_endpoint: `${provider.url.origin}/o/userinfo`,
        end_session_endpoint: `${provider.url.origin}/o/end-session`,
      });
    }
    if (url.pathname === '/o/token' && request.method === 'POST') {
      if (request.headers.get('Authorization') !== 'Basic ' + btoa('calendar-client:client-secret')) {
        return Response.json({ error: 'invalid_client' }, { status: 401 });
      }
      const form = new URLSearchParams(await request.text());
      const grant = grants.get(form.get('code') ?? '');
      const verifier = form.get('code_verifier') ?? '';
      if (!grant || createHash('sha256').update(verifier).digest('base64url') !== grant.challenge) {
        return Response.json({ error: 'invalid_grant' }, { status: 400 });
      }
      const part = (value: object) => Buffer.from(JSON.stringify(value)).toString('base64url');
      const idToken = [part({ alg: 'RS256' }), part({
        iss: issuer, aud: 'calendar-client', sub: grant.user.sub, nonce: grant.nonce, exp: Math.floor(Date.now() / 1000) + 300,
      }), 'signature'].join('.');
      return Response.json({ access_token: 'access:' + form.get('code'), id_token: idToken, token_type: 'Bearer' });
    }
    if (url.pathname === '/o/userinfo') {
      const code = request.headers.get('Authorization')?.replace('Bearer access:', '') ?? '';
      const grant = grants.get(code);
      return grant ? Response.json(grant.user) : Response.json({ error: 'invalid_token' }, { status: 401 });
    }
    return new Response('not found', { status: 404 });
  },
});

let dir: string;
let app: ReturnType<typeof createCalendarServer>;
const feedToken = 'test-feed-token';
const adminToken = 'test-admin-token';

beforeAll(() => {
  dir = mkdtempSync(join(tmpdir(), 'diy-calendar-auth-'));
  app = createCalendarServer({
    adminToken,
    feedToken,
    name: 'Team',
    publicUrl: 'http://127.0.0.1:1',
    oidc: {
      issuer: `${provider.url.origin}/o/calendar/`,
      clientId: 'calendar-client',
      clientSecret: 'client-secret',
      scopes: 'openid profile email',
      group: 'calendar-users',
      adminGroup: 'calendar-admins',
    },
  }, join(dir, 'calendar.sqlite'), dir, 0, '127.0.0.1');
});

afterAll(async () => {
  await app.stop();
  await provider.stop();
  rmSync(dir, { recursive: true, force: true });
});

const at = (path: string) => new URL(path, app.server.url);

async function signIn(user: Record<string, unknown>) {
  const login = await fetch(at('/auth/login'), { redirect: 'manual' });
  expect(login.status).toBe(302);
  const authorize = new URL(login.headers.get('Location')!);
  expect(authorize.pathname).toBe('/o/authorize');
  expect(authorize.searchParams.get('code_challenge_method')).toBe('S256');
  const state = authorize.searchParams.get('state')!;
  const stateCookie = login.headers.get('Set-Cookie')!.split(';')[0];
  const code = crypto.randomUUID();
  grants.set(code, { nonce: authorize.searchParams.get('nonce')!, challenge: authorize.searchParams.get('code_challenge')!, user });
  return fetch(at(`/auth/callback?code=${code}&state=${state}`), { redirect: 'manual', headers: { Cookie: stateCookie } });
}

async function session(user: Record<string, unknown>) {
  const callback = await signIn(user);
  expect(callback.status).toBe(303);
  const cookie = callback.headers.getSetCookie().find((value) => value.startsWith('calendar_session='))!.split(';')[0];
  const api = (path: string, init: RequestInit = {}) => {
    const headers = new Headers(init.headers);
    headers.set('Cookie', cookie);
    if (init.body) headers.set('Content-Type', 'application/json');
    return fetch(at('/api/v1' + path), { ...init, headers });
  };
  return Object.assign(api, { cookie });
}

const event = (title: string) => JSON.stringify({ title, starts_at: '2027-05-03', ends_at: '2027-05-06', all_day: true });

test('signing in requires the configured group and a callback from the same browser', async () => {
  expect((await fetch(at('/api/v1/session'))).status).toBe(401);
  expect(await (await fetch(at('/api/v1/session'))).json()).toMatchObject({ sign_in: true });
  const outsider = await signIn({ sub: 'outsider', name: 'Outsider', groups: ['other'] });
  expect(outsider.status).toBe(403);
  expect(outsider.headers.getSetCookie().some((value) => value.startsWith('calendar_session='))).toBe(false);

  const login = await fetch(at('/auth/login'), { redirect: 'manual' });
  const state = new URL(login.headers.get('Location')!).searchParams.get('state');
  const stolen = await fetch(at(`/auth/callback?code=x&state=${state}`), { redirect: 'manual' });
  expect(stolen.status).toBe(400);
});

test('each user has their own calendars and the shared calendar follows the admin group', async () => {
  const alice = await session({ sub: 'alice', name: 'Alice', groups: ['calendar-users', 'calendar-admins'] });
  const bob = await session({ sub: 'bob', preferred_username: 'bob', groups: ['calendar-users'] });

  const aliceSession = await (await alice('/session')).json();
  expect(aliceSession.user).toEqual({ name: 'Alice', shared_editor: true });
  expect(aliceSession.calendars.map((c: { name: string }) => c.name)).toEqual(['Team', 'Personal']);
  const bobSession = await (await bob('/session')).json();
  expect(bobSession.calendars[0]).toMatchObject({ id: 'shared', writable: false });
  const alicePersonal = aliceSession.calendars[1].id;
  const bobPersonal = bobSession.calendars[1].id;

  // Personal events stay private: other users and the admin token cannot read or change them.
  const created = await alice(`/events?calendar=${alicePersonal}`, { method: 'POST', body: event('Holiday') });
  expect(created.status).toBe(201);
  const { id } = await created.json();
  expect((await (await alice(`/events?calendar=${alicePersonal}`)).json()).events).toHaveLength(1);
  expect((await (await alice('/events?calendar=shared')).json()).events).toHaveLength(0);
  expect((await bob(`/events?calendar=${alicePersonal}`)).status).toBe(404);
  expect((await bob('/events/' + id)).status).toBe(404);
  expect((await bob('/events/' + id, { method: 'DELETE' })).status).toBe(404);
  const admin = { Authorization: 'Bearer ' + adminToken };
  expect((await fetch(at('/api/v1/events/' + id), { headers: admin })).status).toBe(404);
  expect((await (await fetch(at('/api/v1/events'), { headers: admin })).json()).events).toHaveLength(0);

  // Bob can read the shared calendar but only an admin-group member can write to it.
  expect((await bob('/events?calendar=shared', { method: 'POST', body: event('Party') })).status).toBe(403);
  const shared = await alice('/events?calendar=shared', { method: 'POST', body: event('Party') });
  expect(shared.status).toBe(201);
  expect((await (await bob('/events?calendar=shared')).json()).events).toHaveLength(1);
  expect((await (await fetch(at('/api/v1/events'), { headers: admin })).json()).events).toHaveLength(1);
  expect((await bob(`/events?calendar=${bobPersonal}`, { method: 'POST', body: event('Dentist') })).status).toBe(201);

  // Other sites cannot use the session cookie to write.
  const crossSite = await alice(`/events?calendar=${alicePersonal}`, {
    method: 'POST', body: event('Forged'), headers: { 'Sec-Fetch-Site': 'cross-site' },
  });
  expect(crossSite.status).toBe(403);

  // Each calendar has its own feed, which stops working once the calendar is deleted.
  const feedUrl = new URL(aliceSession.calendars[1].feed_url);
  const feed = await fetch(at(feedUrl.pathname));
  expect(feed.status).toBe(200);
  expect(await feed.text()).toContain('SUMMARY:Holiday');
  expect((await fetch(at(feedUrl.pathname.replace(/[^/]{5}\.ics$/, 'xxxxx.ics')))).status).toBe(404);
  expect(await (await fetch(at(`/feeds/${feedToken}.ics`))).text()).not.toContain('Holiday');

  const work = await alice('/calendars', { method: 'POST', body: JSON.stringify({ name: 'Work' }) });
  expect(work.status).toBe(201);
  const workId = (await work.json()).id;
  expect((await bob('/calendars/' + workId, { method: 'PATCH', body: JSON.stringify({ name: 'Mine' }) })).status).toBe(404);
  expect(await (await alice('/calendars/' + workId, { method: 'PATCH', body: JSON.stringify({ name: 'Office' }) })).json())
    .toMatchObject({ name: 'Office', personal: true });
  expect((await alice('/calendars/' + alicePersonal, { method: 'DELETE' })).status).toBe(204);
  expect((await fetch(at(feedUrl.pathname))).status).toBe(404);
  expect((await alice('/events/' + id)).status).toBe(404);
  expect((await (await alice('/calendars')).json()).calendars.map((c: { name: string }) => c.name)).toEqual(['Team', 'Office']);

  // Signing in again keeps the calendars a user chose to delete deleted.
  const again = await session({ sub: 'alice', name: 'Alice', groups: ['calendar-users'] });
  const later = await (await again('/session')).json();
  expect(later.calendars.map((c: { name: string }) => c.name)).toEqual(['Team', 'Office']);
  expect(later.user.shared_editor).toBe(false);

  const logout = await fetch(at('/auth/logout'), { method: 'POST', redirect: 'manual', headers: { Cookie: alice.cookie } });
  expect(logout.status).toBe(303);
  expect(logout.headers.get('Location')).toContain('/o/end-session');
  expect((await alice('/session')).status).toBe(401);
  expect((await again('/session')).status).toBe(200);
});
