import { useState, useEffect, useRef } from 'react';
import Layout from '../components/Layout';
import { useApi } from '../hooks/useApi';
import { useStack } from '../StackContext';
import type { GraphJson, ServiceJson, SessionJson } from '../types';
import type { PanelState } from '../components/topology/types';
import { INTERNET_ID } from '../components/topology/types';
import TopologyGraph from '../components/topology/TopologyGraph';
import TopologyPanel from '../components/topology/TopologyPanel';

export default function Topology() {
  const { stack } = useStack();
  const { data: graph, refetch } = useApi<GraphJson>(`/api/graph/${stack}`);
  const { data: services } = useApi<ServiceJson[]>(`/api/services/${stack}`, 5000);
  const { data: sessions } = useApi<SessionJson[]>('/api/sessions', 5000);

  const [panel, setPanel] = useState<PanelState>(null);
  const [showRegistered, setShowRegistered] = useState(true);
  const [showUnregistered, setShowUnregistered] = useState(true);

  useEffect(() => { setPanel(null); }, [stack]);

  const refetchRef = useRef(refetch);
  refetchRef.current = refetch;
  useEffect(() => {
    const es = new EventSource('/api/events/stream');
    es.onmessage = (ev) => {
      try {
        const event = JSON.parse(ev.data);
        if (event.type === 'session_created' || event.type === 'session_torn_down') refetchRef.current();
      } catch { /* ignore */ }
    };
    return () => es.close();
  }, []);

  const nodeCount = graph?.nodes.length ?? 0;
  const registeredCount = graph?.nodes.filter(n => n.registered).length ?? 0;
  const edgeCount = graph?.edges.length ?? 0;
  const proxyCount = graph
    ? new Set(graph.edges.filter(e => e.via_proxy).map(e => e.via_proxy!)).size
    : 0;

  const selectedNodeId =
    panel?.type === 'node' ? panel.nodeId :
    panel?.type === 'internet' ? INTERNET_ID :
    null;
  const selectedEdgeKey =
    panel?.type === 'edge' ? `${panel.fromId}\0${panel.toId}` : null;

  function handleNodeClick(nodeId: string) {
    if (nodeId === INTERNET_ID) {
      setPanel(p => p?.type === 'internet' ? null : { type: 'internet' });
      return;
    }
    setPanel(p => p?.type === 'node' && p.nodeId === nodeId ? null : { type: 'node', nodeId });
  }
  function handleEdgeClick(fromId: string, toId: string, edgeIndices: number[]) {
    setPanel(p =>
      p?.type === 'edge' && p.fromId === fromId && p.toId === toId
        ? null
        : { type: 'edge', fromId, toId, edgeIndices },
    );
  }

  return (
    <Layout
      page="topology"
      topbarRight={
        <span style={{ fontSize: 11, color: 'var(--t1)', display: 'flex', alignItems: 'center', gap: 5 }}>
          <span className="live-dot" />live · SSE
        </span>
      }
    >
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

        <div style={{ display: 'flex', alignItems: 'center', gap: 10, padding: '10px 2px', flexWrap: 'wrap' as const }}>
          <span style={{ fontSize: 10, color: 'var(--t2)', letterSpacing: '.08em' }}>FILTER</span>
          <button className={`filter-chip${showRegistered ? ' on' : ''}`} onClick={() => setShowRegistered(p => !p)}>
            Registered
          </button>
          <button className={`filter-chip${showUnregistered ? ' on' : ''}`} onClick={() => setShowUnregistered(p => !p)}>
            Unregistered
          </button>
          <span style={{ width: 1, height: 18, background: 'var(--t3)', margin: '0 2px', display: 'inline-block' }} />
          <span style={{ fontSize: 11, color: 'var(--t2)' }}>Click node or edge to inspect</span>
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
          <div style={{ background: 'rgba(0,0,0,.25)', padding: 16, minHeight: 200 }}>
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
              <TopologyGraph
                graph={graph}
                showRegistered={showRegistered}
                showUnregistered={showUnregistered}
                selectedNodeId={selectedNodeId}
                selectedEdgeKey={selectedEdgeKey}
                onNodeClick={handleNodeClick}
                onEdgeClick={handleEdgeClick}
              />
            )}
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
                    onClick={() => handleEdgeClick(e.from, e.to, [i])}
                    style={{ cursor: 'pointer', background: panel?.type === 'edge' && panel.edgeIndices.includes(i) ? 'rgba(91,156,246,.07)' : undefined }}
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

      {graph && (
        <TopologyPanel
          panel={panel}
          graph={graph}
          services={services ?? null}
          sessions={sessions ?? null}
          onClose={() => setPanel(null)}
          onNodeClick={handleNodeClick}
        />
      )}
    </Layout>
  );
}
