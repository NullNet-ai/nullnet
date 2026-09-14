import { useMemo } from 'react';
import SessionRows from '../components/SessionRows';
import Layout from '../components/Layout';
import { useApi } from '../hooks/useApi';
import { useStack } from '../StackContext';
import type { NodeJson } from '../types';
import { TopologyProvider, useTopologyData, useTopologyUI } from '../components/topology/TopologyContext';
import TopologyGraph from '../components/topology/TopologyGraph';
import TopologyPanel from '../components/topology/TopologyPanel';

function DashboardView() {
  const { stack } = useStack();
  const { graph, services, sessionHistory, refreshSessions, loading, error } = useTopologyData();
  const { panel, dispatch } = useTopologyUI();

  const { data: nodes } = useApi<NodeJson[]>(`/api/nodes/${stack}`, 5000);
  const liveSessions = sessionHistory?.sessions ?? [];
  const sessionCount = sessionHistory?.active_count ?? 0;
  const nodeCount = nodes?.length ?? 0;
  const edgeCount = graph?.edges.length ?? 0;
  const nodeCountG = graph?.nodes.length ?? 0;
  const registeredCount = graph?.nodes.filter(n => n.registered).length ?? 0;
  const proxyCount = graph
    ? new Set(graph.edges.filter(e => e.via_proxy).map(e => e.via_proxy!)).size
    : 0;

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
              <div className="stat-label">Nodes</div>
              <div className="stat-value" style={{ color: 'var(--green)' }}>{nodeCount}</div>
              <div className="stat-sub">connected</div>
            </div>
          </div>

          {/* ── Row 2: connections + services ── */}
          <div style={{ display: 'flex', gap: 12, minHeight: 0 }}>

            <div className="card glass" style={{ flex: 1, minWidth: 0, display: 'flex', flexDirection: 'column', overflow: 'hidden', marginBottom: 0 }}>
              <div className="card-head">
                <span className="card-label">Live Sessions</span>
                <span style={{ fontSize: 10, color: 'var(--t2)' }}>{liveSessions.length} active</span>
              </div>
              <div style={{ flex: 1, overflow: 'auto' }}>
                {error && <div role="alert" style={{ padding: '10px 16px', color: 'var(--red)' }}>{error}</div>}
                <table className="tbl">
                  <thead>
                    <tr>
                      <th>Status</th><th>Service</th><th>Kind</th><th>Policy</th><th>Net ID</th>
                      <th>Peer</th><th>Started</th><th>Ended</th><th>Duration</th><th></th>
                    </tr>
                  </thead>
                  <tbody>
                    <SessionRows sessions={liveSessions} refresh={refreshSessions} />
                    {liveSessions.length === 0 && <tr><td colSpan={10} style={{ padding: '20px 16px', color: 'var(--t2)' }}>
                      {loading ? 'Loading…' : 'No active sessions'}
                    </td></tr>}
                  </tbody>
                </table>
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
          <span className="live-dot" />live · 5s
        </span>
      }
    >
      <TopologyProvider key={stack} stack={stack}>
        <DashboardView />
      </TopologyProvider>
    </Layout>
  );
}
