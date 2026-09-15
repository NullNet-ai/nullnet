import Layout from '../components/Layout';
import TimeSpanFilter from '../components/TimeSpanFilter';
import { useStack } from '../StackContext';
import { TopologyProvider, useTopologyData } from '../components/topology/TopologyContext';
import TopologyGraph from '../components/topology/TopologyGraph';
import TopologyPanel from '../components/topology/TopologyPanel';

function TopologyView() {
  const { graph, error, loading } = useTopologyData();
  const { stack } = useStack();

  if (!graph) {
    return (
      <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'center', height: 'calc(100vh - 110px)', color: 'var(--t2)', fontSize: 11 }}>
        {error ?? (loading ? 'loading topology…' : 'Topology unavailable')}
      </div>
    );
  }

  if (graph.nodes.length === 0 && !graph.proxies?.length) {
    return (
      <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'center', height: 'calc(100vh - 110px)', color: 'var(--t2)', fontSize: 11 }}>
        No sessions or services in this view for stack <b>{stack}</b>
      </div>
    );
  }

  return (
    <>
      <TopologyGraph height="calc(100vh - 110px)" />
      <TopologyPanel />
    </>
  );
}

function TopologyPage() {
  const { range, setRange, error, sessionHistory } = useTopologyData();
  return <Layout page="topology" topbarRight={<span className="live-row">{range ? 'Time span' : 'live · 5s'}</span>}>
    <div style={{ padding: '12px 20px', display: 'flex', alignItems: 'center', flexWrap: 'wrap', gap: 12 }}>
      <TimeSpanFilter value={range} onChange={setRange} />
      {range && sessionHistory && <span style={{ color: 'var(--t2)', fontSize: 11 }}>
        {sessionHistory.sessions.length} sessions loaded
      </span>}
      {error && <span role="alert" style={{ color: 'var(--red)' }}>{error}</span>}
    </div>
    <TopologyView />
  </Layout>;
}

export default function Topology() {
  const { stack } = useStack();
  return <TopologyProvider key={stack} stack={stack}><TopologyPage /></TopologyProvider>;
}
