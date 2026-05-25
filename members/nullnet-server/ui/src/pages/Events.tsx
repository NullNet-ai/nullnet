import { useState, useEffect, useRef } from 'react';
import Layout from '../components/Layout';
import type { EventJson } from '../types';

const KIND_COLORS: Record<string, string> = {
  node_connected: 'var(--green)',
  node_disconnected: 'var(--amber)',
  service_registered: 'var(--cyan)',
  service_unregistered: 'var(--amber)',
  setup_started: 'var(--blue)',
  setup_ack: 'var(--green)',
  setup_timeout: 'var(--red, #f87171)',
  session_created: 'var(--green)',
  session_torn_down: 'var(--t2)',
  config_reloaded: 'var(--cyan)',
};

const KIND_LABELS: Record<string, string> = {
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
      return e.stack;
  }
}

function formatTs(unix: number): string {
  return new Date(unix * 1000).toLocaleTimeString([], { hour12: false });
}

const MAX_EVENTS = 500;

export default function Events() {
  const [events, setEvents] = useState<EventJson[]>([]);
  const [filter, setFilter] = useState<string>('');
  const [paused, setPaused] = useState(false);
  const [liveCount, setLiveCount] = useState(0);
  const pausedRef = useRef(paused);
  pausedRef.current = paused;
  const bottomRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    // Initial load from REST endpoint
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

  // Auto-scroll to bottom when new events arrive (unless paused)
  useEffect(() => {
    if (!paused) {
      bottomRef.current?.scrollIntoView({ behavior: 'smooth' });
    }
  }, [events, paused]);

  const filtered = filter
    ? events.filter(e => e.type === filter)
    : events;

  return (
    <Layout
      page="events"
      topbarRight={
        <span className="live-row">
          <span className={`live-dot${paused ? '' : ''}`} style={{ background: paused ? 'var(--t3)' : 'var(--green)' }}></span>
          {paused ? 'paused' : `live · ${liveCount} received`}
        </span>
      }
    >
      <div className="content">
        <div className="hero-row">
          <span className="hero-num">{filtered.length}</span>
          <span className="hero-label">{filter ? `${filter} events` : 'events in buffer'}</span>
        </div>

        <div className="card">
          <div className="card-head" style={{ gap: 8, flexWrap: 'wrap' }}>
            <span className="card-label">Event Stream</span>
            <div style={{ display: 'flex', gap: 6, flex: 1, flexWrap: 'wrap', alignItems: 'center' }}>
              <select
                value={filter}
                onChange={e => setFilter(e.target.value)}
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
                  <th style={{ width: 180 }}>Type</th>
                  <th>Detail</th>
                </tr>
              </thead>
              <tbody>
                {filtered.length === 0 && (
                  <tr>
                    <td colSpan={3} style={{ color: 'var(--t2)', padding: '20px 16px' }}>
                      {filter ? `No ${filter} events` : 'No events yet — waiting for activity…'}
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
                          color: KIND_COLORS[e.type] ?? 'var(--t1)',
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
            <div ref={bottomRef} />
          </div>
        </div>
      </div>
    </Layout>
  );
}
