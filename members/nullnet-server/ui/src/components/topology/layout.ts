import type { GraphJson } from '../../types';
import { NODE_W, NODE_H, H_GAP, V_GAP, INET_W, INET_H, INET_Y, INET_PROXY_GAP, INTERNET_ID, PLACEHOLDER_PROXY_ID } from './types';
import type { Pos, TopoNode, TopoEdge } from './types';

export function buildTopoGraph(graph: GraphJson): { nodes: TopoNode[]; edges: TopoEdge[] } {
  const nodes: TopoNode[] = graph.nodes.map(n => ({ ...n, kind: 'service' as const }));
  // Proxy nodes come from both inbound (via_proxy) and outbound egress (to).
  const proxyIps = new Set<string>();
  for (const e of graph.edges) {
    if (e.via_proxy) proxyIps.add(e.via_proxy);
    if (e.egress) proxyIps.add(e.to);
  }
  for (const ip of proxyIps) nodes.push({ kind: 'proxy', id: ip });

  // Internet + proxy are always shown, even with no active connections — fall
  // back to a non-interactive placeholder proxy node when none are live.
  if (proxyIps.size === 0) {
    nodes.push({ kind: 'proxy', id: PLACEHOLDER_PROXY_ID, placeholder: true });
  }
  nodes.push({ kind: 'internet', id: INTERNET_ID });

  const inetEdges: TopoEdge[] = [];
  for (const ip of proxyIps) {
    inetEdges.push({ from: INTERNET_ID, to: ip, net_id: -1, setup_ms: 0, isProxyHop: false, isInternetEdge: true, isEgress: false, originalIndices: [] });
  }

  // De-duplicate service/proxy edges by (from, to) key — multiple sessions share one drawn edge.
  // Egress edges are kept in a separate map so they never merge with an inbound
  // proxy hop that happens to share a (from, to) pair but runs the opposite way.
  const edgeMap = new Map<string, TopoEdge>();
  const egressMap = new Map<string, TopoEdge>();
  for (let idx = 0; idx < graph.edges.length; idx++) {
    const e = graph.edges[idx];
    if (e.egress) {
      const k = `${e.from}\0${e.to}`;
      if (!egressMap.has(k)) {
        egressMap.set(k, { from: e.from, to: e.to, net_id: e.net_id, setup_ms: e.setup_ms, isProxyHop: false, isInternetEdge: false, isEgress: true, originalIndices: [] });
      }
      egressMap.get(k)!.originalIndices.push(idx);
    } else if (e.via_proxy) {
      const k1 = `${e.from}\0${e.via_proxy}`;
      if (!edgeMap.has(k1)) {
        edgeMap.set(k1, { from: e.from, to: e.via_proxy, net_id: e.net_id, setup_ms: 0, isProxyHop: true, isInternetEdge: false, isEgress: false, originalIndices: [] });
      }
      edgeMap.get(k1)!.originalIndices.push(idx);

      const k2 = `${e.via_proxy}\0${e.to}`;
      if (!edgeMap.has(k2)) {
        edgeMap.set(k2, { from: e.via_proxy, to: e.to, net_id: e.net_id, setup_ms: e.setup_ms, isProxyHop: true, isInternetEdge: false, isEgress: false, originalIndices: [] });
      }
      edgeMap.get(k2)!.originalIndices.push(idx);
    } else {
      const k = `${e.from}\0${e.to}`;
      if (!edgeMap.has(k)) {
        edgeMap.set(k, { from: e.from, to: e.to, net_id: e.net_id, setup_ms: e.setup_ms, isProxyHop: false, isInternetEdge: false, isEgress: false, originalIndices: [] });
      }
      edgeMap.get(k)!.originalIndices.push(idx);
    }
  }
  return { nodes, edges: [...inetEdges, ...edgeMap.values(), ...egressMap.values()] };
}

