import { useMemo } from 'react';
import type { GraphEdgeJson, SessionJson } from '../../types';
import { spRow, spKey, spCode } from './panelStyles';
import { useTopologyData } from './TopologyContext';

interface Props {
  edges: GraphEdgeJson[];
}

export default function EdgePanel({ edges }: Props) {
  const { chains, sessions } = useTopologyData();

  const chainByProxyNetId = useMemo(() => {
    const m = new Map<number, number[]>();
    for (const c of chains ?? []) m.set(c.proxy_net_id, c.all_net_ids);
    return m;
  }, [chains]);

  const sessionByNetId = useMemo(() => {
    const m = new Map<number, SessionJson>();
    for (const s of sessions ?? []) m.set(s.network_id, s);
    return m;
  }, [sessions]);

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

      {edges.map((e, i) => {
        const session = sessionByNetId.get(e.net_id);
        return (
          <div key={i} style={{
            background: 'rgba(255,255,255,.03)',
            border: '1px solid var(--gb)',
            borderRadius: 6,
            padding: '9px 11px',
            marginBottom: 6,
          }}>
            <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginBottom: session || e.via_proxy ? 6 : 0 }}>
              <span style={{ fontFamily: "'JetBrains Mono',monospace", fontSize: 10, color: 'var(--cyan)' }}>
                {e.via_proxy && chainByProxyNetId.has(e.net_id)
                  ? `nets ${chainByProxyNetId.get(e.net_id)!.join(', ')}`
                  : `net ${e.net_id}`}
              </span>
              {e.setup_ms > 0 && (
                <span style={{ fontSize: 10, color: 'var(--t2)' }}>{e.setup_ms}ms setup</span>
              )}
            </div>
            {e.via_proxy && (
              <div style={{ fontSize: 10, color: '#fbbf24', fontFamily: "'JetBrains Mono',monospace", marginBottom: session ? 6 : 0 }}>
                via {e.via_proxy}
              </div>
            )}
            {session && (
              <table style={{ borderCollapse: 'collapse', width: '100%', fontFamily: "'JetBrains Mono',monospace" }}>
                <tbody>
                  {[['client', session.client_net], ['server', session.server_net]].map(([label, val]) => (
                    <tr key={label}>
                      <td style={{ fontSize: 9, color: 'var(--t2)', paddingRight: 8, paddingTop: 2, whiteSpace: 'nowrap', textTransform: 'uppercase', letterSpacing: '.05em', verticalAlign: 'top' }}>{label}</td>
                      <td style={{ fontSize: 10, color: 'var(--t1)', wordBreak: 'break-all' }}>{val}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
          </div>
        );
      })}
    </>
  );
}
