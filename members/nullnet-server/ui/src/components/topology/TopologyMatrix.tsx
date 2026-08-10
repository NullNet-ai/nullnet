import { useState } from 'react';
import type { GraphJson } from '../../types';
import type { TopoNode, TopoEdge } from './types';
import { buildTopoGraph } from './layout';

interface Props {
  graph: GraphJson;
  selectedNodeId?: string | null;
  selectedEdgeKey?: string | null;
  onNodeClick?: (id: string) => void;
  onEdgeClick?: (fromId: string, toId: string, edgeIndices: number[]) => void;
  onBgClick?: () => void;
}

const CELL = 26;
const LABEL_COL_W = 150;
const HEADER_ROW_H = 150;

function shortLabel(id: string): string {
  return id.length > 18 ? `${id.slice(0, 17)}…` : id;
}

function kindColor(n: TopoNode): string {
  if (n.kind === 'proxy') return 'rgba(251,191,36,.85)';
  return n.kind === 'service' && n.registered ? 'rgba(52,211,153,.85)' : 'rgba(248,113,113,.7)';
}

function edgeColor(e: TopoEdge): string {
  return e.isEgress ? 'rgba(167,139,250,.75)' : e.isProxyHop ? 'rgba(251,191,36,.7)' : 'rgba(91,156,246,.75)';
}

