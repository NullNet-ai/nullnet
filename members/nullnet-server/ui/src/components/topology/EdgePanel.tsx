import type { GraphEdgeJson } from '../../types';
import { NetId, SessionKind } from '../SessionFields';
import SessionDetails from '../SessionDetails';
import { spRow, spKey, spCode } from './panelStyles';

export default function EdgePanel({ edges }: { edges: GraphEdgeJson[] }) {
  const first = edges[0];
  if (!first) return <div style={{ color: 'var(--t2)', fontSize: 11 }}>No sessions or tunnels remain on this connection.</div>;
  const kind = first.session?.direction ?? (first.egress ? 'egress' : first.via_proxy ? 'ingress' : 'backend');
  const from = kind === 'ingress' ? first.session?.detail.proxy_ip ?? first.via_proxy ?? 'Proxy not recorded' : first.from;
  const sessions = edges.flatMap(e => e.session ? [e.session] : []);
  return <>
    <div style={spRow}><div style={spKey}>Kind</div><SessionKind kind={kind} /></div>
    <div style={spRow}><div style={spKey}>From</div><div style={spCode}>{from}</div></div>
    <div style={spRow}><div style={spKey}>To</div><div style={spCode}>{first.to}</div></div>
    <SessionDetails sessions={sessions} />
    {sessions.length === 0 && <div style={{ color: 'var(--t2)', fontSize: 11 }}>
      No active sessions. Tunnel{edges.length === 1 ? '' : 's'} still present:
      {[...new Map(edges.map(e => [e.net_id, e])).values()].map(e => <div key={e.net_id}><NetId id={e.net_id} setupMs={e.setup_ms} /></div>)}
    </div>}
  </>;
}
