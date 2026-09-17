import type { SessionDirection, SessionRecordJson } from '../types';
import { flagEmoji, countryName } from '../geo';

const KIND_BADGE: Record<SessionDirection, string> = { ingress: 'b-amber', egress: 'b-purple', backend: 'b-white' };

export function SessionNet({ session, stacked = false }: { session: SessionRecordJson; stacked?: boolean }) {
  return <NetId id={session.net_id} setupMs={session.detail.setup_ms} stacked={stacked} />;
}

export function NetId({ id, setupMs, stacked = false }: { id: number; setupMs?: number | null; stacked?: boolean }) {
  if (id === 0) return <span style={{ color: 'var(--t3)' }}>n/a</span>;
  return <span title={setupMs == null ? 'Setup duration not recorded' : 'Network setup duration'}>
    {id}{setupMs != null && <>{stacked ? <br /> : ' '}<span style={{ whiteSpace: 'nowrap' }}>({setupMs} ms)</span></>}
  </span>;
}

export function SessionKind({ kind }: { kind: SessionDirection }) {
  return <span className={`badge ${KIND_BADGE[kind]}`}>{kind}</span>;
}

export function SessionStatus({ session }: { session: SessionRecordJson }) {
  const active = session.ended_at == null;
  return <span style={{ fontSize: 10, color: active ? 'var(--green)' : 'var(--t2)' }}>
    <span style={{ width: 6, height: 6, borderRadius: '50%', display: 'inline-block', background: active ? 'var(--green)' : 'var(--t3)', marginRight: 5 }} />
    {active ? 'active' : 'ended'}
  </span>;
}

export function SessionPolicy({ session }: { session: SessionRecordJson }) {
  return session.direction === 'backend' ? <span style={{ color: 'var(--t3)' }}>n/a</span> :
    <span className={`badge ${session.blocked ? 'b-red' : 'b-green'}`}>{session.blocked ? 'blocked' : 'allowed'}</span>;
}

export function SessionPeer({ session: s }: { session: SessionRecordJson }) {
  return <>
    {flagEmoji(s.country_code) && <span title={countryName(s.country_code)} style={{ marginRight: 5 }}>{flagEmoji(s.country_code)}</span>}
    {s.peer_ip}
    {s.org && <div style={{ fontSize: 9, color: 'var(--t2)' }}>{s.org}</div>}
  </>;
}