// Adjacency-matrix view of the same graph the other layouts draw as a
// node-link diagram: rows/cols ordered proxies-then-services (internet
// omitted — every proxy trivially connects to it, an uninformative row/col),
// filled cell = row → col has an edge. Zero edge crossings by construction,
// at the cost of not reading as a path the way the node-link views do.
export default function TopologyMatrix({
  graph, selectedNodeId = null, selectedEdgeKey = null, onNodeClick, onEdgeClick, onBgClick,
}: Props) {
  const { nodes, edges } = buildTopoGraph(graph);
  const order = nodes
    .filter(n => n.kind !== 'internet')
    .sort((a, b) => (a.kind === b.kind ? a.id.localeCompare(b.id) : a.kind === 'proxy' ? -1 : 1));
  const indexOf = new Map(order.map((n, i) => [n.id, i]));

  const edgeByPair = new Map<string, TopoEdge>();
  for (const e of edges) {
    if (e.isInternetEdge) continue;
    edgeByPair.set(`${e.from}\0${e.to}`, e);
  }

  // The row/column crosshair tracks two INDEPENDENT indices — hovering a cell
  // highlights its own row and column (which are different, off-diagonal
  // indices), while hovering a row/column label or selecting a node
  // highlights that single node's row and column (necessarily the same
  // index, so the crosshair sits on the diagonal in that case only).
  const [hoveredCell, setHoveredCell] = useState<{ rowId: string; colId: string } | null>(null);
  let hi = -1, hj = -1;
  if (hoveredCell) {
    hi = indexOf.get(hoveredCell.rowId) ?? -1;
    hj = indexOf.get(hoveredCell.colId) ?? -1;
  } else if (selectedNodeId) {
    hi = hj = indexOf.get(selectedNodeId) ?? -1;
  } else if (selectedEdgeKey) {
    const e = edgeByPair.get(selectedEdgeKey);
    if (e) { hi = indexOf.get(e.from) ?? -1; hj = indexOf.get(e.to) ?? -1; }
  }
  const hasHighlight = hi >= 0 || hj >= 0;

  const w = LABEL_COL_W + order.length * CELL;
  const h = HEADER_ROW_H + order.length * CELL;

  return (
    <svg viewBox={`0 0 ${w} ${h}`} xmlns="http://www.w3.org/2000/svg"
      style={{ width: '100%', display: 'block', fontFamily: "'Plus Jakarta Sans',sans-serif" }}>
      {onBgClick && <rect x={0} y={0} width={w} height={h} fill="transparent" onClick={onBgClick} />}

      {order.map((_, i) => (
        <line key={`h${i}`} x1={LABEL_COL_W} y1={HEADER_ROW_H + i * CELL} x2={w} y2={HEADER_ROW_H + i * CELL}
          stroke="rgba(255,255,255,.06)" />
      ))}
      {order.map((_, j) => (
        <line key={`v${j}`} x1={LABEL_COL_W + j * CELL} y1={0} x2={LABEL_COL_W + j * CELL} y2={h}
          stroke="rgba(255,255,255,.06)" />
      ))}

      {hi >= 0 && (
        <rect x={0} y={HEADER_ROW_H + hi * CELL} width={w} height={CELL}
          fill="rgba(91,156,246,.06)" pointerEvents="none" />
      )}
      {hj >= 0 && (
        <rect x={LABEL_COL_W + hj * CELL} y={0} width={CELL} height={h}
          fill="rgba(91,156,246,.06)" pointerEvents="none" />
      )}

      {/* Every (row, col) position is hoverable — not just the ones with an
          edge — so pointing anywhere in the grid tells you which row/column
          it belongs to. Diagonal cells stay content-free (no self-edges). */}
      {order.map((rowNode, i) => order.map((colNode, j) => {
        if (i === j) return null;
        const key = `${rowNode.id}\0${colNode.id}`;
        const e = edgeByPair.get(key);
        const isSel = selectedEdgeKey === key;
        const dimmed = hasHighlight && i !== hi && j !== hj;
        const x = LABEL_COL_W + j * CELL, y = HEADER_ROW_H + i * CELL;
        const count = e?.originalIndices.length ?? 0;
        return (
          <g key={key}
            onMouseEnter={() => setHoveredCell({ rowId: rowNode.id, colId: colNode.id })}
            onMouseLeave={() => setHoveredCell(null)}
            onClick={e && onEdgeClick ? (ev: React.MouseEvent) => { ev.stopPropagation(); onEdgeClick(e.from, e.to, e.originalIndices); } : undefined}
            style={{ cursor: e && onEdgeClick ? 'pointer' : 'default', opacity: dimmed ? 0.15 : 1 }}>
            <rect x={x} y={y} width={CELL} height={CELL} fill="transparent" />
            {e && (
              <>
                <title>{`${e.from} → ${e.to}${count > 1 ? ` (${count} sessions)` : ''}`}</title>
                <rect x={x + 2} y={y + 2} width={CELL - 4} height={CELL - 4} rx="3"
                  fill={edgeColor(e)} opacity={isSel ? 1 : 0.7}
                  stroke={isSel ? 'rgba(91,156,246,.9)' : 'none'} strokeWidth={isSel ? 1.5 : 0} />
                {count > 1 && (
                  <text x={x + CELL / 2} y={y + CELL / 2 + 3} textAnchor="middle"
                    fill="rgba(3,5,8,.85)" fontSize="8" fontWeight="700" pointerEvents="none">
                    {count}
                  </text>
                )}
              </>
            )}
          </g>
        );
      }))}

      {order.map((n, i) => (
        <g key={`r${n.id}`}
          onClick={onNodeClick ? (ev: React.MouseEvent) => { ev.stopPropagation(); onNodeClick(n.id); } : undefined}
          onMouseEnter={() => setHoveredCell({ rowId: n.id, colId: n.id })} onMouseLeave={() => setHoveredCell(null)}
          style={{ cursor: onNodeClick ? 'pointer' : 'default' }}>
          <rect x={0} y={HEADER_ROW_H + i * CELL} width={LABEL_COL_W} height={CELL} fill="transparent" />
          <circle cx={10} cy={HEADER_ROW_H + i * CELL + CELL / 2} r="3" fill={kindColor(n)} />
          <text x={20} y={HEADER_ROW_H + i * CELL + CELL / 2 + 3}
            fill={n.id === selectedNodeId ? 'rgba(91,156,246,.95)' : 'rgba(255,255,255,.7)'}
            fontSize="9" fontFamily="'JetBrains Mono',monospace">
            {shortLabel(n.id)}
          </text>
        </g>
      ))}

      {order.map((n, j) => {
        const x = LABEL_COL_W + j * CELL + CELL / 2;
        const y = HEADER_ROW_H - 8;
        return (
          <g key={`c${n.id}`}
            onClick={onNodeClick ? (ev: React.MouseEvent) => { ev.stopPropagation(); onNodeClick(n.id); } : undefined}
            onMouseEnter={() => setHoveredCell({ rowId: n.id, colId: n.id })} onMouseLeave={() => setHoveredCell(null)}
            style={{ cursor: onNodeClick ? 'pointer' : 'default' }}>
            <rect x={x - CELL / 2} y={0} width={CELL} height={HEADER_ROW_H} fill="transparent" />
            <g transform={`rotate(-45, ${x}, ${y})`}>
              <circle cx={x} cy={y} r="3" fill={kindColor(n)} />
              <text x={x + 6} y={y + 3}
                fill={n.id === selectedNodeId ? 'rgba(91,156,246,.95)' : 'rgba(255,255,255,.7)'}
                fontSize="9" fontFamily="'JetBrains Mono',monospace">
                {shortLabel(n.id)}
              </text>
            </g>
          </g>
        );
      })}
    </svg>
  );
}
