import { useEffect, useRef } from 'react';
import type { GraphJson, SessionJson } from '../../types';
import { NODE_W, NODE_H, INET_W, INET_H, INTERNET_ID } from './types';
import type { TopoNode } from './types';
import {
  buildTopoGraph, layoutNodes, svgDims,
  edgePath, edgeMidpoint, egressEdgePath, egressLabelPoint, inetEdgePath, longEdgePath, edgeLabelPoints,
} from './layout';

// A modest bow is enough to visually separate two curves, but their labels
// are much wider than the stroke — they need far more room to actually clear
// each other, so the two use different magnitudes of the same offset.
const MUTUAL_PATH_OFFSET = 10;
const MUTUAL_LABEL_OFFSET = 55;

// Small dark pill behind a default edge label so two labels landing at
// nearby positions in a dense graph (e.g. an egress label and an unrelated
// edge's session count) stay legible instead of blending into each other.
function EdgeLabel({ x, y, text, color }: { x: number; y: number; text: string; color: string }) {
  const w = text.length * 5.2 + 10;
  return (
    <g pointerEvents="none">
      <rect x={x - w / 2} y={y - 9} width={w} height={12} rx="3" fill="rgba(3,5,8,.78)" />
      <text x={x} y={y} textAnchor="middle" fill={color} fontSize="9">{text}</text>
    </g>
  );
}

