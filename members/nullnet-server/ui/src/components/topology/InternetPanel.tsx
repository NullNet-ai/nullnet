import { useMemo } from 'react';
import { spRow, spKey, SpSep } from './panelStyles';
import { useTopologyData, useTopologyUI } from './TopologyContext';
import { flagEmoji, countryName } from '../../geo';
import { formatTimestamp, formatTimestampFull } from '../../lib/time';

export default function InternetPanel() {
  const { sessions, chains } = useTopologyData();
  const { focusedClientIp, dispatch } = useTopologyUI();

  const chainByProxyNetId = useMemo(() => {
    const m = new Map<number, number[]>();
    for (const c of chains ?? []) m.set(c.proxy_net_id, c.all_net_ids);
    return m;
  }, [chains]);

  const sessionList = useMemo(
    () => [...(sessions ?? [])].sort((a, b) => b.created_at - a.created_at),
    [sessions],
  );

  if (sessionList.length === 0) {
    return (
      <div style={{ color: 'var(--t2)', fontSize: 11, textAlign: 'center', paddingTop: 24 }}>
        No active clients
      </div>
    );
  }

  return (
    <>
      <div style={spRow}>
        <div style={spKey}>Summary</div>
        <div style={{ fontSize: 12, color: 'var(--t0)' }}>
          <span style={{ color: 'var(--cyan)', fontFamily: "'JetBrains Mono',monospace" }}>{sessionList.length}</span>
          {' '}client{sessionList.length !== 1 ? 's' : ''}
        </div>
      </div>

      <SpSep />

      {sessionList.map(s => {
        const isFocused = focusedClientIp === s.client_ip;
        const netIds = chainByProxyNetId.get(s.network_id);
        const netIdsLabel = netIds ? `nets ${netIds.join(', ')}` : `net ${s.network_id}`;
        return (
          <div
            key={s.id}
            onClick={() => dispatch({ type: 'CLIENT_FOCUSED', ip: s.client_ip })}
            style={{
              display: 'flex', alignItems: 'center', gap: 8,
              padding: '5px 6px',
              marginBottom: 2,
              borderRadius: 5,
              border: `1px solid ${isFocused ? 'rgba(91,156,246,.45)' : 'transparent'}`,
              borderBottom: isFocused ? undefined : '1px solid var(--t3)',
              background: isFocused ? 'rgba(91,156,246,.08)' : 'transparent',
              cursor: 'pointer',
              transition: 'background .12s, border-color .12s',
            }}
          >
            <div style={{ width: 6, height: 6, borderRadius: '50%', background: isFocused ? 'var(--blue)' : 'rgba(91,156,246,.4)', flexShrink: 0 }} />
            <div style={{ flex: 1, minWidth: 0 }}>
              <div style={{ fontSize: 11, fontFamily: "'JetBrains Mono',monospace", color: isFocused ? 'var(--t0)' : 'var(--t1)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                {flagEmoji(s.country_code) && (
                  <span title={countryName(s.country_code)} style={{ marginRight: 5, cursor: 'default' }}>{flagEmoji(s.country_code)}</span>
                )}
                {s.client_ip}
              </div>
              <div style={{ fontSize: 9.5, color: 'var(--t2)' }}>
                {s.service}{s.org ? ` · ${s.org}` : ''}
              </div>
            </div>
            <div style={{ flexShrink: 0, textAlign: 'right', maxWidth: 110 }}>
              <div
                title={netIdsLabel}
                style={{
                  fontSize: 9.5, fontFamily: "'JetBrains Mono',monospace", color: 'var(--cyan)',
                  overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap',
                }}
              >
                {netIdsLabel}
              </div>
              <div style={{ fontSize: 9, color: 'var(--t2)' }} title={formatTimestampFull(s.created_at)}>
                {formatTimestamp(s.created_at)}
              </div>
            </div>
          </div>
        );
      })}
    </>
  );
}