export function layoutNodes(nodes: TopoNode[], edges: TopoEdge[]): { pos: Map<string, Pos>; waypoints: Map<string, Pos[]> } {
  const hasInternet = nodes.some(n => n.kind === 'internet');
  const proxyNodes = nodes.filter(n => n.kind === 'proxy');
  const serviceNodes = nodes.filter(n => n.kind === 'service');
  const pos = new Map<string, Pos>();

  // Proxy row y shifts down when internet node is present
  const proxyRowY = hasInternet ? INET_Y + INET_H + INET_PROXY_GAP : V_GAP;

  proxyNodes.forEach((n, i) => {
    pos.set(n.id, { x: H_GAP + i * (NODE_W + H_GAP), y: proxyRowY });
  });

  // Internet node — centered over the proxy row
  if (hasInternet && proxyNodes.length > 0) {
    const proxyRowCenter = H_GAP + ((proxyNodes.length - 1) * (NODE_W + H_GAP)) / 2 + NODE_W / 2;
    pos.set(INTERNET_ID, { x: proxyRowCenter - INET_W / 2, y: INET_Y });
  }

  const svcOffsetY = proxyNodes.length > 0 ? proxyRowY + NODE_H + V_GAP : V_GAP;
  const svcSet = new Set(serviceNodes.map(n => n.id));
  const out = new Map<string, Set<string>>();
  const inc = new Map<string, Set<string>>();
  for (const n of serviceNodes) { out.set(n.id, new Set()); inc.set(n.id, new Set()); }
  for (const e of edges) {
    if (svcSet.has(e.from) && svcSet.has(e.to)) {
      out.get(e.from)!.add(e.to);
      inc.get(e.to)!.add(e.from);
    }
  }

  // The dependency graph is legitimately cyclic, so drop DFS back edges first.
  // On an acyclic graph nothing is dropped and the layering below is unchanged.
  const dag = new Map<string, Set<string>>();
  const dagInc = new Map<string, Set<string>>();
  for (const n of serviceNodes) { dag.set(n.id, new Set()); dagInc.set(n.id, new Set()); }
  {
    const state = new Map<string, 'open' | 'done'>();
    const visit = (id: string) => {
      state.set(id, 'open');
      for (const next of out.get(id) ?? []) {
        if (state.get(next) === 'open') continue; // back edge: closes a cycle
        dag.get(id)!.add(next);
        dagInc.get(next)!.add(id);
        if (!state.has(next)) visit(next);
      }
      state.set(id, 'done');
    };
    // Seed from real roots first; then from services reached straight off a
    // proxy, so a session whose entry point sits inside a cycle still lays out
    // from that entry point; then anything still unvisited.
    const proxyTargets = edges.filter(e => e.isProxyHop && svcSet.has(e.to)).map(e => e.to);
    for (const n of serviceNodes) if (!inc.get(n.id)?.size && !state.has(n.id)) visit(n.id);
    for (const id of proxyTargets) if (!state.has(id)) visit(id);
    for (const n of serviceNodes) if (!state.has(n.id)) visit(n.id);
  }

  const layer = new Map<string, number>();
  const q: string[] = serviceNodes.filter(n => !dagInc.get(n.id)?.size).map(n => n.id);
  q.forEach(id => layer.set(id, 0));
  for (let i = 0; i < q.length; i++) {
    const id = q[i], l = layer.get(id)!;
    for (const next of dag.get(id) ?? []) {
      if (l + 1 > (layer.get(next) ?? 0)) {
        layer.set(next, l + 1);
        q.push(next);
      }
    }
  }
  for (const n of serviceNodes) { if (!layer.has(n.id)) layer.set(n.id, 0); }

  const byLayer = new Map<number, string[]>();
  for (const [id, l] of layer) {
    if (!byLayer.has(l)) byLayer.set(l, []);
    byLayer.get(l)!.push(id);
  }

  // Within-layer ordering: minimize edge crossings with a barycenter heuristic
  // instead of a fixed alphabetical sort. Alphabetical order is still the seed
  // (and the tiebreak), so output stays deterministic across the 5s poll.
  const order = new Map<number, string[]>();
  for (const [l, ids] of byLayer) order.set(l, [...ids].sort());
  const maxLayer = Math.max(0, ...order.keys());

  // Barycenters are computed over the FULL (non-DFS-stripped) adjacency, so a
  // back edge dropped only for layering purposes still pulls its endpoints
  // toward each other visually.
  const allNeighbors = new Map<string, Set<string>>();
  for (const n of serviceNodes) allNeighbors.set(n.id, new Set([...out.get(n.id)!, ...inc.get(n.id)!]));

  // Layer 0 has no layer above it — its "down" reference is the fixed proxy
  // row instead, so entry services line up under the proxy they actually
  // enter through.
  const proxySet = new Set(proxyNodes.map(n => n.id));
  const proxyIndexById = new Map(proxyNodes.map((n, i) => [n.id, i]));
  const svcProxyNeighbors = new Map<string, Set<string>>();
  for (const n of serviceNodes) svcProxyNeighbors.set(n.id, new Set());
  for (const e of edges) {
    if (svcSet.has(e.from) && proxySet.has(e.to)) svcProxyNeighbors.get(e.from)!.add(e.to);
    if (proxySet.has(e.from) && svcSet.has(e.to)) svcProxyNeighbors.get(e.to)!.add(e.from);
  }

  // Long-edge routing: any edge whose endpoints land more than one layer
  // apart gets a chain of invisible waypoint nodes, one per intermediate
  // layer. They're added to `order` and `allNeighbors` just like real nodes,
  // so the crossing-reduction sweep below pushes real nodes out of the way
  // of the edge instead of letting it cut a straight line through them.
  // Proxies sit at virtual layer -1 for this purpose only (their own row is
  // fixed, never reordered). Egress edges participate too — a service several
  // layers deep still needs to reach the proxy row, and without a reserved
  // lane its bow-right routing has to sweep across whatever sits in between.
  // Internet edges are routed separately and don't participate.
  const waypointIds = new Map<string, string[]>(); // edge key -> dummy ids, source→dest order
  const effLayer = (id: string): number | null => (proxySet.has(id) ? -1 : layer.get(id) ?? null);
  let dummySeq = 0;
  for (const e of edges) {
    if (e.isInternetEdge) continue;
    const fromOk = svcSet.has(e.from) || proxySet.has(e.from);
    const toOk = svcSet.has(e.to) || proxySet.has(e.to);
    if (!fromOk || !toOk) continue;
    const lu = effLayer(e.from), lv = effLayer(e.to);
    if (lu === null || lv === null) continue;
    const lo = Math.min(lu, lv), hi = Math.max(lu, lv);
    if (hi - lo <= 1) continue; // adjacent (or same) layer — no dummies needed

    const chainLayers: number[] = [];
    for (let l = Math.max(lo + 1, 0); l < hi; l++) chainLayers.push(l);
    if (!chainLayers.length) continue;
    if (lu > lv) chainLayers.reverse(); // keep the chain in source→dest order

    const ids = chainLayers.map(l => {
      const id = `__wp${dummySeq++}`;
      order.get(l)!.push(id);
      return id;
    });
    const chain = [e.from, ...ids, e.to];
    for (let i = 0; i < chain.length - 1; i++) {
      const a = chain[i], b = chain[i + 1];
      if (!allNeighbors.has(a)) allNeighbors.set(a, new Set());
      if (!allNeighbors.has(b)) allNeighbors.set(b, new Set());
      allNeighbors.get(a)!.add(b);
      allNeighbors.get(b)!.add(a);
    }
    waypointIds.set(`${e.from}\0${e.to}`, ids);
  }

  const indexOf = (ids: string[]): Map<string, number> => {
    const m = new Map<string, number>();
    ids.forEach((id, i) => m.set(id, i));
    return m;
  };

  // Reorders one layer against a fixed reference layer's current positions.
  // Nodes with no neighbor in the reference layer keep their current index
  // (as their "barycenter"), so disconnected nodes don't jump around.
  const reorderLayer = (l: number, refIndex: Map<string, number>, neighborsOf: (id: string) => Set<string>) => {
    const ids = order.get(l);
    if (!ids || ids.length < 2) return;
    const prevIndex = indexOf(ids);
    const keyed = ids.map(id => {
      const refIdxs = [...neighborsOf(id)].map(n => refIndex.get(n)).filter((v): v is number => v !== undefined);
      const bary = refIdxs.length ? refIdxs.reduce((a, b) => a + b, 0) / refIdxs.length : prevIndex.get(id)!;
      return { id, bary };
    });
    keyed.sort((a, b) => a.bary - b.bary || a.id.localeCompare(b.id));
    order.set(l, keyed.map(k => k.id));
  };

  const SWEEPS = 4;
  for (let s = 0; s < SWEEPS; s++) {
    if (s % 2 === 0) {
      for (let l = 0; l <= maxLayer; l++) {
        // A waypoint dummy has no entry in svcProxyNeighbors (that map is
        // only ever populated for real services) — falling back to
        // allNeighbors picks up the proxy id linked in via the chain above.
        if (l === 0) reorderLayer(l, proxyIndexById, id => {
          const svc = svcProxyNeighbors.get(id);
          return svc?.size ? svc : (allNeighbors.get(id) ?? new Set());
        });
        else reorderLayer(l, indexOf(order.get(l - 1) ?? []), id => allNeighbors.get(id) ?? new Set());
      }
    } else {
      for (let l = maxLayer; l >= 0; l--) {
        reorderLayer(l, indexOf(order.get(l + 1) ?? []), id => allNeighbors.get(id) ?? new Set());
      }
    }
  }

  for (const l of [...order.keys()].sort((a, b) => a - b)) {
    const row = order.get(l)!;
    row.forEach((id, i) => pos.set(id, { x: H_GAP + i * (NODE_W + H_GAP), y: svcOffsetY + l * (NODE_H + V_GAP) }));
  }

  const waypoints = new Map<string, Pos[]>();
  for (const [edgeKey, ids] of waypointIds) {
    waypoints.set(edgeKey, ids.map(id => pos.get(id)!));
  }
  return { pos, waypoints };
}

