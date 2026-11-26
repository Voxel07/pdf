/** The session cookie is missing or expired. `available` is false when sign-in is not configured. */
export class SignInRequired extends Error {
  constructor(message: string, public available: boolean) {
    super(message);
  }
}

export async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const headers = new Headers(init?.headers);
  if (init?.body) headers.set('Content-Type', 'application/json');
  const response = await fetch('/api/v1' + path, { ...init, headers, credentials: 'same-origin' });
  if (!response.ok) {
    const body = await response.json().catch(() => null);
    const message = body?.error ?? `Request failed (HTTP ${response.status}).`;
    if (response.status === 401) throw new SignInRequired(message, body?.sign_in !== false);
    throw new Error(message);
  }
  if (response.status === 204) return undefined as T;
  return response.json();
}
