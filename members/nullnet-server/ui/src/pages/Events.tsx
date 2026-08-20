import { useState, useEffect, useRef, useCallback } from 'react';
import { useSearchParams } from 'react-router-dom';
import Layout from '../components/Layout';
import { apiFetch } from '../lib/apiFetch';
import { formatTimestamp, formatTimestampFull } from '../lib/time';
import type { EventJson, EventsPage, Severity } from '../types';

const SEVERITY_COLOR: Record<Severity, string> = {
  info: 'var(--green)',
  warning: 'var(--amber)',
  error: 'var(--red, #f87171)',
};

const KIND_LABELS: Record<string, string> = {
  // Server events
  node_connected: 'node_connected',
  node_disconnected: 'node_disconnected',
  service_registered: 'service_registered',
  service_unregistered: 'service_unregistered',
  service_declaration_skipped: 'service_declaration_skipped',
  setup_started: 'setup_started',
  setup_ack: 'setup_ack',
  setup_timeout: 'setup_timeout',
  session_created: 'session_created',
  session_torn_down: 'session_torn_down',
  net_teardown_unconfirmed: 'net_teardown_unconfirmed',
  config_reloaded: 'config_reloaded',
  config_stack_removed: 'config_stack_removed',
  route_conflict: 'route_conflict',
  all_replicas_removed: 'all_replicas_removed',
  service_reachability_toggled: 'service_reachability_toggled',
  proxy_client_timed_out: 'proxy_client_timed_out',
  sticky_session_reused: 'sticky_session_reused',
  stale_session_evicted: 'stale_session_evicted',
  max_networks_limit_enforced: 'max_networks_limit_enforced',
  net_id_pool_exhausted: 'net_id_pool_exhausted',
  proxy_chain_setup_failed: 'proxy_chain_setup_failed',
  backend_trigger_setup_bailed: 'backend_trigger_setup_bailed',
  udp_port_pool_exhausted: 'udp_port_pool_exhausted',
  legacy_config_import_failed: 'legacy_config_import_failed',
  file_watch_failed: 'file_watch_failed',
  port_mapping_conflict: 'port_mapping_conflict',
  // Client error
  vxlan_setup_failed: 'vxlan_setup_failed',
  vlan_setup_failed: 'vlan_setup_failed',
  vxlan_teardown_failed: 'vxlan_teardown_failed',
  vlan_teardown_failed: 'vlan_teardown_failed',
  dnat_install_failed: 'dnat_install_failed',
  dnat_removal_failed: 'dnat_removal_failed',
  host_mapping_failed: 'host_mapping_failed',
  control_channel_closed: 'control_channel_closed',
  control_channel_ack_failed: 'control_channel_ack_failed',
  services_list_update_failed: 'services_list_update_failed',
  backend_trigger_send_failed: 'backend_trigger_send_failed',
  egress_trigger_send_failed: 'egress_trigger_send_failed',
  gateway_forward_install_failed: 'gateway_forward_install_failed',
  firewall_rules_load_failed: 'firewall_rules_load_failed',
  container_suspend_failed: 'container_suspend_failed',
  container_resume_failed: 'container_resume_failed',
  backend_trigger_setup_timed_out: 'backend_trigger_setup_timed_out',
  egress_steer_setup_timed_out: 'egress_steer_setup_timed_out',
  egress_steer_install_failed: 'egress_steer_install_failed',
  nfqueue_bind_failed: 'nfqueue_bind_failed',
  conntrack_subscribe_failed: 'conntrack_subscribe_failed',
  mss_clamp_install_failed: 'mss_clamp_install_failed',
  egress_policy_check_failed: 'egress_policy_check_failed',
  conntrack_flush_failed: 'conntrack_flush_failed',
  // Client info
  vxlan_setup_completed: 'vxlan_setup_completed',
  vlan_setup_completed: 'vlan_setup_completed',
  control_channel_established: 'control_channel_established',
  services_list_updated: 'services_list_updated',
  // Proxy error
  upstream_lookup_failed: 'upstream_lookup_failed',
  proxy_request_missing_host: 'proxy_request_missing_host',
  proxy_request_invalid_host: 'proxy_request_invalid_host',
  upstream_ip_parse_failed: 'upstream_ip_parse_failed',
  proxy_client_not_inet: 'proxy_client_not_inet',
  tls_certificate_invalid: 'tls_certificate_invalid',
  tcp_listener_bind_failed: 'tcp_listener_bind_failed',
  udp_listener_bind_failed: 'udp_listener_bind_failed',
  tcp_upstream_connect_failed: 'tcp_upstream_connect_failed',
  udp_upstream_connect_failed: 'udp_upstream_connect_failed',
  // Proxy info
  proxy_request_routed: 'proxy_request_routed',
  proxy_connected: 'proxy_connected',
  proxy_disconnected: 'proxy_disconnected',
  // Certificate
  certificate_installed: 'certificate_installed',
  certificate_renewed: 'certificate_renewed',
  certificate_removed: 'certificate_removed',
  certificate_renewal_failed: 'certificate_renewal_failed',
  certificate_credentials_store_failed: 'certificate_credentials_store_failed',
};

