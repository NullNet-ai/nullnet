// Wraps fetch with cookie-based auth: always sends credentials, and on a 401
// (expired/invalid access token) transparently refreshes the session once
// and retries — only falling through to a hard redirect to /login if the
// refresh itself fails (refresh token missing/expired/revoked).

let refreshPromise: Promise<boolean> | null = null;

function refreshSession(): Promise<boolean> {
  if (!refreshPromise) {
    refreshPromise = fetch('/api/auth/refresh', { method: 'POST', credentials: 'include' })
      .then(res => res.ok)
      .catch(() => false)
      .finally(() => {
        refreshPromise = null;
      });
  }
  return refreshPromise;
}

export async function apiFetch(input: string, init?: RequestInit): Promise<Response> {
  const res = await fetch(input, { ...init, credentials: 'include' });
  if (res.status !== 401) return res;

  // Auth endpoints 401ing is either an expected "not logged in" state (login
  // itself) or the refresh call failing on its own merits — never chase our
  // own tail trying to refresh those.
  if (input.startsWith('/api/auth/')) return res;

  const refreshed = await refreshSession();
  if (!refreshed) {
    window.location.href = '/login';
    return res;
  }
  return fetch(input, { ...init, credentials: 'include' });
}
