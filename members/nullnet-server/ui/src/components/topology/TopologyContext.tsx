import { createContext, useContext, useEffect, useMemo, useReducer, useRef, useState } from 'react';
import type { ChainJson, GraphJson, ServiceJson, SessionJson, SessionRecordJson, SessionsHistoryPage } from '../../types';
import type { LayoutMode, PanelState } from './types';
import { LAYOUT_MODES } from './types';

const LAYOUT_MODE_STORAGE_KEY = 'topology-layout-mode';
import { useApi } from '../../hooks/useApi';
import { useSessionHistory } from '../../hooks/useSessionHistory';
import type { TimeSpan } from '../TimeSpanFilter';
import { appendTimeSpan } from '../../lib/sessions';
import { sessionGraph } from './sessionGraph';

// ── Data context ──────────────────────────────────────────────────────────────

interface TopologyData {
  sessionHistory: SessionsHistoryPage | null;
  refreshSessions: () => void;
  records: SessionRecordJson[];
  range: TimeSpan | null;
  setRange: (range: TimeSpan | null) => void;
  loading: boolean;
  error: string | null;
  graph: GraphJson | null;
  services: ServiceJson[] | null;
  sessions: SessionJson[] | null;
  chains: ChainJson[] | null;
}

const TopologyDataContext = createContext<TopologyData>({
  sessionHistory: null, refreshSessions: () => {},
  records: [], range: null, setRange: () => {}, loading: true, error: null,
  graph: null,
  services: null,
  sessions: null,
  chains: null,
});

// ── UI reducer ────────────────────────────────────────────────────────────────

interface UIState {
  panel: PanelState;
  focusedClientIp: string | null;
  layoutMode: LayoutMode;
}

export type UIAction =
  | { type: 'NODE_CLICKED'; nodeId: string }
  | { type: 'EDGE_CLICKED'; fromId: string; toId: string; edgeIndices: number[] }
  | { type: 'PANEL_CLOSED' }
  | { type: 'CLIENT_FOCUSED'; ip: string }
  | { type: 'FOCUS_CLEARED' }
  | { type: 'STACK_CHANGED' }
  | { type: 'LAYOUT_MODE_CHANGED'; mode: LayoutMode };

function loadInitialLayoutMode(): LayoutMode {
  if (typeof localStorage === 'undefined') return 'layered';
  const stored = localStorage.getItem(LAYOUT_MODE_STORAGE_KEY);
  return (LAYOUT_MODES as string[]).includes(stored ?? '') ? (stored as LayoutMode) : 'layered';
}

const initialUIState: UIState = {
  panel: null,
  focusedClientIp: null,
  layoutMode: loadInitialLayoutMode(),
};

function uiReducer(state: UIState, action: UIAction): UIState {
  switch (action.type) {
    case 'NODE_CLICKED': {
      return {
        ...state,
        panel:
          state.panel?.type === 'node' && state.panel.nodeId === action.nodeId
            ? null
            : { type: 'node', nodeId: action.nodeId },
      };
    }
    case 'EDGE_CLICKED':
      return {
        ...state,
        panel:
          state.panel?.type === 'edge' &&
          state.panel.fromId === action.fromId &&
          state.panel.toId === action.toId
            ? null
            : { type: 'edge', fromId: action.fromId, toId: action.toId, edgeIndices: action.edgeIndices },
      };
    case 'PANEL_CLOSED':
      return { ...state, panel: null };
    case 'CLIENT_FOCUSED':
      return {
        ...state,
        focusedClientIp: state.focusedClientIp === action.ip ? null : action.ip,
      };
    case 'FOCUS_CLEARED':
      return { ...state, focusedClientIp: null };
    case 'STACK_CHANGED':
      return { ...state, panel: null, focusedClientIp: null };
    case 'LAYOUT_MODE_CHANGED':
      return { ...state, layoutMode: action.mode };
  }
}

// ── UI context ────────────────────────────────────────────────────────────────

interface TopologyUI extends UIState {
  selectedNodeId: string | null;
  selectedEdgeKey: string | null;
  focusedNetIds: Set<number> | null;
  focusedSessions: SessionJson[] | null;
  nodeIps: Map<string, string>;
  dispatch: React.Dispatch<UIAction>;
}


const TopologyUIContext = createContext<TopologyUI>({
  ...initialUIState,
  selectedNodeId: null,
  selectedEdgeKey: null,
  focusedNetIds: null,
  focusedSessions: null,
  nodeIps: new Map(),
  dispatch: () => {},
});

// ── Provider ──────────────────────────────────────────────────────────────────

