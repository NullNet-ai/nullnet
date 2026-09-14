import type { SessionRecordJson } from '../types';
import type { TimeSpan } from '../components/TimeSpanFilter';

export function appendTimeSpan(params: URLSearchParams, span: TimeSpan | null) {
  if (span) {
    params.set('since', String(span.since));
    params.set('until', String(span.until));
  }
}

export function byEnd(a: SessionRecordJson, b: SessionRecordJson): number {
  const ae = a.ended_at;
  const be = b.ended_at;
  if (ae != null && be != null && ae !== be) return be - ae;
  if (ae == null && be != null) return -1;
  if (ae != null && be == null) return 1;
  return b.started_at - a.started_at || b.id - a.id;
}

export function duration(from: number, to: number): string {
  const secs = Math.max(0, to - from);
  if (secs < 60) return `${secs}s`;
  if (secs < 3600) return `${Math.floor(secs / 60)}m ${secs % 60}s`;
  const hours = Math.floor(secs / 3600);
  if (hours < 24) return `${hours}h ${Math.floor((secs % 3600) / 60)}m`;
  return `${Math.floor(hours / 24)}d ${hours % 24}h`;
}
