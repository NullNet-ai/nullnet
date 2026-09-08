import { useMemo, useRef, useState } from 'react';
import Layout from '../components/Layout';
import Modal from '../components/Modal';
import { useApi } from '../hooks/useApi';
import { apiFetch } from '../lib/apiFetch';
import { useStack } from '../StackContext';
import type {
  FilterConditionJson,
  FilterFieldJson,
  FilterPolicyJson,
  ServiceConfigJson,
  ServiceConfigListJson,
} from '../types';

// A service is only ever discoverable via a host match — `build_match_entries`
// (server-side) never registers a replica for a service without one. There is
// no third "no match" option: a service that only needs to exist as a chain
// placeholder doesn't need declaring at all (referencing its name in a
// dependency branch or trigger chain already registers it implicitly).
type MatchKind = 'docker' | 'process';
type ProtocolKind = 'http' | 'tcp' | 'udp';

// Traffic filter (issue #143) form state — mirrors `FilterPolicyJson`, but
// `values` stays a comma-separated string while being edited (same
// convention the old country-code input used).
type FilterAction = 'none' | 'block' | 'allow';

interface FilterRuleForm {
  field: FilterFieldJson;
  condition: FilterConditionJson;
  values: string;
}

interface FilterPolicyForm {
  action: FilterAction;
  // OR-of-ANDs: every rule within a group must match; groups are OR'ed.
  groups: FilterRuleForm[][];
}

const EMPTY_FILTER: FilterPolicyForm = { action: 'none', groups: [] };

const FIELD_LABELS: Record<FilterFieldJson, string> = {
  country: 'Country',
  org: 'Organization',
  src_ip: 'Src IP',
  dst_ip: 'Dst IP',
};

function conditionsForField(field: FilterFieldJson): { value: FilterConditionJson; label: string }[] {
  return field === 'src_ip' || field === 'dst_ip'
    ? [
        { value: 'contains', label: 'is within' },
        { value: 'not_contains', label: 'is not within' },
      ]
    : [
        { value: 'equal', label: 'is one of' },
        { value: 'not_equal', label: 'is none of' },
      ];
}

function emptyRule(field: FilterFieldJson): FilterRuleForm {
  return { field, condition: conditionsForField(field)[0].value, values: '' };
}

function filterToForm(json: FilterPolicyJson | undefined | null): FilterPolicyForm {
  if (!json || json === 'none') return { action: 'none', groups: [] };
  const [action, body] = 'block' in json ? (['block', json.block] as const) : (['allow', json.allow] as const);
  return {
    action,
    groups: body.groups.map(group =>
      group.map(r => ({ field: r.field, condition: r.condition, values: textFromList(r.values) })),
    ),
  };
}

function formToFilter(form: FilterPolicyForm): FilterPolicyJson {
  if (form.action === 'none') return 'none';
  const groups = form.groups
    .map(group =>
      group
        .map(r => ({
          field: r.field,
          condition: r.condition,
          values: listFromText(r.values).map(v => (r.field === 'country' ? v.toUpperCase() : v)),
        }))
        .filter(r => r.values.length > 0),
    )
    .filter(group => group.length > 0);
  // An empty group (no conditions with a value) or no groups at all can't be
  // validated server-side — rather than error on submit, treat it as if the
  // action were still "none".
  if (groups.length === 0) return 'none';
  return form.action === 'block' ? { block: { groups } } : { allow: { groups } };
}

