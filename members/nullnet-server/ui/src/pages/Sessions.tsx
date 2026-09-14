import { useState } from 'react';
import { useSearchParams } from 'react-router-dom';
import SessionRows from '../components/SessionRows';
import Layout from '../components/Layout';
import { useStack } from '../StackContext';
import TimeSpanFilter from '../components/TimeSpanFilter';
import { useSessionHistory } from '../hooks/useSessionHistory';

export default function Sessions() {
  const { stack } = useStack();
  const [searchParams, setSearchParams] = useSearchParams();
  const directionFilter = searchParams.get('direction') ?? '';
  const serviceFilter = searchParams.get('service') ?? '';
  const statusFilter = (searchParams.get('status') ?? '');
  const policyFilter = (searchParams.get('policy') ?? '');

  function setFilter(key: string, value: string) {
    setSearchParams(prev => {
      const next = new URLSearchParams(prev);
      if (value) next.set(key, value); else next.delete(key);
      return next;
    }, { replace: true });
  }

  const since = searchParams.get('since');
  const until = searchParams.get('until');
  const span = since != null && until != null ? { since: Number(since), until: Number(until) } : null;
  const query = new URLSearchParams();
  if (directionFilter) query.set('direction', directionFilter);
  if (serviceFilter) query.set('service', serviceFilter);
  if (statusFilter) query.set('active', String(statusFilter === 'active'));
  if (policyFilter) query.set('blocked', String(policyFilter === 'blocked'));
  if (since != null) query.set('since', since);
  if (until != null) query.set('until', until);
  const queryKey = `${stack}\0${query}`;
  const [pagination, setPagination] = useState({ key: queryKey, pages: 1 });
  const pages = pagination.key === queryKey ? pagination.pages : 1;
  const { data, loading, error, refresh } = useSessionHistory(stack, query.toString(), pages);
  const services = data?.services ?? [];
  const activeCount = data?.active_count ?? 0;
  const nextBeforeId = data?.next_before_id;
  const sessions = data?.sessions ?? [];

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
  return (
    <Layout
      page="sessions"
      topbarRight={<span className="live-row"><span className="live-dot"></span>live · 5s</span>}
    >
      <div className="content">
        <div className="hero-row">
          <span className="hero-num">{activeCount}</span>
          <span className="hero-label">active sessions · {sessions.length} loaded</span>
        </div>

        <div style={{ marginBottom: 16 }}>
          <TimeSpanFilter key={`${since}:${until}`} value={span} defaultLabel="All time" onChange={next => setSearchParams(prev => {
            const params = new URLSearchParams(prev);
            if (next) { params.set('since', String(next.since)); params.set('until', String(next.until)); }
            else { params.delete('since'); params.delete('until'); }
            return params;
          }, { replace: true })} />
          {error && <div role="alert" style={{ color: 'var(--red)', marginTop: 8 }}>{error}</div>}
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
                  Kind
                  {/* The popup list is a native surface, not composited over the
                      page — it needs an explicit opaque background (see Events.tsx). */}
                  <select value={directionFilter} onChange={e => setFilter('direction', e.target.value)} style={selectStyle}>
                    <option value="" style={optionStyle}>All</option>
                    <option value="ingress" style={optionStyle}>Ingress</option>
                    <option value="egress" style={optionStyle}>Egress</option>
                    <option value="backend" style={optionStyle}>Backend</option>
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
                <th style={th(140)}>Net ID</th>
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
              <SessionRows sessions={sessions} refresh={refresh} stackedNet />
              {!loading && sessions.length === 0 && (
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
                      onClick={() => setPagination({ key: queryKey, pages: pages + 1 })}
                      disabled={loading}
                      style={{
                        background: 'var(--g1)',
                        border: '1px solid var(--gb)',
                        color: 'var(--t2)',
                        borderRadius: 4,
                        padding: '4px 14px',
                        fontSize: 11,
                        cursor: loading ? 'default' : 'pointer',
                      }}
                    >
                      {loading ? 'Loading…' : 'Load older'}
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
