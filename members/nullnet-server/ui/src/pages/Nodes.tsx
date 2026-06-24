import { useState } from 'react';
import Layout from '../components/Layout';
import { useApi } from '../hooks/useApi';
import { useStack } from '../StackContext';
import type { NodeJson } from '../types';
import { useDragResize } from '../hooks/useDragResize';

export default function Nodes() {
  const { stack } = useStack();
  const { data: nodes, loading } = useApi<NodeJson[]>(`/api/nodes/${stack}`, 5000);
  const [selected, setSelected] = useState<string | null>(null);
  const { width: dpWidth, onResizeStart } = useDragResize(300, 200, 560);

  const selectedNode = nodes?.find(n => n.ip === selected) ?? null;

  function select(ip: string) {
    setSelected(prev => (prev === ip ? null : ip));
  }

  return (
    <Layout
      page="nodes"
      topbarRight={
        <>
          <span style={{ fontSize: 11, color: 'var(--t2)' }}>{nodes?.length ?? 0} connected</span>
          <span className="live-row"><span className="live-dot"></span>live · 5s</span>
        </>
      }
    >
      <div className="workspace" style={{ minHeight: 'calc(100vh - 48px)' }}>
        <div className="nodes-content">
          <div className="page-title">Connected Nodes</div>
          <div className="page-sub">Click a node to inspect hosted services</div>

          {loading && <div style={{ color: 'var(--t2)', fontSize: 12 }}>Loading…</div>}

          <div className="nodes-grid">
            {nodes?.map(node => (
              <div
                key={node.ip}
                className={`node-card${selected === node.ip ? ' sel' : ''}`}
                onClick={() => select(node.ip)}
              >
                <div className="nc-head">
                  <div>
                    <div className="nc-name">
                      <span className="dot dot-g"></span>
                      {node.ip}
                      <span className="badge b-green">Online</span>
                    </div>
                    <div className="nc-ip">{node.ip}</div>
                  </div>
                  <div className="nc-meta">
                    <div>{node.hosted_services.length} service{node.hosted_services.length !== 1 ? 's' : ''}</div>
                  </div>
                </div>

                <div className="nc-stats">
                  <div className="nc-stat">
                    <div className="nc-stat-k">Services</div>
                    <div className="nc-stat-v">{node.hosted_services.length}</div>
                  </div>
                </div>

                {node.hosted_services.length > 0 && (
                  <div className="nc-svcs">
                    {node.hosted_services.map(s => (
                      <span key={`${s.stack}/${s.name}`} className="svc-tag">{s.name}</span>
                    ))}
                  </div>
                )}
              </div>
            ))}

            {!loading && nodes?.length === 0 && (
              <div style={{ color: 'var(--t2)', fontSize: 12, gridColumn: '1/-1' }}>No nodes connected</div>
            )}
          </div>
        </div>

        <div className="dp" style={{ width: dpWidth }}>
          <div className="drag-handle" onMouseDown={onResizeStart} />
          <div className="dp-head">
            <span className="dp-title" title={selectedNode?.ip}>{selectedNode ? selectedNode.ip : '–'}</span>
            {selectedNode && (
              <button className="dp-close" onClick={() => setSelected(null)}>✕</button>
            )}
          </div>

          {selectedNode ? (
            <div className="dp-body">
              <div className="dp-sec">
                <div className="dp-sec-title">Connection</div>
                <div className="dp-row">
                  <span className="dp-k">IP Address</span>
                  <span className="dp-v code">{selectedNode.ip}</span>
                </div>
                <div className="dp-row">
                  <span className="dp-k">Status</span>
                  <span className="dp-v"><span className="badge b-green">Online</span></span>
                </div>
              </div>

              <div className="dp-sec">
                <div className="dp-sec-title">Hosted Services</div>
                {selectedNode.hosted_services.length > 0 ? (
                  <table className="mini-tbl">
                    <thead>
                      <tr><th>Service</th><th>Stack</th></tr>
                    </thead>
                    <tbody>
                      {selectedNode.hosted_services.map(s => (
                        <tr key={`${s.stack}/${s.name}`}>
                          <td style={{ fontWeight: 500 }}>{s.name}</td>
                          <td style={{ fontFamily: "'JetBrains Mono',monospace", color: 'var(--t2)', fontSize: 10 }}>{s.stack}</td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                ) : (
                  <div style={{ color: 'var(--t2)', fontSize: 11 }}>No services hosted</div>
                )}
              </div>
            </div>
          ) : (
            <div className="empty-dp" style={{ flex: 1 }}>
              <span style={{ fontSize: 28, opacity: .2 }}>◉</span>
              <span style={{ fontSize: 12 }}>Select a node</span>
            </div>
          )}
        </div>
      </div>
    </Layout>
  );
}
