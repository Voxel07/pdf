import { createHash, randomBytes } from 'node:crypto';
import { HttpError, parseEvent, type CalendarStore } from './events';

const prefix = '/internal/pdfsign';
const uuid = '[0-9a-f-]{36}';
const eventPath = new RegExp(`^${prefix}/(session|integration)/events(?:/(${uuid})(/rsvp)?)?$`);
const credentialPath = new RegExp(`^${prefix}/credentials(?:/(${uuid}))?$`);
const feedPath = new RegExp(`^${prefix}/feeds/(pdfc_[a-zA-Z0-9]{48})\\.ics$`);
const digest = (value: string) => createHash('sha256').update(value).digest('hex');

function requiredHeader(request: Request, name: string) {
  const value = request.headers.get(name);
  if (!value) throw new HttpError(400, 'Invalid pdfsign request.');
  return value;
}

function pagination(value: string | null, fallback: number) {
  if (value === null) return fallback;
  if (!/^\d{1,10}$/.test(value) || Number(value) > 0xffffffff) {
    throw new HttpError(400, 'Invalid event pagination.');
  }
  return Number(value);
}

export async function handlePdfsign(
  request: Request,
  store: CalendarStore,
  json: (data: unknown, status?: number) => Response,
  requestJson: (request: Request) => Promise<unknown>,
) {
  const { pathname, searchParams } = new URL(request.url);
  const { method } = request;
  if (pathname === prefix + '/health' && method === 'GET') return new Response(null, { status: 204 });

  const feed = feedPath.exec(pathname);
  if (feed && method === 'GET') {
    if (store.findCredential(digest(feed[1]))?.scope !== 'calendar') {
      throw new HttpError(404, 'Calendar feed not found.', 'not_found');
    }
    const origin = requiredHeader(request, 'X-Pdfsign-Public-Url');
    return new Response(store.renderFeed('pdfsign events', origin), {
      headers: { 'Content-Type': 'text/calendar; charset=utf-8' },
    });
  }

  const credential = credentialPath.exec(pathname);
  if (credential) {
    const id = credential[1];
    if (!id && method === 'GET') return json(store.listCredentials());
    if (id && method === 'DELETE') {
      if (!store.revokeCredential(id)) throw new HttpError(404, 'Event credential not found.', 'not_found');
      return new Response(null, { status: 204 });
    }
    if (!id && method === 'POST') {
      const { name, scope } = await bodyObject(requestJson, request);
      if (typeof name !== 'string' || !name.trim() || [...name.trim()].length > 120) {
        throw new HttpError(400, 'Credential name must contain 1 to 120 characters.');
      }
      if (scope !== 'read' && scope !== 'write' && scope !== 'calendar') {
        throw new HttpError(400, 'Invalid credential scope.');
      }
      const actor = requiredHeader(request, 'X-Pdfsign-User');
      const origin = requiredHeader(request, 'X-Pdfsign-Public-Url');
      const token = (scope === 'calendar' ? 'pdfc_' : 'pdfe_') + randomBytes(24).toString('hex');
      const entry = { id: crypto.randomUUID(), name: name.trim(), scope, created_at: new Date().toISOString() };
      store.saveCredential(entry.id, entry.name, scope, digest(token), entry.created_at, actor);
      const secret = scope === 'calendar' ? `${origin}/api/events/calendar/${token}.ics` : token;
      return json({ ...entry, secret }, 201);
    }
  }

  const event = eventPath.exec(pathname);
  if (event) {
    const [, access, id, rsvp] = event;
    let user: string | null = null;
    let actor: string;
    if (access === 'session') {
      user = requiredHeader(request, 'X-Pdfsign-User');
      actor = 'staff:' + user;
    } else {
      const secret = request.headers.get('X-Pdfsign-Event-Token');
      const scoped = secret && /^pdfe_[a-zA-Z0-9]{48}$/.test(secret) ? store.findCredential(digest(secret)) : null;
      if (!scoped || scoped.scope === 'calendar') {
        throw new HttpError(401, 'Missing or invalid credentials.', 'unauthorized');
      }
      if (method !== 'GET' && scoped.scope !== 'write') {
        throw new HttpError(403, 'An event write token is required.', 'forbidden');
      }
      actor = 'integration:' + scoped.id;
    }
    if (rsvp && id && user && method === 'PUT') {
      const { response } = await bodyObject(requestJson, request);
      if (response !== 'going' && response !== 'maybe' && response !== 'declined') {
        throw new HttpError(400, 'Invalid RSVP response.');
      }
      return json(store.respond(id, user, response));
    }
    if (!rsvp && method === 'GET') {
      if (id) return json(store.getDetail(id, user));
      const phase = searchParams.get('phase');
      if (phase !== null && !['upcoming', 'ongoing', 'ended', 'cancelled'].includes(phase)) {
        throw new HttpError(400, 'Invalid event phase.');
      }
      const limit = Math.min(100, Math.max(1, pagination(searchParams.get('limit'), 30)));
      return json(store.page(phase, limit, pagination(searchParams.get('offset'), 0)));
    }
    if (!id && method === 'POST') {
      const key = request.headers.get('Idempotency-Key');
      if (key !== null && !/^[!-~]{1,128}$/.test(key)) {
        throw new HttpError(400, 'Idempotency-Key must be 1 to 128 visible ASCII characters.');
      }
      const input = parseEvent(await requestJson(request));
      const fingerprint = digest(JSON.stringify(input));
      const { event: created, replay } = store.create(input, key, actor, fingerprint);
      return json(store.getDetail(created.id), replay ? 200 : 201);
    }
    if (!rsvp && id && method === 'PUT') {
      const input = await bodyObject(requestJson, request);
      if (typeof input.revision !== 'number' || !Number.isSafeInteger(input.revision)) {
        throw new HttpError(400, 'A valid event revision is required.');
      }
      store.edit(id, input, false, `"${input.revision}"`, true);
      return json(store.getDetail(id));
    }
  }
  throw new HttpError(404, 'Not found.', 'not_found');
}

async function bodyObject(read: (request: Request) => Promise<unknown>, request: Request) {
  const body = await read(request);
  if (!body || typeof body !== 'object' || Array.isArray(body)) throw new HttpError(400, 'Expected a JSON object.');
  return body as Record<string, unknown>;
}
