import { mkdirSync } from 'node:fs';
import { mkdtemp, readFile, rm, stat } from 'node:fs/promises';
import { createHash, createHmac, timingSafeEqual } from 'node:crypto';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import type { CalendarEvent, CalendarInfo } from '../src/lib/events';
import { createAuth, issuerUrl, type OidcConfig } from './auth';
import { HttpError, openCalendar, parseCalendarName, parseEvent, SHARED, type UiSession } from './events';
import { handlePdfsign } from './pdfsign';
import { startWebhookWorker, type WebhookConfig } from './webhooks';

interface Config {
  adminToken: string;
  feedToken: string;
  pdfsignToken?: string;
  name: string;
  publicUrl?: string;
  webhook?: WebhookConfig;
  oidc?: OidcConfig;
}

/** The admin token reaches only the shared calendar; a signed-in user also reaches their own. */
type Principal = { kind: 'admin' } | { kind: 'user'; user: UiSession };

const calendarId = '(?:shared|[0-9a-f-]{36})';

function digest(value: string) {
  return createHash('sha256').update(value).digest();
}

function json(data: unknown, status = 200, headers?: HeadersInit) {
  const response = Response.json(data, { status, headers });
  response.headers.set('Cache-Control', 'no-store');
  return response;
}

function eventResponse(event: CalendarEvent, status = 200, created = false) {
  const headers = new Headers({ ETag: `"${event.revision}"` });
  if (created) headers.set('Location', `/api/v1/events/${event.id}`);
  return json(event, status, headers);
}

async function requestJson(request: Request): Promise<unknown> {
  if (request.headers.get('Content-Type')?.split(';')[0].trim().toLowerCase() !== 'application/json') {
    throw new HttpError(415, 'Send JSON with Content-Type: application/json.');
  }
  try {
    return await request.json();
  } catch {
    throw new HttpError(400, 'Malformed JSON.');
  }
}

function allow(methods: string) {
  return json({ error: 'Method not allowed.' }, 405, { Allow: methods });
}

