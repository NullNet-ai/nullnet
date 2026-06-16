import Layout from '../components/Layout';
import { useStack } from '../StackContext';
import { TopologyProvider, useTopologyData, useTopologyUI } from '../components/topology/TopologyContext';
import TopologyGraph from '../components/topology/TopologyGraph';
import TopologyPanel from '../components/topology/TopologyPanel';

function TopologyView() {
  const { graph } = useTopologyData();
  const { panel, dispatch } = useTopologyUI();
  const { stack } = useStack();

  const nodeCount = graph?.nodes.length ?? 0;
  const registeredCount = graph?.nodes.filter(n => n.registered).length ?? 0;
  const edgeCount = graph?.edges.length ?? 0;
  const proxyCount = graph
    ? new Set(graph.edges.filter(e => e.via_proxy).map(e => e.via_proxy!)).size
    : 0;

  return (
    <>
      <div className="content">
        <div className="stats">
          <div className="stat glass">
            <div className="stat-label">Services</div>
            <div className="stat-value">{registeredCount}<span className="denom">/{nodeCount}</span></div>
            <div className="stat-sub">{nodeCount - registeredCount} unregistered</div>
          </div>
          <div className="stat glass">
            <div className="stat-label">Active Edges</div>
            <div className="stat-value" style={{ color: 'var(--cyan)' }}>{edgeCount}</div>
            <div className="stat-sub">live connections</div>
          </div>
          <div className="stat glass">
            <div className="stat-label">Entry Points</div>
            <div className="stat-value" style={{ color: 'var(--green)' }}>
              {graph?.nodes.filter(n => n.entry_point).length ?? '—'}
            </div>
            <div className="stat-sub">with timeout</div>
          </div>
        </div>


        <div className="card glass">
          <div className="card-head">
            <span className="card-label">Service Topology</span>
            <span style={{ fontSize: 10, color: 'var(--t2)', display: 'flex', gap: 10 }}>
              {graph ? (
                <>
                  <span>{nodeCount} services</span>
                  {proxyCount > 0 && <span style={{ color: '#fbbf24' }}>{proxyCount} prox{proxyCount === 1 ? 'y' : 'ies'}</span>}
                  <span>{edgeCount} edges</span>
                </>
              ) : 'loading…'}
            </span>
          </div>
          <div style={{ background: 'rgba(0,0,0,.25)', padding: 0 }}>
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
                <span style={{ width: 20, height: 1.5, display: 'inline-block', backgroundImage: 'repeating-linear-gradient(90deg, rgba(251,191,36,.45) 0, rgba(251,191,36,.45) 4px, transparent 4px, transparent 7px)' }} />
                via proxy
              </span>
            </div>
          )}
        </div>

        {graph && graph.edges.length > 0 && (
          <div className="card glass" style={{ marginTop: 12 }}>
            <div className="card-head">
              <span className="card-label">Active Connections</span>
            </div>
            <table className="tbl">
              <thead>
                <tr>
                  <th>From</th>
                  <th>Via Proxy</th>
                  <th>To</th>
                  <th>Net ID</th>
                  <th>Setup</th>
                </tr>
              </thead>
              <tbody>
                {graph.edges.map((e, i) => (
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
                    <td style={{ fontFamily: "'JetBrains Mono',monospace", color: 'var(--cyan)' }}>{e.net_id}</td>
                    <td style={{ fontFamily: "'JetBrains Mono',monospace", color: 'var(--t2)', fontSize: 11 }}>
                      {e.setup_ms > 0 ? `${e.setup_ms}ms` : '—'}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </div>

      <TopologyPanel />
    </>
  );
}

export default function Topology() {
  const { stack } = useStack();
  return (
    <Layout
      page="topology"
      topbarRight={
        <span style={{ fontSize: 11, color: 'var(--t1)', display: 'flex', alignItems: 'center', gap: 5 }}>
          <span className="live-dot" />live · SSE
        </span>
      }
    >
      <TopologyProvider stack={stack}>
        <TopologyView />
      </TopologyProvider>
    </Layout>
  );
}
