import { useTopologyData, useTopologyUI } from './TopologyContext';
import TopologyGraphSvg from './TopologyGraphSvg';
import ZoomFrame from './ZoomFrame';

export default function TopologyGraph() {
  const { graph } = useTopologyData();
  const {
    selectedNodeId,
    selectedEdgeKey,
    focusedNetIds,
    focusedSessions,
    nodeIps,
    dispatch,
  } = useTopologyUI();

  if (!graph) return null;

  return (
    <ZoomFrame height={520}>
      <TopologyGraphSvg
        graph={graph}
        selectedNodeId={selectedNodeId}
        selectedEdgeKey={selectedEdgeKey}
        focusedNetIds={focusedNetIds}
        focusedSessions={focusedSessions}
        nodeIps={nodeIps}
        onNodeClick={id => dispatch({ type: 'NODE_CLICKED', nodeId: id })}
        onEdgeClick={(fromId, toId, edgeIndices) =>
          dispatch({ type: 'EDGE_CLICKED', fromId, toId, edgeIndices })
        }
        onBgClick={() => dispatch({ type: 'PANEL_CLOSED' })}
      />
    </ZoomFrame>
  );
}
