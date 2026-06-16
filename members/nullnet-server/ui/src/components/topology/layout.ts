import type { GraphJson } from '../../types';
import { NODE_W, NODE_H, H_GAP, V_GAP, INET_W, INET_H, INET_Y, INET_PROXY_GAP, INTERNET_ID } from './types';
import type { Pos, TopoNode, TopoEdge } from './types';

export function buildTopoGraph(graph: GraphJson): { nodes: TopoNode[]; edges: TopoEdge[] } {
  const nodes: TopoNode[] = graph.nodes.map(n => ({ ...n, kind: 'service' as const }));
  const proxyIps = new Set<string>();
  for (const e of graph.edges) { if (e.via_proxy) proxyIps.add(e.via_proxy); }
  for (const ip of proxyIps) nodes.push({ kind: 'proxy', id: ip });

  // Internet node — added whenever there are proxy nodes
  if (proxyIps.size > 0) {
    nodes.push({ kind: 'internet', id: INTERNET_ID });
  }

  const inetEdges: TopoEdge[] = [];
  for (const ip of proxyIps) {
    inetEdges.push({ from: INTERNET_ID, to: ip, net_id: -1, setup_ms: 0, isProxyHop: false, isInternetEdge: true, originalIndices: [] });
  }

  // De-duplicate service/proxy edges by (from, to) key — multiple sessions share one drawn edge.
  const edgeMap = new Map<string, TopoEdge>();
  for (let idx = 0; idx < graph.edges.length; idx++) {
    const e = graph.edges[idx];
    if (e.via_proxy) {
      const k1 = `${e.from}\0${e.via_proxy}`;
      if (!edgeMap.has(k1)) {
        edgeMap.set(k1, { from: e.from, to: e.via_proxy, net_id: e.net_id, setup_ms: 0, isProxyHop: true, isInternetEdge: false, originalIndices: [] });
      }
      edgeMap.get(k1)!.originalIndices.push(idx);

      const k2 = `${e.via_proxy}\0${e.to}`;
      if (!edgeMap.has(k2)) {
        edgeMap.set(k2, { from: e.via_proxy, to: e.to, net_id: e.net_id, setup_ms: e.setup_ms, isProxyHop: true, isInternetEdge: false, originalIndices: [] });
      }
      edgeMap.get(k2)!.originalIndices.push(idx);
    } else {
      const k = `${e.from}\0${e.to}`;
      if (!edgeMap.has(k)) {
        edgeMap.set(k, { from: e.from, to: e.to, net_id: e.net_id, setup_ms: e.setup_ms, isProxyHop: false, isInternetEdge: false, originalIndices: [] });
      }
      edgeMap.get(k)!.originalIndices.push(idx);
    }
  }
  return { nodes, edges: [...inetEdges, ...edgeMap.values()] };
}

export function layoutNodes(nodes: TopoNode[], edges: TopoEdge[]): Map<string, Pos> {
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

  const layer = new Map<string, number>();
  const q: string[] = serviceNodes.filter(n => !inc.get(n.id)?.size).map(n => n.id);
  q.forEach(id => layer.set(id, 0));
  for (let i = 0; i < q.length; i++) {
    const id = q[i], l = layer.get(id)!;
    for (const next of out.get(id) ?? []) {
      layer.set(next, Math.max(layer.get(next) ?? 0, l + 1));
      q.push(next);
    }
  }
  for (const n of serviceNodes) { if (!layer.has(n.id)) layer.set(n.id, 0); }

  const byLayer = new Map<number, string[]>();
  for (const [id, l] of layer) {
    if (!byLayer.has(l)) byLayer.set(l, []);
    byLayer.get(l)!.push(id);
  }
  for (const l of [...byLayer.keys()].sort((a, b) => a - b)) {
    const row = byLayer.get(l)!.sort();
    row.forEach((id, i) => pos.set(id, { x: H_GAP + i * (NODE_W + H_GAP), y: svcOffsetY + l * (NODE_H + V_GAP) }));
  }
  return pos;
}

