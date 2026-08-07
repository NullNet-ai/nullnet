import { useState } from 'react';
import Layout from '../components/Layout';
import Modal from '../components/Modal';
import { useApi } from '../hooks/useApi';
import { apiFetch } from '../lib/apiFetch';
import { useStack } from '../StackContext';
import type { RouteJson, RoutesResponseJson } from '../types';

type RedirectStatus = 301 | 302 | 307 | 308;

interface RouteFormState {
  host: string;
  path: string;
  targetKind: 'service' | 'redirect';
  service: string;
  redirectTo: string;
  redirectStatus: RedirectStatus;
}

const EMPTY_FORM: RouteFormState = {
  host: '',
  path: '/',
  targetKind: 'service',
  service: '',
  redirectTo: '',
  redirectStatus: 301,
};

function routeToForm(r: RouteJson): RouteFormState {
  return {
    host: r.host,
    path: r.path,
    targetKind: r.target.kind,
    service: r.target.kind === 'service' ? r.target.service : '',
    redirectTo: r.target.kind === 'redirect' ? r.target.to : '',
    redirectStatus: r.target.kind === 'redirect' ? (r.target.status as RedirectStatus) : 301,
  };
}

function formToRoute(f: RouteFormState): RouteJson {
  return {
    host: f.host.trim(),
    path: f.path.trim() || '/',
    target:
      f.targetKind === 'service'
        ? { kind: 'service', service: f.service }
        : { kind: 'redirect', to: f.redirectTo.trim(), status: f.redirectStatus },
  };
}

function targetLabel(r: RouteJson): string {
  return r.target.kind === 'service' ? `→ ${r.target.service}` : `redirect ${r.target.status} → ${r.target.to}`;
}