// Used for both the egress (Country/Org/Dst IP) and ingress (Country/Org/Src
// IP) instances of the widget below.
function FilterPolicyEditor({
  label,
  value,
  onChange,
  fields,
}: {
  label: string;
  value: FilterPolicyForm;
  onChange: (next: FilterPolicyForm) => void;
  fields: FilterFieldJson[];
}) {
  function updateGroup(gi: number, next: FilterRuleForm[]) {
    onChange({ ...value, groups: value.groups.map((g, i) => (i === gi ? next : g)) });
  }
  function removeGroup(gi: number) {
    onChange({ ...value, groups: value.groups.filter((_, i) => i !== gi) });
  }
  function addGroup() {
    onChange({ ...value, groups: [...value.groups, [emptyRule(fields[0])]] });
  }
  function updateRule(gi: number, ri: number, patch: Partial<FilterRuleForm>) {
    updateGroup(
      gi,
      value.groups[gi].map((r, i) => (i === ri ? { ...r, ...patch } : r)),
    );
  }
  function removeRule(gi: number, ri: number) {
    updateGroup(
      gi,
      value.groups[gi].filter((_, i) => i !== ri),
    );
  }
  function addRule(gi: number) {
    updateGroup(gi, [...value.groups[gi], emptyRule(fields[0])]);
  }

  return (
    <div className="modal-field">
      <span>{label}</span>
      <select
        value={value.action}
        onChange={e => {
          const action = e.target.value as FilterAction;
          onChange({
            action,
            groups: action === 'none' ? [] : value.groups.length > 0 ? value.groups : [[emptyRule(fields[0])]],
          });
        }}
      >
        <option value="none">None</option>
        <option value="block">Block matching traffic</option>
        <option value="allow">Allow only matching traffic</option>
      </select>
      {value.action !== 'none' && (
        <div style={{ display: 'flex', flexDirection: 'column', gap: 8, marginTop: 8 }}>
          {value.groups.map((group, gi) => (
            <div key={gi}>
              {gi > 0 && (
                <div style={{ textAlign: 'center', fontSize: 11, color: 'var(--t2)', margin: '4px 0' }}>OR</div>
              )}
              <div style={{ border: '1px solid var(--t3)', borderRadius: 6, padding: 8 }}>
                {group.map((rule, ri) => (
                  <div
                    key={ri}
                    style={{ display: 'flex', flexWrap: 'wrap', gap: 6, marginBottom: 6, alignItems: 'center' }}
                  >
                    {ri > 0 && <span style={{ fontSize: 11, color: 'var(--t2)' }}>AND</span>}
                    <select
                      value={rule.field}
                      onChange={e => {
                        const field = e.target.value as FilterFieldJson;
                        updateRule(gi, ri, { field, condition: conditionsForField(field)[0].value });
                      }}
                    >
                      {fields.map(f => (
                        <option key={f} value={f}>
                          {FIELD_LABELS[f]}
                        </option>
                      ))}
                    </select>
                    <select
                      value={rule.condition}
                      onChange={e => updateRule(gi, ri, { condition: e.target.value as FilterConditionJson })}
                    >
                      {conditionsForField(rule.field).map(c => (
                        <option key={c.value} value={c.value}>
                          {c.label}
                        </option>
                      ))}
                    </select>
                    <input
                      value={rule.values}
                      onChange={e => updateRule(gi, ri, { values: e.target.value })}
                      placeholder={rule.field === 'country' ? 'US, CA' : rule.field === 'org' ? 'Google LLC' : '10.0.0.0/8'}
                      spellCheck={false}
                      style={{ flex: '1 1 120px', minWidth: 0 }}
                    />
                    <button type="button" className="teardown-btn" onClick={() => removeRule(gi, ri)}>
                      ×
                    </button>
                  </div>
                ))}
                <div style={{ display: 'flex', justifyContent: 'space-between', marginTop: 4 }}>
                  <button
                    type="button"
                    className="card-action"
                    style={{ background: 'none', border: 'none', cursor: 'pointer' }}
                    onClick={() => addRule(gi)}
                  >
                    + Add condition (AND)
                  </button>
                  <button type="button" className="teardown-btn" onClick={() => removeGroup(gi)}>
                    Remove group
                  </button>
                </div>
              </div>
            </div>
          ))}
          <button
            type="button"
            className="card-action"
            style={{ background: 'none', border: 'none', cursor: 'pointer', alignSelf: 'start' }}
            onClick={addGroup}
          >
            + Add group (OR)
          </button>
        </div>
      )}
    </div>
  );
}

// Sentinel for "+ Type a custom name…", picked from a chain step's <select>
// below — never a real value, replaced with '' on save if left untouched.
const CUSTOM_STEP = '\u0000custom';

interface TriggerFormState {
  port: string;
  chain: string[];
}

