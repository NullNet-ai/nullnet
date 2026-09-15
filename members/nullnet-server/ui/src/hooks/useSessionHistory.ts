import { useEffect, useState } from 'react';
import type { SessionsHistoryPage } from '../types';
import { apiFetch } from '../lib/apiFetch';

export async function fetchSessionPages(stack: string, query: string, pages: number, signal: AbortSignal): Promise<SessionsHistoryPage> {
  const result: SessionsHistoryPage = { sessions: [], services: [], active_count: 0, next_before_id: null };
  const params = new URLSearchParams(query);
  params.set('limit', '500');
  for (let i = 0; i < pages; i++) {
    const response = await apiFetch(`/api/sessions/${encodeURIComponent(stack)}/history?${params}`, { signal });
    if (!response.ok) throw new Error(`Could not load sessions (HTTP ${response.status})`);
    const page: SessionsHistoryPage = await response.json();
    result.sessions.push(...page.sessions);
    result.services = page.services;
    result.active_count = page.active_count;
    result.next_before_id = page.next_before_id;
    if (page.next_before_id == null) break;
    params.set('before_id', String(page.next_before_id));
  }
  return result;
}

export function useSessionHistory(stack: string, query: string, pages = 1, poll = true) {
  const key = `${stack}\0${query}\0${pages}`;
  const [tick, setTick] = useState(0);
  const [state, setState] = useState<{ key: string; data: SessionsHistoryPage | null; error: string | null; updatedAt: number | null }>({ updatedAt: null, key: '', data: null, error: null });
  useEffect(() => {
    const controller = new AbortController();
    let timer: ReturnType<typeof setTimeout>;
    fetchSessionPages(stack, query, pages, controller.signal)
      .then(data => { if (!controller.signal.aborted) setState({ key, data, error: null, updatedAt: Date.now() }); })
      .catch(error => {
        if (!controller.signal.aborted) setState(prev => ({ key, data: prev.key === key ? prev.data : null, error: String(error), updatedAt: prev.key === key ? prev.updatedAt : null }));
      })
      .finally(() => {
        if (poll && !controller.signal.aborted) timer = setTimeout(() => setTick(t => t + 1), 5000);
      });
    return () => { controller.abort(); clearTimeout(timer); };
  }, [stack, query, pages, poll, tick, key]);
  const current = state.key === key;
  return {
    data: current ? state.data : null,
    updatedAt: current ? state.updatedAt : null,
    error: current ? state.error : null,
    loading: !current || (!state.data && !state.error),
    refresh: () => setTick(t => t + 1),
  };
}
