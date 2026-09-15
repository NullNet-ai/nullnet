import { spRow, spKey, spVal, spCode } from './panelStyles';

export default function NodeSessionSummary({ kind, id, count, historical }: {
  kind: 'proxy' | 'service';
  id: string;
  count: number;
  historical: boolean;
}) {
  return <>
    <div style={spRow}>
      <div style={spKey}>Role</div>
      <span className={`badge ${kind === 'proxy' ? 'b-amber' : 'b-dim'}`}>{kind === 'proxy' ? 'Proxy' : 'Service'}</span>
    </div>
    <div style={spRow}>
      <div style={spKey}>{kind === 'proxy' ? 'IP Address' : 'Service'}</div>
      <div style={spCode}>{id}</div>
    </div>
    <div style={spRow}>
      <div style={spKey}>{historical ? 'Sessions' : 'Active Sessions'}</div>
      <div style={{ ...spVal, color: 'var(--cyan)' }}>{count}</div>
    </div>
  </>;
}
