import { useState } from 'react';
import Layout from '../components/Layout';
import { useApi } from '../hooks/useApi';
import { useStack } from '../StackContext';
import type { ServiceJson } from '../types';

type Filter = 'all' | 'online' | 'offline';

export default function Services() {
  const { stack } = useStack();
  const { data: services, loading } = useApi<ServiceJson[]>(`/api/services/${stack}`, 5000);
  const [query, setQuery] = useState('');
  const [filter, setFilter] = useState<Filter>('all');
  const [expanded, setExpanded] = useState<Set<string>>(new Set());

  const toggle = (name: string) =>
    setExpanded(prev => {
      const next = new Set(prev);
      next.has(name) ? next.delete(name) : next.add(name);
      return next;
    });

  const visible = (services ?? []).filter(svc => {
    if (filter === 'online' && !svc.registered) return false;
    if (filter === 'offline' && svc.registered) return false;
    if (query && !svc.name.toLowerCase().includes(query.toLowerCase())) return false;
    return true;
  });

  return (
    <Layout
      page="services"
      topbarRight={
        <span className="live-row"><span className="live-dot"></span>live · 5s</span>
      }
    >
      <div className="content">
        <div className="page-head">
          <div className="page-title">Services</div>
          <div className="controls">
            <input
              className="search"
              type="text"
              placeholder="search…"
              value={query}
              onChange={e => setQuery(e.target.value)}
            />
            <button className={`filter-chip ${filter === 'all' ? 'on' : ''}`} onClick={() => setFilter('all')}>All</button>
            <button className={`filter-chip ${filter === 'online' ? 'on' : ''}`} onClick={() => setFilter('online')}>Online</button>
            <button className={`filter-chip ${filter === 'offline' ? 'on' : ''}`} onClick={() => setFilter('offline')}>Offline</button>
          </div>
        </div>

        <div className="card">
          <table className="tbl">
            <thead>
              <tr>
                <th style={{ width: 28 }}></th>
                <th>Name</th>
                <th>Status</th>
                <th>Replicas</th>
                <th>Dependencies</th>
                <th>Sessions</th>
                <th>Timeout</th>
              </tr>
            </thead>
            <tbody>
              {loading && (
                <tr><td colSpan={7} style={{ color: 'var(--t2)', padding: '20px 16px' }}>Loading…</td></tr>
              )}
              {visible.map(svc => {
                const isOpen = expanded.has(svc.name);
                const totalSessions = svc.replicas.reduce((n, r) => n + r.active_sessions, 0);
                const pausedCount = svc.replicas.filter(r => r.suspended).length;
                return (
                  <>
                    <tr key={svc.name} onClick={() => toggle(svc.name)} style={{ cursor: 'pointer' }}>
                      <td>
                        <button className="expand-btn">{isOpen ? '▾' : '›'}</button>
                      </td>
                      <td style={{ fontWeight: 500 }}>{svc.name}</td>
                      <td>
                        <span className={`badge ${svc.registered ? 'b-green' : 'b-dim'}`}>
                          {svc.registered ? 'Online' : 'Offline'}
                        </span>
                      </td>
                      <td style={{ fontFamily: "'JetBrains Mono',monospace", color: 'var(--t1)' }}>
                        {svc.replicas.length}
                        {pausedCount > 0 && (
                          <span className="badge b-amber" style={{ marginLeft: 6 }}>{pausedCount} paused</span>
                        )}
                      </td>
                      <td>
                        {svc.proxy_dependencies.flat().length > 0
                          ? svc.proxy_dependencies.flat().map((d, i) => <span key={`${d}-${i}`} className="dep-tag">{d}</span>)
                          : <span style={{ color: 'var(--t2)', fontSize: 11 }}>—</span>}
                      </td>
                      <td style={{ fontFamily: "'JetBrains Mono',monospace", color: 'var(--cyan)' }}>
                        {totalSessions > 0 ? totalSessions : <span style={{ color: 'var(--t2)' }}>0</span>}
                      </td>
                      <td style={{ fontFamily: "'JetBrains Mono',monospace", color: 'var(--t2)', fontSize: 11 }}>
                        {svc.timeout_secs ? `${svc.timeout_secs}s` : '—'}
                      </td>
                    </tr>
                    {isOpen && (
                      <tr key={`${svc.name}-expand`}>
                        <td colSpan={7} style={{ padding: 0 }}>
                          <div className="expand-inner open">
                            <div className="expand-grid">
                              <div className="exp-card">
                                <div className="exp-head">Replicas</div>
                                {svc.replicas.length > 0 ? (
                                  <table className="sub-tbl">
                                    <thead>
                                      <tr><th>IP</th><th>Port</th><th>Sessions</th><th>State</th>{svc.replicas.some(r => r.docker_container) && <th>Container</th>}</tr>
                                    </thead>
                                    <tbody>
                                      {svc.replicas.map((r, i) => (
                                        <tr key={i}>
                                          <td style={{ fontFamily: "'JetBrains Mono',monospace", color: 'var(--cyan)' }}>{r.ip}</td>
                                          <td style={{ fontFamily: "'JetBrains Mono',monospace", color: 'var(--t1)' }}>:{r.port}</td>
                                          <td style={{ fontFamily: "'JetBrains Mono',monospace", color: r.active_sessions > 0 ? 'var(--green)' : 'var(--t2)' }}>{r.active_sessions}</td>
                                          <td><span className={`badge ${r.suspended ? 'b-amber' : 'b-green'}`}>{r.suspended ? 'Paused' : 'Running'}</span></td>
                                          {r.docker_container && <td style={{ color: 'var(--t2)', fontSize: 10 }}>{r.docker_container}</td>}
                                        </tr>
                                      ))}
                                    </tbody>
                                  </table>
                                ) : (
                                  <div style={{ padding: '10px 12px', color: 'var(--t2)', fontSize: 11 }}>No replicas</div>
                                )}
                              </div>
                              <div className="exp-card">
                                <div className="exp-head">Config</div>
                                <table className="sub-tbl">
                                  <tbody>
                                    {svc.max_networks != null && (
                                      <tr>
                                        <td style={{ color: 'var(--t2)' }}>Max networks</td>
                                        <td style={{ fontFamily: "'JetBrains Mono',monospace" }}>{svc.max_networks}</td>
                                      </tr>
                                    )}
                                    {Object.entries(svc.triggers).map(([port, chain]) => (
                                      <tr key={port}>
                                        <td style={{ color: 'var(--t2)' }}>Trigger :{port}</td>
                                        <td style={{ color: 'var(--cyan)', fontSize: 10 }}>{chain.join(' → ')}</td>
                                      </tr>
                                    ))}
                                    {svc.proxy_dependencies.flat().length > 0 && (
                                      <tr>
                                        <td style={{ color: 'var(--t2)' }}>Dependencies</td>
                                        <td>{svc.proxy_dependencies.flat().map((d, i) => <span key={`${d}-${i}`} className="dep-tag">{d}</span>)}</td>
                                      </tr>
                                    )}
                                  </tbody>
                                </table>
                              </div>
                            </div>
                          </div>
                        </td>
                      </tr>
                    )}
                  </>
                );
              })}
              {!loading && visible.length === 0 && (
                <tr><td colSpan={7} style={{ color: 'var(--t2)', padding: '20px 16px' }}>No services match</td></tr>
              )}
            </tbody>
          </table>
        </div>
      </div>
    </Layout>
  );
}