const ALL_KINDS = Object.keys(KIND_LABELS);

function eventDetail(e: EventJson): string {
  switch (e.type) {
    case 'node_connected':
    case 'node_disconnected':
      return e.ip;
    case 'service_registered':
    case 'service_unregistered':
      return `${e.name} · ${e.stack}`;
    case 'service_declaration_skipped':
      return `${e.service} · ${e.node} · ${e.reason}`;
    case 'setup_started':
      return `net ${e.net_id} · ${e.service} ← ${e.client_ip}`;
    case 'setup_ack':
      return `net ${e.net_id} · ${e.service} · ${e.latency_ms}ms`;
    case 'setup_timeout':
      return `net ${e.net_id} · ${e.service}`;
    case 'session_created':
      return `net ${e.net_id} · ${e.service} ← ${e.client_ip}`;
    case 'session_torn_down':
      return `net ${e.net_id} · ${e.service} · ${e.client_ip}`;
    case 'net_teardown_unconfirmed':
      return `net ${e.net_id} · ${e.node_ip} never confirmed teardown`;
    case 'config_reloaded':
    case 'config_stack_removed':
      return e.stack;
    case 'route_conflict':
      return `${e.host}${e.path} · ${e.stack_a} vs ${e.stack_b}`;
    case 'all_replicas_removed':
      return `${e.service} · ${e.stack} · ${e.ip}`;
    case 'service_reachability_toggled':
      return `${e.service} · ${e.stack} · ${e.reachable ? 'reachable' : 'unreachable'}`;
    case 'proxy_client_timed_out':
      return `${e.service} · ${e.client_ip}`;
    case 'sticky_session_reused':
    case 'stale_session_evicted':
      return `${e.service} · ${e.client_ip} via ${e.proxy_ip}`;
    case 'max_networks_limit_enforced':
      return `${e.service} · proxy ${e.proxy_ip} · net ${e.net_id} · limit ${e.limit}`;
    case 'net_id_pool_exhausted':
    case 'udp_port_pool_exhausted':
    case 'proxy_chain_setup_failed':
      return `${e.service} · ${e.client_ip}`;
    case 'backend_trigger_setup_bailed':
      return `${e.service} · port ${e.port}`;
    case 'legacy_config_import_failed':
      return `${e.stack} · ${e.error_message}`;
    case 'file_watch_failed':
      return `${e.target} · ${e.error_message}`;
    case 'port_mapping_conflict':
      return `${e.protocol}/${e.listen_port} · ${e.stack_a}/${e.service_a} vs ${e.stack_b}/${e.service_b}`;
    // Client error
    case 'vxlan_setup_failed':
    case 'vxlan_teardown_failed':
      return `vxlan ${e.vxlan_id} · ${e.ns_name} · code ${e.error_code}`;
    case 'vlan_setup_failed':
      return `vlan ${e.vlan_id} · ${e.local_veth} · ${e.error_reason}`;
    case 'vlan_teardown_failed':
      return `vlan ${e.vlan_id} · ${e.error_reason}`;
    case 'dnat_install_failed':
    case 'dnat_removal_failed':
      return `port ${e.port} → ${e.overlay_ip}`;
    case 'host_mapping_failed':
      return `${e.hostname} → ${e.ip}${e.docker_container ? ` (${e.docker_container})` : ''}`;
    case 'control_channel_closed':
      return '—';
    case 'control_channel_ack_failed':
      return `${e.message_type} · msg ${e.msg_id}`;
    case 'services_list_update_failed':
      return `${e.num_services} services · ${e.error_message}`;
    case 'backend_trigger_send_failed':
      return `${e.service_name} · port ${e.port} · ${e.error_message}`;
    case 'egress_trigger_send_failed':
      return `${e.service_name} → ${e.dst_ip}:${e.dst_port} · ${e.error_message}`;
    case 'gateway_forward_install_failed':
      return `vxlan ${e.vxlan_id} · ${e.br_net}`;
    case 'firewall_rules_load_failed':
      return `${e.path} · ${e.error_message}`;
    case 'container_suspend_failed':
    case 'container_resume_failed':
      return `${e.docker_container} · ${e.error_message}`;
    case 'backend_trigger_setup_timed_out':
      return `${e.service_name}:${e.port} · ${e.docker_container} · ${e.error_message}`;
    case 'egress_steer_setup_timed_out':
      return `${e.docker_container} → ${e.dst_ip}:${e.dst_port} · ${e.error_message}`;
    case 'egress_steer_install_failed':
      return `vxlan ${e.vxlan_id} · ${e.docker_container ?? '—'} · ${e.error_message}`;
    case 'nfqueue_bind_failed':
      return `queue ${e.queue_id} · ${e.error_message}`;
    case 'conntrack_subscribe_failed':
      return e.error_message;
    case 'mss_clamp_install_failed':
      return e.error_message;
    case 'egress_policy_check_failed':
      return `${e.docker_container} → ${e.dst_ip} · ${e.error_message}`;
    case 'conntrack_flush_failed':
      return `${e.ip} · ${e.error_message}`;
    // Client info
    case 'vxlan_setup_completed':
      return `vxlan ${e.vxlan_id} · ${e.ns_name}`;
    case 'vlan_setup_completed':
      return `vlan ${e.vlan_id}`;
    case 'control_channel_established':
      return '—';
    case 'services_list_updated':
      return `${e.num_services} services`;
    // Proxy error
    case 'upstream_lookup_failed':
      return `${e.service_name} · ${e.client_ip} · ${e.error_message}`;
    case 'proxy_request_missing_host':
    case 'proxy_request_invalid_host':
      return e.client_ip;
    case 'upstream_ip_parse_failed':
      return `${e.raw_ip} · ${e.service_name}`;
    case 'proxy_client_not_inet':
      return e.address_family;
    case 'tls_certificate_invalid':
      return `${e.domain} · ${e.reason}`;
    case 'tcp_listener_bind_failed':
    case 'udp_listener_bind_failed':
      return `:${e.listen_port} · ${e.service_name} · ${e.error_message}`;
    case 'tcp_upstream_connect_failed':
    case 'udp_upstream_connect_failed':
      return `${e.service_name} · ${e.client_ip} · ${e.error_message}`;
    // Certificate events
    case 'certificate_installed':
    case 'certificate_renewed':
    case 'certificate_removed':
      return e.domain;
    case 'certificate_renewal_failed':
    case 'certificate_credentials_store_failed':
      return `${e.domain} · ${e.error_message}`;
    // Proxy info
    case 'proxy_request_routed':
      return `${e.service_name} · ${e.client_ip} → ${e.upstream_ip} · ${e.latency_ms}ms`;
    case 'proxy_connected':
    case 'proxy_disconnected':
      return e.ip;
  }
}