// Every node is NODE_W x NODE_H except the internet node, which is its own
// (smaller) pill size — shared by any layout math that needs real node extents.
export function nodeSize(node: TopoNode | undefined): { w: number; h: number } {
  return node?.kind === 'internet' ? { w: INET_W, h: INET_H } : { w: NODE_W, h: NODE_H };
}

export function svgDims(pos: Map<string, Pos>, nodes: TopoNode[]): { w: number; h: number } {
  const nodeById = new Map(nodes.map(n => [n.id, n]));
  let maxX = 0, maxY = 0;
  for (const [id, { x, y }] of pos.entries()) {
    const { w: nw, h: nh } = nodeSize(nodeById.get(id));
    maxX = Math.max(maxX, x + nw);
    maxY = Math.max(maxY, y + nh);
  }
  return { w: maxX + H_GAP, h: maxY + V_GAP };
}

// `offset` shifts both endpoints sideways (vertical routing) or up/down
// (horizontal routing) by the same amount — used to pull a mutual pair
// (A→B and B→A both present, e.g. a two-node cycle) apart into two parallel
// tracks. Without it, the midpoint-symmetric formula below puts both
// directions' curves — and their labels — on the exact same line.
export function edgePath(from: Pos, to: Pos, offset = 0): string {
  const fromMidY = from.y + NODE_H / 2;
  const toMidY = to.y + NODE_H / 2;
  if (toMidY > fromMidY + NODE_H) {
    const x1 = from.x + NODE_W / 2 + offset, y1 = from.y + NODE_H;
    const x2 = to.x + NODE_W / 2 + offset, y2 = to.y;
    const cy = (y1 + y2) / 2;
    return `M ${x1} ${y1} C ${x1} ${cy}, ${x2} ${cy}, ${x2} ${y2}`;
  }
  if (fromMidY > toMidY + NODE_H) {
    const x1 = from.x + NODE_W / 2 + offset, y1 = from.y;
    const x2 = to.x + NODE_W / 2 + offset, y2 = to.y + NODE_H;
    const cy = (y1 + y2) / 2;
    return `M ${x1} ${y1} C ${x1} ${cy}, ${x2} ${cy}, ${x2} ${y2}`;
  }
  const goRight = to.x >= from.x;
  const x1 = goRight ? from.x + NODE_W : from.x;
  const y1 = from.y + NODE_H / 2 + offset;
  const x2 = goRight ? to.x : to.x + NODE_W;
  const y2 = to.y + NODE_H / 2 + offset;
  const cx = (x1 + x2) / 2;
  return `M ${x1} ${y1} C ${cx} ${y1}, ${cx} ${y2}, ${x2} ${y2}`;
}

