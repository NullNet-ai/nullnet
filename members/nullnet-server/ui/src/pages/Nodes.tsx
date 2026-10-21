import RefreshStatus from '../components/RefreshStatus';
import { useState } from 'react';
import Layout from '../components/Layout';
import { useApi } from '../hooks/useApi';
import { useStack } from '../StackContext';
import type { IngressControlState, NodeJson, PressureWindowJson, SetupPressureWindowJson } from '../types';
import { useDragResize } from '../hooks/useDragResize';
import { recentPressureWindows, timedLinePoints } from '../lib/pressureChart';

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

type ThroughputWindow = Pick<PressureWindowJson, 'age_ms' | 'duration_ms' | 'arrivals_per_sec' | 'completions_per_sec'>;

function RoutingChart({ windows }: { windows: ThroughputWindow[] }) {
  const arrivals = windows.map(window => window.arrivals_per_sec);
  const completions = windows.map(window => window.completions_per_sec);
  const max = Math.max(1, ...arrivals, ...completions);
  return (
    <div className="pressure-chart-wrap">
      <div className="pressure-legend">
        <span><i className="legend-line arrivals" />Arrivals/s</span>
        <span><i className="legend-line completions" />Completions/s</span>
        <span className="pressure-range">last 60s</span>
      </div>
      <div className="pressure-plot-row">
        <div className="chart-y-scale"><span>{formatRate(max)}</span><span>{formatRate(max / 2)}</span><span>0</span></div>
        <svg className="pressure-chart" viewBox="0 0 300 78" preserveAspectRatio="none" aria-label="Routing arrivals and completions per second">
          <line x1="0" y1="77" x2="300" y2="77" className="chart-axis" />
          <line x1="0" y1="39" x2="300" y2="39" className="chart-grid" />
          <polyline points={timedLinePoints(windows, window => window.arrivals_per_sec, 300, 76, max)} className="chart-line arrivals" />
          <polyline points={timedLinePoints(windows, window => window.completions_per_sec, 300, 76, max)} className="chart-line completions" />
        </svg>
      </div>
    </div>
  );
}

function DetailCharts<T extends ThroughputWindow,>({
  windows,
  pendingLabel,
  latencyLabel,
  pendingValue,
  latencyUs,
}: {
  windows: T[];
  pendingLabel: string;
  latencyLabel: string;
  pendingValue: (window: T) => number;
  latencyUs: (window: T) => number;
}) {
  const pending = windows.map(pendingValue);
  const latencies = windows.map(window => latencyUs(window) / 1000);
  const pendingMax = Math.max(1, ...pending);
  const latencyMax = Math.max(1, ...latencies);
  return (
    <div className="queue-charts">
      <div className="queue-chart-row">
        <span>{pendingLabel}</span>
        <div className="queue-plot">
          <div className="queue-y-scale"><span>{formatRate(pendingMax)}</span><span>0</span></div>
          <svg viewBox="0 0 220 31" preserveAspectRatio="none" aria-label={pendingLabel}>
            <line x1="0" y1="30" x2="220" y2="30" className="chart-axis" />
            <polyline points={timedLinePoints(windows, pendingValue, 220, 29, pendingMax)} className="chart-line waiters" />
          </svg>
        </div>
        <strong>{pending.at(-1) ?? 0}</strong>
      </div>
      <div className="queue-chart-row">
        <span>{latencyLabel}</span>
        <div className="queue-plot">
          <div className="queue-y-scale"><span>{formatWait(latencyMax * 1000)}</span><span>0</span></div>
          <svg viewBox="0 0 220 31" preserveAspectRatio="none" aria-label={latencyLabel}>
            <line x1="0" y1="30" x2="220" y2="30" className="chart-axis" />
            <polyline points={timedLinePoints(windows, window => latencyUs(window) / 1000, 220, 29, latencyMax)} className="chart-line wait" />
          </svg>
        </div>
        <strong>{formatWait(windows.at(-1) ? latencyUs(windows.at(-1)!) : undefined)}</strong>
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
              const latest = recentPressureWindows(node.setup_pressure_windows).at(-1);
              return (
                <div key={node.ip} className={`node-card${selected === node.ip ? ' sel' : ''}`} onClick={() => select(node.ip)}>
                  <div className="nc-head">
                    <div>
                      <div className="nc-name"><span className="dot dot-g"></span>{node.ip}</div>
                      <div className="nc-observed">Setup observed {formatAge(node.setup_pressure_age_ms)}</div>
                    </div>
                    <div className="nc-badges">
                      {node.is_proxy && <span className="badge b-amber">Proxy</span>}
                      <span className={`badge ${stateBadge(node.ingress_control.state)}`}>
                        {stateLabel(node.ingress_control.state)}
                      </span>
                    </div>
                  </div>

                  <div className="nc-stats">
                    <div className="nc-stat"><div className="nc-stat-k">Setup/s</div><div className="nc-stat-v">{formatRate(latest?.arrivals_per_sec)}</div></div>
                    <div className="nc-stat"><div className="nc-stat-k">Completed/s</div><div className="nc-stat-v">{formatRate(latest?.completions_per_sec)}</div></div>
                    <div className="nc-stat"><div className="nc-stat-k">In flight</div><div className="nc-stat-v">{latest?.in_flight ?? '–'}</div></div>
                    <div className="nc-stat"><div className="nc-stat-k">Mean setup</div><div className="nc-stat-v compact">{formatWait(latest?.mean_setup_us)}</div></div>
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
            <div className="dp-title-group">
              <span className="dp-title" title={selectedNode?.ip}>{selectedNode ? selectedNode.ip : '–'}</span>
              {selectedNode?.is_proxy && <span className="badge b-amber">Proxy</span>}
            </div>
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
                <div className="dp-sec-title">Setup pressure</div>
                <RoutingChart windows={recentPressureWindows(selectedNode.setup_pressure_windows)} />
                <DetailCharts<SetupPressureWindowJson>
                  windows={recentPressureWindows(selectedNode.setup_pressure_windows)}
                  pendingLabel="In flight"
                  latencyLabel="Mean setup"
                  pendingValue={window => window.in_flight}
                  latencyUs={window => window.mean_setup_us}
                />
                <div className="pressure-observed">Latest setup sample {formatAge(selectedNode.setup_pressure_age_ms)}</div>
              </div>

              {selectedNode.is_proxy && (
                <div className="dp-sec">
                  <div className="dp-sec-title">Proxy routing pressure</div>
                  <RoutingChart windows={recentPressureWindows(selectedNode.pressure_windows)} />
                  <DetailCharts<PressureWindowJson>
                    windows={recentPressureWindows(selectedNode.pressure_windows)}
                    pendingLabel="Q waiters"
                    latencyLabel="Mean routing wait"
                    pendingValue={window => window.waiters}
                    latencyUs={window => window.mean_wait_us}
                  />
                  <div className="pressure-observed">Latest proxy sample {formatAge(selectedNode.pressure_age_ms)}</div>
                </div>
              )}

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
