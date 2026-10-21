import RefreshStatus from '../components/RefreshStatus';
import { useState } from 'react';
import Layout from '../components/Layout';
import { useApi } from '../hooks/useApi';
import { useStack } from '../StackContext';
import type { IngressControlState, NodeJson, PressureWindowJson } from '../types';
import { useDragResize } from '../hooks/useDragResize';

function stateLabel(state: IngressControlState) {
  if (state === 'multiplexing') return 'Multiplexing';
  if (state === 'recovery') return 'Recovery test';
  return 'Normal';
}

function stateBadge(state: IngressControlState) {
  if (state === 'multiplexing') return 'b-amber';
  if (state === 'recovery') return 'b-blue';
  return 'b-green';
}

function formatAge(ms: number | null | undefined) {
  if (ms == null) return 'No samples yet';
  if (ms < 1000) return 'just now';
  if (ms < 60_000) return `${Math.floor(ms / 1000)}s ago`;
  return `${Math.floor(ms / 60_000)}m ago`;
}

function formatStateAge(ms: number | null | undefined) {
  return ms == null ? 'No control change' : formatAge(ms);
}

function formatWait(us: number | null | undefined) {
  if (us == null) return '–';
  if (us < 1000) return `${Math.round(us)} µs`;
  return `${(us / 1000).toFixed(us < 10_000 ? 1 : 0)} ms`;
}

function formatRate(value: number | null | undefined) {
  if (value == null) return '–';
  return value >= 100 ? Math.round(value).toLocaleString() : value.toFixed(1);
}

function linePoints(values: number[], width: number, height: number, max: number) {
  if (values.length === 1) {
    const y = height - (values[0] / max) * height;
    return `0,${y} ${width},${y}`;
  }
  return values.map((value, index) => {
    const x = (index / (values.length - 1)) * width;
    const y = height - (value / max) * height;
    return `${x},${y}`;
  }).join(' ');
}

function RoutingChart({ windows }: { windows: PressureWindowJson[] }) {
  if (windows.length === 0) return <div className="pressure-empty">Waiting for routing samples…</div>;
  const arrivals = windows.map(window => window.arrivals_per_sec);
  const completions = windows.map(window => window.completions_per_sec);
  const max = Math.max(1, ...arrivals, ...completions);
  return (
    <div className="pressure-chart-wrap">
      <div className="pressure-legend">
        <span><i className="legend-line arrivals" />Arrivals/s</span>
        <span><i className="legend-line completions" />Completions/s</span>
        <span className="pressure-range">last {windows.length}s</span>
      </div>
      <svg className="pressure-chart" viewBox="0 0 300 82" preserveAspectRatio="none" aria-label="Routing arrivals and completions per second">
        <line x1="0" y1="81" x2="300" y2="81" className="chart-axis" />
        <line x1="0" y1="41" x2="300" y2="41" className="chart-grid" />
        <polyline points={linePoints(arrivals, 300, 78, max)} className="chart-line arrivals" />
        <polyline points={linePoints(completions, 300, 78, max)} className="chart-line completions" />
      </svg>
    </div>
  );
}

function QueueChart({ windows }: { windows: PressureWindowJson[] }) {
  if (windows.length === 0) return null;
  const waiters = windows.map(window => window.waiters);
  const waits = windows.map(window => window.mean_wait_us / 1000);
  const waiterMax = Math.max(1, ...waiters);
  const waitMax = Math.max(1, ...waits);
  return (
    <div className="queue-charts">
      <div className="queue-chart-row">
        <span>Waiters</span>
        <svg viewBox="0 0 220 34" preserveAspectRatio="none" aria-label="Routing waiters">
          <polyline points={linePoints(waiters, 220, 31, waiterMax)} className="chart-line waiters" />
        </svg>
        <strong>{waiters.at(-1) ?? 0}</strong>
      </div>
      <div className="queue-chart-row">
        <span>Mean wait</span>
        <svg viewBox="0 0 220 34" preserveAspectRatio="none" aria-label="Mean routing slot wait">
          <polyline points={linePoints(waits, 220, 31, waitMax)} className="chart-line wait" />
        </svg>
        <strong>{formatWait(windows.at(-1)?.mean_wait_us)}</strong>
      </div>
    </div>
  );
}

