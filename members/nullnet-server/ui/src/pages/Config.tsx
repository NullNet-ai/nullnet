import { useState } from 'react';
import Layout from '../components/Layout';
import Modal from '../components/Modal';
import { useApi } from '../hooks/useApi';
import { apiFetch } from '../lib/apiFetch';
import { useStack } from '../StackContext';
import type { ServiceConfigJson, ServiceConfigListJson } from '../types';

type MatchKind = 'none' | 'docker' | 'process';
type ProtocolKind = 'http' | 'tcp' | 'udp';
type CountryMode = 'none' | 'block' | 'allow';

// A dependency branch / trigger chain is edited as one comma-separated text
// field rather than its own repeatable list-of-inputs — still a world away
// from hand-typing TOML array-of-arrays syntax, without a custom tag-picker.
interface TriggerFormState {
  port: string;
  chain: string;
}

interface ServiceFormState {
  name: string;
  matchKind: MatchKind;
  matchValue: string;
  port: string;
  reachable: boolean;
  timeout: string;
  maxNetworks: string;
  protocol: ProtocolKind;
  listenPort: string;
  dependencies: string[];
  triggers: TriggerFormState[];
  egressMode: CountryMode;
  egressCodes: string;
  ingressMode: CountryMode;
  ingressCodes: string;
}

const EMPTY_FORM: ServiceFormState = {
  name: '',
  matchKind: 'none',
  matchValue: '',
  port: '',
  reachable: false,
  timeout: '0',
  maxNetworks: '',
  protocol: 'http',
  listenPort: '',
  dependencies: [],
  triggers: [],
  egressMode: 'none',
  egressCodes: '',
  ingressMode: 'none',
  ingressCodes: '',
};

// A stack name maps to a bare filename-turned-DB-key, so keep it to safe
// identifier chars — mirrors the server's `valid_stack_name`.
const validName = (n: string) => /^[A-Za-z0-9_-]+$/.test(n);

function listFromText(s: string): string[] {
  return s
    .split(',')
    .map(x => x.trim())
    .filter(Boolean);
}

function textFromList(list: string[] | null | undefined): string {
  return (list ?? []).join(', ');
}

function serviceToForm(s: ServiceConfigJson): ServiceFormState {
  return {
    name: s.name,
    matchKind: s.docker_container ? 'docker' : s.process_path ? 'process' : 'none',
    matchValue: s.docker_container ?? s.process_path ?? '',
    port: s.port != null ? String(s.port) : '',
    reachable: s.timeout != null,
    timeout: s.timeout != null ? String(s.timeout) : '0',
    maxNetworks: s.max_networks != null ? String(s.max_networks) : '',
    protocol: s.protocol ?? 'http',
    listenPort: s.listen_port != null ? String(s.listen_port) : '',
    dependencies: s.proxy_dependencies.map(textFromList),
    triggers: s.triggers.map(t => ({ port: String(t.port), chain: textFromList(t.chain) })),
    egressMode: s.egress_blocked_countries ? 'block' : s.egress_allowed_countries ? 'allow' : 'none',
    egressCodes: textFromList(s.egress_blocked_countries ?? s.egress_allowed_countries),
    ingressMode: s.ingress_blocked_countries ? 'block' : s.ingress_allowed_countries ? 'allow' : 'none',
    ingressCodes: textFromList(s.ingress_blocked_countries ?? s.ingress_allowed_countries),
  };
}

function formToService(f: ServiceFormState): ServiceConfigJson {
  const codes = (text: string) => listFromText(text).map(c => c.toUpperCase());
  return {
    name: f.name.trim(),
    docker_container: f.matchKind === 'docker' ? f.matchValue.trim() : null,
    process_path: f.matchKind === 'process' ? f.matchValue.trim() : null,
    port: f.port.trim() !== '' ? Number(f.port) : null,
    timeout: f.reachable ? Number(f.timeout || '0') : null,
    proxy_dependencies: f.dependencies.map(listFromText).filter(branch => branch.length > 0),
    triggers: f.triggers
      .filter(t => t.port.trim() !== '')
      .map(t => ({ port: Number(t.port), chain: listFromText(t.chain) })),
    max_networks: f.maxNetworks.trim() !== '' ? Number(f.maxNetworks) : null,
    protocol: f.protocol,
    listen_port: f.protocol !== 'http' && f.listenPort.trim() !== '' ? Number(f.listenPort) : null,
    egress_blocked_countries: f.egressMode === 'block' ? codes(f.egressCodes) : null,
    egress_allowed_countries: f.egressMode === 'allow' ? codes(f.egressCodes) : null,
    ingress_blocked_countries: f.ingressMode === 'block' ? codes(f.ingressCodes) : null,
    ingress_allowed_countries: f.ingressMode === 'allow' ? codes(f.ingressCodes) : null,
  };
}

