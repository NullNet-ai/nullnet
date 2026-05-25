import { useState, useEffect, useCallback } from 'react';

interface ApiState<T> {
  data: T | null;
  loading: boolean;
  error: string | null;
}

export function useApi<T>(url: string, refreshMs?: number): ApiState<T> & { refetch: () => void } {
  const [state, setState] = useState<ApiState<T>>({ data: null, loading: true, error: null });

  const load = useCallback(async () => {
    try {
      const res = await fetch(url);
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

export function useApiText(url: string, refreshMs?: number): { text: string | null; loading: boolean; error: string | null } {
  const [state, setState] = useState<{ text: string | null; loading: boolean; error: string | null }>({ text: null, loading: true, error: null });

  const load = useCallback(async () => {
    try {
      const res = await fetch(url);
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const text = await res.text();
      setState({ text, loading: false, error: null });
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

  return state;
}
