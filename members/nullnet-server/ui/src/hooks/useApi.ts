import { useState, useEffect, useCallback, useRef } from 'react';
import { poll } from '../lib/poll';
import { apiFetch } from '../lib/apiFetch';

interface ApiState<T> {
  data: T | null;
  loading: boolean;
  error: string | null;
}

export function useApi<T>(url: string, refreshMs?: number): ApiState<T> & { updatedAt: number | null; refetch: () => Promise<void> } {
  const [state, setState] = useState<ApiState<T> & { url: string; updatedAt: number | null }>({ url: '', data: null, loading: true, error: null, updatedAt: null });
  const polling = useRef<ReturnType<typeof poll> | null>(null);
  useEffect(() => {
    const current = poll(async signal => {
      try {
        const res = await apiFetch(url, { signal });
        if (!res.ok) throw new Error(`HTTP ${res.status}`);
        const data: T = await res.json();
        if (!signal.aborted) setState({ url, data, loading: false, error: null, updatedAt: Date.now() });
      } catch (e) {
        if (!signal.aborted) setState(prev => ({
          url, data: prev.url === url ? prev.data : null, loading: false, error: String(e),
          updatedAt: prev.url === url ? prev.updatedAt : null,
        }));
      }
    }, refreshMs);
    polling.current = current;
    return () => { current.stop(); polling.current = null; };
  }, [url, refreshMs]);

  const refetch = useCallback(() => polling.current?.refresh() ?? Promise.resolve(), []);
  return state.url === url ? { ...state, refetch }
    : { data: null, loading: true, error: null, updatedAt: null, refetch };
}

export function useApiText(url: string, refreshMs?: number): { text: string | null; loading: boolean; error: string | null; refetch: () => void } {
  const [state, setState] = useState<{ text: string | null; loading: boolean; error: string | null }>({ text: null, loading: true, error: null });

  const load = useCallback(async () => {
    try {
      const res = await apiFetch(url);
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const text = await res.text();
      setState({ text, loading: false, error: null });
    } catch (e) {
      setState(prev => ({ ...prev, loading: false, error: String(e) }));
    }
  }, [url]);

  useEffect(() => {
    // Reset on URL (e.g. stack) change so we show "Loading…" rather than briefly
    // rendering the previous URL's content. Polling calls `load` directly and
    // doesn't hit this reset.
    setState({ text: null, loading: true, error: null });
    load();
    if (!refreshMs) return;
    const id = setInterval(load, refreshMs);
    return () => clearInterval(id);
  }, [load, refreshMs]);

  return { ...state, refetch: load };
}
