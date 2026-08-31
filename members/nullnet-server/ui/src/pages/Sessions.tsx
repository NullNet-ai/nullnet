import { useState, useEffect, useCallback, useMemo } from 'react';
import { useSearchParams } from 'react-router-dom';
import Layout from '../components/Layout';
import { apiFetch } from '../lib/apiFetch';
import { useStack } from '../StackContext';
import type { SessionDirection, SessionRecordJson, SessionsHistoryPage } from '../types';
import { flagEmoji, countryName } from '../geo';
import { formatTimestamp, formatTimestampFull } from '../lib/time';

const PAGE_SIZE = 100;
const REFRESH_MS = 5000;

const DIRECTIONS: SessionDirection[] = ['ingress', 'egress'];
const DIRECTION_COLOR: Record<SessionDirection, string> = {
  ingress: 'var(--cyan)',
  egress: 'var(--amber)',
};
// Which way the external peer sits relative to the service.
const DIRECTION_ARROW: Record<SessionDirection, string> = { ingress: '←', egress: '→' };

type StatusFilter = '' | 'active' | 'ended';

function buildQuery(
  direction: string,
  service: string,
  status: StatusFilter,
  beforeId: number | null,
): string {
  const params = new URLSearchParams();
  if (direction) params.set('direction', direction);
  if (service) params.set('service', service);
  if (status) params.set('active', String(status === 'active'));
  if (beforeId != null) params.set('before_id', String(beforeId));
  params.set('limit', String(PAGE_SIZE));
  return params.toString();
}

/// Newest first. `started_at` rather than `id` so the live rows the server
/// synthesizes for sessions with no stored row (negative ids) still sort by age.
function byRecency(a: SessionRecordJson, b: SessionRecordJson): number {
  return b.started_at - a.started_at || b.id - a.id;
}

function duration(from: number, to: number): string {
  const secs = Math.max(0, to - from);
  if (secs < 60) return `${secs}s`;
  if (secs < 3600) return `${Math.floor(secs / 60)}m ${secs % 60}s`;
  const hours = Math.floor(secs / 3600);
  return `${hours}h ${Math.floor((secs % 3600) / 60)}m`;
}

/// The direction-specific middle column: the path the session actually takes.
function detailText(s: SessionRecordJson): string {
  if (s.direction === 'ingress') {
    const hop = `${s.detail.client_net ?? '?'} → ${s.detail.server_net ?? '?'}`;
    const depth = s.detail.chain_depth;
    return depth != null && depth > 1 ? `${hop} · ${depth} chains` : hop;
  }
  const from = s.detail.container ?? s.detail.node_ip ?? '?';
  return `${from} → proxy ${s.detail.proxy_ip ?? '?'}`;
}

