import type { GraphNodeJson } from '../../types';

export const NODE_W = 130;
export const NODE_H = 50;
export const H_GAP = 60;
export const V_GAP = 70;

export interface Pos { x: number; y: number }

export type PanelState =
  | null
  | { type: 'node'; nodeId: string }
  | { type: 'edge'; fromId: string; toId: string; edgeIndices: number[] };

export interface TopoServiceNode extends GraphNodeJson { kind: 'service' }
export interface TopoProxyNode { kind: 'proxy'; id: string }
export type TopoNode = TopoServiceNode | TopoProxyNode;

export interface TopoEdge {
  from: string;
  to: string;
  net_id: number;
  setup_ms: number;
  isProxyHop: boolean;
  isEgress: boolean;
  originalIndices: number[];
}

// ── Layout modes ──────────────────────────────────────────────────────────────

export type LayoutMode = 'layered' | 'matrix';

export const LAYOUT_MODES: LayoutMode[] = ['layered', 'matrix'];