export default function RoutesPage() {
  const { stack } = useStack();
  const { data, loading, error, refetch } = useApi<RoutesResponseJson>(`/api/routes/${stack}`);
  const routes = data?.routes ?? [];
  const httpServices = data?.http_services ?? [];

  const [modalOpen, setModalOpen] = useState(false);
  const [editingIndex, setEditingIndex] = useState<number | null>(null);
  const [form, setForm] = useState<RouteFormState>(EMPTY_FORM);
  const [busy, setBusy] = useState(false);
  const [formError, setFormError] = useState<string | null>(null);
  const [deleting, setDeleting] = useState<Set<number>>(new Set());
  const [listError, setListError] = useState<string | null>(null);

  function openAdd() {
    setEditingIndex(null);
    setForm({ ...EMPTY_FORM, service: httpServices[0] ?? '' });
    setFormError(null);
    setModalOpen(true);
  }

  function openEdit(i: number) {
    setEditingIndex(i);
    setForm(routeToForm(routes[i]));
    setFormError(null);
    setModalOpen(true);
  }

  // The API is a whole-list replace (like the raw-TOML config save) rather
  // than per-route CRUD endpoints, so every add/edit/delete recomputes the
  // full array client-side and POSTs it — the server re-validates it the
  // same way a hand-edited [[route]] block would be.
  async function persist(nextRoutes: RouteJson[]): Promise<{ ok: boolean; error?: string }> {
    try {
      const res = await apiFetch(`/api/routes/${stack}`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(nextRoutes),
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
    const entry = formToRoute(form);
    const next =
      editingIndex === null ? [...routes, entry] : routes.map((r, i) => (i === editingIndex ? entry : r));
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
    const target = routes[i];
    if (!confirm(`Delete route "${target.host} ${target.path}"?`)) return;
    setDeleting(prev => new Set(prev).add(i));
    setListError(null);
    const next = routes.filter((_, idx) => idx !== i);
    const r = await persist(next);
    if (r.ok) refetch();
    else setListError(r.error ?? 'unknown error');
    setDeleting(prev => {
      const n = new Set(prev);
      n.delete(i);
      return n;
    });
  }

  const formValid =
    form.host.trim() !== '' &&
    (form.targetKind === 'service' ? form.service.trim() !== '' : form.redirectTo.trim() !== '');

  return (
    <Layout page="routes">
      <div className="content">
        <div className="page-title">Routes</div>
        <div className="page-sub">
          Path-based HTTP routing and redirects for stack{' '}
          <span style={{ fontFamily: "'JetBrains Mono',monospace", color: 'var(--t1)' }}>{stack}</span> — like an
          NGINX <code>location</code> block. A host with no explicit route here keeps routing by its own name,
          unchanged.
        </div>

        <div className="card">
          <div className="card-head">
            <span className="card-label">Routes</span>
            <button
              className="card-action"
              style={{ background: 'none', border: 'none', cursor: 'pointer' }}
              onClick={openAdd}
            >
              + Add route
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
                <th>Host</th>
                <th>Path</th>
                <th>Target</th>
                <th></th>
              </tr>
            </thead>
            <tbody>
              {loading && (
                <tr>
                  <td colSpan={4} style={{ color: 'var(--t2)', padding: '20px 16px' }}>
                    Loading…
                  </td>
                </tr>
              )}
              {!loading && error && (
                <tr>
                  <td colSpan={4} style={{ color: 'var(--red)', padding: '20px 16px' }}>
                    Failed to load: {error}
                  </td>
                </tr>
              )}
              {routes.map((r, i) => (
                <tr key={`${r.host}-${r.path}-${i}`}>
                  <td style={{ fontFamily: "'JetBrains Mono',monospace" }}>{r.host}</td>
                  <td style={{ fontFamily: "'JetBrains Mono',monospace", color: 'var(--t2)' }}>{r.path}</td>
                  <td style={{ fontSize: 12 }}>{targetLabel(r)}</td>
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
              {!loading && !error && routes.length === 0 && (
                <tr>
                  <td colSpan={4} style={{ color: 'var(--t2)', padding: '20px 16px' }}>
                    No explicit routes — every http service routes by its own name.
                  </td>
                </tr>
              )}
            </tbody>
          </table>
        </div>
      </div>

      <Modal open={modalOpen} onClose={() => setModalOpen(false)} title={editingIndex === null ? 'Add route' : 'Edit route'}>
        <form onSubmit={submit} style={{ display: 'flex', flexDirection: 'column', gap: 12 }}>
          <label className="modal-field">
            <span>Host</span>
            <input
              value={form.host}
              onChange={e => setForm(f => ({ ...f, host: e.target.value }))}
              placeholder="ops.example.com"
              spellCheck={false}
              autoFocus
            />
          </label>
          <label className="modal-field">
            <span>Path prefix</span>
            <input
              value={form.path}
              onChange={e => setForm(f => ({ ...f, path: e.target.value }))}
              placeholder="/"
              spellCheck={false}
            />
          </label>
          <label className="modal-field">
            <span>Target</span>
            <select
              value={form.targetKind}
              onChange={e => setForm(f => ({ ...f, targetKind: e.target.value as 'service' | 'redirect' }))}
            >
              <option value="service">Backend service</option>
              <option value="redirect">Redirect</option>
            </select>
          </label>
          {form.targetKind === 'service' ? (
            <label className="modal-field">
              <span>Service</span>
              <select value={form.service} onChange={e => setForm(f => ({ ...f, service: e.target.value }))}>
                <option value="" disabled>
                  select a service…
                </option>
                {httpServices.map(s => (
                  <option key={s} value={s}>
                    {s}
                  </option>
                ))}
              </select>
              {httpServices.length === 0 && (
                <div style={{ fontSize: 11, color: 'var(--t2)' }}>
                  No proxy-reachable http services declared in this stack yet.
                </div>
              )}
            </label>
          ) : (
            <>
              <label className="modal-field">
                <span>Redirect to</span>
                <input
                  value={form.redirectTo}
                  onChange={e => setForm(f => ({ ...f, redirectTo: e.target.value }))}
                  placeholder="https://example.com/ or /new-path"
                  spellCheck={false}
                />
              </label>
              <label className="modal-field">
                <span>Status</span>
                <select
                  value={form.redirectStatus}
                  onChange={e => setForm(f => ({ ...f, redirectStatus: Number(e.target.value) as RedirectStatus }))}
                >
                  <option value={301}>301 Moved Permanently</option>
                  <option value={302}>302 Found</option>
                  <option value={307}>307 Temporary Redirect</option>
                  <option value={308}>308 Permanent Redirect</option>
                </select>
              </label>
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
