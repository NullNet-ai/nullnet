import type { GraphJson, SessionJson } from '../../types';
import { NODE_W, NODE_H, INET_W, INET_H, INTERNET_ID } from './types';
import { buildTopoGraph, layoutNodes, svgDims, edgePath, inetEdgePath, edgeLabelPoints } from './layout';

interface Props {
  graph: GraphJson;
  selectedNodeId?: string | null;
  selectedEdgeKey?: string | null;
  focusedNetIds?: Set<number> | null;
  focusedSessions?: SessionJson[] | null;
  nodeIps?: Map<string, string>;
  onNodeClick?: (id: string) => void;
  onEdgeClick?: (fromId: string, toId: string, edgeIndices: number[]) => void;
}

export default function TopologyGraphSvg({
  graph,
  selectedNodeId = null,
  selectedEdgeKey = null,
  focusedNetIds = null,
  focusedSessions = null,
  nodeIps = new Map(),
  onNodeClick,
  onEdgeClick,
}: Props) {
  const { nodes, edges } = buildTopoGraph(graph);

  const pos = layoutNodes(nodes, edges);
  const { w, h } = svgDims(pos, nodes);

  const focusedEdgeKeys = new Set<string>();
  const focusedNodeIds = new Set<string>();
  if (focusedNetIds) {
    for (const e of edges) {
      if (e.isInternetEdge) continue;
      if (e.originalIndices.some(i => focusedNetIds.has(graph.edges[i]?.net_id))) {
        focusedEdgeKeys.add(`${e.from}\0${e.to}`);
        focusedNodeIds.add(e.from);
        focusedNodeIds.add(e.to);
      }
    }
    focusedNodeIds.add(INTERNET_ID);
  }

  function getNodeIp(id: string): string | null {
    const node = nodes.find(n => n.id === id);
    if (!node) return null;
    if (node.kind === 'proxy') return node.id;
    if (node.kind === 'service') return nodeIps.get(id) ?? null;
    return null;
  }

  const interactive = !!(onNodeClick || onEdgeClick);

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

      {/* Internet → Proxy edges */}
      {edges.filter(e => e.isInternetEdge).map((e, i) => {
        const fp = pos.get(e.from);
        const tp = pos.get(e.to);
        if (!fp || !tp) return null;
        const dimmed = focusedNetIds != null && !focusedNodeIds.has(e.to);
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
            opacity={dimmed ? 0.1 : 1}
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
        const dimmed = focusedNetIds != null && !focusedEdgeKeys.has(edgeKey);
        const count = e.originalIndices.length;
        const stroke = isSel
          ? 'rgba(91,156,246,.9)'
          : e.isProxyHop ? 'rgba(251,191,36,.35)' : 'rgba(255,255,255,.18)';
        const arrowId = isSel ? 'arr-sel' : e.isProxyHop ? 'arr-proxy' : 'arr';
        const midX = (fp.x + tp.x) / 2 + NODE_W / 2;
        const midY = (fp.y + tp.y) / 2 + NODE_H / 2;

        const isFocusedEdge = focusedNetIds != null && focusedEdgeKeys.has(edgeKey);
        const session: SessionJson | null = isFocusedEdge
          ? (focusedSessions?.find(s => e.originalIndices.some(idx => graph.edges[idx]?.net_id === s.network_id)) ?? null)
          : null;
        const lp = isFocusedEdge ? edgeLabelPoints(fp, tp) : null;
        const srcIp = isFocusedEdge ? getNodeIp(e.from) : null;
        const dstIp = isFocusedEdge ? getNodeIp(e.to) : null;

        return (
          <g
            key={i}
            onClick={onEdgeClick ? () => onEdgeClick(e.from, e.to, e.originalIndices) : undefined}
            style={{ cursor: onEdgeClick ? 'pointer' : 'default', opacity: dimmed ? 0.1 : 1 }}
          >
            {onEdgeClick && <path d={edgePath(fp, tp)} fill="none" stroke="transparent" strokeWidth="14" />}
            <path
              d={edgePath(fp, tp)} fill="none" stroke={stroke}
              strokeWidth={isSel ? 2 : 1.5}
              strokeDasharray={e.isProxyHop && !isSel ? '4 3' : undefined}
              markerEnd={`url(#${arrowId})`}
              pointerEvents="none"
            />

            {/* Default labels — hidden when client is focused */}
            {interactive && !isSel && !isFocusedEdge && count > 1 && (
              <text x={midX} y={midY} textAnchor="middle" fill="rgba(255,255,255,.45)" fontSize="9" pointerEvents="none">
                {count} sessions
              </text>
            )}
            {interactive && !isSel && !isFocusedEdge && count === 1 && e.setup_ms > 0 && (
              <text x={midX} y={midY} textAnchor="middle" fill="rgba(255,255,255,.3)" fontSize="9" pointerEvents="none">
                net {e.net_id} · {e.setup_ms}ms
              </text>
            )}

            {/* Focused-client edge labels */}
            {isFocusedEdge && lp && (
              <g pointerEvents="none">
                {srcIp && (
                  <g>
                    <rect x={lp.src.x - 52} y={lp.src.y - 10} width={104} height={14} rx="3"
                      fill="rgba(3,5,8,.82)" stroke="rgba(255,255,255,.08)" />
                    <text x={lp.src.x} y={lp.src.y} textAnchor={lp.src.anchor}
                      fill="rgba(255,255,255,.75)" fontSize="8" fontFamily="'JetBrains Mono',monospace">
                      <tspan fill="rgba(255,255,255,.35)">src  </tspan>{srcIp}
                    </text>
                  </g>
                )}
                {dstIp && (
                  <g>
                    <rect x={lp.dst.x - 52} y={lp.dst.y - 10} width={104} height={14} rx="3"
                      fill="rgba(3,5,8,.82)" stroke="rgba(255,255,255,.08)" />
                    <text x={lp.dst.x} y={lp.dst.y} textAnchor={lp.dst.anchor}
                      fill="rgba(255,255,255,.75)" fontSize="8" fontFamily="'JetBrains Mono',monospace">
                      <tspan fill="rgba(255,255,255,.35)">dst  </tspan>{dstIp}
                    </text>
                  </g>
                )}
                {session && (
                  <g>
                    <rect x={lp.mid.x - 67} y={lp.mid.y - 24} width={134} height={48} rx="5"
                      fill="rgba(3,5,8,.88)" stroke="rgba(255,255,255,.07)" />
                    <text x={lp.mid.x} y={lp.mid.y - 11} textAnchor="middle"
                      fill="rgba(91,156,246,.9)" fontSize="8.5" fontWeight="600">
                      VNI {session.network_id}
                    </text>
                    <text x={lp.mid.x} y={lp.mid.y + 2} textAnchor="middle"
                      fill="rgba(255,255,255,.5)" fontSize="7.5" fontFamily="'JetBrains Mono',monospace">
                      <tspan fill="rgba(255,255,255,.3)">src  </tspan>{session.client_net}
                    </text>
                    <text x={lp.mid.x} y={lp.mid.y + 15} textAnchor="middle"
                      fill="rgba(255,255,255,.5)" fontSize="7.5" fontFamily="'JetBrains Mono',monospace">
                      <tspan fill="rgba(255,255,255,.3)">dst  </tspan>{session.server_net}
                    </text>
                  </g>
                )}
              </g>
            )}
          </g>
        );
      })}

      {/* Nodes */}
      {nodes.map((n, ni) => {
        const p = pos.get(n.id);
        if (!p) return null;
        const isSel = n.id === selectedNodeId;
        const nodeDimmed = focusedNetIds != null && !focusedNodeIds.has(n.id);
        const clickHandler = onNodeClick ? () => onNodeClick(n.id) : undefined;
        const clipId = `nc-${ni}`;

        if (n.kind === 'internet') {
          return (
            <g key={INTERNET_ID} onClick={clickHandler} style={{ cursor: onNodeClick ? 'pointer' : 'default', opacity: nodeDimmed ? 0.12 : 1 }}>
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
            <g key={n.id} onClick={clickHandler} style={{ cursor: onNodeClick ? 'pointer' : 'default', opacity: nodeDimmed ? 0.12 : 1 }}>
              <defs>
                <clipPath id={clipId}>
                  <rect x={p.x + 4} y={p.y + 2} width={NODE_W - 8} height={NODE_H - 4} />
                </clipPath>
              </defs>
              {isSel && (
                <rect x={p.x - 3} y={p.y - 3} width={NODE_W + 6} height={NODE_H + 6} rx="10"
                  fill="none" stroke="rgba(91,156,246,.6)" strokeWidth="1.5" />
              )}
              <rect x={p.x} y={p.y} width={NODE_W} height={NODE_H} rx="8"
                fill="rgba(251,191,36,.06)" stroke="rgba(251,191,36,.4)"
                strokeWidth="1" strokeDasharray="5 3" filter="url(#gT)" />
              <circle cx={p.x + 15} cy={p.y + 22} r="3.5" fill="#fbbf24" />
              <g clipPath={`url(#${clipId})`} pointerEvents="none">
                <text x={p.x + 27} y={p.y + 20} fill="rgba(251,191,36,.9)" fontSize="9.5" fontWeight="500">proxy</text>
                <text x={p.x + 27} y={p.y + 32} fill="rgba(255,255,255,.4)" fontSize="8" fontFamily="'JetBrains Mono',monospace">{n.id}</text>
              </g>
            </g>
          );
        }

        const color = n.registered ? '#34d399' : '#f87171';
        const strokeColor = n.registered ? 'rgba(52,211,153,.3)' : 'rgba(248,113,113,.2)';
        const ip = nodeIps.get(n.id);
        return (
          <g key={n.id} onClick={clickHandler} style={{ cursor: onNodeClick ? 'pointer' : 'default', opacity: nodeDimmed ? 0.12 : 1 }}>
            <title>{n.id}</title>
            <defs>
              <clipPath id={clipId}>
                <rect x={p.x + 4} y={p.y + 2} width={NODE_W - 8} height={NODE_H - 4} />
              </clipPath>
            </defs>
            {isSel && (
              <rect x={p.x - 3} y={p.y - 3} width={NODE_W + 6} height={NODE_H + 6} rx="10"
                fill="none" stroke="rgba(91,156,246,.6)" strokeWidth="1.5" />
            )}
            <rect x={p.x} y={p.y} width={NODE_W} height={NODE_H} rx="8"
              fill="rgba(255,255,255,.04)" stroke={strokeColor} strokeWidth="1" filter="url(#gT)" />
            <circle cx={p.x + 15} cy={p.y + 17} r="3.5" fill={color} />
            <g clipPath={`url(#${clipId})`} pointerEvents="none">
              <text x={p.x + 27} y={p.y + 20} fill="rgba(255,255,255,.85)" fontSize="9.5" fontWeight="500">{n.id}</text>
              {ip && (
                <text x={p.x + 27} y={p.y + 31} fill="rgba(255,255,255,.35)" fontSize="8" fontFamily="'JetBrains Mono',monospace">{ip}</text>
              )}
              <text x={p.x + 27} y={p.y + (ip ? 42 : 31)} fill="rgba(255,255,255,.3)" fontSize="7.5">
                {n.registered ? `${n.active_replica_count}/${n.replica_count} active` : 'unregistered'}
                {n.entry_point ? ' · entry' : ''}
              </text>
            </g>
          </g>
        );
      })}
    </svg>
  );
}
