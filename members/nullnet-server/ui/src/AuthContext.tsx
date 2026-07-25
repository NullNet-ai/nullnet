import { createContext, useCallback, useContext, useEffect, useState } from 'react';

export interface AuthUser {
  id: string;
  username: string;
  role: 'admin' | 'user';
  scopes: string[];
  mfaEnabled: boolean;
}

interface AuthContextValue {
  user: AuthUser | null;
  loading: boolean;
  refetchMe: () => Promise<void>;
}

const AuthContext = createContext<AuthContextValue>({
  user: null,
  loading: true,
  refetchMe: async () => {},
});

export function AuthProvider({ children }: { children: React.ReactNode }) {
  const [user, setUser] = useState<AuthUser | null>(null);
  const [loading, setLoading] = useState(true);

  const load = useCallback(async (signal?: AbortSignal) => {
    try {
      // Plain fetch, not apiFetch: checking "am I logged in" legitimately 401s
      // for an anonymous visitor — that's not a session-expired event, so it
      // shouldn't trigger apiFetch's refresh-then-redirect side effect.
      const res = await fetch('/api/auth/me', { credentials: 'include', signal });
      if (!res.ok) {
        setUser(null);
        return;
      }
      const data = await res.json();
      setUser({
        id: data.id,
        username: data.username,
        role: data.role,
        scopes: data.scopes,
        mfaEnabled: data.mfa_enabled,
      });
    } catch (e) {
      if ((e as Error).name === 'AbortError') return;
      setUser(null);
    } finally {
      if (!signal?.aborted) setLoading(false);
    }
  }, []);

  useEffect(() => {
    const controller = new AbortController();
    (async () => {
      await load(controller.signal);
    })();
    return () => controller.abort();
  }, [load]);

  return <AuthContext.Provider value={{ user, loading, refetchMe: load }}>{children}</AuthContext.Provider>;
}

export function useAuth() {
  return useContext(AuthContext);
}