export function TopologyProvider({
  stack,
  children,
}: {
  stack: string;
  children: React.ReactNode;
}) {
  const [range, setRange] = useState<TimeSpan | null>(null);
  const params = new URLSearchParams();
  if (!range) params.set('active', 'true');
  appendTimeSpan(params, range);
  const query = params.toString();
  const { data, loading, error, refresh: refreshSessions } = useSessionHistory(stack, query, Infinity);
  const { data: liveGraph, error: graphError } = useApi<GraphJson>(`/api/graph/${stack}`, 5000);
  const { data: services } = useApi<ServiceJson[]>(`/api/services/${stack}`, range ? undefined : 5000);
  const { data: chains } = useApi<ChainJson[]>(`/api/chains/${stack}`, range ? undefined : 5000);
  const records = useMemo(() => (data?.sessions ?? []).filter(s => !s.blocked), [data]);
  const graph = useMemo(() => {
    if (!data || !liveGraph) return null;
    return sessionGraph(liveGraph, records, range != null);
  }, [data, liveGraph, range, records]);
  const sessions = useMemo<SessionJson[]>(() => range ? [] : records
    .filter(s => s.direction === 'ingress' && s.ended_at == null)
    .map(s => ({
      id: s.id, network_id: s.net_id, client_ip: s.peer_ip, service: s.service,
      client_net: s.detail.client_net ?? '', server_net: s.detail.server_net ?? '',
      chain_depth: s.detail.chain_depth ?? 1, created_at: s.started_at,
      country_code: s.country_code, asn: s.asn, org: s.org,
    })), [records, range]);

  const [uiState, dispatch] = useReducer(uiReducer, initialUIState);

  // Persist the chosen layout mode across reloads (kept out of the reducer to
  // keep it a pure function of state+action).
  useEffect(() => {
    localStorage.setItem(LAYOUT_MODE_STORAGE_KEY, uiState.layoutMode);
  }, [uiState.layoutMode]);

  // Reset panel and focus when the active stack changes (not on initial mount).
  const selectionKey = `${stack}\0${query}`;
  const prevStackRef = useRef(selectionKey);
  useEffect(() => {
    if (prevStackRef.current !== selectionKey) {
      prevStackRef.current = selectionKey;
      dispatch({ type: 'STACK_CHANGED' });
    }
  }, [selectionKey]);

  useEffect(() => {
    if (!uiState.focusedClientIp || !sessions) return;
    if (!sessions.some(s => s.client_ip === uiState.focusedClientIp)) {
      dispatch({ type: 'FOCUS_CLEARED' });
    }
  }, [sessions, uiState.focusedClientIp]);

  const nodeIps = useMemo(() => {
    const m = new Map<string, string>();
    for (const svc of range ? [] : services ?? []) {
      if (svc.replicas.length > 0) m.set(svc.name, svc.replicas[0].ip);
    }
    return m;
  }, [services, range]);

  const focusedSessions = useMemo(
    () =>
      uiState.focusedClientIp && sessions
        ? sessions.filter(s => s.client_ip === uiState.focusedClientIp)
        : null,
    [uiState.focusedClientIp, sessions],
  );

  const focusedNetIds = useMemo<Set<number> | null>(() => {
    if (!focusedSessions) return null;
    const proxyNetIds = new Set(focusedSessions.map(s => s.network_id));
    const all = new Set(proxyNetIds);
    if (chains) {
      for (const chain of chains) {
        if (proxyNetIds.has(chain.proxy_net_id)) {
          for (const id of chain.all_net_ids) all.add(id);
        }
      }
    }
    return all;
  }, [focusedSessions, chains]);

  const selectedNodeId =
    uiState.panel?.type === 'node' ? uiState.panel.nodeId :
    null;

  const selectedEdgeKey =
    uiState.panel?.type === 'edge'
      ? `${uiState.panel.fromId}\0${uiState.panel.toId}`
      : null;

  return (
    <TopologyDataContext.Provider value={{ sessionHistory: data, refreshSessions, graph, services, sessions, chains, records, range, setRange, loading, error: error ?? graphError }}>
      <TopologyUIContext.Provider
        value={{
          ...uiState,
          selectedNodeId,
          selectedEdgeKey,
          focusedNetIds,
          focusedSessions,
          nodeIps,
          dispatch,
        }}
      >
        {children}
      </TopologyUIContext.Provider>
    </TopologyDataContext.Provider>
  );
}

// ── Hooks ─────────────────────────────────────────────────────────────────────

export function useTopologyData() {
  return useContext(TopologyDataContext);
}

export function useTopologyUI() {
  return useContext(TopologyUIContext);
}