export default function Sessions() {
  const { stack } = useStack();
  const [searchParams, setSearchParams] = useSearchParams();
  const directionFilter = searchParams.get('direction') ?? '';
  const serviceFilter = searchParams.get('service') ?? '';
  const statusFilter = (searchParams.get('status') ?? '') as StatusFilter;

  function setFilter(key: string, value: string) {
    setSearchParams(prev => {
      const next = new URLSearchParams(prev);
      if (value) next.set(key, value); else next.delete(key);
      return next;
    }, { replace: true });
  }

  const [rows, setRows] = useState<SessionRecordJson[]>([]);
  const [nextBeforeId, setNextBeforeId] = useState<number | null>(null);
  const [services, setServices] = useState<string[]>([]);
  const [activeCount, setActiveCount] = useState(0);
  const [loading, setLoading] = useState(true);
  const [loadingOlder, setLoadingOlder] = useState(false);
  const [tearing, setTearing] = useState<Set<number>>(new Set());

  const fetchPage = useCallback(async (beforeId: number | null): Promise<SessionsHistoryPage> => {
    const qs = buildQuery(directionFilter, serviceFilter, statusFilter, beforeId);
    const res = await apiFetch(`/api/sessions/${stack}/history?${qs}`);
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    return res.json();
  }, [stack, directionFilter, serviceFilter, statusFilter]);

  // Refresh the newest page. Older pages already pulled in stay put: their rows
  // are merged by id, so a session that ended since the last poll updates in
  // place instead of the list jumping back to page one.
  const refresh = useCallback(async (replace: boolean) => {
    try {
      const page = await fetchPage(null);
      setServices(page.services);
      setActiveCount(page.active_count);
      setNextBeforeId(prev => (replace ? page.next_before_id : prev));
      setRows(prev => {
        if (replace) return page.sessions;
        const byId = new Map(prev.map(r => [r.id, r]));
        // Synthesized live rows (negative ids) only exist while the server still
        // reports them; drop the stale ones rather than pinning them forever.
        for (const r of prev) if (r.id < 0) byId.delete(r.id);
        for (const r of page.sessions) byId.set(r.id, r);
        return [...byId.values()].sort(byRecency);
      });
    } catch {
      if (replace) {
        setRows([]);
        setNextBeforeId(null);
      }
    }
  }, [fetchPage]);

  // Reload from scratch whenever the stack or a filter changes.
  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    fetchPage(null)
      .then(page => {
        if (cancelled) return;
        setRows(page.sessions);
        setNextBeforeId(page.next_before_id);
        setServices(page.services);
        setActiveCount(page.active_count);
      })
      .catch(() => {
        if (cancelled) return;
        setRows([]);
        setNextBeforeId(null);
      })
      .finally(() => { if (!cancelled) setLoading(false); });
    return () => { cancelled = true; };
  }, [fetchPage]);

  useEffect(() => {
    const id = setInterval(() => refresh(false), REFRESH_MS);
    return () => clearInterval(id);
  }, [refresh]);

  const loadOlder = useCallback(async () => {
    if (nextBeforeId == null || loadingOlder) return;
    setLoadingOlder(true);
    try {
      const page = await fetchPage(nextBeforeId);
      setRows(prev => [...prev, ...page.sessions].sort(byRecency));
      setNextBeforeId(page.next_before_id);
    } catch {
      // leave the cursor as-is; the button stays clickable to retry
    } finally {
      setLoadingOlder(false);
    }
  }, [fetchPage, nextBeforeId, loadingOlder]);

  async function teardown(netId: number) {
    if (!confirm(`Force teardown session ${netId}?`)) return;
    setTearing(prev => new Set(prev).add(netId));
    try {
      await apiFetch(`/api/sessions/${stack}/${netId}`, { method: 'DELETE' });
      await refresh(false);
    } finally {
      setTearing(prev => { const next = new Set(prev); next.delete(netId); return next; });
    }
  }

  const sorted = useMemo(() => rows.slice().sort(byRecency), [rows]);

  const chipStyle = (active: boolean, color: string) => ({
    background: active ? color : 'var(--g1)',
    border: `1px solid ${active ? color : 'var(--gb)'}`,
    color: active ? 'var(--bg, #0a0a0a)' : 'var(--t2)',
    borderRadius: 4,
    padding: '2px 10px',
    fontSize: 11,
    cursor: 'pointer',
    fontWeight: active ? 600 : 400,
  });

  const selectStyle = {
    background: 'var(--g1)',
    border: '1px solid var(--gb)',
    color: 'var(--t1)',
    borderRadius: 4,
    padding: '2px 6px',
    fontSize: 11,
    cursor: 'pointer',
  };
  const optionStyle = { background: 'var(--bg)', color: 'var(--t0)' };
  const mono = { fontFamily: "'JetBrains Mono',monospace" };

  return (
    <Layout
      page="sessions"
      topbarRight={<span className="live-row"><span className="live-dot"></span>live · 5s</span>}
    >
      <div className="content">
        <div className="hero-row">
          <span className="hero-num">{activeCount}</span>
          <span className="hero-label">active sessions · {sorted.length} loaded</span>
        </div>

        <div className="card">
          <div className="card-head" style={{ gap: 8, flexWrap: 'wrap' }}>
            <span className="card-label">Session History</span>
            <div style={{ display: 'flex', gap: 6, flex: 1, flexWrap: 'wrap', alignItems: 'center' }}>
              <button style={chipStyle(directionFilter === '', 'var(--t2)')} onClick={() => setFilter('direction', '')}>
                All
              </button>
              {DIRECTIONS.map(d => (
                <button
                  key={d}
                  style={chipStyle(directionFilter === d, DIRECTION_COLOR[d])}
                  onClick={() => setFilter('direction', directionFilter === d ? '' : d)}
                >
                  {d.charAt(0).toUpperCase() + d.slice(1)}
                </button>
              ))}

              <span style={{ width: 1, height: 16, background: 'var(--gb)', margin: '0 2px' }} />

              {/* The popup list is a native surface, not composited over the page —
                  it needs an explicit opaque background (see Events.tsx). */}
              <select value={serviceFilter} onChange={e => setFilter('service', e.target.value)} style={selectStyle}>
                <option value="" style={optionStyle}>All services</option>
                {services.map(s => (
                  <option key={s} value={s} style={optionStyle}>{s}</option>
                ))}
              </select>

              <select
                value={statusFilter}
                onChange={e => setFilter('status', e.target.value)}
                style={selectStyle}
              >
                <option value="" style={optionStyle}>Active &amp; ended</option>
                <option value="active" style={optionStyle}>Active only</option>
                <option value="ended" style={optionStyle}>Ended only</option>
              </select>
            </div>
            <span style={{ fontSize: 11, color: 'var(--t2)' }}>auto-refresh 5s</span>
          </div>

          <table className="tbl">
            <thead>
              <tr>
                <th style={{ width: 70 }}>Status</th>
                <th style={{ width: 80 }}>Flow</th>
                <th style={{ width: 60 }}>Net ID</th>
                <th>Service</th>
                <th>Peer</th>
                <th>Path</th>
                <th style={{ width: 110 }}>Started</th>
                <th style={{ width: 110 }}>Ended</th>
                <th></th>
              </tr>
            </thead>
            <tbody>
              {loading && (
                <tr><td colSpan={9} style={{ color: 'var(--t2)', padding: '20px 16px' }}>Loading…</td></tr>
              )}
              {sorted.map(s => {
                const live = s.ended_at == null;
                return (
                  <tr key={s.id} style={live ? undefined : { opacity: 0.72 }}>
                    <td style={{ whiteSpace: 'nowrap' }}>
                      <span
                        style={{
                          width: 6, height: 6, borderRadius: '50%', display: 'inline-block',
                          background: live ? 'var(--green)' : 'var(--t3)', marginRight: 5,
                        }}
                      />
                      <span style={{ fontSize: 10, color: live ? 'var(--green)' : 'var(--t2)' }}>
                        {live ? 'live' : 'ended'}
                      </span>
                    </td>
                    <td>
                      <span style={{ ...mono, fontSize: 10, color: DIRECTION_COLOR[s.direction] }}>
                        {s.direction}
                      </span>
                      {s.blocked && (
                        <div style={{ fontSize: 9, color: 'var(--red, #f87171)', fontWeight: 600 }}>blocked</div>
                      )}
                    </td>
                    <td style={{ ...mono, fontWeight: 500, color: 'var(--blue)' }}>{s.net_id}</td>
                    <td style={{ fontWeight: 500 }}>{s.service}</td>
                    <td style={{ ...mono, color: 'var(--t1)' }}>
                      <span style={{ color: 'var(--t3)', marginRight: 4 }}>{DIRECTION_ARROW[s.direction]}</span>
                      {flagEmoji(s.country_code) && (
                        <span title={countryName(s.country_code)} style={{ marginRight: 5, cursor: 'default' }}>
                          {flagEmoji(s.country_code)}
                        </span>
                      )}
                      {s.peer_ip}
                      {s.org && <div style={{ fontSize: 9, color: 'var(--t2)' }}>{s.org}</div>}
                    </td>
                    <td style={{ ...mono, fontSize: 11, color: 'var(--cyan)' }}>{detailText(s)}</td>
                    <td
                      style={{ ...mono, fontSize: 10, color: 'var(--t2)' }}
                      title={formatTimestampFull(s.started_at)}
                    >
                      {formatTimestamp(s.started_at)}
                    </td>
                    <td
                      style={{ ...mono, fontSize: 10, color: 'var(--t2)' }}
                      title={s.ended_at != null ? formatTimestampFull(s.ended_at) : undefined}
                    >
                      {s.ended_at != null
                        ? `${formatTimestamp(s.ended_at)} (${duration(s.started_at, s.ended_at)})`
                        : '—'}
                    </td>
                    <td>
                      {live && s.direction === 'ingress' && (
                        <button
                          className="teardown-btn"
                          onClick={() => teardown(s.net_id)}
                          disabled={tearing.has(s.net_id)}
                        >
                          {tearing.has(s.net_id) ? '…' : 'Teardown'}
                        </button>
                      )}
                    </td>
                  </tr>
                );
              })}
              {!loading && sorted.length === 0 && (
                <tr>
                  <td colSpan={9} style={{ color: 'var(--t2)', padding: '20px 16px' }}>
                    {directionFilter || serviceFilter || statusFilter
                      ? 'No matching sessions'
                      : 'No sessions recorded yet'}
                  </td>
                </tr>
              )}
              {nextBeforeId != null && (
                <tr>
                  <td colSpan={9} style={{ padding: '10px 16px', textAlign: 'center' }}>
                    <button
                      onClick={loadOlder}
                      disabled={loadingOlder}
                      style={{
                        background: 'var(--g1)',
                        border: '1px solid var(--gb)',
                        color: 'var(--t2)',
                        borderRadius: 4,
                        padding: '4px 14px',
                        fontSize: 11,
                        cursor: loadingOlder ? 'default' : 'pointer',
                      }}
                    >
                      {loadingOlder ? 'Loading…' : 'Load older'}
                    </button>
                  </td>
                </tr>
              )}
            </tbody>
          </table>
        </div>
      </div>
    </Layout>
  );
}
