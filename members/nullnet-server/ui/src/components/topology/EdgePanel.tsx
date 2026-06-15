import type { GraphEdgeJson } from '../../types';
import { spRow, spKey, spCode } from './panelStyles';

interface Props {
  edges: GraphEdgeJson[];
}

export default function EdgePanel({ edges }: Props) {
  if (edges.length === 0) return null;
  const first = edges[0];

  return (
    <>
      <div style={spRow}>
        <div style={spKey}>Type</div>
        <span className={`badge ${first.via_proxy ? 'b-amber' : 'b-blue'}`}>
          {first.via_proxy ? 'Proxied' : 'Direct'}
        </span>
      </div>
      <div style={spRow}>
        <div style={spKey}>From</div>
        <div style={spCode}>{first.from}</div>
      </div>
      <div style={spRow}>
        <div style={spKey}>To</div>
        <div style={spCode}>{first.to}</div>
      </div>
      {first.via_proxy && (
        <div style={spRow}>
          <div style={spKey}>Via Proxy</div>
          <div style={{ ...spCode, color: '#fbbf24' }}>{first.via_proxy}</div>
        </div>
      )}

      <div style={{ marginTop: 16, marginBottom: 8, fontSize: 10, fontWeight: 600, color: 'var(--t2)', letterSpacing: '.08em' }}>
        SESSIONS ({edges.length})
      </div>

      {edges.map((e, i) => (
        <div key={i} style={{
          background: 'rgba(255,255,255,.03)',
          border: '1px solid var(--gb)',
          borderRadius: 6,
          padding: '9px 11px',
          marginBottom: 6,
        }}>
          <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginBottom: 4 }}>
            <span style={{ fontFamily: "'JetBrains Mono',monospace", fontSize: 10, color: 'var(--cyan)' }}>
              net {e.net_id}
            </span>
            {e.setup_ms > 0 && (
              <span style={{ fontSize: 10, color: 'var(--t2)' }}>{e.setup_ms}ms setup</span>
            )}
          </div>
          {e.via_proxy && (
            <div style={{ fontSize: 10, color: '#fbbf24', fontFamily: "'JetBrains Mono',monospace" }}>
              via {e.via_proxy}
            </div>
          )}
        </div>
      ))}
    </>
  );
}
