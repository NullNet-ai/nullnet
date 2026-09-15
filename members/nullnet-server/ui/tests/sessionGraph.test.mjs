import assert from 'node:assert/strict';
import { Buffer } from 'node:buffer';
import { readFile } from 'node:fs/promises';
import { test } from 'node:test';
import ts from 'typescript';

const source = await readFile(new URL('../src/components/topology/sessionGraph.ts', import.meta.url), 'utf8');
const { outputText } = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.ESNext } });
const { sessionGraph } = await import(`data:text/javascript;base64,${Buffer.from(outputText).toString('base64')}`);
const live = {
  nodes: ['api', 'idle'].map(id => ({ id, registered: false, entry_point: false, replica_count: 0, active_replica_count: 0, paused_replica_count: 0 })),
  proxies: [],
  edges: [{ from: 'api', to: 'idle', net_id: 1, setup_ms: 0 }],
};

test('an empty historical range retains configured services without live edges', () => {
  const graph = sessionGraph(live, [], true);
  assert.deepEqual(graph.nodes, live.nodes);
  assert.deepEqual(graph.edges, []);
});

test('historical sessions add removed services without dropping idle current services', () => {
  const session = { service: 'api', peer_ip: 'removed', direction: 'backend', net_id: 2, detail: {} };
  const graph = sessionGraph(live, [session], true);
  assert.deepEqual(graph.nodes.map(n => n.id), ['api', 'idle', 'removed']);
  assert.equal(graph.edges.length, 1);
  assert.equal(graph.edges[0].to, 'removed');
});
