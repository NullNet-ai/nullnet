import type { GraphEdgeJson } from '../../types';
import NodeSessionSummary from './NodeSessionSummary';

interface Props {
  ip: string;
  edges: GraphEdgeJson[];
  historical: boolean;
}

export default function ProxyNodePanel({ ip, edges, historical }: Props) {
  const sessionCount = edges.filter(e => e.session && (historical || e.session.ended_at == null) &&
    (e.via_proxy === ip || (e.egress && e.to === ip))).length;

  return <NodeSessionSummary kind="proxy" id={ip} count={sessionCount} historical={historical} />;
}