interface ServiceFormState {
  name: string;
  matchKind: MatchKind;
  matchValue: string;
  hostIp: string;
  port: string;
  reachable: boolean;
  timeout: string;
  maxNetworks: string;
  protocol: ProtocolKind;
  listenPort: string;
  dependencies: string[][];
  triggers: TriggerFormState[];
  egressFilter: FilterPolicyForm;
  ingressFilter: FilterPolicyForm;
}

const EMPTY_FORM: ServiceFormState = {
  name: '',
  matchKind: 'docker',
  matchValue: '',
  hostIp: '',
  port: '',
  reachable: false,
  timeout: '0',
  maxNetworks: '',
  protocol: 'http',
  listenPort: '',
  dependencies: [],
  triggers: [],
  egressFilter: EMPTY_FILTER,
  ingressFilter: EMPTY_FILTER,
};

// One step of a dependency branch or trigger chain: a `<select>` of every
// service name already known in this stack — same pattern as the Routes
// page's service picker. A chain step is often a placeholder with no
// [[services]] entry of its own (see MatchKind), so "+ Type a custom
// name…" drops to a plain text input instead of blocking on the list;
// an already-saved custom name renders as that text input straight away.
function ChainStepList({
  steps,
  onChange,
  knownNames,
}: {
  steps: string[];
  onChange: (next: string[]) => void;
  knownNames: string[];
}) {
  function update(i: number, value: string) {
    onChange(steps.map((s, idx) => (idx === i ? value : s)));
  }
  function remove(i: number) {
    onChange(steps.filter((_, idx) => idx !== i));
  }
  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: 6 }}>
      {steps.map((step, i) => {
        const isCustom = step === CUSTOM_STEP || (step !== '' && !knownNames.includes(step));
        return (
          <div key={i} style={{ display: 'flex', gap: 6 }}>
            {isCustom ? (
              <input
                value={step === CUSTOM_STEP ? '' : step}
                onChange={e => update(i, e.target.value)}
                placeholder="service name"
                spellCheck={false}
                autoFocus={step === CUSTOM_STEP}
              />
            ) : (
              <select value={step} onChange={e => update(i, e.target.value)}>
                <option value="" disabled>
                  select a service…
                </option>
                {knownNames.map(n => (
                  <option key={n} value={n}>
                    {n}
                  </option>
                ))}
                <option value={CUSTOM_STEP}>+ Type a custom name…</option>
              </select>
            )}
            <button type="button" className="teardown-btn" onClick={() => remove(i)}>
              ×
            </button>
          </div>
        );
      })}
      <button
        type="button"
        className="card-action"
        style={{ background: 'none', border: 'none', cursor: 'pointer', alignSelf: 'start' }}
        onClick={() => onChange([...steps, ''])}
      >
        + Add step
      </button>
    </div>
  );
}

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
    // A service without either match key (only possible from data written
    // before this option was removed, or via a hand-edited TOML import)
    // falls back to 'docker' — its match value stays blank, so the form
    // won't validate until a real host match is filled in.
    matchKind: s.process_path ? 'process' : 'docker',
    matchValue: s.docker_container ?? s.process_path ?? '',
    hostIp: s.host_ip ?? '',
    port: s.port != null ? String(s.port) : '',
    reachable: s.timeout != null,
    timeout: s.timeout != null ? String(s.timeout) : '0',
    maxNetworks: s.max_networks != null ? String(s.max_networks) : '',
    protocol: s.protocol ?? 'http',
    listenPort: s.listen_port != null ? String(s.listen_port) : '',
    dependencies: s.proxy_dependencies.map(branch => [...branch]),
    triggers: s.triggers.map(t => ({ port: String(t.port), chain: [...t.chain] })),
    egressFilter: filterToForm(s.egress_filter),
    ingressFilter: filterToForm(s.ingress_filter),
  };
}

