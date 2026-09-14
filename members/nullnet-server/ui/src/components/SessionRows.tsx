import { useMemo, useState } from 'react';
import type { SessionRecordJson } from '../types';
import { useStack } from '../StackContext';
import { useNow } from '../hooks/useNow';
import { apiFetch } from '../lib/apiFetch';
import { SessionKind, SessionStatus, SessionPolicy, SessionPeer, SessionNet } from './SessionFields';
import { byEnd, duration } from '../lib/sessions';
import { formatTimestamp, formatTimestampFull } from '../lib/time';

export default function SessionRows({ sessions, refresh, stackedNet = false }: { sessions: SessionRecordJson[]; refresh: () => void; stackedNet?: boolean }) {
  const { stack } = useStack();
  const [tearing, setTearing] = useState<Set<number>>(new Set());
  const now = useNow();
  const sorted = useMemo(() => sessions.slice().sort(byEnd), [sessions]);

  async function teardown(netId: number) {
    if (!confirm(`Force teardown session ${netId}?`)) return;
    setTearing(prev => new Set(prev).add(netId));
    try {
      await apiFetch(`/api/sessions/${stack}/${netId}`, { method: 'DELETE' });
      refresh();
    } finally {
      setTearing(prev => { const next = new Set(prev); next.delete(netId); return next; });
    }
  }

  const mono = { fontFamily: "'JetBrains Mono',monospace" };
  return <>
    {sorted.map(s => {
      const active = s.ended_at == null;
      const attempts = s.detail.attempts;
      return (
        <tr key={s.id} style={active ? undefined : { opacity: 0.72 }}>
          <td style={{ whiteSpace: 'nowrap' }}>
            <SessionStatus session={s} />
          </td>
          <td style={{ fontWeight: 500, overflowWrap: 'anywhere' }} title={s.service}>
            {s.service}
          </td>
          <td>
            <SessionKind kind={s.direction} />
          </td>
          <td>
            <SessionPolicy session={s} />
          </td>
          <td style={{ ...mono, fontWeight: 500, color: 'var(--blue)' }}>
            {/* A denied ingress connection is refused before an edge
                exists, so it has no net id to show. */}
            <SessionNet session={s} stacked={stackedNet} />
          </td>
          <td style={{ ...mono, color: 'var(--t1)' }}>
            <SessionPeer session={s} />
          </td>
          <td
            style={{ ...mono, fontSize: 10, color: 'var(--t2)' }}
            title={formatTimestampFull(s.started_at)}
          >
            {formatTimestamp(s.started_at)}
          </td>
          <td
            style={{ ...mono, fontSize: 10, color: 'var(--t2)' }}
            title={s.ended_at != null ? formatTimestampFull(s.ended_at) : undefined}
          >
            {s.ended_at != null ? formatTimestamp(s.ended_at) : '—'}
          </td>
          {/* A denied connection never ran, so its elapsed time says
              nothing — how many times the peer tried does. */}
          <td
            style={{ ...mono, fontSize: 10, color: active ? 'var(--green)' : 'var(--t2)' }}
            title={
              attempts != null
                ? `Denied over ${duration(s.started_at, s.last_seen)}`
                : active ? 'Still running' : undefined
            }
          >
            {attempts != null
              ? `${attempts} attempt${attempts === 1 ? '' : 's'}`
              : duration(s.started_at, s.ended_at ?? now)}
          </td>
          <td>
            {active && s.direction === 'ingress' && (
              <button
                className="teardown-btn"
                onClick={() => teardown(s.net_id)}
                disabled={tearing.has(s.net_id)}
              >
                {tearing.has(s.net_id) ? '…' : 'Teardown'}
              </button>
            )}
          </td>
        </tr>
      );
    })}
  </>;
}
