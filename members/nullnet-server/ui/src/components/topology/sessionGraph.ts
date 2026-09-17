import type { GraphJson, GraphEdgeJson, GraphNodeJson, SessionRecordJson } from '../../types';

export function sessionGraph(live: GraphJson, sessions: SessionRecordJson[], historical: boolean): GraphJson {
  const nodes = new Map<string, GraphNodeJson>(live.nodes.map(n => [n.id, n]));
  const proxies = new Set(live.proxies);
  function service(id: string) {
    if (!nodes.has(id)) nodes.set(id, { id, registered: false, entry_point: false, replica_count: 0, active_replica_count: 0, paused_replica_count: 0 });
  }
  const represented = new Set<string>();
  const edges: GraphEdgeJson[] = sessions.map(s => {
    service(s.service);
    const source = !historical ? live.edges.find(e => e.net_id === s.net_id && (
      s.direction === 'ingress' ? e.via_proxy && e.to === s.service && e.from === s.peer_ip :
      s.direction === 'egress' ? e.egress && e.from === s.service : !e.egress && !e.via_proxy && e.from === s.service && e.to === s.peer_ip
    )) : undefined;
    if (source) represented.add(`${s.direction}\0${source.net_id}\0${source.from}\0${source.to}`);
    const proxy = s.detail.proxy_ip ?? source?.via_proxy ?? (source?.egress ? source.to : undefined)
      ?? (s.direction === 'ingress' ? 'Proxy not recorded' : undefined);
    if (proxy) proxies.add(proxy);
    if (s.direction === 'backend') service(s.peer_ip);
    return {
      from: s.direction === 'ingress' ? proxy ?? '' : s.service,
      to: s.direction === 'ingress' ? s.service : s.direction === 'backend' ? s.peer_ip : proxy ?? '',
      via_proxy: s.direction === 'ingress' ? proxy : undefined,
      egress: s.direction === 'egress',
      net_id: s.net_id,
      setup_ms: s.detail.setup_ms ?? source?.setup_ms ?? 0,
      session: s,
      session_count: 1,
    };
  });
  if (!historical) for (const e of live.edges) {
    const kind = e.egress ? 'egress' : e.via_proxy ? 'ingress' : 'backend';
    if (!represented.has(`${kind}\0${e.net_id}\0${e.from}\0${e.to}`)) {
      edges.push({ ...e, from: e.via_proxy ?? e.from, session_count: 0 });
    }
  }
  return { nodes: [...nodes.values()], proxies: [...proxies], edges, historical };
}
