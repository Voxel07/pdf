import { config } from './config';

export class ApiError extends Error {
  constructor(public status: number, public code: string, message: string) {
    super(message);
  }
}

export async function eventsAvailable() {
  return (await eventsProblem()) === null;
}

/** Why event commands are unavailable, or null when the calendar is reachable through pdfsign. */
export async function eventsProblem(): Promise<string | null> {
  const url = `${config.apiUrl}/api/events/health`;
  try {
    const response = await fetch(url, { redirect: 'error', signal: AbortSignal.timeout(6000) });
    if (response.status === 204) return null;
    if (response.status === 404) return `${url} returned 404. Set EVENTS_ENABLED=true for pdfsign too and restart it.`;
    if (response.status === 503) return 'pdfsign cannot reach the calendar. Its log names the cause.';
    return `${url} returned HTTP ${response.status}.`;
  } catch (e) {
    return `cannot reach pdfsign at ${config.apiUrl} (${e instanceof Error ? e.message : e}). Check PDFSIGN_URL.`;
  }
}

async function call<T>(method: string, path: string, body?: unknown): Promise<T> {
  const res = await fetch(`${config.apiUrl}/api/v1${path}`, {
    method,
    headers: {
      authorization: `Bearer ${config.apiKey}`,
      ...(body !== undefined ? { 'content-type': 'application/json' } : {}),
    },
    body: body !== undefined ? JSON.stringify(body) : undefined,
  });
  if (!res.ok) {
    let msg = res.statusText, code = 'error';
    try {
      const j: any = await res.json();
      msg = j.error ?? msg;
      code = j.code ?? code;
    } catch {}
    throw new ApiError(res.status, code, msg);
  }
  return res.json() as Promise<T>;
}

export interface Submission {
  id: string;
  template_slug: string;
  template_name: string;
  person_name: string;
  status: 'pending' | 'completed' | 'revoked' | 'expired';
  valid: boolean;
  valid_until: string | null;
  completed_at: string | null;
  url: string;
  created?: boolean;
}

export interface Status {
  registered: boolean;
  person_name?: string;
  valid: boolean;
  valid_until?: string | null;
  submission: Submission | null;
}

export interface PersonView {
  person: { id: string; name: string };
  discord_accounts: { discord_id: string; username: string | null }[];
  submissions: Submission[];
}

export const api = {
  status: (discordId: string, template: string) =>
    call<Status>('GET', `/persons/${discordId}/status?template=${encodeURIComponent(template)}`),
  person: (discordId: string) => call<PersonView>('GET', `/persons/${discordId}`),
  register: (discordId: string, name: string, username?: string, zip?: string) =>
    call<PersonView>('PUT', `/persons/${discordId}`, { name, username, zip }),
  link: (template: string, discordId: string, username?: string, name?: string) =>
    call<Submission>('POST', '/links', { template, discord_id: discordId, username, name }),
  linkAccounts: (oldId: string, newId: string, newUsername?: string) =>
    call<PersonView>('POST', '/persons/link', { old_discord_id: oldId, new_discord_id: newId, new_username: newUsername }),
};
