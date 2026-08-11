import { useTopologyData, useTopologyUI } from './TopologyContext';
import TopologyGraphSvg from './TopologyGraphSvg';
import TopologyMatrix from './TopologyMatrix';
import ZoomFrame from './ZoomFrame';
import LayoutModeToggle from './LayoutModeToggle';

interface Props {
  height?: number | string;
  fill?: boolean;
  anchor?: 'center' | 'top-left';
  grow?: boolean;
}

export default function TopologyGraph({ height = 520, fill, anchor, grow }: Props) {
  const { graph } = useTopologyData();
  const {
    layoutMode,
    selectedNodeId,
    selectedEdgeKey,
    focusedNetIds,
    focusedSessions,
    nodeIps,
    dispatch,
  } = useTopologyUI();

  if (!graph) return null;

  return (
    <ZoomFrame
      height={height}
      fill={fill}
      anchor={anchor}
      grow={grow}
      overlay={
        <LayoutModeToggle
          mode={layoutMode}
          onChange={mode => dispatch({ type: 'LAYOUT_MODE_CHANGED', mode })}
        />
      }
    >
      {layoutMode === 'matrix' ? (
        <TopologyMatrix
          graph={graph}
          selectedNodeId={selectedNodeId}
          selectedEdgeKey={selectedEdgeKey}
          onNodeClick={id => dispatch({ type: 'NODE_CLICKED', nodeId: id })}
          onEdgeClick={(fromId, toId, edgeIndices) =>
            dispatch({ type: 'EDGE_CLICKED', fromId, toId, edgeIndices })
          }
          onBgClick={() => dispatch({ type: 'PANEL_CLOSED' })}
        />
      ) : (
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
      )}
    </ZoomFrame>
  );
}
