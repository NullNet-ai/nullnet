import type { GraphEdgeJson } from '../../types';
import { spRow, spKey, spVal, spCode } from './panelStyles';

interface Props {
  ip: string;
  edges: GraphEdgeJson[];
  historical: boolean;
}

export default function ProxyNodePanel({ ip, edges, historical }: Props) {
  const sessionCount = edges.filter(e => e.session && (historical || e.session.ended_at == null) &&
    (e.via_proxy === ip || (e.egress && e.to === ip))).length;

  return (
    <>
      <div style={spRow}>
        <div style={spKey}>Role</div>
        <span className="badge b-amber">Proxy entry</span>
      </div>
      <div style={spRow}>
        <div style={spKey}>IP Address</div>
        <div style={spCode}>{ip}</div>
      </div>
      <div style={spRow}>
        <div style={spKey}>{historical ? 'Sessions' : 'Active Sessions'}</div>
        <div style={{ ...spVal, color: 'var(--cyan)' }}>{sessionCount}</div>
      </div>
    </>
  );
}
