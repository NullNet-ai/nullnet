import { useMemo, useState } from 'react';
import Layout from '../components/Layout';
import { useApi } from '../hooks/useApi';
import { useStack } from '../StackContext';
import type { NodeJson, PoolJson } from '../types';
import { TopologyProvider, useTopologyData, useTopologyUI } from '../components/topology/TopologyContext';
import TopologyGraph from '../components/topology/TopologyGraph';
import TopologyPanel from '../components/topology/TopologyPanel';

function DashboardView() {
  const { stack } = useStack();
  const { graph, sessions, chains } = useTopologyData();
  const { panel, dispatch } = useTopologyUI();
  const [connectionsOpen, setConnectionsOpen] = useState(true);

  const { data: nodes } = useApi<NodeJson[]>(`/api/nodes/${stack}`, 5000);
  const { data: pool } = useApi<PoolJson>('/api/pool', 5000);

  const chainByProxyNetId = useMemo(() => {
    const m = new Map<number, number[]>();
    for (const c of chains ?? []) m.set(c.proxy_net_id, c.all_net_ids);
    return m;
  }, [chains]);

  const sessionByNetId = useMemo(() => {
    const m = new Map<number, NonNullable<typeof sessions>[number]>();
    for (const s of sessions ?? []) m.set(s.network_id, s);
    return m;
  }, [sessions]);

  const sessionCount = sessions?.length ?? 0;
  const nodeCount = nodes?.length ?? 0;
  const edgeCount = graph?.edges.length ?? 0;
  const nodeCountG = graph?.nodes.length ?? 0;
  const proxyCount = graph
    ? new Set(graph.edges.filter(e => e.via_proxy).map(e => e.via_proxy!)).size
    : 0;
  const poolPct = pool ? ((pool.in_use / pool.total) * 100).toFixed(1) : null;

  return (
    <>
      <div className="content">

        {/* Active Connections — always-visible collapsible card with stats in header */}
        <div className="card glass" style={{ marginBottom: 12 }}>
          <div
            className="card-head"
            onClick={() => setConnectionsOpen(v => !v)}
            style={{ cursor: 'pointer', userSelect: 'none' }}
          >
            <span className="card-label">Active Connections</span>
            <span style={{ display: 'flex', gap: 14, alignItems: 'center', fontSize: 10, color: 'var(--t2)' }}>
              <span>
                <span style={{ fontFamily: "'JetBrains Mono',monospace", color: 'var(--cyan)', marginRight: 4 }}>{sessionCount}</span>
                sessions
              </span>
              <span style={{ color: 'var(--t3)' }}>·</span>
              <span>
                <span style={{ fontFamily: "'JetBrains Mono',monospace", color: 'var(--green)', marginRight: 4 }}>{nodeCount}</span>
                nodes
              </span>
              <span style={{ color: 'var(--t3)' }}>·</span>
              <span>
                <span style={{ fontFamily: "'JetBrains Mono',monospace", color: 'var(--blue)', marginRight: 4 }}>{edgeCount}</span>
                edges
              </span>
              {poolPct !== null && (
                <>
                  <span style={{ color: 'var(--t3)' }}>·</span>
                  <span>
                    <span style={{ fontFamily: "'JetBrains Mono',monospace", color: 'var(--t1)', marginRight: 4 }}>{poolPct}%</span>
                    pool
                  </span>
                </>
              )}
              <span style={{ fontSize: 11, lineHeight: 1, marginLeft: 2 }}>{connectionsOpen ? '▾' : '▸'}</span>
            </span>
          </div>

          {connectionsOpen && (
            edgeCount === 0 ? (
              <div style={{ padding: '20px 18px', color: 'var(--t2)', fontSize: 11, textAlign: 'center' }}>
                No active connections
              </div>
            ) : (
              <table className="tbl">
                <thead>
                  <tr>
                    <th>From</th>
                    <th>Via Proxy</th>
                    <th>To</th>
                    <th>Net ID</th>
                    <th>Client Net</th>
                    <th>Server Net</th>
                    <th>Setup</th>
                  </tr>
                </thead>
                <tbody>
                  {graph!.edges.map((e, i) => {
                    const session = sessionByNetId.get(e.net_id);
                    return (
                      <tr
                        key={i}
                        onClick={() => dispatch({ type: 'EDGE_CLICKED', fromId: e.from, toId: e.to, edgeIndices: [i] })}
                        style={{
                          cursor: 'pointer',
                          background: panel?.type === 'edge' && panel.edgeIndices.includes(i)
                            ? 'rgba(91,156,246,.07)'
                            : undefined,
                        }}
                      >
                        <td style={{ fontFamily: "'JetBrains Mono',monospace", fontWeight: 500 }}>{e.from}</td>
                        <td style={{ fontFamily: "'JetBrains Mono',monospace", fontSize: 11, color: '#fbbf24' }}>
                          {e.via_proxy ?? <span style={{ color: 'var(--t3)' }}>—</span>}
                        </td>
                        <td style={{ fontFamily: "'JetBrains Mono',monospace", fontWeight: 500 }}>{e.to}</td>
                        <td style={{ fontFamily: "'JetBrains Mono',monospace", color: 'var(--cyan)' }}>
                          {e.via_proxy && chainByProxyNetId.has(e.net_id)
                            ? chainByProxyNetId.get(e.net_id)!.join(', ')
                            : e.net_id}
                        </td>
                        <td style={{ fontFamily: "'JetBrains Mono',monospace", fontSize: 11, color: 'var(--t1)' }}>
                          {session?.client_net ?? <span style={{ color: 'var(--t3)' }}>—</span>}
                        </td>
                        <td style={{ fontFamily: "'JetBrains Mono',monospace", fontSize: 11, color: 'var(--t1)' }}>
                          {session?.server_net ?? <span style={{ color: 'var(--t3)' }}>—</span>}
                        </td>
                        <td style={{ fontFamily: "'JetBrains Mono',monospace", color: 'var(--t2)', fontSize: 11 }}>
                          {e.setup_ms > 0 ? `${e.setup_ms}ms` : '—'}
                        </td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            )
          )}
        </div>

        {/* Full-width topology card */}
        <div className="card glass">
          <div className="card-head">
            <span className="card-label">Service Topology</span>
            <span style={{ fontSize: 10, color: 'var(--t2)', display: 'flex', gap: 10, alignItems: 'center' }}>
              {graph ? (
                <>
                  <span>{nodeCountG} services</span>
                  {proxyCount > 0 && <span style={{ color: '#fbbf24' }}>{proxyCount} prox{proxyCount === 1 ? 'y' : 'ies'}</span>}
                  <span>{edgeCount} edges</span>
                </>
              ) : 'loading…'}
            </span>
          </div>

          <div style={{ background: 'rgba(0,0,0,.25)' }}>
            {!graph && (
              <div style={{ color: 'var(--t2)', fontSize: 11, padding: '40px 0', textAlign: 'center' }}>
                loading topology…
              </div>
            )}
            {graph && graph.nodes.length === 0 && (
              <div style={{ color: 'var(--t2)', fontSize: 11, padding: '40px 0', textAlign: 'center' }}>
                No services registered for stack <b>{stack}</b>
              </div>
            )}
            {graph && graph.nodes.length > 0 && <TopologyGraph />}
          </div>

          {proxyCount > 0 && (
            <div style={{ padding: '8px 14px 10px', display: 'flex', gap: 16, fontSize: 10, color: 'var(--t2)', borderTop: '1px solid var(--gb)' }}>
              <span style={{ display: 'flex', alignItems: 'center', gap: 5 }}>
                <span style={{ width: 20, height: 1.5, background: 'rgba(255,255,255,.25)', display: 'inline-block' }} />
                direct
              </span>
              <span style={{ display: 'flex', alignItems: 'center', gap: 5 }}>
                <span style={{ width: 20, height: 1.5, display: 'inline-block', backgroundImage: 'repeating-linear-gradient(90deg,rgba(251,191,36,.45) 0,rgba(251,191,36,.45) 4px,transparent 4px,transparent 7px)' }} />
                via proxy
              </span>
            </div>
          )}
        </div>

      </div>

      <TopologyPanel />
    </>
  );
}

export default function Dashboard() {
  const { stack } = useStack();
  return (
    <Layout
      page="dashboard"
      topbarRight={
        <span style={{ fontSize: 11, color: 'var(--t1)', display: 'flex', alignItems: 'center', gap: 5 }}>
          <span className="live-dot" />live · SSE
        </span>
      }
    >
      <TopologyProvider stack={stack}>
        <DashboardView />
      </TopologyProvider>
    </Layout>
  );
}