export function createCalendarServer(config: Config, filename: string, webDir: string, port: number, hostname: string) {
  const store = openCalendar(filename, Boolean(config.webhook));
  const adminHash = digest(config.adminToken);
  const feedHash = digest(config.feedToken);
  const pdfsignHash = config.pdfsignToken ? digest(config.pdfsignToken) : undefined;
  const auth = createAuth(config.oidc, config.publicUrl, store);
  let snapshotRunning = false;

  // Personal feed links are derived from the feed token, so rotating it rotates every link.
  const feedSecret = (calendar: string) => createHmac('sha256', config.feedToken).update('calendar:' + calendar).digest('base64url');
  const feedUrl = (calendar: string) => calendar === SHARED
    ? `${config.publicUrl ?? ''}/feeds/${config.feedToken}.ics`
    : `${config.publicUrl ?? ''}/feeds/${calendar}/${feedSecret(calendar)}.ics`;

  function calendars(principal: Principal): CalendarInfo[] {
    const shared: CalendarInfo = {
      id: SHARED, name: config.name, personal: false, feed_url: feedUrl(SHARED),
      writable: principal.kind === 'admin' || principal.user.shared_editor,
    };
    if (principal.kind === 'admin') return [shared];
    return [shared, ...store.ownCalendars(principal.user.sub).map((calendar) => ({
      id: calendar.id, name: calendar.name, personal: true, writable: true, feed_url: feedUrl(calendar.id),
    }))];
  }

  /** Throws 404 for calendars the principal cannot see and 403 for read-only ones it must write. */
  function access(principal: Principal, calendar: string, write: boolean) {
    const found = calendars(principal).find((entry) => entry.id === calendar);
    if (!found) throw new HttpError(404, 'Calendar not found.');
    if (write && !found.writable) throw new HttpError(403, 'You cannot change events in this calendar.');
    return found;
  }

  function principalUser(principal: Principal) {
    if (principal.kind !== 'user') throw new HttpError(403, 'Personal calendars require signing in to the web app.');
    return principal.user;
  }

  async function snapshot() {
    if (snapshotRunning) throw new HttpError(409, 'A calendar backup is already running.');
    snapshotRunning = true;
    let staging: string | undefined;
    try {
      staging = await mkdtemp(join(tmpdir(), 'calendar-backup-'));
      const file = join(staging, 'calendar.sqlite');
      store.snapshot(file);
      if ((await stat(file)).size > 128 * 1024 * 1024) {
        throw new HttpError(413, 'The calendar snapshot exceeds 128 MiB.');
      }
      return new Response(await readFile(file), {
        headers: { 'Content-Type': 'application/vnd.sqlite3', 'Cache-Control': 'no-store' },
      });
    } finally {
      snapshotRunning = false;
      if (staging) await rm(staging, { recursive: true, force: true });
    }
  }

  async function route(request: Request) {
    const { pathname } = new URL(request.url);
    const { method } = request;

    if (pathname === '/health' && method === 'GET') return new Response('ok');

    if (pathname.startsWith('/internal/pdfsign/')) {
      const token = /^Bearer (.+)$/i.exec(request.headers.get('Authorization') ?? '')?.[1];
      if (!pdfsignHash || !token || !timingSafeEqual(digest(token), pdfsignHash)) {
        return json({ error: 'Not found.' }, 404);
      }
      if (pathname === '/internal/pdfsign/backup') {
        if (method !== 'GET') return allow('GET');
        return snapshot();
      }
      return handlePdfsign(request, store, json, requestJson);
    }

    const feed = /^\/feeds\/([A-Za-z0-9_-]+)\.ics$/.exec(pathname);
    const personalFeed = /^\/feeds\/([0-9a-f-]{36})\/([A-Za-z0-9_-]{43})\.ics$/.exec(pathname);
    const feedName = feed && timingSafeEqual(digest(feed[1]), feedHash) ? config.name
      : personalFeed && timingSafeEqual(digest(personalFeed[2]), digest(feedSecret(personalFeed[1])))
        ? store.calendarName(personalFeed[1]) : undefined;
    if (feedName !== undefined) {
      if (method !== 'GET' && method !== 'HEAD') return allow('GET, HEAD');
      const body = method === 'HEAD' ? null : store.renderFeed(feedName, undefined, personalFeed ? personalFeed[1] : SHARED);
      return new Response(body, {
        headers: { 'Content-Type': 'text/calendar; charset=utf-8', 'Cache-Control': 'no-cache, private' },
      });
    }

    if (pathname === '/auth/login') return method === 'GET' ? auth.login() : allow('GET');
    if (pathname === '/auth/callback') return method === 'GET' ? auth.callback(request) : allow('GET');
    if (pathname === '/auth/logout') return method === 'POST' ? auth.logout(request) : allow('POST');

    if (pathname.startsWith('/api/')) {
      const token = /^Bearer (.+)$/i.exec(request.headers.get('Authorization') ?? '')?.[1];
      let principal: Principal;
      if (token) {
        if (!timingSafeEqual(digest(token), adminHash)) {
          return json({ error: 'A valid admin token is required.' }, 401, { 'WWW-Authenticate': 'Bearer' });
        }
        principal = { kind: 'admin' };
      } else {
        const user = auth.user(request);
        if (!user) {
          return json({ error: 'Sign in to continue.', sign_in: auth.available }, 401, { 'WWW-Authenticate': 'Bearer' });
        }
        // Cookies must not let other sites act for the user.
        if (method !== 'GET' && method !== 'HEAD' && ['cross-site', 'same-site'].includes(request.headers.get('Sec-Fetch-Site') ?? '')) {
          return json({ error: 'Cross-site requests are not allowed.' }, 403);
        }
        principal = { kind: 'user', user };
      }
      if (pathname === '/api/v1/session') {
        if (method !== 'GET') return allow('GET');
        const user = principalUser(principal);
        return json({ user: { name: user.name, shared_editor: user.shared_editor }, calendars: calendars(principal) });
      }
      if (pathname === '/api/v1/settings') {
        if (method !== 'GET') return allow('GET');
        return json({ name: config.name, feed_url: feedUrl(SHARED) });
      }
      if (pathname === '/api/v1/calendars') {
        if (method === 'GET') return json({ calendars: calendars(principal) });
        if (method !== 'POST') return allow('GET, POST');
        const user = principalUser(principal);
        const body = await requestJson(request) as { name?: unknown } | null;
        const created = store.createCalendar(user.sub, parseCalendarName(body?.name));
        return json(calendars(principal).find((entry) => entry.id === created.id), 201);
      }
      const calendarMatch = /^\/api\/v1\/calendars\/([0-9a-f-]{36})$/.exec(pathname);
      if (calendarMatch) {
        const user = principalUser(principal);
        if (method === 'PATCH') {
          const body = await requestJson(request) as { name?: unknown } | null;
          store.renameCalendar(calendarMatch[1], user.sub, parseCalendarName(body?.name));
          return json(calendars(principal).find((entry) => entry.id === calendarMatch[1]));
        }
        if (method === 'DELETE') {
          store.deleteCalendar(calendarMatch[1], user.sub);
          return new Response(null, { status: 204 });
        }
        return allow('PATCH, DELETE');
      }
      if (pathname === '/api/v1/events') {
        const calendar = new URL(request.url).searchParams.get('calendar') ?? SHARED;
        if (!new RegExp(`^${calendarId}$`).test(calendar)) throw new HttpError(404, 'Calendar not found.');
        if (method === 'GET') {
          access(principal, calendar, false);
          return json({ events: store.list(calendar) });
        }
        if (method !== 'POST') return allow('GET, POST');
        access(principal, calendar, true);
        const key = request.headers.get('Idempotency-Key');
        if (key !== null && !/^[A-Za-z0-9._:-]{1,128}$/.test(key)) {
          throw new HttpError(400, 'Idempotency-Key must be 1 to 128 characters and contain only letters, numbers, dots, underscores, colons or hyphens.');
        }
        const { event, replay } = store.create(parseEvent(await requestJson(request)), key, calendar);
        return eventResponse(event, replay ? 200 : 201, true);
      }
      const eventMatch = /^\/api\/v1\/events\/([0-9a-f-]{36})$/.exec(pathname);
      if (eventMatch) {
        const id = eventMatch[1];
        const calendar = store.calendarOf(id);
        if (!calendar) throw new HttpError(404, 'Event not found.');
        try {
          access(principal, calendar, false);
        } catch {
          throw new HttpError(404, 'Event not found.');
        }
        const revision = request.headers.get('If-Match');
        if (method === 'GET') return eventResponse(store.get(id, calendar));
        if (method === 'PUT' || method === 'PATCH') {
          access(principal, calendar, true);
          return eventResponse(store.edit(id, calendar, await requestJson(request), method === 'PATCH', revision));
        }
        if (method === 'DELETE') {
          access(principal, calendar, true);
          store.erase(id, calendar, revision);
          return new Response(null, { status: 204 });
        }
        return allow('GET, PUT, PATCH, DELETE');
      }
    }

    const bundledAsset = /^\/assets\/[\w.-]+$/.test(pathname);
    const pwaAsset = /^\/pwa\/(?:manifest\.webmanifest|[\w-]+\.png)$/.test(pathname);
    if ((method === 'GET' || method === 'HEAD') && (pathname === '/' || bundledAsset || pwaAsset)) {
      const file = Bun.file(join(webDir, pathname === '/' ? 'index.html' : pathname.slice(1)));
      if (await file.exists()) {
        return new Response(method === 'GET' ? file : null, {
          headers: {
            'Content-Type': file.type,
            'Cache-Control': bundledAsset ? 'public, max-age=31536000, immutable' : 'no-cache',
            'X-Content-Type-Options': 'nosniff',
            'Referrer-Policy': 'no-referrer',
          },
        });
      }
    }
    return json({ error: 'Not found.' }, 404);
  }

  const server = Bun.serve({
    port,
    hostname,
    maxRequestBodySize: 32 * 1024,
    async fetch(request) {
      try {
        return await route(request);
      } catch (error) {
        if (error instanceof HttpError) return json({ error: error.message, code: error.code }, error.status);
        console.error('Calendar request failed:', error);
        return json({ error: 'Internal server error.' }, 500);
      }
    },
  });
  const stopWorker = config.webhook ? startWebhookWorker(store, config.webhook) : undefined;

  return {
    server,
    async stop() {
      await server.stop();
      await stopWorker?.();
      store.close();
    },
  };
}

