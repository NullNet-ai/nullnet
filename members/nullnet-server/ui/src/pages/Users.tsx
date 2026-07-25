import { useState } from 'react';
import Layout from '../components/Layout';
import Modal from '../components/Modal';
import { useApi } from '../hooks/useApi';
import { apiFetch } from '../lib/apiFetch';
import { useAuth } from '../AuthContext';
import { ALL_SCOPES } from '../types';
import type { Scope, UserJson } from '../types';

// Matches the server's minimum in http_server/auth/users.rs — kept in sync by hand.
const MIN_PASSWORD_LEN = 8;

interface IdentityFieldsProps {
  username: string;
  role: 'admin' | 'user';
  scopes: Scope[];
  onUsernameChange: (v: string) => void;
  onRoleChange: (v: 'admin' | 'user') => void;
  onToggleScope: (s: Scope) => void;
}

/// Username/role/scopes — the fields shared by the create and edit dialogs.
/// Password and MFA reset live in their own single-purpose dialogs instead of
/// being crammed into this same form, so each dialog stays a single task.
function IdentityFields({ username, role, scopes, onUsernameChange, onRoleChange, onToggleScope }: IdentityFieldsProps) {
  return (
    <>
      <label className="modal-field">
        <span>Username</span>
        <input value={username} onChange={e => onUsernameChange(e.target.value)} autoComplete="off" />
      </label>
      <label className="modal-field">
        <span>Role</span>
        <select value={role} onChange={e => onRoleChange(e.target.value as 'admin' | 'user')}>
          <option value="user">user</option>
          <option value="admin">admin</option>
        </select>
      </label>
      {role === 'user' ? (
        <div className="modal-field">
          <span>Scopes</span>
          <div className="scope-grid">
            {ALL_SCOPES.map(s => (
              <label key={s} className="scope-check">
                <input type="checkbox" checked={scopes.includes(s)} onChange={() => onToggleScope(s)} />
                {s}
              </label>
            ))}
          </div>
        </div>
      ) : (
        <div style={{ fontSize: 11, color: 'var(--t2)' }}>Admins implicitly have every scope.</div>
      )}
    </>
  );
}

interface CreateFormState {
  username: string;
  password: string;
  role: 'admin' | 'user';
  scopes: Scope[];
}

const EMPTY_CREATE_FORM: CreateFormState = { username: '', password: '', role: 'user', scopes: [] };

interface EditFormState {
  username: string;
  role: 'admin' | 'user';
  scopes: Scope[];
}

