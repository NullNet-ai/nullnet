import { useEffect, useState } from 'react';
import { QRCodeSVG } from 'qrcode.react';
import Modal from './Modal';
import { apiFetch } from '../lib/apiFetch';
import { useAuth } from '../AuthContext';

interface Props {
  open: boolean;
  onClose: () => void;
}

type Step =
  | { kind: 'loading' }
  | { kind: 'ready'; secret: string; otpauthUri: string }
  | { kind: 'error'; message: string };

export default function MfaSetupDialog({ open, onClose }: Props) {
  const { refetchMe } = useAuth();
  const [step, setStep] = useState<Step>({ kind: 'loading' });
  const [code, setCode] = useState('');
  const [busy, setBusy] = useState(false);
  const [confirmError, setConfirmError] = useState<string | null>(null);

  useEffect(() => {
    if (!open) return;
    (async () => {
      setStep({ kind: 'loading' });
      setCode('');
      setConfirmError(null);
      try {
        // The dialog is only ever opened when MFA isn't enabled yet (see the
        // Layout banner), so no current code is needed here — an empty body
        // is still required since the server now expects valid JSON.
        const res = await apiFetch('/api/auth/mfa/setup', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({}),
        });
        const data = await res.json().catch(() => ({}));
        if (!res.ok) {
          setStep({ kind: 'error', message: data.error ?? `HTTP ${res.status}` });
          return;
        }
        setStep({ kind: 'ready', secret: data.secret, otpauthUri: data.otpauth_uri });
      } catch (e) {
        setStep({ kind: 'error', message: String(e) });
      }
    })();
  }, [open]);

  async function confirm(e: React.FormEvent) {
    e.preventDefault();
    setBusy(true);
    setConfirmError(null);
    try {
      const res = await apiFetch('/api/auth/mfa/confirm', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ code }),
      });
      const data = await res.json().catch(() => ({}));
      if (!res.ok) {
        setConfirmError(data.error ?? `HTTP ${res.status}`);
        return;
      }
      await refetchMe();
      onClose();
    } catch (e) {
      setConfirmError(String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Modal open={open} onClose={onClose} title="Set up MFA">
      {step.kind === 'loading' && <div style={{ color: 'var(--t2)', fontSize: 12 }}>Generating secret…</div>}
      {step.kind === 'error' && <div className="modal-err">{step.message}</div>}
      {step.kind === 'ready' && (
        <form onSubmit={confirm} style={{ display: 'flex', flexDirection: 'column', gap: 12 }}>
          <div style={{ fontSize: 12, color: 'var(--t2)' }}>
            Scan with your authenticator app (Google Authenticator, Aegis, 1Password, etc.), or enter the secret manually.
          </div>
          <div style={{ background: '#fff', padding: 12, borderRadius: 8, alignSelf: 'center' }}>
            <QRCodeSVG value={step.otpauthUri} size={180} />
          </div>
          <label className="modal-field">
            <span>Secret (manual entry)</span>
            <input readOnly value={step.secret} onFocus={e => e.target.select()} style={{ fontSize: 11 }} />
          </label>
          <label className="modal-field">
            <span>Enter the 6-digit code to confirm</span>
            <input
              value={code}
              onChange={e => setCode(e.target.value.replace(/\D/g, '').slice(0, 6))}
              inputMode="numeric"
              maxLength={6}
              autoFocus
            />
          </label>
          {confirmError && <div className="modal-err">{confirmError}</div>}
          <div className="modal-actions">
            <button className="save-btn" disabled={busy || code.length !== 6}>
              {busy ? 'Confirming…' : 'Confirm'}
            </button>
          </div>
        </form>
      )}
    </Modal>
  );
}
