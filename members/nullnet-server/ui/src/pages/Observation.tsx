import { useMemo, useState } from 'react';
import { useStack } from '../StackContext';
import { useAuth } from '../AuthContext';
import Layout from '../components/Layout';
import Modal from '../components/Modal';
import { useApi } from '../hooks/useApi';
import { apiFetch } from '../lib/apiFetch';
import type { GraphJson, ObservationJson, ObservationListJson, ObservationLink } from '../types';
import TopologyMatrix from '../components/topology/TopologyMatrix';
import ZoomFrame from '../components/topology/ZoomFrame';

const date = (seconds: number) => new Date(seconds * 1000).toLocaleString();

function ObservationMatrix({ observation }: { observation: ObservationJson }) {
  const graph = useMemo<GraphJson>(() => ({
    historical: true, proxies: [],
    nodes: observation.services.map(id => ({ id, registered: false, entry_point: false, replica_count: 0, active_replica_count: 0, paused_replica_count: 0 })),
    edges: observation.counts.map(e => ({ from: e.source, to: e.destination, net_id: 0, setup_ms: 0, session_count: e.count })),
  }), [observation]);
  const allowedLinks = useMemo(() => new Set(observation.saved_links.map(l => `${l.source}\0${l.destination}`)), [observation]);
  return <ZoomFrame height="calc(100vh - 350px)"><TopologyMatrix graph={graph} allowedLinks={allowedLinks} showProxies={false} /></ZoomFrame>;
}

function Changes({ title, links }: { title: string; links: ObservationLink[] }) {
  return <div style={{ margin: '16px 0' }}>
    <strong>{title} ({links.length})</strong>
    {links.length === 0 ? <div style={{ color: 'var(--t2)', marginTop: 6 }}>None</div> :
      <ul style={{ paddingLeft: 18, maxHeight: 180, overflowY: 'auto', fontFamily: "'JetBrains Mono',monospace", fontSize: 11 }}>
        {links.map(l => <li key={`${l.source}\0${l.destination}`}>{l.source} → {l.destination}</li>)}
      </ul>}
  </div>;
}

