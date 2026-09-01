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

// Matches the topology: session/ingress edges are blue, egress edges purple
// (see TopologyGraphSvg's arr-egress marker, TopologyMatrix, EdgePanel's
// b-blue/b-purple badges). Amber is reserved for proxy hops there, so it must
// not be reused for a direction here.
const DIRECTION_BADGE: Record<SessionDirection, string> = {
  ingress: 'b-blue',
  egress: 'b-purple',
};

type StatusFilter = '' | 'active' | 'ended';
type PolicyFilter = '' | 'blocked' | 'allowed';

function buildQuery(
  direction: string,
  service: string,
  status: StatusFilter,
  policy: PolicyFilter,
  beforeId: number | null,
): string {
  const params = new URLSearchParams();
  if (direction) params.set('direction', direction);
  if (service) params.set('service', service);
  if (status) params.set('active', String(status === 'active'));
  if (policy) params.set('blocked', String(policy === 'blocked'));
  if (beforeId != null) params.set('before_id', String(beforeId));
  params.set('limit', String(PAGE_SIZE));
  return params.toString();
}

/// Most recently ended first. A session that has not ended sorts above every
/// one that has — it is still running, so its end is later than any of them.
/// Ties fall back to start time (`started_at` rather than `id`, so the active
/// rows the server synthesizes for sessions with no stored row still sort by age).
function byEnd(a: SessionRecordJson, b: SessionRecordJson): number {
  const ae = a.ended_at;
  const be = b.ended_at;
  if (ae != null && be != null && ae !== be) return be - ae;
  if (ae == null && be != null) return -1;
  if (ae != null && be == null) return 1;
  return b.started_at - a.started_at || b.id - a.id;
}

function duration(from: number, to: number): string {
  const secs = Math.max(0, to - from);
  if (secs < 60) return `${secs}s`;
  if (secs < 3600) return `${Math.floor(secs / 60)}m ${secs % 60}s`;
  const hours = Math.floor(secs / 3600);
  if (hours < 24) return `${hours}h ${Math.floor((secs % 3600) / 60)}m`;
  return `${Math.floor(hours / 24)}d ${hours % 24}h`;
}

