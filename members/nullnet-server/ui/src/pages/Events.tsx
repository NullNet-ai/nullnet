import { useState, useEffect, useRef } from 'react';
import Layout from '../components/Layout';
import type { EventJson, Severity } from '../types';

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
  setup_started: 'setup_started',
  setup_ack: 'setup_ack',
  setup_timeout: 'setup_timeout',
  session_created: 'session_created',
  session_torn_down: 'session_torn_down',
  config_reloaded: 'config_reloaded',
  config_stack_removed: 'config_stack_removed',
  all_replicas_removed: 'all_replicas_removed',
  service_reachability_toggled: 'service_reachability_toggled',
  proxy_client_timed_out: 'proxy_client_timed_out',
  sticky_session_reused: 'sticky_session_reused',
  max_networks_limit_enforced: 'max_networks_limit_enforced',
  net_id_pool_exhausted: 'net_id_pool_exhausted',
  proxy_chain_setup_failed: 'proxy_chain_setup_failed',
  backend_trigger_setup_bailed: 'backend_trigger_setup_bailed',
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
  firewall_rules_load_failed: 'firewall_rules_load_failed',
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
  // Proxy info
  proxy_request_routed: 'proxy_request_routed',
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
    case 'config_reloaded':
    case 'config_stack_removed':
      return e.stack;
    case 'all_replicas_removed':
      return `${e.service} · ${e.stack} · ${e.ip}`;
    case 'service_reachability_toggled':
      return `${e.service} · ${e.stack} · ${e.reachable ? 'reachable' : 'unreachable'}`;
    case 'proxy_client_timed_out':
      return `${e.service} · ${e.client_ip}`;
    case 'sticky_session_reused':
      return `${e.service} · ${e.client_ip} via ${e.proxy_ip}`;
    case 'max_networks_limit_enforced':
      return `${e.service} · proxy ${e.proxy_ip} · net ${e.net_id} · limit ${e.limit}`;
    case 'net_id_pool_exhausted':
    case 'proxy_chain_setup_failed':
      return `${e.service} · ${e.client_ip}`;
    case 'backend_trigger_setup_bailed':
      return `${e.service} · port ${e.port}`;
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
    case 'firewall_rules_load_failed':
      return `${e.path} · ${e.error_message}`;
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
    // Proxy info
    case 'proxy_request_routed':
      return `${e.service_name} · ${e.client_ip} → ${e.upstream_ip} · ${e.latency_ms}ms`;
  }
}

function formatTs(unix: number): string {
  return new Date(unix * 1000).toLocaleTimeString([], { hour12: false });
}

const MAX_EVENTS = 500;
const SEVERITIES: Severity[] = ['info', 'warning', 'error'];

export default function Events() {
  const [events, setEvents] = useState<EventJson[]>([]);
  const [kindFilter, setKindFilter] = useState<string>('');
  const [severityFilter, setSeverityFilter] = useState<Severity | ''>('');
  const [paused, setPaused] = useState(false);
  const [liveCount, setLiveCount] = useState(0);
  const pausedRef = useRef(paused);
  pausedRef.current = paused;

  useEffect(() => {
    fetch('/api/events')
      .then(r => r.json())
      .then((data: EventJson[]) => setEvents(data))
      .catch(() => {});
  }, []);

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
    background: active ? color : 'var(--s1)',
    border: `1px solid ${active ? color : 'var(--border)'}`,
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
            {kindFilter ? `${kindFilter} events` : severityFilter ? `${severityFilter} events` : 'events in buffer'}
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
                  onClick={() => setSeverityFilter(prev => prev === s ? '' : s)}
                >
                  {s.charAt(0).toUpperCase() + s.slice(1)}
                </button>
              ))}

              {/* Divider */}
              <span style={{ width: 1, height: 16, background: 'var(--border)', margin: '0 2px' }} />

              {/* Kind dropdown */}
              <select
                value={kindFilter}
                onChange={e => setKindFilter(e.target.value)}
                style={{
                  background: 'var(--s1)',
                  border: '1px solid var(--border)',
                  color: 'var(--t1)',
                  borderRadius: 4,
                  padding: '2px 6px',
                  fontSize: 11,
                  cursor: 'pointer',
                }}
              >
                <option value="">All types</option>
                {ALL_KINDS.map(k => (
                  <option key={k} value={k}>{KIND_LABELS[k]}</option>
                ))}
              </select>

              <button
                onClick={() => setPaused(p => !p)}
                style={{
                  background: paused ? 'var(--blue)' : 'var(--s1)',
                  border: '1px solid var(--border)',
                  color: 'var(--t1)',
                  borderRadius: 4,
                  padding: '2px 10px',
                  fontSize: 11,
                  cursor: 'pointer',
                }}
              >
                {paused ? 'Resume' : 'Pause'}
              </button>
              {filtered.length > 0 && (
                <button
                  onClick={() => setEvents([])}
                  style={{
                    background: 'var(--s1)',
                    border: '1px solid var(--border)',
                    color: 'var(--t2)',
                    borderRadius: 4,
                    padding: '2px 10px',
                    fontSize: 11,
                    cursor: 'pointer',
                  }}
                >
                  Clear
                </button>
              )}
            </div>
          </div>

          <div style={{ overflowY: 'auto', maxHeight: 520 }}>
            <table className="tbl">
              <thead>
                <tr>
                  <th style={{ width: 72 }}>Time</th>
                  <th style={{ width: 200 }}>Type</th>
                  <th>Detail</th>
                </tr>
              </thead>
              <tbody>
                {filtered.length === 0 && (
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
                    <td style={{ fontFamily: "'JetBrains Mono',monospace", fontSize: 10, color: 'var(--t2)', whiteSpace: 'nowrap' }}>
                      {formatTs(e.timestamp)}
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
              </tbody>
            </table>
          </div>
        </div>
      </div>
    </Layout>
  );
}