function ObservationPage({ stack }: { stack: string }) {
  const { user } = useAuth();
  const canWrite = user?.role === 'admin' || user?.scopes.includes('config:write');
  const { data, error, refetch } = useApi<ObservationListJson>(`/api/observation/${encodeURIComponent(stack)}`, 5000);
  const current = data?.stack === stack ? data : null;
  const observations = current?.observations;
  const active = observations?.find(o => o.ended_at == null);
  const latest = observations?.[0];
  const [selectedId, setSelectedId] = useState<number | null>(null);
  const selected = observations?.find(o => o.id === selectedId) ?? latest;
  const [confirmStart, setConfirmStart] = useState(false);
  const [result, setResult] = useState<ObservationJson | null>(null);
  const [busy, setBusy] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);

  async function action(path: string) {
    setBusy(true);
    setActionError(null);
    try {
      const res = await apiFetch(`/api/observation/${encodeURIComponent(stack)}/${path}`, { method: 'POST' });
      const body = await res.json();
      if (!res.ok) throw new Error(body.error ?? `HTTP ${res.status}`);
      const observation = body as ObservationJson;
      setSelectedId(observation.id);
      setConfirmStart(false);
      setResult(observation.ended_at != null && !observation.applied ? observation : null);
      await refetch();
    } catch (e) {
      setActionError(String(e));
    } finally {
      setBusy(false);
    }
  }

  return <Layout page="observation" topbarRight={active ? <span className="live-row">observing · 5s</span> : undefined}>
    <div className="content">
      <div className="page-title">Observation</div>
      <div className="page-sub">Discover backend dependencies from service-to-service sessions.</div>
      {error && <div className="modal-err" role="alert">Could not load observation: {error}</div>}
      {actionError && !confirmStart && !result && <div className="modal-err" role="alert">{actionError}</div>}
      {!current && !error && <div>Loading…</div>}
      {current && <>
        <div className="card" style={{ padding: 18, marginBottom: 18 }}>
          <label style={{ display: 'flex', gap: 10, alignItems: 'center', fontSize: 13 }}>
            <input type="checkbox" checked={!!active} disabled={busy || !canWrite || (!active && current.conflicts.length > 0)}
              onChange={() => {
                if (active) void action(`${active.id}/stop`);
                else { setActionError(null); setConfirmStart(true); }
              }} />
            Allow every service to trigger every other service ({current.trigger_count} directed backend links)
          </label>
          <div style={{ color: 'var(--t2)', fontSize: 12, marginTop: 12 }}>
            {latest ? `Last activated ${date(latest.started_at)}${latest.ended_at == null ? ' · active' : ` · stopped ${date(latest.ended_at)}`}` : 'Never activated'}
          </div>
          {current.conflicts.length > 0 && <div className="modal-err" role="alert" style={{ marginTop: 12 }}>
            Observation cannot start:
            <ul>{current.conflicts.map((conflict, i) => <li key={i}>{conflict}</li>)}</ul>
          </div>}
          <p style={{ color: 'var(--t2)', fontSize: 12, marginBottom: 0 }}>
            Counts include backend sessions observed since activation and survive session retention. Stopping freezes the counts for later review.
          </p>
        </div>
        {selected && <>
          <div style={{ display: 'flex', gap: 12, alignItems: 'center', flexWrap: 'wrap', marginBottom: 12 }}>
            <select aria-label="Observation period" value={selected.id} onChange={e => setSelectedId(Number(e.target.value))}
              style={{ background: 'var(--bg)', color: 'var(--t1)', padding: 6, border: '1px solid var(--t3)', borderRadius: 4 }}>
              {observations?.map(o => <option key={o.id} value={o.id}>
                {date(o.started_at)} — {o.ended_at == null ? 'now (active)' : date(o.ended_at)}{o.applied ? ' · applied' : ''}
              </option>)}
            </select>
            {selected.ended_at != null && <button className="save-btn" onClick={() => { setActionError(null); setResult(selected); }}>Review suggestions</button>}
            <span style={{ fontSize: 11, color: 'var(--t2)' }}>Gray border: allowed in saved config at activation · fill: observed sessions</span>
          </div>
          <ObservationMatrix observation={selected} />
        </>}
      </>}
    </div>
    <Modal open={confirmStart} onClose={() => { if (!busy) setConfirmStart(false); }} title="Start observation?">
      <p>Enable {current?.trigger_count} directed backend triggers in <strong>{stack}</strong>?</p>
      <p>The saved configuration will be read-only until observation stops. Existing proxy dependencies stay unchanged.</p>
      {actionError && <div className="modal-err" role="alert">{actionError}</div>}
      <div style={{ display: 'flex', gap: 10, marginTop: 18 }}>
        <button className="save-btn" disabled={busy} onClick={() => void action('start')}>{busy ? 'Starting…' : 'Start observation'}</button>
        <button className="teardown-btn" disabled={busy} onClick={() => setConfirmStart(false)}>Cancel</button>
      </div>
    </Modal>
    <Modal open={result != null} onClose={() => { if (!busy) setResult(null); }} title="Observation results">
      {result && <>
        <p>{date(result.started_at)} — {date(result.ended_at!)}</p>
        <p>{result.applied ? 'These backend suggestions have been applied.' : 'Observation is stopped. Apply these backend changes, or keep your saved configuration.'}</p>
        <Changes title="New backends" links={result.added} />
        <Changes title="Unused backends to remove" links={result.removed} />
        <p style={{ color: 'var(--t2)', fontSize: 12 }}>“Unused” means no backend sessions were observed during this period.</p>
        {actionError && <div className="modal-err" role="alert">{actionError}</div>}
        <div style={{ display: 'flex', gap: 10, marginTop: 18 }}>
          {!result.applied && <button className="save-btn" disabled={busy || !canWrite || !!active} onClick={() => void action(`${result.id}/apply`)}>
            {busy ? 'Applying…' : 'Apply suggested backends'}
          </button>}
          <button className="teardown-btn" disabled={busy} onClick={() => setResult(null)}>{result.applied ? 'Close' : 'Keep saved configuration'}</button>
        </div>
      </>}
    </Modal>
  </Layout>;
}

export default function Observation() {
  const { stack } = useStack();
  return <ObservationPage key={stack} stack={stack} />;
}