export function svgDims(pos: Map<string, Pos>, nodes: TopoNode[]): { w: number; h: number } {
  const nodeById = new Map(nodes.map(n => [n.id, n]));
  let maxX = 0, maxY = 0;
  for (const [id, { x, y }] of pos.entries()) {
    const n = nodeById.get(id);
    const nw = n?.kind === 'internet' ? INET_W : NODE_W;
    const nh = n?.kind === 'internet' ? INET_H : NODE_H;
    maxX = Math.max(maxX, x + nw);
    maxY = Math.max(maxY, y + nh);
  }
  return { w: maxX + H_GAP, h: maxY + V_GAP };
}

export function edgePath(from: Pos, to: Pos): string {
  const fromMidY = from.y + NODE_H / 2;
  const toMidY = to.y + NODE_H / 2;
  if (toMidY > fromMidY + NODE_H) {
    const x1 = from.x + NODE_W / 2, y1 = from.y + NODE_H;
    const x2 = to.x + NODE_W / 2, y2 = to.y;
    const cy = (y1 + y2) / 2;
    return `M ${x1} ${y1} C ${x1} ${cy}, ${x2} ${cy}, ${x2} ${y2}`;
  }
  if (fromMidY > toMidY + NODE_H) {
    const x1 = from.x + NODE_W / 2, y1 = from.y;
    const x2 = to.x + NODE_W / 2, y2 = to.y + NODE_H;
    const cy = (y1 + y2) / 2;
    return `M ${x1} ${y1} C ${x1} ${cy}, ${x2} ${cy}, ${x2} ${y2}`;
  }
  const goRight = to.x >= from.x;
  const x1 = goRight ? from.x + NODE_W : from.x;
  const y1 = from.y + NODE_H / 2;
  const x2 = goRight ? to.x : to.x + NODE_W;
  const y2 = to.y + NODE_H / 2;
  const cx = (x1 + x2) / 2;
  return `M ${x1} ${y1} C ${cx} ${y1}, ${cx} ${y2}, ${x2} ${y2}`;
}

// Positions for per-edge labels when a client is focused.
// src/dst are placed just outside the node endpoints; mid is the curve midpoint.
type TextAnchor = 'start' | 'middle' | 'end';

export function edgeLabelPoints(from: Pos, to: Pos): {
  src: { x: number; y: number; anchor: TextAnchor };
  dst: { x: number; y: number; anchor: TextAnchor };
  mid: { x: number; y: number };
} {
  const fromMidY = from.y + NODE_H / 2;
  const toMidY = to.y + NODE_H / 2;

  if (toMidY > fromMidY + NODE_H) {
    // downward — exits bottom-center, enters top-center
    const x1 = from.x + NODE_W / 2, y1 = from.y + NODE_H;
    const x2 = to.x + NODE_W / 2,   y2 = to.y;
    return {
      src: { x: x1, y: y1 + 13, anchor: 'middle' },
      dst: { x: x2, y: y2 - 7,  anchor: 'middle' },
      mid: { x: (x1 + x2) / 2,  y: (y1 + y2) / 2 },
    };
  }
  if (fromMidY > toMidY + NODE_H) {
    // upward — exits top-center, enters bottom-center
    const x1 = from.x + NODE_W / 2, y1 = from.y;
    const x2 = to.x + NODE_W / 2,   y2 = to.y + NODE_H;
    return {
      src: { x: x1, y: y1 - 7,  anchor: 'middle' },
      dst: { x: x2, y: y2 + 13, anchor: 'middle' },
      mid: { x: (x1 + x2) / 2,  y: (y1 + y2) / 2 },
    };
  }
  // horizontal — exits left/right side
  const goRight = to.x >= from.x;
  const x1 = goRight ? from.x + NODE_W : from.x;
  const y1 = from.y + NODE_H / 2;
  const x2 = goRight ? to.x : to.x + NODE_W;
  const y2 = to.y + NODE_H / 2;
  const midX = (x1 + x2) / 2;
  const midY = (y1 + y2) / 2;
  const t = goRight ? 1 : -1;
  return {
    src: { x: x1 + t * 10, y: midY - 16, anchor: 'middle' },
    dst: { x: x2 - t * 10, y: midY - 16, anchor: 'middle' },
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
