import assert from 'node:assert/strict';
import { test } from 'node:test';
import React from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { edgeMidpoint, edgePath } from '../src/components/topology/layout.ts';

globalThis.React = React;
const [{ default: EdgePanel }, { default: SessionRows }, { default: TopologyGraphSvg }, { default: TopologyMatrix }] = await Promise.all([
  import('../src/components/topology/EdgePanel.tsx'),
  import('../src/components/SessionRows.tsx'),
  import('../src/components/topology/TopologyGraphSvg.tsx'),
  import('../src/components/topology/TopologyMatrix.tsx'),
]);

const node = {
  id: 'solo', registered: false, entry_point: false,
  replica_count: 0, active_replica_count: 0, paused_replica_count: 0,
};

test('the diagonal shows observed and configured self-backends', () => {
  const observed = renderToStaticMarkup(React.createElement(TopologyMatrix, {
    graph: {
      historical: true,
      proxies: [],
      nodes: [node],
      edges: [{ from: 'solo', to: 'solo', net_id: 0, setup_ms: 0, session_count: 2 }],
    },
    allowedLinks: new Set(['solo\0solo']),
    showProxies: false,
  }));
  assert.match(observed, /solo → solo \(2 sessions\)/);

  const configured = renderToStaticMarkup(React.createElement(TopologyMatrix, {
    graph: { historical: true, proxies: [], nodes: [node], edges: [] },
    allowedLinks: new Set(['solo\0solo']),
    showProxies: false,
  }));
  assert.match(configured, /solo → solo: allowed in saved configuration/);
});

test('the layered view draws a loop with an inspectable self-session', () => {
  const position = { x: 0, y: 70 };
  assert.equal(edgePath(position, position), 'M 102 70 C 102 14, 28 14, 28 70');
  assert.deepEqual(edgeMidpoint(position, position), { x: 65, y: 28 });

  const session = {
    id: 9, direction: 'backend', service: 'solo', peer_ip: 'solo',
    net_id: 4, blocked: false, started_at: 1, last_seen: 1, detail: {},
  };
  const edge = { from: 'solo', to: 'solo', net_id: 4, setup_ms: 5, session };
  const graph = { historical: true, proxies: [], nodes: [node], edges: [edge] };
  const layered = renderToStaticMarkup(React.createElement(TopologyGraphSvg, {
    graph,
    onEdgeClick: () => {},
  }));
  assert.match(layered, /M 162 70 C 162 14, 88 14, 88 70/);
  assert.match(layered, />1 session</);

  const details = renderToStaticMarkup(React.createElement(EdgePanel, { edges: [edge] }));
  assert.match(details, />backend</);
  assert.equal((details.match(/>solo</g) ?? []).length, 2);
  assert.match(details, />1 session</);

  const rows = renderToStaticMarkup(React.createElement('table', null,
    React.createElement('tbody', null,
      React.createElement(SessionRows, { sessions: [session], refresh: () => {} }),
    ),
  ));
  assert.match(rows, />backend</);
  assert.equal((rows.match(/>solo</g) ?? []).length, 2);
});
