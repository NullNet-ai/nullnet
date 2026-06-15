import { useState } from 'react';
import Layout from '../components/Layout';
import { useApi } from '../hooks/useApi';
import type { CertJson } from '../types';

type CredField = { key: string; label: string; optional?: boolean; textarea?: boolean };
type Provider = { id: string; name: string; fields: CredField[] };

// Provider ids/field keys mirror the server's DnsProviderCredentials enum.
const PROVIDERS: Provider[] = [
  { id: 'cloudflare', name: 'Cloudflare', fields: [{ key: 'api_token', label: 'API token' }] },
  { id: 'route53', name: 'AWS Route 53', fields: [
    { key: 'access_key_id', label: 'Access key ID' },
    { key: 'secret_access_key', label: 'Secret access key' },
    { key: 'region', label: 'Region (default us-east-1)', optional: true },
  ] },
  { id: 'azure', name: 'Azure DNS', fields: [
    { key: 'tenant_id', label: 'Tenant ID' },
    { key: 'client_id', label: 'Client ID' },
    { key: 'client_secret', label: 'Client secret' },
    { key: 'subscription_id', label: 'Subscription ID' },
    { key: 'resource_group', label: 'Resource group' },
  ] },
  { id: 'google', name: 'Google Cloud DNS', fields: [
    { key: 'project_id', label: 'Project ID' },
    { key: 'client_email', label: 'Service account email' },
    { key: 'private_key', label: 'Service account private key (PEM)', textarea: true },
  ] },
  { id: 'digitalocean', name: 'DigitalOcean', fields: [{ key: 'api_token', label: 'API token' }] },
  { id: 'hetzner', name: 'Hetzner DNS', fields: [{ key: 'api_token', label: 'API token' }] },
  { id: 'namecheap', name: 'Namecheap', fields: [
    { key: 'api_user', label: 'API user' },
    { key: 'api_key', label: 'API key' },
    { key: 'client_ip', label: 'Whitelisted client IP' },
  ] },
  { id: 'ovh', name: 'OVH', fields: [
    { key: 'app_key', label: 'Application key' },
    { key: 'app_secret', label: 'Application secret' },
    { key: 'consumer_key', label: 'Consumer key' },
    { key: 'endpoint', label: 'Endpoint (default eu.api.ovh.com)', optional: true },
  ] },
  { id: 'porkbun', name: 'Porkbun', fields: [
    { key: 'api_key', label: 'API key' },
    { key: 'secret_api_key', label: 'Secret API key' },
  ] },
  { id: 'godaddy', name: 'GoDaddy', fields: [
    { key: 'api_key', label: 'API key' },
    { key: 'api_secret', label: 'API secret' },
  ] },
  { id: 'vultr', name: 'Vultr', fields: [{ key: 'api_key', label: 'API key' }] },
];

function formatExpiry(unix: number | null): { text: string; color: string } {
  if (unix === null) return { text: 'unknown', color: 'var(--t2)' };
  const ms = unix * 1000;
  const days = Math.floor((ms - Date.now()) / 86_400_000);
  const date = new Date(ms).toLocaleDateString();
  if (days < 0) return { text: `expired (${date})`, color: 'var(--red, #e5484d)' };
  if (days < 30) return { text: `${days}d · ${date}`, color: 'var(--amber)' };
  return { text: `${days}d · ${date}`, color: 'var(--t1)' };
}

