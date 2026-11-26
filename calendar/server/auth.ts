// Sign-in for the web UI through an OpenID Connect provider such as authentik. The calendar is a
// confidential client using the authorization code flow with PKCE. ID tokens come straight from
// the token endpoint over TLS, so the TLS connection authenticates them (OIDC Core 3.1.3.7) and
// the calendar validates their claims instead of their signature. Sessions are HttpOnly cookies.
import { createHash, randomBytes } from 'node:crypto';
import { HttpError, type CalendarStore, type UiSession } from './events';

export interface OidcConfig {
  issuer: string;
  clientId: string;
  clientSecret: string;
  scopes: string;
  /** Required for sign-in when set. */
  group?: string;
  /** Required to edit the shared calendar when set. */
  adminGroup?: string;
}

interface Discovery {
  issuer: string;
  authorization_endpoint: string;
  token_endpoint: string;
  userinfo_endpoint: string;
  end_session_endpoint?: string;
}

const SESSION_COOKIE = 'calendar_session';
const STATE_COOKIE = 'calendar_oidc_state';
const SESSION_MS = 12 * 3_600_000;

const hash = (value: string) => createHash('sha256').update(value).digest('hex');
const token = (bytes: number) => randomBytes(bytes).toString('base64url');

export function cookie(request: Request, name: string) {
  for (const part of (request.headers.get('Cookie') ?? '').split(';')) {
    const [key, ...value] = part.trim().split('=');
    if (key === name) return value.join('=');
  }
}

/** Accepts HTTPS issuers, and HTTP only on loopback hosts for local testing. */
export function issuerUrl(value: string) {
  const url = URL.parse(value);
  const loopback = url && ['localhost', '127.0.0.1', '[::1]'].includes(url.hostname);
  if (!url || !(url.protocol === 'https:' || (url.protocol === 'http:' && loopback)) || url.username || url.password || url.search || url.hash) {
    throw new Error('CALENDAR_OIDC_ISSUER must be an HTTPS URL without credentials, a query or a fragment.');
  }
  return url;
}

function sameOrigin(value: unknown, issuer: URL) {
  return typeof value === 'string' && URL.parse(value)?.origin === issuer.origin;
}

async function providerJson(url: string, init?: RequestInit) {
  const response = await fetch(url, { ...init, redirect: 'error', signal: AbortSignal.timeout(10_000) });
  const text = await response.text();
  if (text.length > 128 * 1024) throw new Error('The identity provider response is too large.');
  const body = JSON.parse(text || 'null');
  if (!response.ok) {
    const detail = body && typeof body.error === 'string' ? `: ${body.error}` : '';
    throw new Error(`The identity provider answered HTTP ${response.status}${detail}.`);
  }
  if (!body || typeof body !== 'object') throw new Error('The identity provider sent an invalid response.');
  return body as Record<string, unknown>;
}

function claims(idToken: string) {
  const payload = idToken.split('.')[1];
  if (!payload) throw new Error('The ID token is malformed.');
  return JSON.parse(Buffer.from(payload, 'base64url').toString('utf8')) as Record<string, unknown>;
}

function page(status: number, title: string, message: string, headers?: HeadersInit) {
  const escape = (text: string) => text.replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);
  const html = `<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>${escape(title)}</title><style>body{font-family:system-ui,sans-serif;background:#f4f5f7;color:#1c2230;display:grid;place-items:center;min-height:100vh;margin:0}
@media (prefers-color-scheme:dark){body{background:#020409;color:#e8e5df}a{color:#d6a24d}}
main{max-width:28rem;padding:1.5rem}a{color:#2f5bd3}</style></head>
<body><main><h1>${escape(title)}</h1><p>${escape(message)}</p><p><a href="/">Back to the calendar</a></p></main></body></html>`;
  const response = new Response(html, { status, headers });
  response.headers.set('Content-Type', 'text/html; charset=utf-8');
  response.headers.set('Cache-Control', 'no-store');
  response.headers.set('Content-Security-Policy', "default-src 'none'; style-src 'unsafe-inline'");
  return response;
}

