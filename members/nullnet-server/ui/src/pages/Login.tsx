import { useState } from 'react';
import type { FormEvent } from 'react';
import { useNavigate } from 'react-router-dom';
import { useAuth } from '../AuthContext';

type Step = { kind: 'credentials' } | { kind: 'mfa'; mfaToken: string };

export default function Login() {
  const navigate = useNavigate();
  const { refetchMe } = useAuth();
  const [username, setUsername] = useState('');
  const [password, setPassword] = useState('');
  const [code, setCode] = useState('');
  const [step, setStep] = useState<Step>({ kind: 'credentials' });
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function submitCredentials(e: FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      const res = await fetch('/api/auth/login', {
        method: 'POST',
        credentials: 'include',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ username, password }),
      });
      const data = await res.json().catch(() => ({}));
      if (!res.ok) {
        setError(data.error ?? `HTTP ${res.status}`);
        return;
      }
      if (data.mfa_required) {
        setStep({ kind: 'mfa', mfaToken: data.mfa_token });
        return;
      }
      await refetchMe();
      navigate('/', { replace: true });
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function submitCode(e: FormEvent) {
    e.preventDefault();
    if (step.kind !== 'mfa') return;
    setBusy(true);
    setError(null);
    try {
      const res = await fetch('/api/auth/mfa/verify', {
        method: 'POST',
        credentials: 'include',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ mfa_token: step.mfaToken, code }),
      });
      const data = await res.json().catch(() => ({}));
      if (!res.ok) {
        setError(data.error ?? `HTTP ${res.status}`);
        return;
      }
      await refetchMe();
      navigate('/', { replace: true });
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="login-page">
      <div className="login-card glass">
        <div className="login-logo">
          <div className="logo-name">Nullnet</div>
          <div className="logo-sub">Control Plane</div>
        </div>

        {step.kind === 'credentials' ? (
          <form onSubmit={submitCredentials} className="login-form">
            <label htmlFor="login-username">Username</label>
            <input
              id="login-username"
              value={username}
              onChange={e => setUsername(e.target.value)}
              autoFocus
              autoComplete="username"
            />
            <label htmlFor="login-password">Password</label>
            <input
              id="login-password"
              type="password"
              value={password}
              onChange={e => setPassword(e.target.value)}
              autoComplete="current-password"
            />
            {error && <div className="login-error">{error}</div>}
            <button className="save-btn login-submit" disabled={busy || !username || !password}>
              {busy ? 'Signing in…' : 'Sign in'}
            </button>
          </form>
        ) : (
          <form onSubmit={submitCode} className="login-form">
            <div className="login-sub">Enter the 6-digit code from your authenticator app.</div>
            <label htmlFor="login-code">Code</label>
            <input
              id="login-code"
              value={code}
              onChange={e => setCode(e.target.value.replace(/\D/g, '').slice(0, 6))}
              autoFocus
              inputMode="numeric"
              maxLength={6}
            />
            {error && <div className="login-error">{error}</div>}
            <button className="save-btn login-submit" disabled={busy || code.length !== 6}>
              {busy ? 'Verifying…' : 'Verify'}
            </button>
          </form>
        )}
      </div>
    </div>
  );
}
