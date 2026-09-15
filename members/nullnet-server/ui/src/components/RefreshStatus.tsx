import { useNow } from '../hooks/useNow';

export default function RefreshStatus({ updatedAt, failed, refreshMs = 5000, paused = false, live = false }: { updatedAt: number | null; failed: boolean; refreshMs?: number | null; paused?: boolean; live?: boolean }) {
  const now = useNow();
  const stale = failed || (!paused && refreshMs != null && updatedAt != null && now * 1000 - updatedAt > refreshMs * 3);
  return <span style={{ fontSize: 11, color: stale ? 'var(--amber)' : 'var(--t2)', display: 'flex', alignItems: 'center', gap: 6 }}>
    <span className="dot" style={{ background: paused ? 'var(--t3)' : stale ? 'var(--amber)' : updatedAt == null && !live ? 'var(--t3)' : 'var(--green)' }} />
    {paused ? 'Paused' : live && !failed ? 'Live · connected' : updatedAt == null ? (failed ? 'Update failed' : 'Loading…') : `${stale ? 'Data may be outdated · last update' : 'Updated'} ${new Date(updatedAt).toLocaleTimeString()}`}
  </span>;
}
