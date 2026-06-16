import Layout from '../components/Layout';
import { useApi } from '../hooks/useApi';
import { useStack } from '../StackContext';
import type { SessionJson, ServiceJson, NodeJson, PoolJson, GraphJson } from '../types';
import TopologyGraphSvg from '../components/topology/TopologyGraphSvg';

export default function Dashboard() {
  const { stack } = useStack();
  const { data: sessions } = useApi<SessionJson[]>('/api/sessions', 5000);
  const { data: services } = useApi<ServiceJson[]>(`/api/services/${stack}`, 5000);
  const { data: nodes } = useApi<NodeJson[]>('/api/nodes', 5000);
  const { data: pool } = useApi<PoolJson>('/api/pool', 5000);
  const { data: graph } = useApi<GraphJson>(`/api/graph/${stack}`, 5000);

  const totalSvc = services?.length ?? 0;
  const onlineSvc = services?.filter(s => s.registered).length ?? 0;
  const sessionCount = sessions?.length ?? 0;
  const nodeCount = nodes?.length ?? 0;
  const poolPct = pool ? ((pool.in_use / pool.total) * 100).toFixed(1) : '—';
  const poolUsed = pool ? `${pool.in_use.toLocaleString()} of ${pool.total.toLocaleString()} IDs` : '—';

  return (
    <Layout
      page="dashboard"
      topbarRight={
        <span style={{ fontSize: 11, color: 'var(--t1)', display: 'flex', alignItems: 'center', gap: 5 }}>
          <span className="live-dot"></span>live · 5s
        </span>
      }
    >
      <div className="content">
        <div className="stats">
          <div className="stat glass">
            <div className="stat-label">Services Online</div>
            <div className="stat-value">
              {onlineSvc}<span className="denom">/{totalSvc}</span>
            </div>
            <div className="stat-sub">{totalSvc - onlineSvc} unregistered</div>
          </div>
          <div className="stat glass">
            <div className="stat-label">Active Sessions</div>
            <div className="stat-value" style={{ color: 'var(--cyan)' }}>{sessionCount}</div>
            <div className="stat-sub">isolated networks</div>
          </div>
          <div className="stat glass">
            <div className="stat-label">Connected Nodes</div>
            <div className="stat-value" style={{ color: 'var(--green)' }}>{nodeCount}</div>
            <div className="stat-sub">agent nodes</div>
          </div>
          <div className="stat glass">
            <div className="stat-label">Pool Used</div>
            <div className="stat-value">
              {pool ? <>{poolPct}<span className="denom">%</span></> : '—'}
            </div>
            <div className="stat-sub">{poolUsed}</div>
          </div>
        </div>

        <div className="bot-grid">
          <div className="card glass">
            <div className="card-head">
              <span className="card-label">Topology</span>
              <span style={{ fontSize: 10, color: 'var(--t2)', display: 'flex', alignItems: 'center', gap: 5 }}>
                <span className="live-dot" />live · 5s
              </span>
            </div>
            <div style={{ background: 'rgba(0,0,0,.25)', padding: 12 }}>
              {!graph && (
                <div style={{ color: 'var(--t2)', fontSize: 11, padding: '40px 0', textAlign: 'center' }}>loading topology…</div>
              )}
              {graph && <TopologyGraphSvg graph={graph} />}
            </div>
          </div>

          <div className="card glass">
            <div className="card-head">
              <span className="card-label">Recent Sessions</span>
            </div>
            {sessions && sessions.length > 0 ? (
              sessions.slice(0, 6).map(s => (
                <div key={s.id} className="ev">
                  <div className="ev-accent" style={{ background: 'var(--green)' }}></div>
                  <span className="ev-time">{new Date(s.created_at * 1000).toLocaleTimeString()}</span>
                  <span className="ev-text">
                    <b>NET {s.id}</b> — {s.service} ← {s.client_ip}
                  </span>
                </div>
              ))
            ) : (
              <div style={{ padding: '20px 18px', color: 'var(--t2)', fontSize: 11 }}>
                {sessions === null ? 'Loading…' : 'No active sessions'}
              </div>
            )}
          </div>
        </div>

        <div className="card glass" style={{ marginTop: 12 }}>
          <div className="card-head">
            <span className="card-label">Services</span>
            <span style={{ fontSize: 10, color: 'var(--t2)' }}>{onlineSvc}/{totalSvc} online</span>
          </div>
          <table className="tbl">
            <thead>
              <tr>
                <th>Name</th>
                <th>Status</th>
                <th>Replicas</th>
                <th>Dependencies</th>
                <th>Timeout</th>
              </tr>
            </thead>
            <tbody>
              {services?.map(svc => (
                <tr key={svc.name}>
                  <td style={{ fontWeight: 500 }}>{svc.name}</td>
                  <td>
                    <span className={`badge ${svc.registered ? 'b-green' : 'b-dim'}`}>
                      {svc.registered ? 'Online' : 'Offline'}
                    </span>
                  </td>
                  <td style={{ fontFamily: "'JetBrains Mono',monospace", color: 'var(--t1)' }}>
                    {svc.replicas.length}
                  </td>
                  <td>
                    {svc.proxy_dependencies.flat().map((d, i) => (
                      <span key={`${d}-${i}`} className="dep-tag">{d}</span>
                    ))}
                  </td>
                  <td style={{ fontFamily: "'JetBrains Mono',monospace", color: 'var(--t2)', fontSize: 11 }}>
                    {svc.timeout_secs ? `${svc.timeout_secs}s` : '—'}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </div>
    </Layout>
  );
}
