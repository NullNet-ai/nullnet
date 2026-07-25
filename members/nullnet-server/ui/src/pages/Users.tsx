import { useState } from 'react';
import Layout from '../components/Layout';
import Modal from '../components/Modal';
import { useApi } from '../hooks/useApi';
import { apiFetch } from '../lib/apiFetch';
import { ALL_SCOPES } from '../types';
import type { Scope, UserJson } from '../types';

interface FormState {
  username: string;
  password: string;
  role: 'admin' | 'user';
  scopes: Scope[];
}

const EMPTY_FORM: FormState = { username: '', password: '', role: 'user', scopes: [] };
// Matches the server's minimum in http_server/auth/users.rs — kept in sync by hand.
const MIN_PASSWORD_LEN = 8;

export default function Users() {
  const { data: users, loading, refetch } = useApi<UserJson[]>('/api/auth/users', 10000);
  const [createOpen, setCreateOpen] = useState(false);
  const [editing, setEditing] = useState<UserJson | null>(null);
  const [form, setForm] = useState<FormState>(EMPTY_FORM);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [deleting, setDeleting] = useState<Set<string>>(new Set());

  const list = users ?? [];

  function openCreate() {
    setEditing(null);
    setForm(EMPTY_FORM);
    setError(null);
    setCreateOpen(true);
  }

  function openEdit(u: UserJson) {
    setCreateOpen(false);
    setEditing(u);
    setForm({ username: u.username, password: '', role: u.role, scopes: u.scopes as Scope[] });
    setError(null);
  }

  function toggleScope(s: Scope) {
    setForm(f => ({
      ...f,
      scopes: f.scopes.includes(s) ? f.scopes.filter(x => x !== s) : [...f.scopes, s],
    }));
  }

  async function submitCreate(e: React.FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      const res = await apiFetch('/api/auth/users', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(form),
      });
      const data = await res.json().catch(() => ({}));
      if (!res.ok) {
        setError(data.error ?? `HTTP ${res.status}`);
        return;
      }
      setCreateOpen(false);
      refetch();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function submitEdit(e: React.FormEvent) {
    e.preventDefault();
    if (!editing) return;
    setBusy(true);
    setError(null);
    try {
      const body: Record<string, unknown> = {
        username: form.username,
        role: form.role,
        scopes: form.scopes,
      };
      if (form.password) body.password = form.password;
      const res = await apiFetch(`/api/auth/users/${editing.id}`, {
        method: 'PATCH',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
      });
      if (!res.ok) {
        const data = await res.json().catch(() => ({}));
        setError(data.error ?? `HTTP ${res.status}`);
        return;
      }
      setEditing(null);
      refetch();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function resetMfa() {
    if (!editing) return;
    setBusy(true);
    setError(null);
    try {
      const res = await apiFetch(`/api/auth/users/${editing.id}`, {
        method: 'PATCH',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ reset_mfa: true }),
      });
      if (!res.ok) {
        const data = await res.json().catch(() => ({}));
        setError(data.error ?? `HTTP ${res.status}`);
        return;
      }
      refetch();
      setEditing(null);
    } finally {
      setBusy(false);
    }
  }

  async function remove(u: UserJson) {
    if (!confirm(`Delete user "${u.username}"?`)) return;
    setDeleting(prev => new Set(prev).add(u.id));
    try {
      const res = await apiFetch(`/api/auth/users/${u.id}`, { method: 'DELETE' });
      if (!res.ok) {
        const data = await res.json().catch(() => ({}));
        alert(data.error ?? `HTTP ${res.status}`);
        return;
      }
      refetch();
    } finally {
      setDeleting(prev => {
        const next = new Set(prev);
        next.delete(u.id);
        return next;
      });
    }
  }

  // A blank password when editing means "keep the current one"; otherwise it
  // must meet the same minimum length the server enforces.
  const passwordOk = editing !== null && form.password === '' ? true : form.password.length >= MIN_PASSWORD_LEN;
  const formValid = form.username.trim() !== '' && passwordOk;

  const formFields = (
    <>
      <label className="modal-field">
        <span>Username</span>
        <input value={form.username} onChange={e => setForm(f => ({ ...f, username: e.target.value }))} autoComplete="off" />
      </label>
      <label className="modal-field">
        <span>
          {editing ? 'New password (leave blank to keep current)' : 'Password'} — at least {MIN_PASSWORD_LEN} characters
        </span>
        <input
          type="password"
          value={form.password}
          onChange={e => setForm(f => ({ ...f, password: e.target.value }))}
          autoComplete="new-password"
        />
      </label>
      <label className="modal-field">
        <span>Role</span>
        <select value={form.role} onChange={e => setForm(f => ({ ...f, role: e.target.value as 'admin' | 'user' }))}>
          <option value="user">user</option>
          <option value="admin">admin</option>
        </select>
      </label>
      {form.role === 'user' ? (
        <div className="modal-field">
          <span>Scopes</span>
          <div className="scope-grid">
            {ALL_SCOPES.map(s => (
              <label key={s} className="scope-check">
                <input type="checkbox" checked={form.scopes.includes(s)} onChange={() => toggleScope(s)} />
                {s}
              </label>
            ))}
          </div>
        </div>
      ) : (
        <div style={{ fontSize: 11, color: 'var(--t2)' }}>Admins implicitly have every scope.</div>
      )}
      {error && <div className="modal-err">{error}</div>}
    </>
  );

  return (
    <Layout page="users">
      <div className="content">
        <div className="hero-row">
          <span className="hero-num">{list.length}</span>
          <span className="hero-label">user accounts</span>
        </div>

        <div className="card">
          <div className="card-head">
            <span className="card-label">Users</span>
            <button
              className="card-action"
              style={{ background: 'none', border: 'none', cursor: 'pointer' }}
              onClick={openCreate}
            >
              + Add user
            </button>
          </div>
          <table className="tbl">
            <thead>
              <tr>
                <th>Username</th>
                <th>Role</th>
                <th>Scopes</th>
                <th>MFA</th>
                <th></th>
              </tr>
            </thead>
            <tbody>
              {loading && (
                <tr>
                  <td colSpan={5} style={{ color: 'var(--t2)', padding: '20px 16px' }}>
                    Loading…
                  </td>
                </tr>
              )}
              {list.map(u => (
                <tr key={u.id}>
                  <td style={{ fontWeight: 500, fontFamily: "'JetBrains Mono',monospace" }}>{u.username}</td>
                  <td>
                    <span className={'badge ' + (u.role === 'admin' ? 'b-purple' : 'b-blue')}>{u.role}</span>
                  </td>
                  <td style={{ fontSize: 11, color: 'var(--t2)' }}>
                    {u.role === 'admin' ? 'all' : u.scopes.length > 0 ? u.scopes.join(', ') : '—'}
                  </td>
                  <td>
                    <span className={'badge ' + (u.mfa_enabled ? 'b-green' : 'b-dim')}>
                      {u.mfa_enabled ? 'enabled' : 'off'}
                    </span>
                  </td>
                  <td style={{ display: 'flex', gap: 8 }}>
                    <button className="save-btn" onClick={() => openEdit(u)}>
                      Edit
                    </button>
                    <button className="teardown-btn" onClick={() => remove(u)} disabled={deleting.has(u.id)}>
                      {deleting.has(u.id) ? '…' : 'Delete'}
                    </button>
                  </td>
                </tr>
              ))}
              {!loading && list.length === 0 && (
                <tr>
                  <td colSpan={5} style={{ color: 'var(--t2)', padding: '20px 16px' }}>
                    No users
                  </td>
                </tr>
              )}
            </tbody>
          </table>
        </div>
      </div>

      <Modal open={createOpen} onClose={() => setCreateOpen(false)} title="Add user">
        <form onSubmit={submitCreate} style={{ display: 'flex', flexDirection: 'column', gap: 12 }}>
          {formFields}
          <div className="modal-actions">
            <button className="save-btn" disabled={busy || !formValid}>
              {busy ? 'Creating…' : 'Create'}
            </button>
          </div>
        </form>
      </Modal>

      <Modal open={editing !== null} onClose={() => setEditing(null)} title={`Edit ${editing?.username ?? ''}`}>
        <form onSubmit={submitEdit} style={{ display: 'flex', flexDirection: 'column', gap: 12 }}>
          {formFields}
          <div className="modal-actions">
            <button className="save-btn" disabled={busy || !formValid}>
              {busy ? 'Saving…' : 'Save'}
            </button>
            {editing?.mfa_enabled && (
              <button type="button" className="teardown-btn" onClick={resetMfa} disabled={busy}>
                Reset MFA
              </button>
            )}
          </div>
        </form>
      </Modal>
    </Layout>
  );
}