function formToService(f: ServiceFormState): ServiceConfigJson {
  // CUSTOM_STEP left untouched (the "+ Type a custom name…" option picked,
  // but nothing typed into the text box it dropped to) counts as unset.
  const chain = (steps: string[]) =>
    steps.map(s => (s === CUSTOM_STEP ? '' : s.trim())).filter(Boolean);
  return {
    name: f.name.trim(),
    docker_container: f.matchKind === 'docker' ? f.matchValue.trim() : null,
    process_path: f.matchKind === 'process' ? f.matchValue.trim() : null,
    host_ip: f.hostIp.trim() || null,
    port: f.port.trim() !== '' ? Number(f.port) : null,
    timeout: f.reachable ? Number(f.timeout || '0') : null,
    proxy_dependencies: f.dependencies.map(chain).filter(branch => branch.length > 0),
    triggers: f.triggers
      .filter(t => t.port.trim() !== '')
      .map(t => ({ port: Number(t.port), chain: chain(t.chain) })),
    max_networks: f.maxNetworks.trim() !== '' ? Number(f.maxNetworks) : null,
    protocol: f.protocol,
    listen_port: f.protocol !== 'http' && f.listenPort.trim() !== '' ? Number(f.listenPort) : null,
    egress_filter: formToFilter(f.egressFilter),
    ingress_filter: formToFilter(f.ingressFilter),
  };
}

