import type { GraphNodeJson } from '../../types';

export const NODE_W = 130;
export const NODE_H = 50;
export const H_GAP = 60;
export const V_GAP = 70;

export const INET_W = 140;
export const INET_H = 22;
export const INET_Y = 35;          // top y of internet node
export const INET_PROXY_GAP = 50;  // gap between internet bottom and proxy top

export const INTERNET_ID = 'internet';

export interface Pos { x: number; y: number }

export type PanelState =
  | null
  | { type: 'node'; nodeId: string }
  | { type: 'edge'; fromId: string; toId: string; edgeIndices: number[] }
  | { type: 'internet' };

export interface TopoServiceNode extends GraphNodeJson { kind: 'service' }
export interface TopoProxyNode { kind: 'proxy'; id: string }
export interface TopoInternetNode { kind: 'internet'; id: string }
export type TopoNode = TopoServiceNode | TopoProxyNode | TopoInternetNode;

export interface TopoEdge {
  from: string;
  to: string;
  net_id: number;
  setup_ms: number;
  isProxyHop: boolean;
  isInternetEdge: boolean;
  originalIndices: number[];
}