const MAX_EVENTS = 500;
const PAGE_SIZE = 100;
const SEVERITIES: Severity[] = ['info', 'warning', 'error'];

/// Fetch one most-recent-first page of persisted events matching the given filters.
async function fetchEventsPage(
  kind: string,
  severity: Severity | '',
  beforeId: number | null,
): Promise<EventsPage> {
  const params = new URLSearchParams();
  if (kind) params.set('kind', kind);
  if (severity) params.set('severity', severity);
  if (beforeId != null) params.set('before_id', String(beforeId));
  params.set('limit', String(PAGE_SIZE));
  const res = await apiFetch(`/api/events?${params.toString()}`);
  if (!res.ok) throw new Error(`GET /api/events failed: ${res.status}`);
  return res.json();
}

export default function Events() {
  const [searchParams, setSearchParams] = useSearchParams();
  const kindFilter = searchParams.get('kind') ?? '';
  const severityFilter = (searchParams.get('severity') ?? '') as Severity | '';

  function setKindFilter(kind: string) {
    setSearchParams(prev => {
      const next = new URLSearchParams(prev);
      if (kind) next.set('kind', kind); else next.delete('kind');
      return next;
    }, { replace: true });
  }

  function setSeverityFilter(s: Severity | '') {
    setSearchParams(prev => {
      const next = new URLSearchParams(prev);
      if (s) next.set('severity', s); else next.delete('severity');
      return next;
    }, { replace: true });
  }

  // Oldest-first, matching the order live events arrive in. History (server-
  // paginated, filtered by kind/severity) is prepended to the front; live SSE
  // events are appended to the back.
  const [events, setEvents] = useState<EventJson[]>([]);
  const [nextBeforeId, setNextBeforeId] = useState<number | null>(null);
  const [loadingOlder, setLoadingOlder] = useState(false);
  const [initialLoading, setInitialLoading] = useState(true);
  const [paused, setPaused] = useState(false);
  const [liveCount, setLiveCount] = useState(0);
  const pausedRef = useRef(paused);
  pausedRef.current = paused;

  // Reload the first page whenever the kind/severity filter changes (and on mount).
  useEffect(() => {
    let cancelled = false;
    setInitialLoading(true);
    fetchEventsPage(kindFilter, severityFilter, null)
      .then(page => {
        if (cancelled) return;
        setEvents(page.events.slice().reverse());
        setNextBeforeId(page.next_before_id);
      })
      .catch(() => {
        if (!cancelled) {
          setEvents([]);
          setNextBeforeId(null);
        }
      })
      .finally(() => {
        if (!cancelled) setInitialLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [kindFilter, severityFilter]);

  const loadOlder = useCallback(async () => {
    if (nextBeforeId == null || loadingOlder) return;
    setLoadingOlder(true);
    try {
      const page = await fetchEventsPage(kindFilter, severityFilter, nextBeforeId);
      setEvents(prev => [...page.events.slice().reverse(), ...prev]);
      setNextBeforeId(page.next_before_id);
    } catch {
      // leave the cursor as-is; the button just stays clickable to retry
    } finally {
      setLoadingOlder(false);
    }
  }, [kindFilter, severityFilter, nextBeforeId, loadingOlder]);

  // Live tail: the stream carries no backfill (history comes from the
  // paginated fetch above), so every message here is genuinely new.
  useEffect(() => {
    const es = new EventSource('/api/events/stream');

    es.onmessage = (ev) => {
      try {
        const event: EventJson = JSON.parse(ev.data);
        if (!pausedRef.current) {
          setEvents(prev => {
            const next = [...prev, event];
            return next.length > MAX_EVENTS ? next.slice(next.length - MAX_EVENTS) : next;
          });
          setLiveCount(c => c + 1);
        }
      } catch {
        // ignore malformed
      }
    };

    return () => es.close();
  }, []);

  const filtered = events
    .filter(e => !kindFilter || e.type === kindFilter)
    .filter(e => !severityFilter || e.severity === severityFilter)
    .slice()
    .reverse();

  const chipStyle = (active: boolean, color: string) => ({
    background: active ? color : 'var(--g1)',
    border: `1px solid ${active ? color : 'var(--gb)'}`,
    color: active ? 'var(--bg, #0a0a0a)' : 'var(--t2)',
    borderRadius: 4,
    padding: '2px 10px',
    fontSize: 11,
    cursor: 'pointer',
    fontWeight: active ? 600 : 400,
  });

  return (
    <Layout
      page="events"
      topbarRight={
        <span className="live-row">
          <span style={{ width: 6, height: 6, borderRadius: '50%', display: 'inline-block', background: paused ? 'var(--t3)' : 'var(--green)', marginRight: 5 }} />
          {paused ? 'paused' : `live · ${liveCount} received`}
        </span>
      }
    >
      <div className="content">
        <div className="hero-row">
          <span className="hero-num">{filtered.length}</span>
          <span className="hero-label">
            {kindFilter ? `${kindFilter} events` : severityFilter ? `${severityFilter} events` : 'events loaded'}
          </span>
        </div>

        <div className="card">
          <div className="card-head" style={{ gap: 8, flexWrap: 'wrap' }}>
            <span className="card-label">Event Stream</span>
            <div style={{ display: 'flex', gap: 6, flex: 1, flexWrap: 'wrap', alignItems: 'center' }}>
              {/* Severity chips */}
              <button style={chipStyle(severityFilter === '', 'var(--t2)')} onClick={() => setSeverityFilter('')}>
                All
              </button>
              {SEVERITIES.map(s => (
                <button
                  key={s}
                  style={chipStyle(severityFilter === s, SEVERITY_COLOR[s])}
                  onClick={() => setSeverityFilter(severityFilter === s ? '' : s)}
                >
                  {s.charAt(0).toUpperCase() + s.slice(1)}
                </button>
              ))}

              {/* Divider */}
              <span style={{ width: 1, height: 16, background: 'var(--gb)', margin: '0 2px' }} />

              {/* Kind dropdown. The popup list is a native surface, not composited
                  over the page — it needs an explicit opaque background (the
                  translucent --g1 wash the closed box uses would render as the
                  browser's own default, i.e. white). */}
              <select
                value={kindFilter}
                onChange={e => setKindFilter(e.target.value)}
                style={{
                  background: 'var(--g1)',
                  border: '1px solid var(--gb)',
                  color: 'var(--t1)',
                  borderRadius: 4,
                  padding: '2px 6px',
                  fontSize: 11,
                  cursor: 'pointer',
                }}
              >
                <option value="" style={{ background: 'var(--bg)', color: 'var(--t0)' }}>All types</option>
                {ALL_KINDS.map(k => (
                  <option key={k} value={k} style={{ background: 'var(--bg)', color: 'var(--t0)' }}>{KIND_LABELS[k]}</option>
                ))}
              </select>

              <button
                onClick={() => setPaused(p => !p)}
                style={{
                  background: paused ? 'var(--blue)' : 'var(--g1)',
                  border: '1px solid var(--gb)',
                  color: 'var(--t1)',
                  borderRadius: 4,
                  padding: '2px 10px',
                  fontSize: 11,
                  cursor: 'pointer',
                }}
              >
                {paused ? 'Resume' : 'Pause'}
              </button>
            </div>
          </div>

          <div style={{ overflowY: 'auto', maxHeight: 520 }}>
            <table className="tbl">
              <thead>
                <tr>
                  <th style={{ width: 130 }}>Time</th>
                  <th style={{ width: 200 }}>Type</th>
                  <th>Detail</th>
                </tr>
              </thead>
              <tbody>
                {filtered.length === 0 && !initialLoading && (
                  <tr>
                    <td colSpan={3} style={{ color: 'var(--t2)', padding: '20px 16px' }}>
                      {kindFilter || severityFilter
                        ? 'No matching events'
                        : 'No events yet — waiting for activity…'}
                    </td>
                  </tr>
                )}
                {filtered.map((e, i) => (
                  <tr key={i}>
                    <td
                      style={{ fontFamily: "'JetBrains Mono',monospace", fontSize: 10, color: 'var(--t2)', whiteSpace: 'nowrap' }}
                      title={formatTimestampFull(e.timestamp)}
                    >
                      {formatTimestamp(e.timestamp)}
                    </td>
                    <td>
                      <span
                        style={{
                          fontFamily: "'JetBrains Mono',monospace",
                          fontSize: 11,
                          color: SEVERITY_COLOR[e.severity],
                          fontWeight: 500,
                        }}
                      >
                        {e.type}
                      </span>
                    </td>
                    <td style={{ fontFamily: "'JetBrains Mono',monospace", fontSize: 11, color: 'var(--t1)' }}>
                      {eventDetail(e)}
                    </td>
                  </tr>
                ))}
                {nextBeforeId != null && (
                  <tr>
                    <td colSpan={3} style={{ padding: '10px 16px', textAlign: 'center' }}>
                      <button
                        onClick={loadOlder}
                        disabled={loadingOlder}
                        style={{
                          background: 'var(--g1)',
                          border: '1px solid var(--gb)',
                          color: 'var(--t2)',
                          borderRadius: 4,
                          padding: '4px 14px',
                          fontSize: 11,
                          cursor: loadingOlder ? 'default' : 'pointer',
                        }}
                      >
                        {loadingOlder ? 'Loading…' : 'Load older'}
                      </button>
                    </td>
                  </tr>
                )}
              </tbody>
            </table>
          </div>
        </div>
      </div>
    </Layout>
  );
}
