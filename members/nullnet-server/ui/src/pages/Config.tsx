import { useEffect, useState } from 'react';
import Layout from '../components/Layout';
import { useApi, useApiText } from '../hooks/useApi';
import { apiFetch } from '../lib/apiFetch';
import { useStack } from '../StackContext';

type Status =
  | { kind: 'idle' }
  | { kind: 'saving' }
  | { kind: 'ok' }
  | { kind: 'error'; msg: string };

const STARTER_TEMPLATE = `# Stack configuration — one [[services]] block per service.
# This example is a proxy-reachable Docker service; uncomment the
# optional fields below as needed.
[[services]]
name = "example"              # service identifier (unique within the stack)
docker_container = "example"  # host-match key: Swarm service / container name
port = 8080                   # backend port on the service's replicas
timeout = 60                  # proxy-reachable entry point; idle timeout seconds (0 = none)

# --- optional ---
# process_path = "/usr/bin/example"      # non-Docker match key (use instead of docker_container)
# max_networks = 2                       # cap proxy networks; extra clients reuse one
# proxy_dependencies = [["db.example"]]  # dep chains brought up on proxy setup
# protocol = "tcp"                       # "http" (default) | "tcp" | "udp"
# listen_port = 5432                     # external proxy port; required for tcp/udp
# egress_blocked_countries = ["RU", "CN"]   # egress deny-list by client dest country
# egress_allowed_countries = ["US", "IT"]   # egress allow-list (exclusive with the deny-list)
# ingress_blocked_countries = ["RU"]         # ingress deny-list by client source country
# ingress_allowed_countries = ["US", "IT"]   # ingress allow-list (needs a timeout / entry point)

# [[services.triggers]]                  # backend-triggered chain: observed port -> chain
# port = 5555
# chain = ["worker.example"]
`;

// A stack name maps to a bare filename, so keep it to safe identifier chars.
const validName = (n: string) => /^[A-Za-z0-9_-]+$/.test(n);

export default function Config() {
  const { stack, setStack } = useStack();
  const { text, loading, error, refetch } = useApiText(`/api/config/${stack}`);
  const { data: stacks, refetch: refetchStacks } = useApi<string[]>('/api/stacks', 10000);

  const [content, setContent] = useState<string>('');
  const [original, setOriginal] = useState<string>('');
  const [status, setStatus] = useState<Status>({ kind: 'idle' });
  const [newName, setNewName] = useState<string>('');

  // Sync the editor when the loaded file (or stack) changes.
  useEffect(() => {
    if (text !== null) {
      setContent(text);
      setOriginal(text);
      setStatus({ kind: 'idle' });
    }
  }, [text]);

  const noStack = !stack.trim();
  const notFound = !noStack && !loading && !!error && error.includes('404');
  const dirty = content !== original;

  async function postConfig(name: string, body: string): Promise<{ ok: boolean; error?: string }> {
    try {
      const res = await apiFetch(`/api/config/${name}`, {
        method: 'POST',
        headers: { 'Content-Type': 'text/plain' },
        body,
      });
      const data = await res.json().catch(() => ({ ok: res.ok, error: `HTTP ${res.status}` }));
      return { ok: res.ok && data.ok, error: data.error };
    } catch (e) {
      return { ok: false, error: String(e) };
    }
  }

  async function save() {
    setStatus({ kind: 'saving' });
    const r = await postConfig(stack, content);
    if (r.ok) {
      setOriginal(content);
      setStatus({ kind: 'ok' });
    } else {
      setStatus({ kind: 'error', msg: r.error ?? 'unknown error' });
    }
  }

  async function createStack(name: string) {
    setStatus({ kind: 'saving' });
    const r = await postConfig(name, STARTER_TEMPLATE);
    if (r.ok) {
      setStatus({ kind: 'idle' });
      setNewName('');
      refetchStacks();
      if (name === stack) refetch();
      else setStack(name);
    } else {
      setStatus({ kind: 'error', msg: r.error ?? 'unknown error' });
    }
  }

  async function removeStack() {
    if (!confirm(`Delete stack "${stack}"? Its services are torn down immediately.`)) return;
    const res = await apiFetch(`/api/config/${stack}`, { method: 'DELETE' });
    if (res.ok) {
      const others = (stacks ?? []).filter(s => s !== stack);
      refetchStacks();
      setStack(others[0] ?? '');
    } else {
      const data = await res.json().catch(() => ({ error: `HTTP ${res.status}` }));
      setStatus({ kind: 'error', msg: data.error ?? `HTTP ${res.status}` });
    }
  }

  const errorLine = status.kind === 'error' && (
    <span className="cfg-err">
      <span className="badge b-red">Error</span>
      <span className="cfg-err-msg">{status.msg}</span>
    </span>
  );

  return (
    <Layout page="config">
      <div className="content">
        <div className="page-title">Configuration</div>
        <div className="page-sub">
          {noStack
            ? 'No stacks configured yet.'
            : <>Live service configuration for stack{' '}
                <span style={{ fontFamily: "'JetBrains Mono',monospace", color: 'var(--t1)' }}>{stack}</span>
                {' '}— edits are validated and applied without a restart.</>}
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
              onKeyDown={e => { if (e.key === 'Enter' && validName(newName)) createStack(newName); }}
            />
            <button
              className="save-btn"
              onClick={() => createStack(newName)}
              disabled={!validName(newName) || status.kind === 'saving'}
            >
              {status.kind === 'saving' ? 'Creating…' : 'Create stack'}
            </button>
            {errorLine}
          </div>
        )}

        {!noStack && loading && <div style={{ color: 'var(--t2)', fontSize: 12 }}>Loading…</div>}

        {notFound && (
          <div className="cfg-empty">
            <div style={{ color: 'var(--t2)', fontSize: 13 }}>
              Stack <span style={{ fontFamily: "'JetBrains Mono',monospace", color: 'var(--t1)' }}>{stack}</span> doesn't exist yet.
            </div>
            <button className="save-btn" onClick={() => createStack(stack)} disabled={status.kind === 'saving'}>
              {status.kind === 'saving' ? 'Creating…' : 'Create stack'}
            </button>
            {errorLine}
          </div>
        )}

        {!noStack && error && !notFound && (
          <div style={{ color: 'var(--red)', fontSize: 12 }}>
            Failed to load config: {error}
          </div>
        )}

        {!noStack && !loading && !error && (
          <>
            <textarea
              className="cfg-edit"
              value={content}
              spellCheck={false}
              onChange={e => {
                setContent(e.target.value);
                if (status.kind !== 'idle') setStatus({ kind: 'idle' });
              }}
            />
            <div className="cfg-actions">
              <button
                className="save-btn"
                onClick={save}
                disabled={!dirty || status.kind === 'saving'}
              >
                {status.kind === 'saving' ? 'Applying…' : 'Save & apply'}
              </button>
              <button className="teardown-btn" onClick={removeStack}>
                Delete stack
              </button>

              {status.kind === 'ok' && (
                <span className="badge b-green">✓ Valid — applied live</span>
              )}
              {status.kind === 'error' && (
                <span className="cfg-err">
                  <span className="badge b-red">Rejected</span>
                  <span className="cfg-err-msg">{status.msg}</span>
                </span>
              )}
              {status.kind === 'idle' && dirty && (
                <span style={{ color: 'var(--t2)', fontSize: 11 }}>Unsaved changes</span>
              )}
            </div>
          </>
        )}
      </div>
    </Layout>
  );
}
