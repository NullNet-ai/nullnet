import { useMemo, useState, type ReactNode } from 'react';
import type { SessionRecordJson } from '../types';
import { useNow } from '../hooks/useNow';
import { SessionNet, SessionPeer, SessionStatus } from './SessionFields';
import { byEnd, duration } from '../lib/sessions';
import { formatTimestamp, formatTimestampFull } from '../lib/time';

const PAGE_SIZE = 50;

export default function SessionDetails({ sessions, onFocus }: { sessions: SessionRecordJson[]; onFocus?: (ip: string) => void }) {
  const [page, setPage] = useState(0);
  const sorted = useMemo(() => sessions.slice().sort(byEnd), [sessions]);
  const lastPage = Math.max(0, Math.ceil(sorted.length / PAGE_SIZE) - 1);
  const currentPage = Math.min(page, lastPage);
  const start = currentPage * PAGE_SIZE;
  const visible = sorted.slice(start, start + PAGE_SIZE);
  const now = useNow();
  function timestamp(value: number) {
    return <span title={formatTimestampFull(value)}>{formatTimestamp(value)}</span>;
  }
  return <>
    <div style={{ color: 'var(--t2)', fontSize: 11, margin: '14px 0 8px' }}>{sessions.length} session{sessions.length === 1 ? '' : 's'}</div>
    {lastPage > 0 && <div style={{ display: 'flex', alignItems: 'center', gap: 8, flexWrap: 'wrap', marginBottom: 10, fontSize: 11 }}>
      <span style={{ color: 'var(--t2)' }}>{start + 1}–{start + visible.length} of {sessions.length}</span>
      <button className="dep-tag" disabled={currentPage === 0} onClick={() => setPage(currentPage - 1)}>Previous</button>
      <button className="dep-tag" disabled={currentPage === lastPage} onClick={() => setPage(currentPage + 1)}>Next</button>
    </div>}
    {visible.map(s => {
      const fields: [string, ReactNode][] = [
        ['Status', <SessionStatus session={s} />],
        ['Net', <SessionNet session={s} />],
        ['Started', timestamp(s.started_at)],
        ['Ended', s.ended_at == null ? '—' : timestamp(s.ended_at)],
        [s.detail.attempts != null ? 'Attempts' : 'Duration', s.detail.attempts ?? duration(s.started_at, s.ended_at ?? now)],
      ];
      if (s.direction !== 'backend') fields.splice(2, 0, ['Peer', <SessionPeer session={s} />]);
      return <div key={s.id} style={{ padding: '10px 12px', border: '1px solid var(--gb)', borderRadius: 6, marginBottom: 8, opacity: s.ended_at == null ? 1 : 0.75 }}>
        <dl style={{ margin: 0, display: 'grid', gridTemplateColumns: 'auto minmax(0, 1fr)', columnGap: 12, rowGap: 7, fontSize: 11 }}>
          {fields.map(([label, value]) => <div key={label} style={{ display: 'contents' }}>
            <dt style={{ color: 'var(--t2)' }}>{label}</dt>
            <dd style={{ margin: 0, overflowWrap: 'anywhere', color: 'var(--t1)' }}>{value}</dd>
          </div>)}
        </dl>
        {onFocus && s.direction === 'ingress' && s.ended_at == null && <button className="dep-tag" style={{ marginTop: 8 }} onClick={() => onFocus(s.peer_ip)}>Focus client</button>}
      </div>;
    })}
  </>;
}
