import { useState } from 'react';
import Layout from '../components/Layout';
import { useApi } from '../hooks/useApi';
import { useStack } from '../StackContext';
import type { SessionJson } from '../types';

export default function Sessions() {
  const { stack } = useStack();
  const { data: sessions, loading, refetch } = useApi<SessionJson[]>(`/api/sessions/${stack}`, 5000);
  const [tearing, setTearing] = useState<Set<number>>(new Set());

  async function teardown(id: number) {
    if (!confirm(`Force teardown session ${id}?`)) return;
    setTearing(prev => new Set(prev).add(id));
    try {
      await fetch(`/api/sessions/${stack}/${id}`, { method: 'DELETE' });
      refetch();
    } finally {
      setTearing(prev => { const next = new Set(prev); next.delete(id); return next; });
    }
  }

  function formatTime(unix: number) {
    return new Date(unix * 1000).toLocaleTimeString();
  }

  const list = sessions ?? [];

  return (
    <Layout
      page="sessions"
      topbarRight={
        <span className="live-row"><span className="live-dot"></span>live · 5s</span>
      }
    >
      <div className="content">
        <div className="hero-row">
          <span className="hero-num">{list.length}</span>
          <span className="hero-label">active isolated networks</span>
        </div>

        <div className="card">
          <div className="card-head">
            <span className="card-label">Active Sessions</span>
            <span style={{ fontSize: 11, color: 'var(--t2)' }}>auto-refresh 5s</span>
          </div>
          <table className="tbl">
            <thead>
              <tr>
                <th>Net ID</th>
                <th>Service</th>
                <th>Client IP</th>
                <th>Client Net</th>
                <th>Server Net</th>
                <th>Chains</th>
                <th>Created</th>
                <th></th>
              </tr>
            </thead>
            <tbody>
              {loading && (
                <tr><td colSpan={8} style={{ color: 'var(--t2)', padding: '20px 16px' }}>Loading…</td></tr>
              )}
              {list.map(s => (
                <tr key={s.id}>
                  <td>
                    <span style={{ fontFamily: "'JetBrains Mono',monospace", fontWeight: 500, color: 'var(--blue)' }}>
                      {s.id}
                    </span>
                  </td>
                  <td style={{ fontWeight: 500 }}>{s.service}</td>
                  <td style={{ fontFamily: "'JetBrains Mono',monospace", color: 'var(--t1)' }}>{s.client_ip}</td>
                  <td style={{ fontFamily: "'JetBrains Mono',monospace", fontSize: 11, color: 'var(--cyan)' }}>{s.client_net}</td>
                  <td style={{ fontFamily: "'JetBrains Mono',monospace", fontSize: 11, color: 'var(--cyan)' }}>{s.server_net}</td>
                  <td style={{ fontFamily: "'JetBrains Mono',monospace", color: s.chain_depth > 1 ? 'var(--amber)' : 'var(--t1)' }}>
                    {s.chain_depth}
                  </td>
                  <td style={{ fontFamily: "'JetBrains Mono',monospace", fontSize: 10, color: 'var(--t2)' }}>
                    {formatTime(s.created_at)}
                  </td>
                  <td>
                    <button
                      className="teardown-btn"
                      onClick={() => teardown(s.id)}
                      disabled={tearing.has(s.id)}
                    >
                      {tearing.has(s.id) ? '…' : 'Teardown'}
                    </button>
                  </td>
                </tr>
              ))}
              {!loading && list.length === 0 && (
                <tr><td colSpan={8} style={{ color: 'var(--t2)', padding: '20px 16px' }}>No active sessions</td></tr>
              )}
            </tbody>
          </table>
        </div>
      </div>
    </Layout>
  );
}
