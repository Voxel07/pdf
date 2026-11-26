import { mkdirSync } from 'node:fs';
import { mkdtemp, readFile, rm, stat } from 'node:fs/promises';
import { createHash, timingSafeEqual } from 'node:crypto';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import type { CalendarEvent } from '../src/lib/events';
import { HttpError, openCalendar, parseEvent } from './events';
import { handlePdfsign } from './pdfsign';
import { startWebhookWorker, type WebhookConfig } from './webhooks';

interface Config {
  adminToken: string;
  feedToken: string;
  pdfsignToken?: string;
  name: string;
  publicUrl?: string;
  webhook?: WebhookConfig;
}

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
  let snapshotRunning = false;

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
    if (feed && timingSafeEqual(digest(feed[1]), feedHash)) {
      if (method !== 'GET' && method !== 'HEAD') return allow('GET, HEAD');
      return new Response(method === 'HEAD' ? null : store.renderFeed(config.name), {
        headers: { 'Content-Type': 'text/calendar; charset=utf-8', 'Cache-Control': 'no-cache, private' },
      });
    }

    if (pathname.startsWith('/api/')) {
      const token = /^Bearer (.+)$/i.exec(request.headers.get('Authorization') ?? '')?.[1];
      if (!token || !timingSafeEqual(digest(token), adminHash)) {
        return json({ error: 'A valid admin token is required.' }, 401, { 'WWW-Authenticate': 'Bearer' });
      }
      if (pathname === '/api/v1/settings') {
        if (method !== 'GET') return allow('GET');
        return json({ name: config.name, feed_url: `${config.publicUrl ?? ''}/feeds/${config.feedToken}.ics` });
      }
      if (pathname === '/api/v1/events') {
        if (method === 'GET') return json({ events: store.list() });
        if (method !== 'POST') return allow('GET, POST');
        const key = request.headers.get('Idempotency-Key');
        if (key !== null && !/^[A-Za-z0-9._:-]{1,128}$/.test(key)) {
          throw new HttpError(400, 'Idempotency-Key must be 1 to 128 characters and contain only letters, numbers, dots, underscores, colons or hyphens.');
        }
        const { event, replay } = store.create(parseEvent(await requestJson(request)), key);
        return eventResponse(event, replay ? 200 : 201, true);
      }
      const eventMatch = /^\/api\/v1\/events\/([0-9a-f-]{36})$/.exec(pathname);
      if (eventMatch) {
        const id = eventMatch[1];
        const revision = request.headers.get('If-Match');
        if (method === 'GET') return eventResponse(store.get(id));
        if (method === 'PUT' || method === 'PATCH') {
          return eventResponse(store.edit(id, await requestJson(request), method === 'PATCH', revision));
        }
        if (method === 'DELETE') {
          store.erase(id, revision);
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
  const dataDir = resolve(Bun.env.CALENDAR_DATA_DIR || 'data');
  mkdirSync(dataDir, { recursive: true });
  const app = createCalendarServer(
    { adminToken, feedToken, pdfsignToken, name, publicUrl: publicUrl ? publicUrl.origin : undefined, webhook },
    join(dataDir, 'calendar.sqlite'), resolve('dist'), Number(Bun.env.PORT || 3090), Bun.env.HOST || '127.0.0.1',
  );
  console.log(`Calendar listening on ${app.server.url}`);
  process.once('SIGTERM', () => void app.stop());
  process.once('SIGINT', () => void app.stop());
}