// Label anchor matching edgePath's own routing (including `offset`) — mirrors
// its branching so a mutual pair's two labels land on their own offset curve
// instead of both sitting at the un-offset geometric midpoint.
export function edgeMidpoint(from: Pos, to: Pos, offset = 0): Pos {
  const fromMidY = from.y + NODE_H / 2;
  const toMidY = to.y + NODE_H / 2;
  if (toMidY > fromMidY + NODE_H || fromMidY > toMidY + NODE_H) {
    return { x: (from.x + to.x) / 2 + NODE_W / 2 + offset, y: (from.y + to.y) / 2 + NODE_H / 2 };
  }
  const goRight = to.x >= from.x;
  const x1 = goRight ? from.x + NODE_W : from.x;
  const x2 = goRight ? to.x : to.x + NODE_W;
  return { x: (x1 + x2) / 2, y: (from.y + to.y) / 2 + NODE_H / 2 + offset };
}

// Multi-hop version of edgePath's downward/upward case, routed through the
// waypoint dummy positions layoutNodes() reserved for this edge — one smooth
// piecewise-cubic path through each intermediate layer's actual slot, instead
// of a single curve that ignores what sits in between. Waypoints are pure
// points (no box to attach to), so only the real `from`/`to` ends get the
// box-edge offset; every waypoint contributes its exact center.
export function longEdgePath(from: Pos, waypoints: Pos[], to: Pos): string {
  const downward = to.y >= from.y;
  const centerOf = (p: Pos, isFirst: boolean, isLast: boolean) => ({
    x: p.x + NODE_W / 2,
    y: isFirst ? (downward ? p.y + NODE_H : p.y)
      : isLast ? (downward ? p.y : p.y + NODE_H)
      : p.y + NODE_H / 2,
  });
  const points = [from, ...waypoints, to];
  const centers = points.map((p, i) => centerOf(p, i === 0, i === points.length - 1));
  let d = `M ${centers[0].x} ${centers[0].y}`;
  for (let i = 0; i < centers.length - 1; i++) {
    const { x: x1, y: y1 } = centers[i], { x: x2, y: y2 } = centers[i + 1];
    const cy = (y1 + y2) / 2;
    d += ` C ${x1} ${cy}, ${x2} ${cy}, ${x2} ${y2}`;
  }
  return d;
}

