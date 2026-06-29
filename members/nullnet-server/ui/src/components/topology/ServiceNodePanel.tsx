import type { ServiceJson } from '../../types';
import type { TopoServiceNode } from './types';
import { spRow, spKey, spVal, spCode, SpSep, SpSection } from './panelStyles';

interface Props {
  node: TopoServiceNode;
  service: ServiceJson | undefined;
  onDepClick: (id: string) => void;
}

export default function ServiceNodePanel({ node, service, onDepClick }: Props) {
  const totalSessions = service?.replicas.reduce((s, r) => s + r.active_sessions, 0) ?? 0;
  const deps = service ? [...new Set(service.proxy_dependencies.flat())] : [];

  return (
    <>
      <div style={spRow}>
        <div style={spKey}>Status</div>
        <div>
          <span className={`badge ${node.registered ? 'b-green' : 'b-dim'}`}>
            {node.registered ? 'Registered' : 'Unregistered'}
          </span>
          {node.entry_point && <span className="badge b-blue" style={{ marginLeft: 6 }}>Entry Point</span>}
        </div>
      </div>

      <div style={spRow}>
        <div style={spKey}>Active Sessions</div>
        <div style={{ ...spVal, color: 'var(--cyan)' }}>{totalSessions}</div>
      </div>

      <div style={spRow}>
        <div style={spKey}>Replicas</div>
        <table style={{ borderCollapse: 'collapse', fontSize: 11, fontFamily: "'JetBrains Mono',monospace" }}>
          <thead>
            <tr>
              {(['active', 'paused', 'total'] as const).map(h => (
                <th key={h} style={{ fontSize: 9, color: 'var(--t2)', paddingBottom: 3, paddingRight: 14, textAlign: 'right', letterSpacing: '.05em', fontWeight: 500, textTransform: 'uppercase', fontFamily: 'inherit' }}>{h}</th>
              ))}
            </tr>
          </thead>
          <tbody>
            <tr>
              <td style={{ paddingRight: 14, textAlign: 'right', color: 'var(--green)' }}>{node.active_replica_count}</td>
              <td style={{ paddingRight: 14, textAlign: 'right', color: node.paused_replica_count > 0 ? '#fbbf24' : 'var(--t2)' }}>{node.paused_replica_count}</td>
              <td style={{ textAlign: 'right', color: 'var(--t1)' }}>{node.replica_count}</td>
            </tr>
          </tbody>
        </table>
      </div>

      {service?.timeout_secs != null && (
        <div style={spRow}>
          <div style={spKey}>Timeout</div>
          <div style={spCode}>{service.timeout_secs}s</div>
        </div>
      )}

      {service?.max_networks != null && (
        <div style={spRow}>
          <div style={spKey}>Max Networks</div>
          <div style={spCode}>{service.max_networks}</div>
        </div>
      )}

      {deps.length > 0 && (
        <div style={spRow}>
          <div style={spKey}>Dependencies</div>
          <div style={{ display: 'flex', flexWrap: 'wrap', gap: 4, marginTop: 2 }}>
            {deps.map(dep => (
              <button key={dep} onClick={() => onDepClick(dep)} className="dep-tag" style={{ cursor: 'pointer' }}>
                {dep}
              </button>
            ))}
          </div>
        </div>
      )}

      {service && service.replicas.length > 0 && (
        <>
          <SpSep />
          <SpSection>Replicas</SpSection>
          <table style={{ width: '100%', borderCollapse: 'collapse' }}>
            <thead>
              <tr>
                {['Host', 'Port', 'Sess'].map(h => (
                  <th key={h} style={{ fontSize: 9, color: 'var(--t2)', padding: '3px 0', textAlign: 'left', borderBottom: '1px solid var(--t3)', letterSpacing: '.05em', fontWeight: 500, textTransform: 'uppercase' as const }}>{h}</th>
                ))}
              </tr>
            </thead>
            <tbody>
              {service.replicas.map((r, i) => (
                <tr key={i}>
                  <td style={{ fontSize: 11, padding: '5px 0', borderBottom: '1px solid rgba(255,255,255,.03)', color: 'var(--cyan)', fontFamily: "'JetBrains Mono',monospace", maxWidth: 100, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' as const }}>{r.ip}</td>
                  <td style={{ fontSize: 11, padding: '5px 0', borderBottom: '1px solid rgba(255,255,255,.03)', color: 'var(--t1)', fontFamily: "'JetBrains Mono',monospace" }}>{r.port}</td>
                  <td style={{ fontSize: 11, padding: '5px 0', borderBottom: '1px solid rgba(255,255,255,.03)', color: 'var(--t2)' }}>{r.active_sessions}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </>
      )}

    </>
  );
}