export default function Nodes() {
  const { stack } = useStack();
  const { data: nodes, loading, error, updatedAt } = useApi<NodeJson[]>(`/api/nodes/${stack}`, 5000);
  const [selected, setSelected] = useState<string | null>(null);
  const { width: dpWidth, onResizeStart } = useDragResize(360, 300, 640);
  const selectedNode = nodes?.find(node => node.ip === selected) ?? null;

  function select(ip: string) {
    setSelected(previous => (previous === ip ? null : ip));
  }

  return (
    <Layout
      page="nodes"
      topbarRight={
        <>
          <span style={{ fontSize: 11, color: 'var(--t2)' }}>{nodes?.length ?? 0} connected</span>
          <RefreshStatus updatedAt={updatedAt} failed={!!error} />
        </>
      }
    >
      <div className="workspace" style={{ minHeight: 'calc(100vh - 48px)' }}>
        <div className="nodes-content">
          <div className="page-title">Connected Nodes</div>
          <div className="page-sub">Current ingress pressure and the last 60 seconds, refreshed every 5 seconds</div>

          {loading && <div style={{ color: 'var(--t2)', fontSize: 12 }}>Loading…</div>}

          <div className="nodes-grid">
            {nodes?.map(node => {
              const latest = node.pressure_windows.at(-1);
              return (
                <div key={node.ip} className={`node-card${selected === node.ip ? ' sel' : ''}`} onClick={() => select(node.ip)}>
                  <div className="nc-head">
                    <div>
                      <div className="nc-name"><span className="dot dot-g"></span>{node.ip}</div>
                      <div className="nc-observed">Observed {formatAge(node.pressure_age_ms)}</div>
                    </div>
                    <span className={`badge ${stateBadge(node.ingress_control.state)}`}>
                      {stateLabel(node.ingress_control.state)}
                    </span>
                  </div>

                  <div className="nc-stats">
                    <div className="nc-stat"><div className="nc-stat-k">Arrivals/s</div><div className="nc-stat-v">{formatRate(latest?.arrivals_per_sec)}</div></div>
                    <div className="nc-stat"><div className="nc-stat-k">Completions/s</div><div className="nc-stat-v">{formatRate(latest?.completions_per_sec)}</div></div>
                    <div className="nc-stat"><div className="nc-stat-k">Waiters</div><div className="nc-stat-v">{latest?.waiters ?? '–'}</div></div>
                    <div className="nc-stat"><div className="nc-stat-k">Mean wait</div><div className="nc-stat-v compact">{formatWait(latest?.mean_wait_us)}</div></div>
                  </div>

                  <div className="nc-groups">
                    <span><strong>{node.ingress_groups.dedicated}</strong> dedicated</span>
                    <span><strong>{node.ingress_groups.multiplexed}</strong> multiplexed</span>
                    <span>{node.hosted_services.length} service{node.hosted_services.length !== 1 ? 's' : ''}</span>
                  </div>

                  {node.hosted_services.length > 0 && (
                    <div className="nc-svcs">
                      {node.hosted_services.map(service => <span key={`${service.stack}/${service.name}`} className="svc-tag">{service.name}</span>)}
                    </div>
                  )}
                </div>
              );
            })}

            {!loading && nodes?.length === 0 && <div style={{ color: 'var(--t2)', fontSize: 12, gridColumn: '1/-1' }}>No nodes connected</div>}
          </div>
        </div>

        <div className="dp" style={{ width: dpWidth }}>
          <div className="drag-handle" onMouseDown={onResizeStart} />
          <div className="dp-head">
            <span className="dp-title" title={selectedNode?.ip}>{selectedNode ? selectedNode.ip : '–'}</span>
            {selectedNode && <button className="dp-close" onClick={() => setSelected(null)}>✕</button>}
          </div>

          {selectedNode ? (
            <div className="dp-body">
              <div className="dp-sec">
                <div className="dp-sec-title">Ingress control</div>
                <div className="dp-row"><span className="dp-k">State</span><span className={`badge ${stateBadge(selectedNode.ingress_control.state)}`}>{stateLabel(selectedNode.ingress_control.state)}</span></div>
                <div className="dp-row"><span className="dp-k">Entered state</span><span className="dp-v">{formatStateAge(selectedNode.ingress_control.state_age_ms)}</span></div>
                {selectedNode.ingress_control.source_proxy && <div className="dp-row"><span className="dp-k">Triggering proxy</span><span className="dp-v code">{selectedNode.ingress_control.source_proxy}</span></div>}
                {selectedNode.ingress_control.state !== 'normal' && (
                  <>
                    <div className="dp-row"><span className="dp-k">Dedicated setup rate</span><span className="dp-v">{selectedNode.ingress_control.state === 'multiplexing' ? 'Paused' : `${formatRate(selectedNode.ingress_control.rate_per_sec)}/s`}</span></div>
                    <div className="dp-row"><span className="dp-k">Available tokens</span><span className="dp-v">{Math.floor(selectedNode.ingress_control.tokens ?? 0)} / {selectedNode.ingress_control.token_capacity}</span></div>
                  </>
                )}
              </div>

              <div className="dp-sec">
                <div className="dp-sec-title">Routing pressure</div>
                <RoutingChart windows={selectedNode.pressure_windows} />
                <QueueChart windows={selectedNode.pressure_windows} />
                <div className="pressure-observed">Latest proxy sample {formatAge(selectedNode.pressure_age_ms)}</div>
              </div>

              <div className="dp-sec">
                <div className="dp-sec-title">Active ingress groups</div>
                <div className="group-summary">
                  <div><strong>{selectedNode.ingress_groups.dedicated}</strong><span>Dedicated</span></div>
                  <div><strong>{selectedNode.ingress_groups.multiplexed}</strong><span>Multiplexed</span></div>
                </div>
              </div>

              <div className="dp-sec">
                <div className="dp-sec-title">Recent control states</div>
                {selectedNode.ingress_control.recent_states.length > 0 ? (
                  <div className="state-history">
                    {selectedNode.ingress_control.recent_states.map((entry, index) => (
                      <div className="state-history-row" key={`${entry.age_ms}-${entry.state}-${index}`}>
                        <span className={`state-mark ${entry.state}`} />
                        <span>{stateLabel(entry.state)}</span>
                        <span className="state-source">via {entry.source_proxy}</span>
                        <time>{formatAge(entry.age_ms)}</time>
                      </div>
                    ))}
                  </div>
                ) : <div className="pressure-empty small">No control changes since server start</div>}
              </div>

              <div className="dp-sec">
                <div className="dp-sec-title">Hosted Services</div>
                {selectedNode.hosted_services.length > 0 ? (
                  <table className="mini-tbl">
                    <thead><tr><th>Service</th><th>Stack</th></tr></thead>
                    <tbody>
                      {selectedNode.hosted_services.map(service => (
                        <tr key={`${service.stack}/${service.name}`}>
                          <td style={{ fontWeight: 500 }}>{service.name}</td>
                          <td style={{ fontFamily: "'JetBrains Mono',monospace", color: 'var(--t2)', fontSize: 10 }}>{service.stack}</td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                ) : <div style={{ color: 'var(--t2)', fontSize: 11 }}>No services hosted</div>}
              </div>
            </div>
          ) : (
            <div className="empty-dp" style={{ flex: 1 }}><span style={{ fontSize: 28, opacity: .2 }}>◉</span><span style={{ fontSize: 12 }}>Select a node</span></div>
          )}
        </div>
      </div>
    </Layout>
  );
}