function matchLabel(s: ServiceConfigJson): string {
  if (s.docker_container) return `docker: ${s.docker_container}`;
  if (s.process_path) return `process: ${s.process_path}`;
  return '—';
}

function protocolLabel(s: ServiceConfigJson): string {
  const proto = s.protocol ?? 'http';
  return proto === 'http' ? 'http' : `${proto} :${s.listen_port ?? '?'}`;
}

export default function Config() {
  const { stack, setStack } = useStack();
  const { data, loading, error, refetch } = useApi<ServiceConfigListJson>(`/api/service-config/${stack}`);
  const { data: stacks, refetch: refetchStacks } = useApi<string[]>('/api/stacks', 10000);
  const services = data?.services ?? [];

  const [modalOpen, setModalOpen] = useState(false);
  const [editingIndex, setEditingIndex] = useState<number | null>(null);
  const [form, setForm] = useState<ServiceFormState>(EMPTY_FORM);
  const [busy, setBusy] = useState(false);
  const [formError, setFormError] = useState<string | null>(null);
  const [deleting, setDeleting] = useState<Set<number>>(new Set());
  const [listError, setListError] = useState<string | null>(null);
  const [newName, setNewName] = useState('');
  const [creating, setCreating] = useState(false);
  const [createError, setCreateError] = useState<string | null>(null);

  const noStack = !stack.trim();
  const notFound = !noStack && !loading && !!error && error.includes('404');

  function openAdd() {
    setEditingIndex(null);
    setForm(EMPTY_FORM);
    setFormError(null);
    setModalOpen(true);
  }

  function openEdit(i: number) {
    setEditingIndex(i);
    setForm(serviceToForm(services[i]));
    setFormError(null);
    setModalOpen(true);
  }

  // Whole-list replace, like the raw-TOML config save and the route editor:
  // every add/edit/delete recomputes the full array client-side and POSTs
  // it — the server re-validates it the same way a hand-edited
  // `[[services]]` block would be.
  async function persist(nextServices: ServiceConfigJson[]): Promise<{ ok: boolean; error?: string }> {
    try {
      const res = await apiFetch(`/api/service-config/${stack}`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ services: nextServices }),
      });
      const data = await res.json().catch(() => ({ ok: res.ok, error: `HTTP ${res.status}` }));
      return { ok: res.ok && data.ok, error: data.error };
    } catch (e) {
      return { ok: false, error: String(e) };
    }
  }

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    setBusy(true);
    setFormError(null);
    const entry = formToService(form);
    const next =
      editingIndex === null ? [...services, entry] : services.map((s, i) => (i === editingIndex ? entry : s));
    const r = await persist(next);
    if (r.ok) {
      setModalOpen(false);
      refetch();
    } else {
      setFormError(r.error ?? 'unknown error');
    }
    setBusy(false);
  }

  async function remove(i: number) {
    const target = services[i];
    if (!confirm(`Delete service "${target.name}"?`)) return;
    setDeleting(prev => new Set(prev).add(i));
    setListError(null);
    const next = services.filter((_, idx) => idx !== i);
    const r = await persist(next);
    if (r.ok) refetch();
    else setListError(r.error ?? 'unknown error');
    setDeleting(prev => {
      const n = new Set(prev);
      n.delete(i);
      return n;
    });
  }

  async function createStack(name: string) {
    setCreating(true);
    setCreateError(null);
    try {
      const res = await apiFetch(`/api/service-config/${name}`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ services: [] }),
      });
      const data = await res.json().catch(() => ({ ok: res.ok, error: `HTTP ${res.status}` }));
      if (res.ok && data.ok) {
        setNewName('');
        refetchStacks();
        setStack(name);
      } else {
        setCreateError(data.error ?? `HTTP ${res.status}`);
      }
    } catch (e) {
      setCreateError(String(e));
    }
    setCreating(false);
  }

  async function removeStack() {
    if (!confirm(`Delete stack "${stack}"? Its services are torn down immediately.`)) return;
    const res = await apiFetch(`/api/service-config/${stack}`, { method: 'DELETE' });
    if (res.ok) {
      const others = (stacks ?? []).filter(s => s !== stack);
      refetchStacks();
      setStack(others[0] ?? '');
    } else {
      const body = await res.json().catch(() => ({ error: `HTTP ${res.status}` }));
      setListError(body.error ?? `HTTP ${res.status}`);
    }
  }

  function addDependencyBranch() {
    setForm(f => ({ ...f, dependencies: [...f.dependencies, ''] }));
  }
  function updateDependencyBranch(i: number, value: string) {
    setForm(f => ({ ...f, dependencies: f.dependencies.map((d, idx) => (idx === i ? value : d)) }));
  }
  function removeDependencyBranch(i: number) {
    setForm(f => ({ ...f, dependencies: f.dependencies.filter((_, idx) => idx !== i) }));
  }

  function addTrigger() {
    setForm(f => ({ ...f, triggers: [...f.triggers, { port: '', chain: '' }] }));
  }
  function updateTrigger(i: number, patch: Partial<TriggerFormState>) {
    setForm(f => ({ ...f, triggers: f.triggers.map((t, idx) => (idx === i ? { ...t, ...patch } : t)) }));
  }
  function removeTrigger(i: number) {
    setForm(f => ({ ...f, triggers: f.triggers.filter((_, idx) => idx !== i) }));
  }

  const formValid = form.name.trim() !== '' && (form.matchKind === 'none' || form.port.trim() !== '');

  const createErrorLine = createError && (
    <span className="cfg-err">
      <span className="badge b-red">Error</span>
      <span className="cfg-err-msg">{createError}</span>
    </span>
  );

  return (
    <Layout page="config">
      <div className="content">
        <div className="page-title">Configuration</div>
        <div className="page-sub">
          {noStack ? (
            'No stacks configured yet.'
          ) : (
            <>
              Services for stack{' '}
              <span style={{ fontFamily: "'JetBrains Mono',monospace", color: 'var(--t1)' }}>{stack}</span> — edits
              are validated and applied without a restart.
            </>
          )}
        </div>

        {noStack && (
          <div className="cfg-empty">
            <div style={{ color: 'var(--t2)', fontSize: 13 }}>Name a stack to create it:</div>
            <input
              className="cfg-name"
              value={newName}
              placeholder="my-stack"
              spellCheck={false}
              autoFocus
              onChange={e => setNewName(e.target.value)}
              onKeyDown={e => {
                if (e.key === 'Enter' && validName(newName)) createStack(newName);
              }}
            />
            <button
              className="save-btn"
              onClick={() => createStack(newName)}
              disabled={!validName(newName) || creating}
            >
              {creating ? 'Creating…' : 'Create stack'}
            </button>
            {createErrorLine}
          </div>
        )}

        {!noStack && loading && <div style={{ color: 'var(--t2)', fontSize: 12 }}>Loading…</div>}

        {notFound && (
          <div className="cfg-empty">
            <div style={{ color: 'var(--t2)', fontSize: 13 }}>
              Stack <span style={{ fontFamily: "'JetBrains Mono',monospace", color: 'var(--t1)' }}>{stack}</span>{' '}
              doesn't exist yet.
            </div>
            <button className="save-btn" onClick={() => createStack(stack)} disabled={creating}>
              {creating ? 'Creating…' : 'Create stack'}
            </button>
            {createErrorLine}
          </div>
        )}

        {!noStack && error && !notFound && (
          <div style={{ color: 'var(--red)', fontSize: 12 }}>Failed to load config: {error}</div>
        )}

        {!noStack && !loading && !error && (
          <>
            <div className="card">
              <div className="card-head">
                <span className="card-label">Services</span>
                <button
                  className="card-action"
                  style={{ background: 'none', border: 'none', cursor: 'pointer' }}
                  onClick={openAdd}
                >
                  + Add service
                </button>
              </div>

              {listError && (
                <div className="modal-err" style={{ margin: '0 16px 12px' }}>
                  {listError}
                </div>
              )}

              <table className="tbl">
                <thead>
                  <tr>
                    <th>Name</th>
                    <th>Match</th>
                    <th>Port</th>
                    <th>Protocol</th>
                    <th>Reachable</th>
                    <th></th>
                  </tr>
                </thead>
                <tbody>
                  {services.map((s, i) => (
                    <tr key={`${s.name}-${i}`}>
                      <td style={{ fontFamily: "'JetBrains Mono',monospace" }}>{s.name}</td>
                      <td style={{ fontSize: 12, color: 'var(--t2)' }}>{matchLabel(s)}</td>
                      <td>{s.port ?? '—'}</td>
                      <td style={{ fontSize: 12 }}>{protocolLabel(s)}</td>
                      <td>
                        {s.timeout != null ? (
                          <span className="badge b-green">timeout {s.timeout}s</span>
                        ) : (
                          <span style={{ color: 'var(--t2)', fontSize: 11 }}>backend-only</span>
                        )}
                      </td>
                      <td style={{ display: 'flex', gap: 8, flexWrap: 'wrap' }}>
                        <button className="save-btn" onClick={() => openEdit(i)}>
                          Edit
                        </button>
                        <button className="teardown-btn" onClick={() => remove(i)} disabled={deleting.has(i)}>
                          {deleting.has(i) ? '…' : 'Delete'}
                        </button>
                      </td>
                    </tr>
                  ))}
                  {services.length === 0 && (
                    <tr>
                      <td colSpan={6} style={{ color: 'var(--t2)', padding: '20px 16px' }}>
                        No services declared yet.
                      </td>
                    </tr>
                  )}
                </tbody>
              </table>
            </div>

            <div style={{ marginTop: 16 }}>
              <button className="teardown-btn" onClick={removeStack}>
                Delete stack
              </button>
            </div>
          </>
        )}
      </div>

      <Modal
        open={modalOpen}
        onClose={() => setModalOpen(false)}
        title={editingIndex === null ? 'Add service' : 'Edit service'}
      >
        <form onSubmit={submit} style={{ display: 'flex', flexDirection: 'column', gap: 12 }}>
          <label className="modal-field">
            <span>Name</span>
            <input
              value={form.name}
              onChange={e => setForm(f => ({ ...f, name: e.target.value }))}
              placeholder="color.com"
              spellCheck={false}
              autoFocus
            />
          </label>

          <label className="modal-field">
            <span>Host match</span>
            <select
              value={form.matchKind}
              onChange={e => setForm(f => ({ ...f, matchKind: e.target.value as MatchKind }))}
            >
              <option value="none">None (dependency-only placeholder)</option>
              <option value="docker">Docker container / Swarm service</option>
              <option value="process">Non-Docker process path</option>
            </select>
          </label>
          {form.matchKind !== 'none' && (
            <label className="modal-field">
              <span>{form.matchKind === 'docker' ? 'Container / service name' : 'Process exe path'}</span>
              <input
                value={form.matchValue}
                onChange={e => setForm(f => ({ ...f, matchValue: e.target.value }))}
                placeholder={form.matchKind === 'docker' ? 'my-app_color' : '/usr/local/bin/metrics-exporter'}
                spellCheck={false}
              />
            </label>
          )}
          {/* Always rendered (not just when a match key is set): a service
              can carry a `port` independent of the match key in the
              underlying TOML, and gating this field on `matchKind` would
              silently drop that value on save if such a service is ever
              edited without first re-selecting a match kind. */}
          <label className="modal-field">
            <span>Backend port{form.matchKind === 'none' ? ' (requires a host match above)' : ''}</span>
            <input
              type="number"
              value={form.port}
              onChange={e => setForm(f => ({ ...f, port: e.target.value }))}
              placeholder="8080"
            />
          </label>

          <label className="scope-check">
            <input
              type="checkbox"
              checked={form.reachable}
              onChange={e => setForm(f => ({ ...f, reachable: e.target.checked }))}
            />
            Proxy-reachable entry point
          </label>
          {form.reachable && (
            <label className="modal-field">
              <span>Idle timeout (seconds, 0 = none)</span>
              <input
                type="number"
                value={form.timeout}
                onChange={e => setForm(f => ({ ...f, timeout: e.target.value }))}
              />
            </label>
          )}
          <label className="modal-field">
            <span>Max networks (optional)</span>
            <input
              type="number"
              value={form.maxNetworks}
              onChange={e => setForm(f => ({ ...f, maxNetworks: e.target.value }))}
              placeholder="unbounded"
            />
          </label>

          <label className="modal-field">
            <span>Protocol</span>
            <select
              value={form.protocol}
              onChange={e => setForm(f => ({ ...f, protocol: e.target.value as ProtocolKind }))}
            >
              <option value="http">http (Host-header routing)</option>
              <option value="tcp">tcp</option>
              <option value="udp">udp</option>
            </select>
          </label>
          {form.protocol !== 'http' && (
            <label className="modal-field">
              <span>Listen port (external, proxy-bound)</span>
              <input
                type="number"
                value={form.listenPort}
                onChange={e => setForm(f => ({ ...f, listenPort: e.target.value }))}
                placeholder="6379"
              />
            </label>
          )}

          <div className="modal-field">
            <span>Proxy dependencies — independent branches, each a comma-separated chain</span>
            {form.dependencies.map((branch, i) => (
              <div key={i} style={{ display: 'flex', gap: 6 }}>
                <input
                  value={branch}
                  onChange={e => updateDependencyBranch(i, e.target.value)}
                  placeholder="db.example, cache.example"
                  spellCheck={false}
                />
                <button type="button" className="teardown-btn" onClick={() => removeDependencyBranch(i)}>
                  ×
                </button>
              </div>
            ))}
            <button
              type="button"
              className="card-action"
              style={{ background: 'none', border: 'none', cursor: 'pointer', alignSelf: 'start' }}
              onClick={addDependencyBranch}
            >
              + Add branch
            </button>
          </div>

          <div className="modal-field">
            <span>Backend triggers — port observed on this host → chain to bring up</span>
            {form.triggers.map((t, i) => (
              <div key={i} style={{ display: 'flex', gap: 6 }}>
                <input
                  type="number"
                  value={t.port}
                  onChange={e => updateTrigger(i, { port: e.target.value })}
                  placeholder="port"
                  style={{ width: 90 }}
                />
                <input
                  value={t.chain}
                  onChange={e => updateTrigger(i, { chain: e.target.value })}
                  placeholder="worker.example, ..."
                  spellCheck={false}
                />
                <button type="button" className="teardown-btn" onClick={() => removeTrigger(i)}>
                  ×
                </button>
              </div>
            ))}
            <button
              type="button"
              className="card-action"
              style={{ background: 'none', border: 'none', cursor: 'pointer', alignSelf: 'start' }}
              onClick={addTrigger}
            >
              + Add trigger
            </button>
          </div>

          <label className="modal-field">
            <span>Egress country policy (destination of this service's outbound traffic)</span>
            <select
              value={form.egressMode}
              onChange={e => setForm(f => ({ ...f, egressMode: e.target.value as CountryMode }))}
            >
              <option value="none">None</option>
              <option value="block">Block listed countries</option>
              <option value="allow">Allow only listed countries</option>
            </select>
          </label>
          {form.egressMode !== 'none' && (
            <label className="modal-field">
              <span>ISO country codes (comma-separated)</span>
              <input
                value={form.egressCodes}
                onChange={e => setForm(f => ({ ...f, egressCodes: e.target.value }))}
                placeholder="RU, CN"
                spellCheck={false}
              />
            </label>
          )}

          <label className="modal-field">
            <span>Ingress country policy (source of proxy clients reaching this service)</span>
            <select
              value={form.ingressMode}
              onChange={e => setForm(f => ({ ...f, ingressMode: e.target.value as CountryMode }))}
            >
              <option value="none">None</option>
              <option value="block">Block listed countries</option>
              <option value="allow">Allow only listed countries</option>
            </select>
          </label>
          {form.ingressMode !== 'none' && (
            <>
              <label className="modal-field">
                <span>ISO country codes (comma-separated)</span>
                <input
                  value={form.ingressCodes}
                  onChange={e => setForm(f => ({ ...f, ingressCodes: e.target.value }))}
                  placeholder="US, IT"
                  spellCheck={false}
                />
              </label>
              {!form.reachable && (
                <div style={{ fontSize: 11, color: 'var(--t2)', marginTop: -8 }}>
                  Requires "Proxy-reachable entry point" above — ingress policy is enforced at the proxy.
                </div>
              )}
            </>
          )}

          {formError && <div className="modal-err">{formError}</div>}
          <div className="modal-actions">
            <button className="save-btn" disabled={busy || !formValid}>
              {busy ? 'Saving…' : editingIndex === null ? 'Add' : 'Save'}
            </button>
          </div>
        </form>
      </Modal>
    </Layout>
  );
}
