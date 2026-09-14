import type { ReactNode } from 'react';
import type { SessionRecordJson } from '../types';
import { SessionNet, SessionPeer, SessionStatus } from './SessionFields';
import { byEnd, duration } from '../lib/sessions';
import { formatTimestamp, formatTimestampFull } from '../lib/time';

export default function SessionDetails({ sessions, onFocus }: { sessions: SessionRecordJson[]; onFocus?: (ip: string) => void }) {
  const now = Math.floor(Date.now() / 1000);
  function timestamp(value: number) {
    return <span title={formatTimestampFull(value)}>{formatTimestamp(value)}</span>;
  }
  return <>
    <div style={{ color: 'var(--t2)', fontSize: 11, margin: '14px 0 8px' }}>{sessions.length} session{sessions.length === 1 ? '' : 's'}</div>
    {sessions.slice().sort(byEnd).map(s => {
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