// Positions for per-edge labels when a client is focused.
// src/dst are placed just outside the node endpoints; mid is the curve midpoint.
type TextAnchor = 'start' | 'middle' | 'end';

export function edgeLabelPoints(
  from: Pos, to: Pos,
  fromSize: { w: number; h: number } = { w: NODE_W, h: NODE_H },
  toSize: { w: number; h: number } = { w: NODE_W, h: NODE_H },
): {
  src: { x: number; y: number; anchor: TextAnchor };
  dst: { x: number; y: number; anchor: TextAnchor };
  mid: { x: number; y: number };
} {
  const fromMidY = from.y + fromSize.h / 2;
  const toMidY = to.y + toSize.h / 2;

  if (toMidY > fromMidY + fromSize.h) {
    // downward — exits bottom-center, enters top-center
    const x1 = from.x + fromSize.w / 2, y1 = from.y + fromSize.h;
    const x2 = to.x + toSize.w / 2,     y2 = to.y;
    return {
      src: { x: x1, y: y1 + 10, anchor: 'middle' },
      dst: { x: x2, y: y2 - 4,  anchor: 'middle' },
      mid: { x: (x1 + x2) / 2,  y: (y1 + y2) / 2 },
    };
  }
  if (fromMidY > toMidY + toSize.h) {
    // upward — exits top-center, enters bottom-center
    const x1 = from.x + fromSize.w / 2, y1 = from.y;
    const x2 = to.x + toSize.w / 2,     y2 = to.y + toSize.h;
    return {
      src: { x: x1, y: y1 - 4,  anchor: 'middle' },
      dst: { x: x2, y: y2 + 10, anchor: 'middle' },
      mid: { x: (x1 + x2) / 2,  y: (y1 + y2) / 2 },
    };
  }
  // horizontal — exits left/right side; stack src above and dst below the VNI box
  // to avoid all three labels colliding in the narrow gap between nodes.
  const x1 = (to.x >= from.x ? from.x + fromSize.w : from.x);
  const y1 = from.y + fromSize.h / 2;
  const x2 = (to.x >= from.x ? to.x : to.x + toSize.w);
  const y2 = to.y + toSize.h / 2;
  const midX = (x1 + x2) / 2;
  const midY = (y1 + y2) / 2;
  return {
    src: { x: midX, y: midY - 30, anchor: 'middle' },
    dst: { x: midX, y: midY + 32, anchor: 'middle' },
    mid: { x: midX, y: midY },
  };
}