export default function Certificates() {
  const { data: certs, loading, refetch } = useApi<CertJson[]>('/api/certificates', 10000);
  const [deleting, setDeleting] = useState<Set<string>>(new Set());

  // Let's Encrypt (ACME / DNS-01) request form
  const [leDomain, setLeDomain] = useState('');
  const [providerId, setProviderId] = useState(PROVIDERS[0].id);
  const [creds, setCreds] = useState<Record<string, string>>({});
  const [propagation, setPropagation] = useState('');
  const [leSubmitting, setLeSubmitting] = useState(false);
  const [leError, setLeError] = useState<string | null>(null);

  const provider = PROVIDERS.find(p => p.id === providerId) ?? PROVIDERS[0];

  const list = certs ?? [];
  const existing = new Set(list.map(c => c.domain));

  async function requestLe(e: React.FormEvent) {
    e.preventDefault();
    setLeError(null);
    setLeSubmitting(true);
    try {
      const credentials: Record<string, string> = { provider: providerId };
      for (const f of provider.fields) {
        const v = (creds[f.key] ?? '').trim();
        if (v) credentials[f.key] = v;
      }
      const body: Record<string, unknown> = { domain: leDomain.trim(), credentials };
      const secs = parseInt(propagation, 10);
      if (!Number.isNaN(secs)) body.dns_propagation_secs = secs;

      const res = await fetch('/api/certificates/request', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
      });
      if (!res.ok) {
        const err = await res.json().catch(() => ({ error: `HTTP ${res.status}` }));
        throw new Error(err.error ?? `HTTP ${res.status}`);
      }
      setLeDomain(''); setCreds({}); setPropagation('');
      refetch();
    } catch (err) {
      setLeError(String(err instanceof Error ? err.message : err));
    } finally {
      setLeSubmitting(false);
    }
  }

  async function remove(d: string) {
    if (!confirm(`Delete certificate for ${d}?`)) return;
    setDeleting(prev => new Set(prev).add(d));
    try {
      await fetch(`/api/certificates/${encodeURIComponent(d)}`, { method: 'DELETE' });
      refetch();
    } finally {
      setDeleting(prev => { const next = new Set(prev); next.delete(d); return next; });
    }
  }

  const leRenew = existing.has(leDomain.trim());
  const credsComplete = provider.fields.every(f => f.optional || (creds[f.key] ?? '').trim() !== '');
  const canRequest = leDomain.trim() !== '' && credsComplete && !leSubmitting;

  return (
    <Layout page="certificates">
      <div className="content">
        <div className="hero-row">
          <span className="hero-num">{list.length}</span>
          <span className="hero-label">installed TLS certificates</span>
        </div>

        <div className="card">
          <div className="card-head">
            <span className="card-label">Certificates</span>
            <span style={{ fontSize: 11, color: 'var(--t2)' }}>private keys encrypted at rest</span>
          </div>
          <table className="tbl">
            <thead>
              <tr>
                <th>Domain</th>
                <th>Expires</th>
                <th></th>
              </tr>
            </thead>
            <tbody>
              {loading && (
                <tr><td colSpan={3} style={{ color: 'var(--t2)', padding: '20px 16px' }}>Loading…</td></tr>
              )}
              {list.map(c => {
                const exp = formatExpiry(c.expires_at);
                return (
                  <tr key={c.domain}>
                    <td style={{ fontWeight: 500, fontFamily: "'JetBrains Mono',monospace" }}>{c.domain}</td>
                    <td style={{ fontFamily: "'JetBrains Mono',monospace", fontSize: 11, color: exp.color }}>{exp.text}</td>
                    <td>
                      <button className="teardown-btn" onClick={() => remove(c.domain)} disabled={deleting.has(c.domain)}>
                        {deleting.has(c.domain) ? '…' : 'Delete'}
                      </button>
                    </td>
                  </tr>
                );
              })}
              {!loading && list.length === 0 && (
                <tr><td colSpan={3} style={{ color: 'var(--t2)', padding: '20px 16px' }}>No certificates installed</td></tr>
              )}
            </tbody>
          </table>
        </div>

        <div className="card" style={{ marginTop: 16 }}>
          <div className="card-head">
            <span className="card-label">Request via Let's Encrypt (DNS-01)</span>
            <span style={{ fontSize: 11, color: 'var(--t2)' }}>credentials stored encrypted · auto-renews before expiry</span>
          </div>
          <form onSubmit={requestLe} style={{ padding: 16, display: 'flex', flexDirection: 'column', gap: 12 }}>
            <label style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
              <span style={{ fontSize: 12, color: 'var(--t2)' }}>Domain (exact or *.wildcard)</span>
              <input
                value={leDomain}
                onChange={e => setLeDomain(e.target.value)}
                placeholder="app.example.com"
                spellCheck={false}
                style={{ fontFamily: "'JetBrains Mono',monospace", padding: '6px 8px' }}
              />
              {leRenew && <span style={{ fontSize: 11, color: 'var(--amber)' }}>overwrites existing «{leDomain.trim()}»</span>}
            </label>

            <label style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
              <span style={{ fontSize: 12, color: 'var(--t2)' }}>DNS provider</span>
              <select
                value={providerId}
                onChange={e => { setProviderId(e.target.value); setCreds({}); }}
                style={{ padding: '6px 8px' }}
              >
                {PROVIDERS.map(p => <option key={p.id} value={p.id}>{p.name}</option>)}
              </select>
            </label>

            {provider.fields.map(f => (
              <label key={f.key} style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
                <span style={{ fontSize: 12, color: 'var(--t2)' }}>{f.label}{f.optional && ' (optional)'}</span>
                {f.textarea ? (
                  <textarea
                    value={creds[f.key] ?? ''}
                    onChange={e => setCreds(c => ({ ...c, [f.key]: e.target.value }))}
                    spellCheck={false}
                    rows={4}
                    style={{ fontFamily: "'JetBrains Mono',monospace", fontSize: 11, padding: '6px 8px', resize: 'vertical' }}
                  />
                ) : (
                  <input
                    value={creds[f.key] ?? ''}
                    onChange={e => setCreds(c => ({ ...c, [f.key]: e.target.value }))}
                    spellCheck={false}
                    autoComplete="off"
                    style={{ fontFamily: "'JetBrains Mono',monospace", padding: '6px 8px' }}
                  />
                )}
              </label>
            ))}

            <label style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
              <span style={{ fontSize: 12, color: 'var(--t2)' }}>DNS propagation wait (seconds, default 30)</span>
              <input
                value={propagation}
                onChange={e => setPropagation(e.target.value)}
                placeholder="30"
                inputMode="numeric"
                style={{ fontFamily: "'JetBrains Mono',monospace", padding: '6px 8px', width: 120 }}
              />
            </label>

            {leError && <span style={{ color: 'var(--red, #e5484d)', fontSize: 12 }}>{leError}</span>}

            <div>
              <button type="submit" className="teardown-btn" disabled={!canRequest}>
                {leSubmitting ? 'Requesting… (may take a minute)' : leRenew ? 'Renew via Let’s Encrypt' : 'Request certificate'}
              </button>
            </div>
          </form>
        </div>
      </div>
    </Layout>
  );
}