export function createAuth(config: OidcConfig | undefined, publicUrl: string | undefined, store: CalendarStore) {
  const secure = publicUrl?.startsWith('https:') ? '; Secure' : '';
  const redirectUri = publicUrl ? `${publicUrl}/auth/callback` : undefined;
  let discovery: Promise<Discovery> | undefined;

  function discover() {
    if (!config) throw new HttpError(503, 'Sign-in is not configured. Set the CALENDAR_OIDC_* variables.');
    discovery ??= (async () => {
      const issuer = issuerUrl(config.issuer);
      const body = await providerJson(`${config.issuer.replace(/\/+$/, '')}/.well-known/openid-configuration`);
      const endpoints = ['authorization_endpoint', 'token_endpoint', 'userinfo_endpoint'] as const;
      if (body.issuer !== config.issuer) throw new Error('The discovered issuer does not match CALENDAR_OIDC_ISSUER.');
      if (!endpoints.every((name) => sameOrigin(body[name], issuer))
        || (body.end_session_endpoint !== undefined && !sameOrigin(body.end_session_endpoint, issuer))) {
        throw new Error('The identity provider endpoints must share the issuer origin.');
      }
      return body as unknown as Discovery;
    })();
    discovery.catch(() => { discovery = undefined; });
    return discovery;
  }

  function sessionCookie(value: string, maxAge: number) {
    return `${SESSION_COOKIE}=${value}; Path=/; HttpOnly; SameSite=Lax; Max-Age=${maxAge}${secure}`;
  }

  async function login() {
    const provider = await discover().catch((error) => {
      if (error instanceof HttpError) throw error;
      console.error('OIDC discovery failed:', error instanceof Error ? error.message : error);
      throw new HttpError(502, 'The identity provider is unavailable.');
    });
    const state = token(32);
    const verifier = token(48);
    const nonce = token(24);
    store.saveLogin(state, verifier, nonce);
    const url = new URL(provider.authorization_endpoint);
    url.search = new URLSearchParams({
      response_type: 'code',
      client_id: config!.clientId,
      redirect_uri: redirectUri!,
      scope: config!.scopes,
      state,
      nonce,
      code_challenge: createHash('sha256').update(verifier).digest('base64url'),
      code_challenge_method: 'S256',
    }).toString();
    return new Response(null, {
      status: 302,
      headers: {
        Location: url.href,
        'Set-Cookie': `${STATE_COOKIE}=${state}; Path=/auth; HttpOnly; SameSite=Lax; Max-Age=900${secure}`,
        'Cache-Control': 'no-store',
      },
    });
  }

  async function callback(request: Request) {
    const clearState = `${STATE_COOKIE}=; Path=/auth; HttpOnly; SameSite=Lax; Max-Age=0${secure}`;
    const fail = (status: number, title: string, message: string) => page(status, title, message, { 'Set-Cookie': clearState });
    const params = new URL(request.url).searchParams;
    if (params.get('error')) {
      return fail(401, 'Sign-in failed', params.get('error_description') || params.get('error')!);
    }
    const code = params.get('code');
    const state = params.get('state');
    if (!code || !state) return fail(400, 'Sign-in failed', 'The identity provider sent no code or state.');
    if (cookie(request, STATE_COOKIE) !== state) {
      return fail(400, 'Sign-in expired', 'This sign-in was started in another browser or has expired. Try again.');
    }
    const login = store.takeLogin(state);
    if (!login) return fail(400, 'Sign-in expired', 'The sign-in request expired or was already used. Try again.');

    let user: UiSession;
    try {
      const provider = await discover();
      const tokens = await providerJson(provider.token_endpoint, {
        method: 'POST',
        headers: {
          Authorization: 'Basic ' + btoa(`${encodeURIComponent(config!.clientId)}:${encodeURIComponent(config!.clientSecret)}`),
          'Content-Type': 'application/x-www-form-urlencoded',
        },
        body: new URLSearchParams({
          grant_type: 'authorization_code',
          code,
          redirect_uri: redirectUri!,
          client_id: config!.clientId,
          code_verifier: login.verifier,
        }),
      });
      if (typeof tokens.access_token !== 'string' || typeof tokens.id_token !== 'string') {
        throw new Error('The token response has no access or ID token.');
      }
      const id = claims(tokens.id_token);
      const audience = Array.isArray(id.aud) ? id.aud : [id.aud];
      if (id.iss !== provider.issuer || !audience.includes(config!.clientId)
        || (audience.length > 1 && id.azp !== config!.clientId)
        || (id.azp !== undefined && id.azp !== config!.clientId)
        || typeof id.exp !== 'number' || id.exp * 1000 <= Date.now()
        || id.nonce !== login.nonce || typeof id.sub !== 'string' || !id.sub) {
        throw new Error('The ID token claims are invalid for this sign-in.');
      }
      const info = await providerJson(provider.userinfo_endpoint, { headers: { Authorization: `Bearer ${tokens.access_token}` } });
      if (info.sub !== id.sub) throw new Error('The userinfo subject does not match the ID token.');
      const groups = Array.isArray(info.groups) ? info.groups.filter((g): g is string => typeof g === 'string') : [];
      if (config!.group && !groups.includes(config!.group)) {
        return fail(403, 'No access', `Your account requires membership in the ${config!.group} group.`);
      }
      const name = [info.name, info.preferred_username, info.email, id.sub]
        .find((value): value is string => typeof value === 'string' && value.trim() !== '')!.trim().slice(0, 120);
      user = {
        sub: id.sub,
        name,
        shared_editor: !config!.adminGroup || groups.includes(config!.adminGroup),
        expires_at: Date.now() + SESSION_MS,
      };
    } catch (error) {
      console.error('OIDC sign-in failed:', error instanceof Error ? error.message : error);
      return fail(502, 'Sign-in failed', 'The identity provider response could not be verified. Try again.');
    }

    store.recordLogin(user.sub, user.name);
    const session = token(32);
    store.saveSession(hash(session), user);
    const headers = new Headers({ Location: '/', 'Cache-Control': 'no-store' });
    headers.append('Set-Cookie', clearState);
    headers.append('Set-Cookie', sessionCookie(session, SESSION_MS / 1000));
    return new Response(null, { status: 303, headers });
  }

  async function logout(request: Request) {
    const session = cookie(request, SESSION_COOKIE);
    if (session) store.deleteSession(hash(session));
    let target = '/';
    if (config) target = (await discover().catch(() => undefined))?.end_session_endpoint ?? '/';
    return new Response(null, {
      status: 303,
      headers: { Location: target, 'Set-Cookie': sessionCookie('', 0), 'Cache-Control': 'no-store' },
    });
  }

  return {
    available: Boolean(config && redirectUri),
    login,
    callback,
    logout,
    user(request: Request) {
      const session = cookie(request, SESSION_COOKIE);
      return session ? store.findSession(hash(session)) : undefined;
    },
  };
}