function requiredToken(name: string) {
  const value = Bun.env[name] ?? '';
  if (!/^[A-Za-z0-9_-]{32,128}$/.test(value)) {
    throw new Error(`${name} must be 32 to 128 URL-safe characters (letters, digits, hyphens, underscores).`);
  }
  return value;
}

function httpUrl(value: string, name: string) {
  const url = URL.parse(value);
  if (!url || !['https:', 'http:'].includes(url.protocol) || url.username || url.password || url.hash) {
    throw new Error(`${name} must be an HTTP(S) URL without credentials or a fragment.`);
  }
  return url;
}

if (import.meta.main) {
  const adminToken = requiredToken('CALENDAR_ADMIN_TOKEN');
  const feedToken = requiredToken('CALENDAR_FEED_TOKEN');
  const pdfsignToken = Bun.env.CALENDAR_PDFSIGN_TOKEN ? requiredToken('CALENDAR_PDFSIGN_TOKEN') : undefined;
  if (adminToken === feedToken || pdfsignToken === adminToken || pdfsignToken === feedToken) {
    throw new Error('Use distinct admin, feed and pdfsign tokens.');
  }
  const name = Bun.env.CALENDAR_NAME?.trim() || 'Calendar';
  if (name.length > 120 || /[\x00-\x1f]/.test(name)) throw new Error('CALENDAR_NAME must be a single line of up to 120 characters.');
  const publicUrl = Bun.env.CALENDAR_PUBLIC_URL && httpUrl(Bun.env.CALENDAR_PUBLIC_URL, 'CALENDAR_PUBLIC_URL');
  if (publicUrl && (publicUrl.pathname !== '/' || publicUrl.search)) {
    throw new Error('CALENDAR_PUBLIC_URL must be an origin (scheme and host).');
  }
  const webhook = Bun.env.CALENDAR_WEBHOOK_URL ? {
    url: httpUrl(Bun.env.CALENDAR_WEBHOOK_URL, 'CALENDAR_WEBHOOK_URL'),
    bearer: Bun.env.CALENDAR_WEBHOOK_BEARER_TOKEN,
    secret: Bun.env.CALENDAR_WEBHOOK_SECRET,
  } : undefined;
  const oidcEnv = ['ISSUER', 'CLIENT_ID', 'CLIENT_SECRET'].map((key) => Bun.env[`CALENDAR_OIDC_${key}`]?.trim() ?? '');
  let oidc: OidcConfig | undefined;
  if (oidcEnv.some(Boolean)) {
    if (!oidcEnv.every(Boolean)) {
      throw new Error('Set CALENDAR_OIDC_ISSUER, CALENDAR_OIDC_CLIENT_ID and CALENDAR_OIDC_CLIENT_SECRET together.');
    }
    if (!publicUrl) throw new Error('CALENDAR_OIDC_* requires CALENDAR_PUBLIC_URL for the sign-in callback.');
    issuerUrl(oidcEnv[0]);
    oidc = {
      issuer: oidcEnv[0],
      clientId: oidcEnv[1],
      clientSecret: oidcEnv[2],
      scopes: Bun.env.CALENDAR_OIDC_SCOPES?.trim() || 'openid profile email',
      group: Bun.env.CALENDAR_OIDC_GROUP?.trim() || undefined,
      adminGroup: Bun.env.CALENDAR_OIDC_ADMIN_GROUP?.trim() || undefined,
    };
  } else {
    console.warn('CALENDAR_OIDC_* is not set: the web app cannot sign anyone in. The API still accepts the admin token.');
  }
  const dataDir = resolve(Bun.env.CALENDAR_DATA_DIR || 'data');
  mkdirSync(dataDir, { recursive: true });
  const app = createCalendarServer(
    { adminToken, feedToken, pdfsignToken, name, publicUrl: publicUrl ? publicUrl.origin : undefined, webhook, oidc },
    join(dataDir, 'calendar.sqlite'), resolve('dist'), Number(Bun.env.PORT || 3090), Bun.env.HOST || '127.0.0.1',
  );
  console.log(`Calendar listening on ${app.server.url}`);
  process.once('SIGTERM', () => void app.stop());
  process.once('SIGINT', () => void app.stop());
}
