import Layout from '../components/Layout';
import { useApi } from '../hooks/useApi';
import type { PoolJson } from '../types';

export default function Pool() {
  const { data: pool, loading } = useApi<PoolJson>('/api/pool', 5000);

  const pct = pool ? (pool.in_use / pool.total) * 100 : 0;
  const pctStr = pct.toFixed(1);
  const warn = pct >= 80;

  return (
    <Layout
      page="pool"
      topbarRight={
        <span className="live-row"><span className="live-dot"></span>live · 5s</span>
      }
    >
      <div className="content">
        <div className="page-title">Network ID Pool</div>
        <div className="page-sub">Network ID allocation status</div>

        {loading && <div style={{ color: 'var(--t2)', fontSize: 12 }}>Loading…</div>}

        {pool && (
          <div className="pool-grid">
            <div className="pool-card">
              <div className="pc-label">ID Pool</div>
              <div className="pc-row">
                <div>
                  <span className="pc-num" style={{ color: warn ? 'var(--amber)' : 'var(--blue)' }}>
                    {pool.in_use.toLocaleString()}
                  </span>
                  <span className="pc-total"> / {pool.total.toLocaleString()}</span>
                </div>
                <span className="pc-pct" style={{ color: warn ? 'var(--amber)' : undefined }}>{pctStr}%</span>
              </div>
              <div className="track">
                <div
                  className="fill"
                  style={{
                    width: `${pct}%`,
                    background: warn ? 'var(--amber)' : 'var(--blue)',
                  }}
                />
              </div>
              <div className="pc-foot">
                {pool.free.toLocaleString()} free
                {warn && <span style={{ color: 'var(--amber)', marginLeft: 8 }}>⚠ above 80% threshold</span>}
              </div>
            </div>

            <div className="pool-card">
              <div className="pc-label">Breakdown</div>
              <table className="sub-tbl" style={{ width: '100%' }}>
                <tbody>
                  <tr>
                    <td style={{ color: 'var(--t2)' }}>Total capacity</td>
                    <td style={{ fontFamily: "'JetBrains Mono',monospace", textAlign: 'right' }}>
                      {pool.total.toLocaleString()}
                    </td>
                  </tr>
                  <tr>
                    <td style={{ color: 'var(--t2)' }}>In use</td>
                    <td style={{ fontFamily: "'JetBrains Mono',monospace", color: 'var(--blue)', textAlign: 'right' }}>
                      {pool.in_use.toLocaleString()}
                    </td>
                  </tr>
                  <tr>
                    <td style={{ color: 'var(--t2)' }}>Free</td>
                    <td style={{ fontFamily: "'JetBrains Mono',monospace", color: 'var(--green)', textAlign: 'right' }}>
                      {pool.free.toLocaleString()}
                    </td>
                  </tr>
                  <tr>
                    <td style={{ color: 'var(--t2)' }}>Utilization</td>
                    <td style={{ fontFamily: "'JetBrains Mono',monospace", color: warn ? 'var(--amber)' : 'var(--t1)', textAlign: 'right' }}>
                      {pctStr}%
                    </td>
                  </tr>
                </tbody>
              </table>
            </div>
          </div>
        )}
      </div>
    </Layout>
  );
}
