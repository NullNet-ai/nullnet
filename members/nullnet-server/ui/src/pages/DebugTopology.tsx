import { useCallback, useRef, useState } from 'react';
import type { GraphJson } from '../types';
import type { LayoutMode } from '../components/topology/types';
import ZoomFrame from '../components/topology/ZoomFrame';
import LayoutModeToggle from '../components/topology/LayoutModeToggle';
import TopologyGraphSvg from '../components/topology/TopologyGraphSvg';
import TopologyMatrix from '../components/topology/TopologyMatrix';

// Not linked from the sidebar — a dev tool for iterating on topology layout
// math against arbitrary GraphJson without a running backend/stack. Reachable
// at /debug/topology. Paste JSON, load a file, or start from one of the
// built-in examples; the graph renders through the exact same components the
// real Topology page uses, so nothing here can drift from production.

const EXAMPLE_MINIMAL: GraphJson = {
  nodes: [
    { id: 'gateway', registered: true, entry_point: true, replica_count: 1, active_replica_count: 1, paused_replica_count: 0 },
    { id: 'auth', registered: true, entry_point: false, replica_count: 1, active_replica_count: 1, paused_replica_count: 0 },
    { id: 'orders', registered: true, entry_point: false, replica_count: 2, active_replica_count: 2, paused_replica_count: 0 },
  ],
  edges: [
    { from: '203.0.113.4', via_proxy: '10.0.0.1', to: 'gateway', net_id: 1, setup_ms: 4 },
    { from: 'gateway', to: 'auth', net_id: 2, setup_ms: 3 },
    { from: 'gateway', to: 'orders', net_id: 3, setup_ms: 5 },
  ],
};

// Mirrors the shape that actually triggers overlapping arrows in layered
// mode: a proxy edge straight into a service two dependency-layers deep
// (upload), plus a short cycle (core <-> audit) — the two patterns worth
// stress-testing long-edge routing against.
const EXAMPLE_DENSE: GraphJson = {
  nodes: [
    { id: 'gateway', registered: true, entry_point: true, replica_count: 1, active_replica_count: 1, paused_replica_count: 0 },
    { id: 'core', registered: true, entry_point: false, replica_count: 1, active_replica_count: 1, paused_replica_count: 0 },
    { id: 'audit', registered: true, entry_point: false, replica_count: 1, active_replica_count: 1, paused_replica_count: 0 },
    { id: 'billing', registered: true, entry_point: false, replica_count: 1, active_replica_count: 1, paused_replica_count: 0 },
    { id: 'upload', registered: true, entry_point: true, replica_count: 1, active_replica_count: 1, paused_replica_count: 0 },
    { id: 'search', registered: true, entry_point: false, replica_count: 1, active_replica_count: 0, paused_replica_count: 1 },
    { id: 'idle-svc', registered: true, entry_point: false, replica_count: 1, active_replica_count: 1, paused_replica_count: 0 },
  ],
  edges: [
    { from: '203.0.113.4', via_proxy: '10.0.0.1', to: 'gateway', net_id: 1, setup_ms: 4 },
    { from: '203.0.113.9', via_proxy: '10.0.0.1', to: 'upload', net_id: 2, setup_ms: 6 },
    { from: 'gateway', to: 'core', net_id: 3, setup_ms: 5 },
    { from: 'core', to: 'audit', net_id: 4, setup_ms: 3 },
    { from: 'audit', to: 'core', net_id: 5, setup_ms: 3 },
    { from: 'core', to: 'billing', net_id: 6, setup_ms: 4 },
    { from: 'billing', to: 'upload', net_id: 7, setup_ms: 2 },
    { from: 'core', to: 'search', net_id: 8, setup_ms: 4 },
    { from: 'core', to: 'upload', net_id: 9, setup_ms: 4 },
  ],
};

function isGraphJson(v: unknown): v is GraphJson {
  return !!v && typeof v === 'object' && Array.isArray((v as GraphJson).nodes) && Array.isArray((v as GraphJson).edges);
}

const inputStyle: React.CSSProperties = {
  background: 'var(--g1)', border: '1px solid var(--gb)', color: 'var(--t0)',
  fontSize: 11, padding: '6px 10px', borderRadius: 5, cursor: 'pointer',
};