interface Props {
  graph: GraphJson;
  selectedNodeId?: string | null;
  selectedEdgeKey?: string | null;
  focusedNetIds?: Set<number> | null;
  focusedSessions?: SessionJson[] | null;
  nodeIps?: Map<string, string>;
  onNodeClick?: (id: string) => void;
  onEdgeClick?: (fromId: string, toId: string, edgeIndices: number[]) => void;
  onBgClick?: () => void;
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
  onBgClick,
}: Props) {
  const { nodes, edges } = buildTopoGraph(graph);
  const nodeById = new Map(nodes.map(n => [n.id, n]));

  const { pos, waypoints } = layoutNodes(nodes, edges);
  const { w, h } = svgDims(pos, nodes);

  // Track edge keys already seen so a connect animation plays only once per
  // newly-appeared edge, never on initial mount and never on later re-renders
  // of an edge that was already present (5s poll / SSE refetch keeps refiring
  // renders for edges that haven't actually changed).
  const seenEdgeKeysRef = useRef<Set<string> | null>(null);
  const currentEdgeKeys = new Set(edges.map(e => `${e.from}\0${e.to}`));
  // Intentional: reads the ref's pre-commit value during render to know which
  // edges are new-since-last-paint; it's only ever written from the effect
  // below, after commit, so this is safe (not read-your-own-write).
  const isNewEdge = (key: string) => seenEdgeKeysRef.current !== null && !seenEdgeKeysRef.current.has(key);
  useEffect(() => {
    seenEdgeKeysRef.current = currentEdgeKeys;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [graph]);

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

  // Selecting a node highlights just its direct connections — independent
  // of, and composable with, the client-session focus above (the two are
  // effectively mutually exclusive in practice, so a plain OR of the two
  // "should this be dimmed" conditions below is sufficient).
  const highlightSource = selectedNodeId;
  const highlightEdgeKeys = new Set<string>();
  const highlightNodeIds = new Set<string>();
  if (highlightSource) {
    for (const e of edges) {
      if (e.from === highlightSource || e.to === highlightSource) {
        highlightEdgeKeys.add(`${e.from}\0${e.to}`);
        highlightNodeIds.add(e.from);
        highlightNodeIds.add(e.to);
      }
    }
    highlightNodeIds.add(highlightSource);
  }

  function getNodeIp(id: string): string | null {
    const node = nodeById.get(id);
    if (!node) return null;
    if (node.kind === 'proxy') return node.id;
    if (node.kind === 'service') return nodeIps.get(id) ?? null;
    return null;
  }

  const interactive = !!(onNodeClick || onEdgeClick);

  // Shared node visuals: internet pill / proxy dashed box / service card.
  function renderNode(n: TopoNode, key: string) {
    const p = pos.get(n.id);
    if (!p) return null;
    const isSel = n.id === selectedNodeId;
    const nodeDimmed = (focusedNetIds != null && !focusedNodeIds.has(n.id)) ||
      (highlightSource != null && !highlightNodeIds.has(n.id));
    const clickHandler = onNodeClick ? (ev: { stopPropagation(): void }) => { ev.stopPropagation(); onNodeClick(n.id); } : undefined;
    const clipId = `nc-${key}`;

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
      if (n.placeholder) {
        return (
          <g key={n.id} style={{ opacity: nodeDimmed ? 0.12 : 1 }}>
            <rect x={p.x} y={p.y} width={NODE_W} height={NODE_H} rx="8"
              fill="rgba(255,255,255,.02)" stroke="rgba(255,255,255,.12)"
              strokeWidth="1" strokeDasharray="5 3" />
            <circle cx={p.x + 15} cy={p.y + 22} r="3.5" fill="none" stroke="rgba(255,255,255,.25)" strokeWidth="1.5" />
            <g clipPath={`url(#${clipId})`} pointerEvents="none">
              <defs>
                <clipPath id={clipId}>
                  <rect x={p.x + 4} y={p.y + 2} width={NODE_W - 8} height={NODE_H - 4} />
                </clipPath>
              </defs>
              <text x={p.x + 27} y={p.y + 20} fill="rgba(255,255,255,.35)" fontSize="9.5" fontWeight="500">proxy</text>
              <text x={p.x + 27} y={p.y + 32} fill="rgba(255,255,255,.25)" fontSize="7.5">no active connections</text>
            </g>
          </g>
        );
      }
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
            {n.registered && n.paused_replica_count > 0 ? ` · ${n.paused_replica_count} paused` : ''}
            {n.entry_point ? ' · entry' : ''}
          </text>
        </g>
      </g>
    );
  }

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
        <marker id="arr-egress" markerWidth="8" markerHeight="8" refX="7" refY="3" orient="auto">
          <path d="M0,0 L0,6 L8,3 z" fill="rgba(167,139,250,.7)" />
        </marker>
      </defs>

      {onBgClick && <rect x={0} y={0} width={w} height={h} fill="transparent" onClick={onBgClick} />}

      {/* Internet → Proxy edges */}
          {/* eslint-disable-next-line react-hooks/refs -- see isNewEdge comment above */}
          {edges.filter(e => e.isInternetEdge).map(e => {
            const fp = pos.get(e.from);
            const tp = pos.get(e.to);
            if (!fp || !tp) return null;
            const dimmed = (focusedNetIds != null && !focusedNodeIds.has(e.to)) ||
              (highlightSource != null && !highlightNodeIds.has(e.to));
            const edgeKey = `${e.from}\0${e.to}`;
            const isNew = isNewEdge(edgeKey);
            return (
              <path
                key={edgeKey}
                d={inetEdgePath(fp, tp)}
                fill="none"
                stroke="rgba(91,156,246,.3)"
                strokeWidth="1.5"
                strokeDasharray="4 3"
                markerEnd="url(#arr-inet)"
                pointerEvents="none"
                opacity={dimmed ? 0.1 : 1}
                style={isNew ? { animation: 'edge-fade-in .9s ease-out' } : undefined}
              />
            );
          })}

          {/* Service / proxy edges */}
          {/* eslint-disable-next-line react-hooks/refs -- see isNewEdge comment above */}
          {edges.filter(e => !e.isInternetEdge).map(e => {
            const fp = pos.get(e.from);
            const tp = pos.get(e.to);
            if (!fp || !tp) return null;
            const edgeKey = `${e.from}\0${e.to}`;
            const isSel = selectedEdgeKey === edgeKey;
            const isNew = isNewEdge(edgeKey);
            const dimmed = (focusedNetIds != null && !focusedEdgeKeys.has(edgeKey)) ||
              (highlightSource != null && !highlightEdgeKeys.has(edgeKey));
            const count = e.originalIndices.length;
            const stroke = isSel
              ? 'rgba(91,156,246,.9)'
              : e.isEgress ? 'rgba(167,139,250,.55)'
              : e.isProxyHop ? 'rgba(251,191,36,.35)' : 'rgba(255,255,255,.18)';
            const arrowId = isSel ? 'arr-sel' : e.isEgress ? 'arr-egress' : e.isProxyHop ? 'arr-proxy' : 'arr';
            const dash = isSel ? undefined : e.isEgress ? '2 4' : e.isProxyHop ? '4 3' : undefined;
            const wp = waypoints.get(edgeKey);
            // A mutual pair (both A→B and B→A present, e.g. a two-node cycle)
            // has a midpoint-symmetric geometry — without an offset, both
            // directions draw the exact same curve and label position.
            const isMutual = !e.isEgress && currentEdgeKeys.has(`${e.to}\0${e.from}`);
            const mutualSign = isMutual ? (e.from < e.to ? -1 : 1) : 0;
            // A layer-skipping egress edge gets the same reserved waypoint
            // lane as any other long edge, so it no longer has to bow across
            // whatever sits between the service and the (usually distant)
            // proxy row. Adjacent-layer egress edges (no waypoints) keep the
            // original right-bow routing, which is short enough to be fine.
            const path = wp?.length ? longEdgePath(fp, wp, tp)
              : e.isEgress ? egressEdgePath(fp, tp)
              : edgePath(fp, tp, mutualSign * MUTUAL_PATH_OFFSET);
            // edgeMidpoint assumes a direct fp-to-tp curve — on a waypoint-
            // routed edge the actual path bends through a reserved slot that
            // can be nowhere near that raw midpoint (and, being just the
            // endpoints' midpoint, has no idea a real node might already sit
            // there). Anchor the label to the middle waypoint instead, which
            // is guaranteed collision-free by construction.
            const { x: midX, y: midY } = wp?.length
              ? { x: wp[Math.floor(wp.length / 2)].x + NODE_W / 2, y: wp[Math.floor(wp.length / 2)].y + NODE_H / 2 }
              : edgeMidpoint(fp, tp, mutualSign * MUTUAL_LABEL_OFFSET);

            const isFocusedEdge = focusedNetIds != null && focusedEdgeKeys.has(edgeKey);
            const session: SessionJson | null = isFocusedEdge
              ? (focusedSessions?.find(s => e.originalIndices.some(idx => graph.edges[idx]?.net_id === s.network_id)) ?? null)
              : null;
            // For chain hops the session is null (chain sessions don't carry the client IP),
            // but we can still display the VNI by pulling it directly from the graph edge.
            const focusedNetId: number | null = isFocusedEdge
              ? (session?.network_id ??
                 e.originalIndices.map(idx => graph.edges[idx]?.net_id).find(id => id !== undefined && focusedNetIds!.has(id)) ??
                 null)
              : null;
            const lp = isFocusedEdge ? edgeLabelPoints(fp, tp) : null;
            const srcIp = isFocusedEdge ? getNodeIp(e.from) : null;
            const dstIp = isFocusedEdge ? getNodeIp(e.to) : null;

            return (
              <g
                key={edgeKey}
                onClick={onEdgeClick ? (ev) => { ev.stopPropagation(); onEdgeClick(e.from, e.to, e.originalIndices); } : undefined}
                style={{ cursor: onEdgeClick ? 'pointer' : 'default', opacity: dimmed ? 0.1 : 1 }}
              >
                {onEdgeClick && <path d={path} fill="none" stroke="transparent" strokeWidth="14" />}
                <path
                  // pathLength normalizes stroke-dasharray/dashoffset to a 0–1 range for the
                  // draw-in animation below — only safe when the edge has no dash pattern of
                  // its own (dash === undefined); setting it on a dashed edge (proxy-hop "4 3",
                  // egress "2 4") would rescale that pattern relative to the whole path and
                  // stretch it into what looks like a solid line once the animation ends.
                  pathLength={dash === undefined ? '1' : undefined}
                  d={path} fill="none" stroke={stroke}
                  strokeWidth={isSel ? 2 : 1.5}
                  strokeDasharray={dash}
                  markerEnd={`url(#${arrowId})`}
                  pointerEvents="none"
                  style={isNew
                    ? { animation: dash === undefined ? 'edge-draw-in .9s ease-out' : 'edge-fade-in .9s ease-out' }
                    : undefined}
                />

                {/* Egress edge marker label — anchored to the curve's own bow
                    peak, or its waypoint-lane midpoint when it has one */}
                {interactive && e.isEgress && !isSel && (() => {
                  const lp = wp?.length ? edgeMidpoint(fp, tp) : egressLabelPoint(fp, tp);
                  return <EdgeLabel x={lp.x} y={lp.y} text="egress" color="rgba(167,139,250,.8)" />;
                })()}

                {/* Default labels — hidden when client is focused or on egress edges */}
                {interactive && !isSel && !isFocusedEdge && !e.isEgress && count > 1 && (
                  <EdgeLabel x={midX} y={midY} text={`${count} sessions`} color="rgba(255,255,255,.55)" />
                )}
                {interactive && !isSel && !isFocusedEdge && !e.isEgress && count === 1 && e.setup_ms > 0 && (
                  <EdgeLabel x={midX} y={midY} text={`net ${e.net_id} · ${e.setup_ms}ms`} color="rgba(255,255,255,.4)" />
                )}

                {/* Focused-client edge labels (session edges only, not egress) */}
                {isFocusedEdge && !e.isEgress && lp && (
                  <g pointerEvents="none">
                    {srcIp && (
                      <g>
                        <rect x={lp.src.x - 50} y={lp.src.y - 9} width={100} height={11} rx="2"
                          fill="rgba(3,5,8,.82)" stroke="rgba(255,255,255,.08)" />
                        <text x={lp.src.x} y={lp.src.y} textAnchor="middle"
                          fill="rgba(255,255,255,.75)" fontSize="7.5" fontFamily="'JetBrains Mono',monospace">
                          <tspan fill="rgba(255,255,255,.35)">src  </tspan>{srcIp}
                        </text>
                      </g>
                    )}
                    {dstIp && (
                      <g>
                        <rect x={lp.dst.x - 50} y={lp.dst.y - 9} width={100} height={11} rx="2"
                          fill="rgba(3,5,8,.82)" stroke="rgba(255,255,255,.08)" />
                        <text x={lp.dst.x} y={lp.dst.y} textAnchor="middle"
                          fill="rgba(255,255,255,.75)" fontSize="7.5" fontFamily="'JetBrains Mono',monospace">
                          <tspan fill="rgba(255,255,255,.35)">dst  </tspan>{dstIp}
                        </text>
                      </g>
                    )}
                    {focusedNetId !== null && (
                      <g>
                        <rect x={lp.mid.x - 60} y={lp.mid.y - 15} width={120} height={session ? 32 : 16} rx="4"
                          fill="rgba(3,5,8,.88)" stroke="rgba(255,255,255,.07)" />
                        <text x={lp.mid.x} y={lp.mid.y - 6} textAnchor="middle"
                          fill="rgba(91,156,246,.9)" fontSize="8" fontWeight="600">
                          VNI {focusedNetId}
                        </text>
                        {session && (
                          <>
                            <text x={lp.mid.x} y={lp.mid.y + 4} textAnchor="middle"
                              fill="rgba(255,255,255,.5)" fontSize="7" fontFamily="'JetBrains Mono',monospace">
                              <tspan fill="rgba(255,255,255,.3)">src  </tspan>{session.client_net}
                            </text>
                            <text x={lp.mid.x} y={lp.mid.y + 13} textAnchor="middle"
                              fill="rgba(255,255,255,.5)" fontSize="7" fontFamily="'JetBrains Mono',monospace">
                              <tspan fill="rgba(255,255,255,.3)">dst  </tspan>{session.server_net}
                            </text>
                          </>
                        )}
                      </g>
                    )}
                  </g>
                )}
              </g>
            );
          })}

      {/* Nodes */}
      {nodes.map((n, ni) => renderNode(n, String(ni)))}
    </svg>
  );
}
