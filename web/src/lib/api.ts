export class ApiError extends Error {
  constructor(public status: number, public code: string, message: string) {
    super(message);
  }
}

async function handle<T>(res: Response): Promise<T> {
  if (!res.ok) {
    let msg = res.statusText;
    let code = 'error';
    try {
      const j = await res.json();
      msg = j.error ?? msg;
      code = j.code ?? code;
    } catch {}
    throw new ApiError(res.status, code, msg);
  }
  const ct = res.headers.get('content-type') ?? '';
  return (ct.includes('application/json') ? res.json() : res.blob()) as Promise<T>;
}

export async function request<T = any>(method: string, url: string, body?: unknown, headers: Record<string, string> = {}): Promise<T> {
  const init: RequestInit = { method, headers: { ...headers } };
  if (body instanceof FormData) {
    init.body = body;
  } else if (body !== undefined) {
    (init.headers as Record<string, string>)['content-type'] = 'application/json';
    init.body = JSON.stringify(body);
  }
  return handle<T>(await fetch(url, init));
}

// ---------------------------------------------------------------------------------------
// Admin API

function toLogin(): never {
  location.href = `/auth/login?next=${encodeURIComponent(location.pathname + location.search)}`;
  throw new ApiError(401, 'unauthorized', 'Redirecting to login…');
}

async function adminReq<T>(method: string, path: string, body?: unknown): Promise<T> {
  try {
    return await request<T>(method, `/api/admin${path}`, body);
  } catch (e) {
    if (e instanceof ApiError && e.status === 401) toLogin();
    throw e;
  }
}

export const admin = {
  get: <T = any>(path: string) => adminReq<T>('GET', path),
  post: <T = any>(path: string, body?: unknown) => adminReq<T>('POST', path, body ?? {}),
  put: <T = any>(path: string, body: unknown) => adminReq<T>('PUT', path, body),
  del: <T = any>(path: string, body?: unknown) => adminReq<T>('DELETE', path, body),
  blob: (path: string) => adminReq<Blob>('GET', path),
  headers: (): Record<string, string> => ({}),
};

export interface CurrentUser {
  id: string;
  username: string;
  name: string;
  email: string | null;
  role: 'admin' | 'staff';
}

export async function downloadBlob(blob: Blob, filename: string) {
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 10_000);
}

export async function openBlob(blob: Blob) {
  const url = URL.createObjectURL(blob);
  window.open(url, '_blank');
  setTimeout(() => URL.revokeObjectURL(url), 60_000);
}

/** Downloads an admin file and returns the response headers (e.g. `x-export-failed`). */
export async function adminDownload(path: string, fallbackName: string): Promise<Headers> {
  const res = await fetch(`/api/admin${path}`);
  if (res.status === 401) toLogin();
  if (!res.ok) return handle(res);
  const cd = res.headers.get('content-disposition') ?? '';
  const m = /filename="([^"]+)"/.exec(cd);
  await downloadBlob(await res.blob(), m?.[1] ?? fallbackName);
  return res.headers;
}

/** Fetches an admin file as a blob together with the file name the server suggests. */
export async function adminFetch(path: string, fallbackName: string): Promise<{ blob: Blob; filename: string }> {
  const res = await fetch(`/api/admin${path}`);
  if (res.status === 401) toLogin();
  if (!res.ok) return handle(res);
  const m = /filename="([^"]+)"/.exec(res.headers.get('content-disposition') ?? '');
  return { blob: await res.blob(), filename: m?.[1] ?? fallbackName };
}

// ---------------------------------------------------------------------------------------
// Types

export type FieldKind = 'text' | 'date' | 'number' | 'select' | 'signature';

export interface Field {
  id: string;
  name: string;
  type: FieldKind;
  required: boolean;
  section: number;
  page: number;
  x: number;
  y: number;
  w: number;
  h: number;
  options?: string[];
  font_size?: number;
  format?: string;
  autofill?: string;
  min?: number;
  max?: number;
  placeholder?: string;
  source_ref?: string;
}

export interface Section {
  id: number;
  name: string;
  required: boolean;
}

export interface Template {
  id: string;
  slug: string;
  name: string;
  description: string;
  validity_days: number | null;
  page_count: number;
  pdf_sha256: string;
  fields: Field[];
  sections: Section[];
  source: string | null;
  archived: boolean;
  created_at: string;
  updated_at: string;
  submission_count?: number;
}

export interface SubmissionSummary {
  id: string;
  template_id: string;
  template_slug: string;
  template_name: string;
  person_id: string;
  person_name: string;
  status: 'pending' | 'completed' | 'revoked' | 'expired';
  valid: boolean;
  valid_until: string | null;
  created_at: string;
  updated_at: string;
  completed_at: string | null;
  locked: boolean;
  source: string | null;
  url: string;
}

export interface ActivityEntry {
  id: number;
  created_at: string;
  level: 'info' | 'warn' | 'error';
  category: string;
  actor: string | null;
  message: string;
}

export interface PersonGroup {
  id: string;
  name: string;
}

export const fmtDate =(s?: string | null) => (s ? new Date(s).toLocaleDateString() : '—');
export const fmtDateTime = (s?: string | null) => (s ? new Date(s).toLocaleString() : '—');

export function randomId(len = 10) {
  const a = 'abcdefghijkmnopqrstuvwxyz23456789';
  let s = '';
  const buf = crypto.getRandomValues(new Uint8Array(len));
  for (const b of buf) s += a[b % a.length];
  return s;
}

export async function copy(text: string) {
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    prompt('Copy:', text);
  }
}
