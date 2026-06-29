import { useMemo } from 'react';
import Layout from '../components/Layout';
import { useApi } from '../hooks/useApi';
import { useStack } from '../StackContext';
import type { NodeJson, PoolJson } from '../types';
import { TopologyProvider, useTopologyData, useTopologyUI } from '../components/topology/TopologyContext';
import TopologyGraph from '../components/topology/TopologyGraph';
import TopologyPanel from '../components/topology/TopologyPanel';

function DashboardView() {
  const { stack } = useStack();
  const { graph, sessions, chains, services } = useTopologyData();
  const { panel, dispatch } = useTopologyUI();

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
  const registeredCount = graph?.nodes.filter(n => n.registered).length ?? 0;
  const proxyCount = graph
    ? new Set(graph.edges.filter(e => e.via_proxy).map(e => e.via_proxy!)).size
    : 0;
  const poolPct = pool ? ((pool.in_use / pool.total) * 100).toFixed(1) : null;

  // sorted: registered first, then alpha
  const sortedNodes = useMemo(() => {
    if (!graph) return [];
    return [...graph.nodes].sort((a, b) => {
      if (a.registered !== b.registered) return a.registered ? -1 : 1;
      return a.id.localeCompare(b.id);
    });
  }, [graph]);

  // look up max_networks from services context for the services card
  const maxNetByName = useMemo(() => {
    const m = new Map<string, number | undefined>();
    for (const s of services ?? []) m.set(s.name, s.max_networks);
    return m;
  }, [services]);

  return (
    <>
      <div className="content">
        {/* 3 rows: stats (auto) | info section (220px) | topology (auto, grows with content) */}
        <div style={{ display: 'grid', gridTemplateRows: 'auto 220px auto', gap: 12 }}>

          {/* ── Row 1: stat cards ── */}
          <div className="stats" style={{ marginBottom: 0 }}>
            <div className="stat glass">
              <div className="stat-label">Sessions</div>
              <div className="stat-value" style={{ color: 'var(--cyan)' }}>{sessionCount}</div>
              <div className="stat-sub">active sessions</div>
            </div>
            <div className="stat glass">
              <div className="stat-label">Services</div>
              <div className="stat-value">
                {registeredCount}<span className="denom">/{nodeCountG}</span>
              </div>
              <div className="stat-sub">{nodeCountG - registeredCount} unregistered</div>
            </div>
            <div className="stat glass">
              <div className="stat-label">Pool</div>
              <div className="stat-value" style={{ color: poolPct && parseFloat(poolPct) > 80 ? 'var(--amber)' : 'var(--t0)' }}>
                {poolPct !== null ? `${poolPct}%` : '—'}
              </div>
              <div className="stat-sub">{pool ? `${pool.in_use} / ${pool.total} in use` : 'loading…'}</div>
            </div>
            <div className="stat glass">
              <div className="stat-label">Nodes</div>
              <div className="stat-value" style={{ color: 'var(--green)' }}>{nodeCount}</div>
              <div className="stat-sub">connected</div>
            </div>
          </div>

          {/* ── Row 2: connections + services ── */}
          <div style={{ display: 'flex', gap: 12, minHeight: 0 }}>

            {/* Active connections */}
            <div className="card glass" style={{ flex: 1, display: 'flex', flexDirection: 'column', overflow: 'hidden', marginBottom: 0 }}>
              <div className="card-head">
                <span className="card-label">Active Connections</span>
                <span style={{ display: 'flex', gap: 10, alignItems: 'center', fontSize: 10, color: 'var(--t2)' }}>
                  <span>
                    <span style={{ fontFamily: "'JetBrains Mono',monospace", color: 'var(--blue)', marginRight: 4 }}>{edgeCount}</span>
                    edges
                  </span>
                  {proxyCount > 0 && (
                    <>
                      <span style={{ color: 'var(--t3)' }}>·</span>
                      <span style={{ color: '#fbbf24' }}>{proxyCount} via proxy</span>
                    </>
                  )}
                </span>
              </div>
              <div style={{ flex: 1, overflowY: 'auto' }}>
                {edgeCount === 0 ? (
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
                )}
              </div>
            </div>

            {/* Services status */}
            <div className="card glass" style={{ width: 300, flexShrink: 0, display: 'flex', flexDirection: 'column', overflow: 'hidden', marginBottom: 0 }}>
              <div className="card-head">
                <span className="card-label">Services</span>
                <span style={{ fontSize: 10, color: 'var(--t2)' }}>
                  {registeredCount} / {nodeCountG} registered
                </span>
              </div>
              <div style={{ flex: 1, overflowY: 'auto' }}>
                {sortedNodes.length === 0 ? (
                  <div style={{ padding: '20px 18px', color: 'var(--t2)', fontSize: 11, textAlign: 'center' }}>
                    No services
                  </div>
                ) : (
                  sortedNodes.map(node => {
                    const isHealthy = node.registered && node.active_replica_count > 0 && node.paused_replica_count === 0;
                    const isDegraded = node.registered && (node.active_replica_count === 0 || node.paused_replica_count > 0);
                    const dotColor = isHealthy ? 'var(--green)' : isDegraded ? 'var(--amber)' : 'var(--red)';
                    const dotGlow = isHealthy
                      ? '0 0 5px rgba(52,211,153,.7)'
                      : isDegraded
                        ? '0 0 5px rgba(251,191,36,.7)'
                        : '0 0 5px rgba(248,113,113,.7)';
                    const maxNet = maxNetByName.get(node.id);
                    const activeSessions = graph!.edges.filter(e => e.from === node.id || e.to === node.id).length;

                    return (
                      <div
                        key={node.id}
                        onClick={() => dispatch({ type: 'NODE_CLICKED', nodeId: node.id })}
                        style={{
                          display: 'flex', alignItems: 'center', gap: 10,
                          padding: '9px 16px',
                          borderBottom: '1px solid rgba(255,255,255,.03)',
                          cursor: 'pointer',
                          background: panel?.type === 'node' && panel.nodeId === node.id
                            ? 'rgba(91,156,246,.07)'
                            : undefined,
                          transition: 'background .12s',
                        }}
                        onMouseEnter={e => { if (!(panel?.type === 'node' && panel.nodeId === node.id)) (e.currentTarget as HTMLElement).style.background = 'rgba(255,255,255,.025)'; }}
                        onMouseLeave={e => { if (!(panel?.type === 'node' && panel.nodeId === node.id)) (e.currentTarget as HTMLElement).style.background = ''; }}
                      >
                        <span style={{ width: 6, height: 6, borderRadius: '50%', background: dotColor, boxShadow: dotGlow, flexShrink: 0 }} />
                        <span style={{ flex: 1, fontSize: 12, fontWeight: 500, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                          {node.id}
                        </span>
                        {node.entry_point && (
                          <span style={{ fontSize: 9, padding: '1px 6px', borderRadius: 20, background: 'rgba(167,139,250,.12)', border: '1px solid rgba(167,139,250,.25)', color: 'var(--purple)', flexShrink: 0, letterSpacing: '.04em' }}>
                            EP
                          </span>
                        )}
                        <span style={{ fontFamily: "'JetBrains Mono',monospace", fontSize: 10, color: 'var(--t2)', flexShrink: 0 }}>
                          {node.active_replica_count}
                          {maxNet !== undefined
                            ? <span style={{ color: 'var(--t3)' }}>/{maxNet}</span>
                            : <span style={{ color: 'var(--t3)' }}>/{node.replica_count}</span>
                          }
                        </span>
                        {activeSessions > 0 && (
                          <span style={{ fontFamily: "'JetBrains Mono',monospace", fontSize: 9, color: 'var(--cyan)', flexShrink: 0 }}>
                            {activeSessions}↔
                          </span>
                        )}
                      </div>
                    );
                  })
                )}
              </div>
            </div>

          </div>

          {/* ── Row 3: topology grows to fit content ── */}
          <div className="card glass" style={{ marginBottom: 0 }}>
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
              {graph && graph.nodes.length > 0 && (
                <TopologyGraph grow anchor="top-left" />
              )}
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
