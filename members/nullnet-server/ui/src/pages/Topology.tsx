import Layout from '../components/Layout';
import { useStack } from '../StackContext';
import { TopologyProvider, useTopologyData } from '../components/topology/TopologyContext';
import TopologyGraph from '../components/topology/TopologyGraph';
import TopologyPanel from '../components/topology/TopologyPanel';

function TopologyView() {
  const { graph } = useTopologyData();
  const { stack } = useStack();

  if (!graph) {
    return (
      <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'center', height: 'calc(100vh - 48px)', color: 'var(--t2)', fontSize: 11 }}>
        loading topology…
      </div>
    );
  }

  if (graph.nodes.length === 0) {
    return (
      <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'center', height: 'calc(100vh - 48px)', color: 'var(--t2)', fontSize: 11 }}>
        No services registered for stack <b>{stack}</b>
      </div>
    );
  }

  return (
    <>
      <TopologyGraph height="calc(100vh - 48px)" />
      <TopologyPanel />
    </>
  );
}

export default function Topology() {
  const { stack } = useStack();
  return (
    <Layout
      page="topology"
      topbarRight={
        <span style={{ fontSize: 11, color: 'var(--t1)', display: 'flex', alignItems: 'center', gap: 5 }}>
          <span className="live-dot" />live · SSE
        </span>
      }
    >
      <TopologyProvider stack={stack}>
        <TopologyView />
      </TopologyProvider>
    </Layout>
  );
}
