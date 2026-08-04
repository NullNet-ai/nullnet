import { useMemo } from 'react';
import type { GraphEdgeJson, SessionJson, EgressDestination } from '../../types';
import { spRow, spKey, spCode } from './panelStyles';
import { useTopologyData } from './TopologyContext';

interface Props {
  edges: GraphEdgeJson[];
}

function formatTime(unix: number): string {
  return new Date(unix * 1000).toLocaleTimeString([], { hour12: false });
}

/// ISO alpha-2 country code → flag emoji (regional-indicator letters). Empty
/// string for anything that isn't two ASCII letters (e.g. a country name).
function flagEmoji(cc?: string): string {
  if (!cc || !/^[A-Za-z]{2}$/.test(cc)) return '';
  const base = 0x1f1e6;
  const u = cc.toUpperCase();
  return String.fromCodePoint(base + u.charCodeAt(0) - 65, base + u.charCodeAt(1) - 65);
}

const regionNames = new Intl.DisplayNames(['en'], { type: 'region' });

/// Full country name for the flag tooltip; falls back to the raw code.
function countryName(cc: string): string {
  try {
    return regionNames.of(cc.toUpperCase()) ?? cc.toUpperCase();
  } catch {
    return cc.toUpperCase();
  }
}

/// Render the contacted-destination list for a single egress edge.
function DestinationList({ destinations }: { destinations: EgressDestination[] }) {
  if (destinations.length === 0) {
    return (
      <div style={{ fontSize: 10, color: 'var(--t2)', fontFamily: "'JetBrains Mono',monospace" }}>
        → internet (no traffic yet)
      </div>
    );
  }
  return (
    <table style={{ borderCollapse: 'collapse', width: '100%', fontFamily: "'JetBrains Mono',monospace" }}>
      <tbody>
        {destinations.map((d, i) => {
          const flag = flagEmoji(d.country_code);
          // thin rule between entries (not above the first)
          const sep = i > 0 ? { borderTop: '1px solid rgba(255,255,255,.07)' } : {};
          return (
            <tr key={d.ip}>
              <td style={{ ...sep, paddingRight: 8, paddingTop: 5, paddingBottom: 4, wordBreak: 'break-all', verticalAlign: 'top' }}>
                <div style={{ fontSize: 10, color: d.blocked ? '#f87171' : '#a78bfa' }}>
                  {flag && (
                    <span title={countryName(d.country_code!)} style={{ marginRight: 5, cursor: 'default' }}>
                      {flag}
                    </span>
                  )}
                  {d.ip}
                  {d.blocked && (
                    <span style={{ marginLeft: 6, fontSize: 8.5, border: '1px solid rgba(248,113,113,.4)', borderRadius: 3, padding: '0 4px', letterSpacing: '.05em' }}>
                      BLOCKED
                    </span>
                  )}
                </div>
                {d.org && (
                  <div style={{ fontSize: 9, color: 'var(--t2)', marginTop: 1 }}>{d.org}</div>
                )}
              </td>
              <td style={{ ...sep, fontSize: 9.5, color: 'var(--t1)', textAlign: 'right', whiteSpace: 'nowrap', verticalAlign: 'top', paddingTop: 5, paddingBottom: 4 }}>
                ×{d.count} · {formatTime(d.last_seen)}
              </td>
            </tr>
          );
        })}
      </tbody>
    </table>
  );
}

export default function EdgePanel({ edges }: Props) {
  const { chains, sessions } = useTopologyData();

  const egressDestCount = useMemo(() => {
    const ips = new Set<string>();
    for (const e of edges) for (const d of e.destinations ?? []) ips.add(d.ip);
    return ips.size;
  }, [edges]);

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
  const isEgress = !!first.egress;

  return (
    <>
      <div style={spRow}>
        <div style={spKey}>Type</div>
        <span className={`badge ${isEgress ? 'b-purple' : first.via_proxy ? 'b-amber' : 'b-blue'}`}>
          {isEgress ? 'Egress' : first.via_proxy ? 'Proxied' : 'Direct'}
        </span>
      </div>
      <div style={spRow}>
        <div style={spKey}>{isEgress ? 'Service' : 'From'}</div>
        <div style={spCode}>{first.from}</div>
      </div>
      <div style={spRow}>
        <div style={spKey}>{isEgress ? 'Gateway' : 'To'}</div>
        <div style={isEgress ? { ...spCode, color: '#a78bfa' } : spCode}>{first.to}</div>
      </div>
      {isEgress && (
        <div style={spRow}>
          <div style={spKey}>Destinations</div>
          <div style={spCode}>
            {egressDestCount > 0
              ? `${egressDestCount} external IP${egressDestCount !== 1 ? 's' : ''}`
              : 'internet (no traffic yet)'}
          </div>
        </div>
      )}
      {first.via_proxy && (
        <div style={spRow}>
          <div style={spKey}>Via Proxy</div>
          <div style={{ ...spCode, color: '#fbbf24' }}>{first.via_proxy}</div>
        </div>
      )}

      <div style={{ marginTop: 16, marginBottom: 8, fontSize: 10, fontWeight: 600, color: 'var(--t2)', letterSpacing: '.08em' }}>
        {isEgress ? `EGRESS EDGES (${edges.length})` : `SESSIONS (${edges.length})`}
      </div>

      {edges.map((e, i) => {
        const session = sessionByNetId.get(e.net_id);
        const netIdsLabel = e.via_proxy && chainByProxyNetId.has(e.net_id)
          ? `nets ${chainByProxyNetId.get(e.net_id)!.join(', ')}`
          : `net ${e.net_id}`;
        return (
          <div key={i} style={{
            background: 'rgba(255,255,255,.03)',
            border: '1px solid var(--gb)',
            borderRadius: 6,
            padding: '9px 11px',
            marginBottom: 6,
          }}>
            <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', gap: 8, marginBottom: session || e.via_proxy || e.egress ? 6 : 0 }}>
              <span
                title={netIdsLabel}
                style={{
                  fontFamily: "'JetBrains Mono',monospace", fontSize: 10, color: 'var(--cyan)',
                  flex: 1, minWidth: 0, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap',
                }}
              >
                {netIdsLabel}
              </span>
              {e.setup_ms > 0 && (
                <span style={{ fontSize: 10, color: 'var(--t2)', flexShrink: 0 }}>{e.setup_ms}ms setup</span>
              )}
            </div>
            {e.egress && (
              <>
                <div style={{ fontSize: 10, color: '#a78bfa', fontFamily: "'JetBrains Mono',monospace", paddingBottom: 7, borderBottom: '1px solid var(--gb)', marginBottom: 3 }}>
                  {e.from} → {e.to} → {(e.destinations?.length ?? 0)} dest
                </div>
                <DestinationList destinations={e.destinations ?? []} />
              </>
            )}
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
