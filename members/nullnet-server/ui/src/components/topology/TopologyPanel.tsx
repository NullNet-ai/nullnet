import { useTopologyData, useTopologyUI } from './TopologyContext';
import ServiceNodePanel from './ServiceNodePanel';
import ProxyNodePanel from './ProxyNodePanel';
import EdgePanel from './EdgePanel';
import InternetPanel from './InternetPanel';

export default function TopologyPanel() {
  const { graph, services } = useTopologyData();
  const { panel, dispatch } = useTopologyUI();

  if (!graph) return null;

  function getTitle(): string {
    if (!panel) return '–';
    if (panel.type === 'internet') return 'Internet Clients';
    if (panel.type === 'edge') return `${panel.fromId} → ${panel.toId}`;
    return panel.nodeId;
  }

  function renderContent() {
    if (!panel || !graph) return null;

    if (panel.type === 'internet') {
      return <InternetPanel />;
    }

    if (panel.type === 'edge') {
      const edges = panel.edgeIndices.map(i => graph.edges[i]).filter(Boolean);
      return <EdgePanel edges={edges} />;
    }

    const { nodeId } = panel;
    const graphNode = graph.nodes.find(n => n.id === nodeId);
    if (graphNode) {
      return (
        <ServiceNodePanel
          node={{ ...graphNode, kind: 'service' }}
          service={services?.find(s => s.name === nodeId)}
          onDepClick={id => dispatch({ type: 'NODE_CLICKED', nodeId: id })}
        />
      );
    }
    if (graph.edges.some(e => e.via_proxy === nodeId)) {
      return <ProxyNodePanel ip={nodeId} edges={graph.edges} />;
    }
    return null;
  }

  return (
    <div style={{
      position: 'fixed', top: 48, right: 0, bottom: 0, width: 268,
      background: 'rgba(3,5,8,.95)',
      backdropFilter: 'blur(28px)', WebkitBackdropFilter: 'blur(28px)',
      borderLeft: '1px solid var(--gb)',
      display: 'flex', flexDirection: 'column',
      transform: panel ? 'translateX(0)' : 'translateX(100%)',
      transition: 'transform .22s cubic-bezier(.4,0,.2,1)',
      zIndex: 50,
    }}>
      <div style={{ padding: '14px 18px', borderBottom: '1px solid var(--t3)', display: 'flex', alignItems: 'center', justifyContent: 'space-between', flexShrink: 0 }}>
        <span style={{
          fontSize: 13, fontWeight: 600, color: 'var(--t0)',
          overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap',
          maxWidth: 210, fontFamily: "'JetBrains Mono',monospace",
        }}>
          {getTitle()}
        </span>
        <button
          onClick={() => dispatch({ type: 'PANEL_CLOSED' })}
          style={{ background: 'none', border: 'none', color: 'var(--t2)', cursor: 'pointer', fontSize: 16, lineHeight: 1, padding: 0, flexShrink: 0 }}
        >
          ✕
        </button>
      </div>
      <div style={{ padding: '16px 18px', overflowY: 'auto', flex: 1 }}>
        {renderContent()}
      </div>
    </div>
  );
}
