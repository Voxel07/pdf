export async function request<T>(token: string, path: string, init?: RequestInit): Promise<T> {
  const headers = new Headers(init?.headers);
  headers.set('Authorization', `Bearer ${token}`);
  if (init?.body) headers.set('Content-Type', 'application/json');
  const response = await fetch('/api/v1' + path, { ...init, headers });
  if (!response.ok) {
    const body = await response.json().catch(() => null);
    throw new Error(body?.error ?? `Request failed (HTTP ${response.status}).`);
  }
  if (response.status === 204) return undefined as T;
  return response.json();
}
