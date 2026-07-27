import { useState, useEffect, useCallback } from 'react';
import { apiFetch } from '../lib/apiFetch';

interface ApiState<T> {
  data: T | null;
  loading: boolean;
  error: string | null;
}

export function useApi<T>(url: string, refreshMs?: number): ApiState<T> & { refetch: () => void } {
  const [state, setState] = useState<ApiState<T>>({ data: null, loading: true, error: null });

  const load = useCallback(async () => {
    try {
      const res = await apiFetch(url);
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const data: T = await res.json();
      setState({ data, loading: false, error: null });
    } catch (e) {
      setState(prev => ({ ...prev, loading: false, error: String(e) }));
    }
  }, [url]);

  useEffect(() => {
    load();
    if (!refreshMs) return;
    const id = setInterval(load, refreshMs);
    return () => clearInterval(id);
  }, [load, refreshMs]);

  return { ...state, refetch: load };
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