export default function Users() {
  const { user: currentUser } = useAuth();
  const { data: users, loading, refetch } = useApi<UserJson[]>('/api/auth/users', 10000);
  const [deleting, setDeleting] = useState<Set<string>>(new Set());

  const [createOpen, setCreateOpen] = useState(false);
  const [createForm, setCreateForm] = useState<CreateFormState>(EMPTY_CREATE_FORM);
  const [createBusy, setCreateBusy] = useState(false);
  const [createError, setCreateError] = useState<string | null>(null);

  const [editing, setEditing] = useState<UserJson | null>(null);
  const [editForm, setEditForm] = useState<EditFormState>({ username: '', role: 'user', scopes: [] });
  const [editBusy, setEditBusy] = useState(false);
  const [editError, setEditError] = useState<string | null>(null);

  const [pwUser, setPwUser] = useState<UserJson | null>(null);
  const [newPassword, setNewPassword] = useState('');
  const [currentPassword, setCurrentPassword] = useState('');
  const [pwBusy, setPwBusy] = useState(false);
  const [pwError, setPwError] = useState<string | null>(null);

  const [mfaUser, setMfaUser] = useState<UserJson | null>(null);
  const [mfaCode, setMfaCode] = useState('');
  const [mfaBusy, setMfaBusy] = useState(false);
  const [mfaError, setMfaError] = useState<string | null>(null);

  const list = users ?? [];

  function openCreate() {
    setCreateForm(EMPTY_CREATE_FORM);
    setCreateError(null);
    setCreateOpen(true);
  }

  function openEdit(u: UserJson) {
    setEditing(u);
    setEditForm({ username: u.username, role: u.role, scopes: u.scopes as Scope[] });
    setEditError(null);
  }

  function openPasswordDialog(u: UserJson) {
    setPwUser(u);
    setNewPassword('');
    setCurrentPassword('');
    setPwError(null);
  }

  function openMfaReset(u: UserJson) {
    setMfaUser(u);
    setMfaCode('');
    setMfaError(null);
  }

  async function submitCreate(e: React.FormEvent) {
    e.preventDefault();
    setCreateBusy(true);
    setCreateError(null);
    try {
      const res = await apiFetch('/api/auth/users', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(createForm),
      });
      const data = await res.json().catch(() => ({}));
      if (!res.ok) {
        setCreateError(data.error ?? `HTTP ${res.status}`);
        return;
      }
      setCreateOpen(false);
      refetch();
    } catch (e) {
      setCreateError(String(e));
    } finally {
      setCreateBusy(false);
    }
  }

  async function submitEdit(e: React.FormEvent) {
    e.preventDefault();
    if (!editing) return;
    setEditBusy(true);
    setEditError(null);
    try {
      const res = await apiFetch(`/api/auth/users/${editing.id}`, {
        method: 'PATCH',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(editForm),
      });
      if (!res.ok) {
        const data = await res.json().catch(() => ({}));
        setEditError(data.error ?? `HTTP ${res.status}`);
        return;
      }
      setEditing(null);
      refetch();
    } catch (e) {
      setEditError(String(e));
    } finally {
      setEditBusy(false);
    }
  }

  async function submitPasswordChange(e: React.FormEvent) {
    e.preventDefault();
    if (!pwUser) return;
    setPwBusy(true);
    setPwError(null);
    try {
      const body: Record<string, unknown> = { password: newPassword };
      if (pwUser.id === currentUser?.id) body.current_password = currentPassword;
      const res = await apiFetch(`/api/auth/users/${pwUser.id}`, {
        method: 'PATCH',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
      });
      if (!res.ok) {
        const data = await res.json().catch(() => ({}));
        setPwError(data.error ?? `HTTP ${res.status}`);
        return;
      }
      setPwUser(null);
      refetch();
    } catch (e) {
      setPwError(String(e));
    } finally {
      setPwBusy(false);
    }
  }

  async function confirmMfaReset() {
    if (!mfaUser) return;
    setMfaBusy(true);
    setMfaError(null);
    try {
      const isSelf = mfaUser.id === currentUser?.id;
      const body: Record<string, unknown> = { reset_mfa: true };
      if (isSelf) body.mfa_code = mfaCode;
      const res = await apiFetch(`/api/auth/users/${mfaUser.id}`, {
        method: 'PATCH',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
      });
      if (!res.ok) {
        const data = await res.json().catch(() => ({}));
        setMfaError(data.error ?? `HTTP ${res.status}`);
        return;
      }
      setMfaUser(null);
      refetch();
    } finally {
      setMfaBusy(false);
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

  const createValid = createForm.username.trim() !== '' && createForm.password.length >= MIN_PASSWORD_LEN;
  const editValid = editForm.username.trim() !== '';

  const pwIsSelf = pwUser !== null && pwUser.id === currentUser?.id;
  const pwValid = newPassword.length >= MIN_PASSWORD_LEN && (!pwIsSelf || currentPassword !== '');

  const mfaIsSelf = mfaUser !== null && mfaUser.id === currentUser?.id;
  const mfaValid = !mfaIsSelf || mfaCode.length === 6;

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
                  <td style={{ display: 'flex', gap: 8, flexWrap: 'wrap' }}>
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
          <IdentityFields
            username={createForm.username}
            role={createForm.role}
            scopes={createForm.scopes}
            onUsernameChange={v => setCreateForm(f => ({ ...f, username: v }))}
            onRoleChange={v => setCreateForm(f => ({ ...f, role: v }))}
            onToggleScope={s =>
              setCreateForm(f => ({
                ...f,
                scopes: f.scopes.includes(s) ? f.scopes.filter(x => x !== s) : [...f.scopes, s],
              }))
            }
          />
          <label className="modal-field">
            <span>Password — at least {MIN_PASSWORD_LEN} characters</span>
            <input
              type="password"
              value={createForm.password}
              onChange={e => setCreateForm(f => ({ ...f, password: e.target.value }))}
              autoComplete="new-password"
            />
          </label>
          {createError && <div className="modal-err">{createError}</div>}
          <div className="modal-actions">
            <button className="save-btn" disabled={createBusy || !createValid}>
              {createBusy ? 'Creating…' : 'Create'}
            </button>
          </div>
        </form>
      </Modal>

      <Modal open={editing !== null} onClose={() => setEditing(null)} title={`Edit ${editing?.username ?? ''}`}>
        <form onSubmit={submitEdit} style={{ display: 'flex', flexDirection: 'column', gap: 12 }}>
          <IdentityFields
            username={editForm.username}
            role={editForm.role}
            scopes={editForm.scopes}
            onUsernameChange={v => setEditForm(f => ({ ...f, username: v }))}
            onRoleChange={v => setEditForm(f => ({ ...f, role: v }))}
            onToggleScope={s =>
              setEditForm(f => ({
                ...f,
                scopes: f.scopes.includes(s) ? f.scopes.filter(x => x !== s) : [...f.scopes, s],
              }))
            }
          />
          {editError && <div className="modal-err">{editError}</div>}
          <div className="modal-actions">
            <button className="save-btn" disabled={editBusy || !editValid}>
              {editBusy ? 'Saving…' : 'Save'}
            </button>
            <button
              type="button"
              className="card-action"
              style={{ background: 'none', border: 'none', cursor: 'pointer' }}
              onClick={() => {
                if (!editing) return;
                setEditing(null);
                openPasswordDialog(editing);
              }}
            >
              Change password
            </button>
            {editing?.mfa_enabled && (
              <button
                type="button"
                className="card-action"
                style={{ background: 'none', border: 'none', cursor: 'pointer' }}
                onClick={() => {
                  if (!editing) return;
                  setEditing(null);
                  openMfaReset(editing);
                }}
              >
                Reset MFA
              </button>
            )}
          </div>
        </form>
      </Modal>

      <Modal open={pwUser !== null} onClose={() => setPwUser(null)} title={`Change password — ${pwUser?.username ?? ''}`}>
        <form onSubmit={submitPasswordChange} style={{ display: 'flex', flexDirection: 'column', gap: 12 }}>
          <label className="modal-field">
            <span>New password — at least {MIN_PASSWORD_LEN} characters</span>
            <input
              type="password"
              value={newPassword}
              onChange={e => setNewPassword(e.target.value)}
              autoComplete="new-password"
              autoFocus
            />
          </label>
          {pwIsSelf && (
            <label className="modal-field">
              <span>Current password</span>
              <input
                type="password"
                value={currentPassword}
                onChange={e => setCurrentPassword(e.target.value)}
                autoComplete="current-password"
              />
            </label>
          )}
          {pwError && <div className="modal-err">{pwError}</div>}
          <div className="modal-actions">
            <button className="save-btn" disabled={pwBusy || !pwValid}>
              {pwBusy ? 'Saving…' : 'Change password'}
            </button>
          </div>
        </form>
      </Modal>

      <Modal open={mfaUser !== null} onClose={() => setMfaUser(null)} title={`Reset MFA — ${mfaUser?.username ?? ''}`}>
        <div style={{ display: 'flex', flexDirection: 'column', gap: 12 }}>
          <div style={{ fontSize: 12, color: 'var(--t2)' }}>
            This will disable MFA for this account. They'll need to set it up again from scratch.
          </div>
          {mfaIsSelf && (
            <label className="modal-field">
              <span>Current MFA code</span>
              <input
                value={mfaCode}
                onChange={e => setMfaCode(e.target.value.replace(/\D/g, '').slice(0, 6))}
                inputMode="numeric"
                maxLength={6}
                autoFocus
              />
            </label>
          )}
          {mfaError && <div className="modal-err">{mfaError}</div>}
          <div className="modal-actions">
            <button className="teardown-btn" onClick={confirmMfaReset} disabled={mfaBusy || !mfaValid}>
              {mfaBusy ? 'Resetting…' : 'Reset MFA'}
            </button>
          </div>
        </div>
      </Modal>
    </Layout>
  );
}
