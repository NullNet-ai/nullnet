import type { GraphJson } from '../../types';
import { NODE_W, NODE_H, INET_W, INET_H, INTERNET_ID } from './types';
import { buildTopoGraph, layoutNodes, svgDims, edgePath, inetEdgePath } from './layout';

interface Props {
  graph: GraphJson;
  showRegistered: boolean;
  showUnregistered: boolean;
  selectedNodeId: string | null;
  selectedEdgeKey: string | null;
  onNodeClick: (id: string) => void;
  onEdgeClick: (fromId: string, toId: string, edgeIndices: number[]) => void;
}

export default function TopologyGraph({
  graph,
  showRegistered,
  showUnregistered,
  selectedNodeId,
  selectedEdgeKey,
  onNodeClick,
  onEdgeClick,
}: Props) {
  const { nodes: allNodes, edges: allEdges } = buildTopoGraph(graph);

  const nodes = allNodes.filter(n => {
    if (n.kind !== 'service') return true;
    if (n.registered && !showRegistered) return false;
    if (!n.registered && !showUnregistered) return false;
    return true;
  });
  const visibleIds = new Set(nodes.map(n => n.id));
  const edges = allEdges.filter(e => visibleIds.has(e.from) && visibleIds.has(e.to));

  const pos = layoutNodes(nodes, edges);
  const { w, h } = svgDims(pos, nodes);

  return (
    <svg
      viewBox={`0 0 ${w} ${h}`}
      xmlns="http://www.w3.org/2000/svg"
      style={{ width: '100%', display: 'block', fontFamily: "'Plus Jakarta Sans',sans-serif" }}
    >
      <defs>
        <filter id="gT"><feGaussianBlur stdDeviation="2.5" result="b"/><feMerge><feMergeNode in="b"/><feMergeNode in="SourceGraphic"/></feMerge></filter>
        <marker id="arr" markerWidth="8" markerHeight="8" refX="7" refY="3" orient="auto">
          <path d="M0,0 L0,6 L8,3 z" fill="rgba(255,255,255,.25)" />
        </marker>
        <marker id="arr-proxy" markerWidth="8" markerHeight="8" refX="7" refY="3" orient="auto">
          <path d="M0,0 L0,6 L8,3 z" fill="rgba(251,191,36,.4)" />
        </marker>
        <marker id="arr-sel" markerWidth="8" markerHeight="8" refX="7" refY="3" orient="auto">
          <path d="M0,0 L0,6 L8,3 z" fill="rgba(91,156,246,.8)" />
        </marker>
        <marker id="arr-inet" markerWidth="8" markerHeight="8" refX="7" refY="3" orient="auto">
          <path d="M0,0 L0,6 L8,3 z" fill="rgba(91,156,246,.35)" />
        </marker>
      </defs>

      {/* Internet → Proxy edges (non-clickable, rendered first) */}
      {edges.filter(e => e.isInternetEdge).map((e, i) => {
        const fp = pos.get(e.from);
        const tp = pos.get(e.to);
        if (!fp || !tp) return null;
        return (
          <path
            key={`inet-${i}`}
            d={inetEdgePath(fp, tp)}
            fill="none"
            stroke="rgba(91,156,246,.3)"
            strokeWidth="1.5"
            strokeDasharray="4 3"
            markerEnd="url(#arr-inet)"
            pointerEvents="none"
          />
        );
      })}

      {/* Service / proxy edges */}
      {edges.filter(e => !e.isInternetEdge).map((e, i) => {
        const fp = pos.get(e.from);
        const tp = pos.get(e.to);
        if (!fp || !tp) return null;
        const edgeKey = `${e.from}\0${e.to}`;
        const isSel = selectedEdgeKey === edgeKey;
        const count = e.originalIndices.length;
        const stroke = isSel
          ? 'rgba(91,156,246,.9)'
          : e.isProxyHop ? 'rgba(251,191,36,.35)' : 'rgba(255,255,255,.18)';
        const arrowId = isSel ? 'arr-sel' : e.isProxyHop ? 'arr-proxy' : 'arr';
        const midX = (fp.x + tp.x) / 2 + NODE_W / 2;
        const midY = (fp.y + tp.y) / 2 + NODE_H / 2;
        return (
          <g key={i} onClick={() => onEdgeClick(e.from, e.to, e.originalIndices)} style={{ cursor: 'pointer' }}>
            <path d={edgePath(fp, tp)} fill="none" stroke="transparent" strokeWidth="14" />
            <path
              d={edgePath(fp, tp)} fill="none" stroke={stroke}
              strokeWidth={isSel ? 2 : 1.5}
              strokeDasharray={e.isProxyHop && !isSel ? '4 3' : undefined}
              markerEnd={`url(#${arrowId})`}
              pointerEvents="none"
            />
            {!isSel && count > 1 && (
              <text x={midX} y={midY} textAnchor="middle" fill="rgba(255,255,255,.45)" fontSize="9" pointerEvents="none">
                {count} sessions
              </text>
            )}
            {!isSel && count === 1 && e.setup_ms > 0 && (
              <text x={midX} y={midY} textAnchor="middle" fill="rgba(255,255,255,.3)" fontSize="9" pointerEvents="none">
                net {e.net_id} · {e.setup_ms}ms
              </text>
            )}
          </g>
        );
      })}

      {/* Nodes */}
      {nodes.map(n => {
        const p = pos.get(n.id);
        if (!p) return null;
        const isSel = n.id === selectedNodeId;

        if (n.kind === 'internet') {
          return (
            <g key={INTERNET_ID} onClick={() => onNodeClick(INTERNET_ID)} style={{ cursor: 'pointer' }}>
              {isSel && (
                <rect x={p.x - 3} y={p.y - 3} width={INET_W + 6} height={INET_H + 6} rx="14"
                  fill="none" stroke="rgba(91,156,246,.6)" strokeWidth="1.5" />
              )}
              <rect x={p.x} y={p.y} width={INET_W} height={INET_H} rx="11"
                fill="rgba(91,156,246,.05)" stroke="rgba(91,156,246,.2)"
                strokeWidth="1" strokeDasharray="4 3" filter="url(#gT)" />
              <text x={p.x + INET_W / 2} y={p.y + INET_H / 2 + 4}
                textAnchor="middle" fill="rgba(91,156,246,.7)" fontSize="9.5" fontWeight="600" pointerEvents="none">
                ⬡ internet
              </text>
            </g>
          );
        }

        if (n.kind === 'proxy') {
          return (
            <g key={n.id} onClick={() => onNodeClick(n.id)} style={{ cursor: 'pointer' }}>
              {isSel && (
                <rect x={p.x - 3} y={p.y - 3} width={NODE_W + 6} height={NODE_H + 6} rx="10"
                  fill="none" stroke="rgba(91,156,246,.6)" strokeWidth="1.5" />
              )}
              <rect x={p.x} y={p.y} width={NODE_W} height={NODE_H} rx="8"
                fill="rgba(251,191,36,.06)" stroke="rgba(251,191,36,.4)"
                strokeWidth="1" strokeDasharray="5 3" filter="url(#gT)" />
              <circle cx={p.x + 15} cy={p.y + 19} r="3.5" fill="#fbbf24" />
              <text x={p.x + 27} y={p.y + 16} fill="rgba(251,191,36,.9)" fontSize="9.5" fontWeight="500" pointerEvents="none">proxy</text>
              <text x={p.x + 27} y={p.y + 28} fill="rgba(255,255,255,.4)" fontSize="8" fontFamily="'JetBrains Mono',monospace" pointerEvents="none">{n.id}</text>
            </g>
          );
        }

        const color = n.registered ? '#34d399' : '#f87171';
        const strokeColor = n.registered ? 'rgba(52,211,153,.3)' : 'rgba(248,113,113,.2)';
        return (
          <g key={n.id} onClick={() => onNodeClick(n.id)} style={{ cursor: 'pointer' }}>
            {isSel && (
              <rect x={p.x - 3} y={p.y - 3} width={NODE_W + 6} height={NODE_H + 6} rx="10"
                fill="none" stroke="rgba(91,156,246,.6)" strokeWidth="1.5" />
            )}
            <rect x={p.x} y={p.y} width={NODE_W} height={NODE_H} rx="8"
              fill="rgba(255,255,255,.04)" stroke={strokeColor} strokeWidth="1" filter="url(#gT)" />
            <circle cx={p.x + 15} cy={p.y + 19} r="3.5" fill={color} />
            <text x={p.x + 27} y={p.y + 16} fill="rgba(255,255,255,.85)" fontSize="9.5" fontWeight="500" pointerEvents="none">{n.id}</text>
            <text x={p.x + 27} y={p.y + 28} fill="rgba(255,255,255,.3)" fontSize="8" pointerEvents="none">
              {n.registered ? `${n.active_replica_count}/${n.replica_count} active` : 'unregistered'}
              {n.entry_point ? ' · entry' : ''}
            </text>
          </g>
        );
      })}
    </svg>
  );
}