export default function Sessions() {
  const { stack } = useStack();
  const [searchParams, setSearchParams] = useSearchParams();
  const directionFilter = searchParams.get('direction') ?? '';
  const serviceFilter = searchParams.get('service') ?? '';
  const statusFilter = (searchParams.get('status') ?? '') as StatusFilter;
  const policyFilter = (searchParams.get('policy') ?? '') as PolicyFilter;

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
  // Ticks with the poll so an active session's duration keeps counting up.
  const [now, setNow] = useState(() => Math.floor(Date.now() / 1000));

  const fetchPage = useCallback(async (beforeId: number | null): Promise<SessionsHistoryPage> => {
    const qs = buildQuery(directionFilter, serviceFilter, statusFilter, policyFilter, beforeId);
    const res = await apiFetch(`/api/sessions/${stack}/history?${qs}`);
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    return res.json();
  }, [stack, directionFilter, serviceFilter, statusFilter, policyFilter]);

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
        // Synthesized active rows (negative ids) only exist while the server still
        // reports them; drop the stale ones rather than pinning them forever.
        for (const r of prev) if (r.id < 0) byId.delete(r.id);
        for (const r of page.sessions) byId.set(r.id, r);
        return [...byId.values()];
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
    const id = setInterval(() => {
      setNow(Math.floor(Date.now() / 1000));
      refresh(false);
    }, REFRESH_MS);
    return () => clearInterval(id);
  }, [refresh]);

  const loadOlder = useCallback(async () => {
    if (nextBeforeId == null || loadingOlder) return;
    setLoadingOlder(true);
    try {
      const page = await fetchPage(nextBeforeId);
      setRows(prev => [...prev, ...page.sessions]);
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

  const sorted = useMemo(() => rows.slice().sort(byEnd), [rows]);

  // Sits inside its column's <th>, so it has to opt out of the uppercase +
  // letter-spacing `.tbl th` applies to its own label.
  const selectStyle = {
    display: 'block',
    marginTop: 5,
    background: 'var(--g1)',
    border: '1px solid var(--gb)',
    color: 'var(--t1)',
    borderRadius: 4,
    padding: '2px 4px',
    fontSize: 10,
    cursor: 'pointer',
    // A long option would otherwise set the control's intrinsic width and drag
    // the whole column wide, since the table is auto-layout. Clamp it to the
    // cell and let the closed box ellipsise; the native popup still shows each
    // name in full, and the title carries the current one.
    width: '100%',
    maxWidth: '100%',
    minWidth: 0,
    boxSizing: 'border-box' as const,
    textOverflow: 'ellipsis' as const,
    fontWeight: 400,
    textTransform: 'none' as const,
    letterSpacing: 'normal',
  };
  const optionStyle = { background: 'var(--bg)', color: 'var(--t0)' };
  // The filter columns are taller than the rest; without this the plain labels
  // would centre against them instead of lining up.
  const th = (width?: number) => ({ verticalAlign: 'top' as const, ...(width ? { width } : {}) });
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
          <div className="card-head">
            <span className="card-label">Session History</span>
            <span style={{ fontSize: 11, color: 'var(--t2)' }}>auto-refresh 5s</span>
          </div>

          <table className="tbl">
            <thead>
              <tr>
                <th style={th(100)}>
                  Status
                  <select value={statusFilter} onChange={e => setFilter('status', e.target.value)} style={selectStyle}>
                    <option value="" style={optionStyle}>All</option>
                    <option value="active" style={optionStyle}>Active</option>
                    <option value="ended" style={optionStyle}>Ended</option>
                  </select>
                </th>
                <th style={th(140)}>
                  Service
                  <select
                    value={serviceFilter}
                    onChange={e => setFilter('service', e.target.value)}
                    style={selectStyle}
                    title={serviceFilter || 'All services'}
                  >
                    <option value="" style={optionStyle}>All</option>
                    {services.map(s => (
                      <option key={s} value={s} style={optionStyle}>{s}</option>
                    ))}
                  </select>
                </th>
                <th style={th(110)}>
                  Direction
                  {/* The popup list is a native surface, not composited over the
                      page — it needs an explicit opaque background (see Events.tsx). */}
                  <select value={directionFilter} onChange={e => setFilter('direction', e.target.value)} style={selectStyle}>
                    <option value="" style={optionStyle}>All</option>
                    <option value="ingress" style={optionStyle}>Ingress</option>
                    <option value="egress" style={optionStyle}>Egress</option>
                  </select>
                </th>
                <th style={th(110)}>
                  Policy
                  <select value={policyFilter} onChange={e => setFilter('policy', e.target.value)} style={selectStyle}>
                    <option value="" style={optionStyle}>All</option>
                    <option value="allowed" style={optionStyle}>Allowed</option>
                    <option value="blocked" style={optionStyle}>Blocked</option>
                  </select>
                </th>
                <th style={th(60)}>Net ID</th>
                <th style={th()}>Peer</th>
                <th style={th(110)}>Started</th>
                <th style={th(110)}>Ended</th>
                <th style={th(80)}>Duration</th>
                <th style={th(80)}></th>
              </tr>
            </thead>
            <tbody>
              {loading && (
                <tr><td colSpan={10} style={{ color: 'var(--t2)', padding: '20px 16px' }}>Loading…</td></tr>
              )}
              {sorted.map(s => {
                const active = s.ended_at == null;
                const attempts = s.detail.attempts;
                return (
                  <tr key={s.id} style={active ? undefined : { opacity: 0.72 }}>
                    <td style={{ whiteSpace: 'nowrap' }}>
                      <span
                        style={{
                          width: 6, height: 6, borderRadius: '50%', display: 'inline-block',
                          background: active ? 'var(--green)' : 'var(--t3)', marginRight: 5,
                        }}
                      />
                      <span style={{ fontSize: 10, color: active ? 'var(--green)' : 'var(--t2)' }}>
                        {active ? 'active' : 'ended'}
                      </span>
                    </td>
                    <td style={{ fontWeight: 500, overflowWrap: 'anywhere' }} title={s.service}>
                      {s.service}
                    </td>
                    <td>
                      <span className={`badge ${DIRECTION_BADGE[s.direction]}`}>{s.direction}</span>
                    </td>
                    <td>
                      <span className={`badge ${s.blocked ? 'b-red' : 'b-green'}`}>
                        {s.blocked ? 'blocked' : 'allowed'}
                      </span>
                    </td>
                    <td style={{ ...mono, fontWeight: 500, color: 'var(--blue)' }}>
                      {/* A denied ingress connection is refused before an edge
                          exists, so it has no net id to show. */}
                      {s.net_id === 0 ? <span style={{ color: 'var(--t3)' }}>n/a</span> : s.net_id}
                    </td>
                    <td style={{ ...mono, color: 'var(--t1)' }}>
                      {flagEmoji(s.country_code) && (
                        <span title={countryName(s.country_code)} style={{ marginRight: 5, cursor: 'default' }}>
                          {flagEmoji(s.country_code)}
                        </span>
                      )}
                      {s.peer_ip}
                      {s.org && <div style={{ fontSize: 9, color: 'var(--t2)' }}>{s.org}</div>}
                    </td>
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
                      {s.ended_at != null ? formatTimestamp(s.ended_at) : '—'}
                    </td>
                    {/* A denied connection never ran, so its elapsed time says
                        nothing — how many times the peer tried does. */}
                    <td
                      style={{ ...mono, fontSize: 10, color: active ? 'var(--green)' : 'var(--t2)' }}
                      title={
                        attempts != null
                          ? `Denied over ${duration(s.started_at, s.last_seen)}`
                          : active ? 'Still running' : undefined
                      }
                    >
                      {attempts != null
                        ? `${attempts} attempt${attempts === 1 ? '' : 's'}`
                        : duration(s.started_at, s.ended_at ?? now)}
                    </td>
                    <td>
                      {active && s.direction === 'ingress' && (
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
                  <td colSpan={10} style={{ color: 'var(--t2)', padding: '20px 16px' }}>
                    {directionFilter || serviceFilter || statusFilter || policyFilter
                      ? 'No matching sessions'
                      : 'No sessions recorded yet'}
                  </td>
                </tr>
              )}
              {nextBeforeId != null && (
                <tr>
                  <td colSpan={10} style={{ padding: '10px 16px', textAlign: 'center' }}>
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