export default function DebugTopology() {
  const [raw, setRaw] = useState(() => JSON.stringify(EXAMPLE_MINIMAL, null, 2));
  const [graph, setGraph] = useState<GraphJson | null>(EXAMPLE_MINIMAL);
  const [error, setError] = useState<string | null>(null);
  const [layoutMode, setLayoutMode] = useState<LayoutMode>('layered');
  const [selectedNodeId, setSelectedNodeId] = useState<string | null>(null);
  const [selectedEdgeKey, setSelectedEdgeKey] = useState<string | null>(null);
  const [inspect, setInspect] = useState<unknown>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);

  const apply = useCallback((text: string) => {
    setRaw(text);
    try {
      const parsed = JSON.parse(text);
      if (!isGraphJson(parsed)) throw new Error('JSON must have "nodes" and "edges" arrays');
      setGraph(parsed);
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, []);

  const loadFile = (file: File) => {
    const reader = new FileReader();
    reader.onload = () => apply(String(reader.result ?? ''));
    reader.readAsText(file);
  };

  const clearSelection = () => {
    setSelectedNodeId(null);
    setSelectedEdgeKey(null);
    setInspect(null);
  };

  const onNodeClick = (id: string) => {
    setSelectedNodeId(prev => (prev === id ? null : id));
    setSelectedEdgeKey(null);
    setInspect(graph?.nodes.find(n => n.id === id) ?? { id });
  };
  const onEdgeClick = (fromId: string, toId: string, edgeIndices: number[]) => {
    const key = `${fromId}\0${toId}`;
    setSelectedEdgeKey(prev => (prev === key ? null : key));
    setSelectedNodeId(null);
    setInspect(edgeIndices.map(i => graph?.edges[i]).filter(Boolean));
  };

  return (
    <div style={{ display: 'flex', width: '100%', height: '100vh', background: 'var(--bg)', color: 'var(--t0)' }}>
      <div style={{
        width: 420, flexShrink: 0, display: 'flex', flexDirection: 'column',
        borderRight: '1px solid var(--gb)', padding: 16, gap: 10, minHeight: 0,
      }}>
        <div>
          <div style={{ fontSize: 13, fontWeight: 700 }}>Topology debug</div>
          <div style={{ fontSize: 10.5, color: 'var(--t2)', marginTop: 4, lineHeight: 1.5 }}>
            Paste a GraphJson ({'{ nodes, edges }'}), or load a file — renders through the
            real topology components, no backend involved.
          </div>
        </div>

        <div style={{ display: 'flex', gap: 6, flexWrap: 'wrap' }}>
          <button style={inputStyle} onClick={() => apply(JSON.stringify(EXAMPLE_MINIMAL, null, 2))}>minimal example</button>
          <button style={inputStyle} onClick={() => apply(JSON.stringify(EXAMPLE_DENSE, null, 2))}>long-edge example</button>
          <button style={inputStyle} onClick={() => fileInputRef.current?.click()}>load file…</button>
          <input ref={fileInputRef} type="file" accept=".json,application/json" style={{ display: 'none' }}
            onChange={e => { const f = e.target.files?.[0]; if (f) loadFile(f); e.target.value = ''; }} />
        </div>

        <textarea
          value={raw}
          onChange={e => apply(e.target.value)}
          spellCheck={false}
          style={{
            flex: 1, minHeight: 0, resize: 'none', background: 'var(--g1)', border: '1px solid var(--gb)',
            borderRadius: 5, color: 'var(--t0)', fontFamily: "'JetBrains Mono',monospace", fontSize: 11,
            padding: 10, lineHeight: 1.5,
          }}
        />

        {error && (
          <div style={{ fontSize: 11, color: '#f87171', fontFamily: "'JetBrains Mono',monospace" }}>{error}</div>
        )}

        {inspect != null && (
          <div style={{ borderTop: '1px solid var(--gb)', paddingTop: 8, maxHeight: 220, overflow: 'auto' }}>
            <div style={{ fontSize: 9, letterSpacing: '.08em', textTransform: 'uppercase', color: 'var(--t2)', marginBottom: 4 }}>
              selected
            </div>
            <pre style={{ fontSize: 10, color: 'var(--t1)', margin: 0, whiteSpace: 'pre-wrap', wordBreak: 'break-all' }}>
              {JSON.stringify(inspect, null, 2)}
            </pre>
          </div>
        )}
      </div>

      <div style={{ flex: 1, position: 'relative', minWidth: 0 }}>
        {graph && (
          <ZoomFrame
            height="100%"
            fill
            overlay={<LayoutModeToggle mode={layoutMode} onChange={setLayoutMode} />}
          >
            {layoutMode === 'matrix' ? (
              <TopologyMatrix
                graph={graph}
                selectedNodeId={selectedNodeId}
                selectedEdgeKey={selectedEdgeKey}
                onNodeClick={onNodeClick}
                onEdgeClick={onEdgeClick}
                onBgClick={clearSelection}
              />
            ) : (
              <TopologyGraphSvg
                graph={graph}
                selectedNodeId={selectedNodeId}
                selectedEdgeKey={selectedEdgeKey}
                onNodeClick={onNodeClick}
                onEdgeClick={onEdgeClick}
                onBgClick={clearSelection}
              />
            )}
          </ZoomFrame>
        )}
      </div>
    </div>
  );
}