export function inetEdgePath(from: Pos, to: Pos): string {
  const x1 = from.x + INET_W / 2;
  const y1 = from.y + INET_H;
  const x2 = to.x + NODE_W / 2;
  const y2 = to.y;
  const cy = (y1 + y2) / 2;
  return `M ${x1} ${y1} C ${x1} ${cy}, ${x2} ${cy}, ${x2} ${y2}`;
}

// Egress edge (initiator service → gateway proxy). Both ends attach on the node's
// RIGHT face and the curve bows further right, so it never sits on top of the
// (vertically routed) inbound proxy edge between the same two nodes. The arrow
// lands on the proxy end.
const EGRESS_BOW = 46;
export function egressEdgePath(from: Pos, to: Pos): string {
  const x1 = from.x + NODE_W, y1 = from.y + NODE_H / 2;
  const x2 = to.x + NODE_W, y2 = to.y + NODE_H / 2;
  const bowX = Math.max(x1, x2) + EGRESS_BOW;
  return `M ${x1} ${y1} C ${bowX} ${y1}, ${bowX} ${y2}, ${x2} ${y2}`;
}

// Label anchor for an egress edge — sits at the curve's own bow peak instead
// of a separately-guessed position, so it can't drift away from the edge it
// labels (it previously used from.x/to.x directly instead of the +NODE_W
// bow origin above, which could put it right on top of an unrelated edge's
// own midpoint label). Offset up slightly so it doesn't sit exactly on that
// midpoint even when the two happen to coincide horizontally.
export function egressLabelPoint(from: Pos, to: Pos): { x: number; y: number } {
  const x1 = from.x + NODE_W, x2 = to.x + NODE_W;
  const y1 = from.y + NODE_H / 2, y2 = to.y + NODE_H / 2;
  return { x: Math.max(x1, x2) + EGRESS_BOW, y: (y1 + y2) / 2 - 9 };
}
