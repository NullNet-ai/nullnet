import { useTopologyData, useTopologyUI } from './TopologyContext';
import TopologyGraphSvg from './TopologyGraphSvg';

export default function TopologyGraph() {
  const { graph } = useTopologyData();
  const {
    showRegistered,
    showUnregistered,
    selectedNodeId,
    selectedEdgeKey,
    focusedNetIds,
    focusedSessions,
    nodeIps,
    dispatch,
  } = useTopologyUI();

  if (!graph) return null;

  return (
    <TopologyGraphSvg
      graph={graph}
      showRegistered={showRegistered}
      showUnregistered={showUnregistered}
      selectedNodeId={selectedNodeId}
      selectedEdgeKey={selectedEdgeKey}
      focusedNetIds={focusedNetIds}
      focusedSessions={focusedSessions}
      nodeIps={nodeIps}
      onNodeClick={id => dispatch({ type: 'NODE_CLICKED', nodeId: id })}
      onEdgeClick={(fromId, toId, edgeIndices) =>
        dispatch({ type: 'EDGE_CLICKED', fromId, toId, edgeIndices })
      }
    />
  );
}
