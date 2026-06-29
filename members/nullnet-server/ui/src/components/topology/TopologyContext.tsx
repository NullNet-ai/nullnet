import { createContext, useContext, useEffect, useMemo, useReducer, useRef } from 'react';
import type { ChainJson, GraphJson, ServiceJson, SessionJson } from '../../types';
import type { PanelState } from './types';
import { INTERNET_ID } from './types';
import { useApi } from '../../hooks/useApi';

// ── Data context ──────────────────────────────────────────────────────────────

interface TopologyData {
  graph: GraphJson | null;
  services: ServiceJson[] | null;
  sessions: SessionJson[] | null;
  chains: ChainJson[] | null;
}

const TopologyDataContext = createContext<TopologyData>({
  graph: null,
  services: null,
  sessions: null,
  chains: null,
});

// ── UI reducer ────────────────────────────────────────────────────────────────

interface UIState {
  panel: PanelState;
  focusedClientIp: string | null;
}

export type UIAction =
  | { type: 'NODE_CLICKED'; nodeId: string }
  | { type: 'EDGE_CLICKED'; fromId: string; toId: string; edgeIndices: number[] }
  | { type: 'PANEL_CLOSED' }
  | { type: 'CLIENT_FOCUSED'; ip: string }
  | { type: 'FOCUS_CLEARED' }
  | { type: 'STACK_CHANGED' };

const initialUIState: UIState = {
  panel: null,
  focusedClientIp: null,
};

function uiReducer(state: UIState, action: UIAction): UIState {
  switch (action.type) {
    case 'NODE_CLICKED': {
      if (action.nodeId === INTERNET_ID) {
        return {
          ...state,
          panel: state.panel?.type === 'internet' ? null : { type: 'internet' },
        };
      }
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
  const { data: graph, refetch } = useApi<GraphJson>(`/api/graph/${stack}`);
  const { data: services } = useApi<ServiceJson[]>(`/api/services/${stack}`, 5000);
  const { data: sessions, refetch: refetchSessions } = useApi<SessionJson[]>(`/api/sessions/${stack}`, 5000);
  const { data: chains, refetch: refetchChains } = useApi<ChainJson[]>(`/api/chains/${stack}`);

  const [uiState, dispatch] = useReducer(uiReducer, initialUIState);

  // Reset panel and focus when the active stack changes (not on initial mount).
  const prevStackRef = useRef(stack);
  useEffect(() => {
    if (prevStackRef.current !== stack) {
      prevStackRef.current = stack;
      dispatch({ type: 'STACK_CHANGED' });
    }
  }, [stack]);

  // SSE: re-fetch graph and chains whenever a session is created or torn down.
  // Also clears client focus immediately when the focused client's session tears down.
  const refetchRef = useRef(refetch);
  refetchRef.current = refetch;
  const refetchChainsRef = useRef(refetchChains);
  refetchChainsRef.current = refetchChains;
  const refetchSessionsRef = useRef(refetchSessions);
  refetchSessionsRef.current = refetchSessions;
  const focusedClientIpRef = useRef(uiState.focusedClientIp);
  focusedClientIpRef.current = uiState.focusedClientIp;
  useEffect(() => {
    const es = new EventSource('/api/events/stream');
    es.onmessage = (ev) => {
      try {
        const event = JSON.parse(ev.data);
        if (event.type === 'session_created' || event.type === 'session_torn_down') {
          refetchRef.current();
          refetchChainsRef.current();
          refetchSessionsRef.current();
        }
        if (event.type === 'session_torn_down' && event.client_ip === focusedClientIpRef.current) {
          dispatch({ type: 'FOCUS_CLEARED' });
        }
      } catch { /* ignore */ }
    };
    return () => es.close();
  }, []);

  useEffect(() => {
    if (!uiState.focusedClientIp || !sessions) return;
    if (!sessions.some(s => s.client_ip === uiState.focusedClientIp)) {
      dispatch({ type: 'FOCUS_CLEARED' });
    }
  }, [sessions, uiState.focusedClientIp]);

  const nodeIps = useMemo(() => {
    const m = new Map<string, string>();
    for (const svc of services ?? []) {
      if (svc.replicas.length > 0) m.set(svc.name, svc.replicas[0].ip);
    }
    return m;
  }, [services]);

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
    uiState.panel?.type === 'internet' ? INTERNET_ID :
    null;

  const selectedEdgeKey =
    uiState.panel?.type === 'edge'
      ? `${uiState.panel.fromId}\0${uiState.panel.toId}`
      : null;

  return (
    <TopologyDataContext.Provider value={{ graph, services, sessions, chains }}>
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