function matchLabel(s: ServiceConfigJson): string {
  if (s.docker_container) return `docker: ${s.docker_container}${s.host_ip ? ` @ ${s.host_ip}` : ''}`;
  if (s.process_path) return `process: ${s.process_path}${s.host_ip ? ` @ ${s.host_ip}` : ''}`;
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
  // Stable across renders with no data change, so it's a safe useMemo dep below.
  const services = useMemo(() => data?.services ?? [], [data]);

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
  const [exporting, setExporting] = useState(false);
  const [importing, setImporting] = useState(false);
  const importInputRef = useRef<HTMLInputElement>(null);

  const noStack = !stack.trim();
  const notFound = !noStack && !loading && !!error && error.includes('404');

  // Every name already declared or referenced anywhere in this stack —
  // suggestions for the dependency-branch/trigger-chain step inputs.
  const knownServiceNames = useMemo(() => {
    const names = new Set<string>();
    for (const s of services) {
      names.add(s.name);
      for (const branch of s.proxy_dependencies) for (const step of branch) names.add(step);
      for (const t of s.triggers) for (const step of t.chain) names.add(step);
    }
    return Array.from(names).sort();
  }, [services]);

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

  // Downloads the stack's current config as the same TOML the server used to
  // read from `services/<stack>.toml` — a manual backup path outside the DB.
  async function exportStack() {
    setExporting(true);
    setListError(null);
    try {
      const res = await apiFetch(`/api/service-config/${stack}/export`);
      if (!res.ok) {
        const body = await res.json().catch(() => ({ error: `HTTP ${res.status}` }));
        setListError(body.error ?? `HTTP ${res.status}`);
        return;
      }
      const blob = await res.blob();
      const url = URL.createObjectURL(blob);
      const a = document.createElement('a');
      a.href = url;
      a.download = `${stack}.toml`;
      a.click();
      URL.revokeObjectURL(url);
    } catch (e) {
      setListError(String(e));
    }
    setExporting(false);
  }

  // Replaces the stack's services+routes with a picked TOML file's contents
  // — validated (and applied live) the same way the widget editor is.
  async function importStack(file: File) {
    setImporting(true);
    setListError(null);
    try {
      const text = await file.text();
      const res = await apiFetch(`/api/service-config/${stack}/import`, {
        method: 'POST',
        headers: { 'Content-Type': 'text/plain' },
        body: text,
      });
      const data = await res.json().catch(() => ({ ok: res.ok, error: `HTTP ${res.status}` }));
      if (res.ok && data.ok) refetch();
      else setListError(data.error ?? `HTTP ${res.status}`);
    } catch (e) {
      setListError(String(e));
    }
    setImporting(false);
  }

  function addDependencyBranch() {
    setForm(f => ({ ...f, dependencies: [...f.dependencies, ['']] }));
  }
  function updateDependencyBranch(i: number, next: string[]) {
    setForm(f => ({ ...f, dependencies: f.dependencies.map((d, idx) => (idx === i ? next : d)) }));
  }
  function removeDependencyBranch(i: number) {
    setForm(f => ({ ...f, dependencies: f.dependencies.filter((_, idx) => idx !== i) }));
  }

  function addTrigger() {
    setForm(f => ({ ...f, triggers: [...f.triggers, { port: '', chain: [''] }] }));
  }
  function updateTrigger(i: number, patch: Partial<TriggerFormState>) {
    setForm(f => ({ ...f, triggers: f.triggers.map((t, idx) => (idx === i ? { ...t, ...patch } : t)) }));
  }
  function removeTrigger(i: number) {
    setForm(f => ({ ...f, triggers: f.triggers.filter((_, idx) => idx !== i) }));
  }

  const formValid = form.name.trim() !== '' && form.port.trim() !== '' && form.matchValue.trim() !== '';

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

            <div style={{ marginTop: 16, display: 'flex', gap: 8, flexWrap: 'wrap' }}>
              <button className="save-btn" onClick={exportStack} disabled={exporting}>
                {exporting ? 'Exporting…' : 'Export as TOML'}
              </button>
              <button
                className="save-btn"
                onClick={() => importInputRef.current?.click()}
                disabled={importing}
              >
                {importing ? 'Importing…' : 'Import from TOML'}
              </button>
              <input
                ref={importInputRef}
                type="file"
                accept=".toml"
                style={{ display: 'none' }}
                onChange={e => {
                  const file = e.target.files?.[0];
                  e.target.value = '';
                  if (file) importStack(file);
                }}
              />
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
              <option value="docker">Docker container / Swarm service</option>
              <option value="process">Non-Docker process path</option>
            </select>
          </label>
          <label className="modal-field">
            <span>{form.matchKind === 'docker' ? 'Container / service name' : 'Process exe path'}</span>
            <input
              value={form.matchValue}
              onChange={e => setForm(f => ({ ...f, matchValue: e.target.value }))}
              placeholder={form.matchKind === 'docker' ? 'my-app_color' : '/usr/local/bin/metrics-exporter'}
              spellCheck={false}
            />
          </label>
          <label className="modal-field">
            <span>Host IP (optional)</span>
            <input
              value={form.hostIp}
              onChange={e => setForm(f => ({ ...f, hostIp: e.target.value }))}
              placeholder="Any host"
              spellCheck={false}
            />
            <small>Register only on this node's IPv4 address.</small>
          </label>
          <label className="modal-field">
            <span>Backend port</span>
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
            <span>Proxy dependencies — independent branches, each an ordered chain</span>
            {form.dependencies.map((branch, i) => (
              <div
                key={i}
                style={{ border: '1px solid var(--t3)', borderRadius: 6, padding: 8, marginBottom: 8 }}
              >
                <ChainStepList
                  steps={branch}
                  onChange={next => updateDependencyBranch(i, next)}
                  knownNames={knownServiceNames}
                />
                <button
                  type="button"
                  className="teardown-btn"
                  style={{ marginTop: 6 }}
                  onClick={() => removeDependencyBranch(i)}
                >
                  Remove branch
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
              <div
                key={i}
                style={{ border: '1px solid var(--t3)', borderRadius: 6, padding: 8, marginBottom: 8 }}
              >
                <input
                  type="number"
                  value={t.port}
                  onChange={e => updateTrigger(i, { port: e.target.value })}
                  placeholder="port observed on this host"
                  style={{ width: '100%', marginBottom: 6 }}
                />
                <ChainStepList
                  steps={t.chain}
                  onChange={next => updateTrigger(i, { chain: next })}
                  knownNames={knownServiceNames}
                />
                <button
                  type="button"
                  className="teardown-btn"
                  style={{ marginTop: 6 }}
                  onClick={() => removeTrigger(i)}
                >
                  Remove trigger
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

          <FilterPolicyEditor
            label="Egress traffic filter (destination of this service's outbound traffic)"
            value={form.egressFilter}
            onChange={next => setForm(f => ({ ...f, egressFilter: next }))}
            fields={['country', 'org', 'dst_ip']}
          />

          <FilterPolicyEditor
            label="Ingress traffic filter (proxy clients reaching this service)"
            value={form.ingressFilter}
            onChange={next => setForm(f => ({ ...f, ingressFilter: next }))}
            fields={['country', 'org', 'src_ip']}
          />
          {form.ingressFilter.action !== 'none' && !form.reachable && (
            <div style={{ fontSize: 11, color: 'var(--t2)', marginTop: -8 }}>
              Requires "Proxy-reachable entry point" above — ingress filters are enforced at the proxy.
            </div>
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
