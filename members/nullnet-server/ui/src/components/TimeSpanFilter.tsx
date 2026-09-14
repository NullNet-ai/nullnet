import { useState } from 'react';

export interface TimeSpan { since: number; until: number }

function localInput(seconds: number): string {
  const date = new Date(seconds * 1000);
  return new Date(date.getTime() - date.getTimezoneOffset() * 60000).toISOString().slice(0, 19);
}

export default function TimeSpanFilter({ value, onChange, defaultLabel = 'Live' }: {
  value: TimeSpan | null;
  onChange: (span: TimeSpan | null) => void;
  defaultLabel?: string;
}) {
  const [editing, setEditing] = useState(value != null);
  const [initial, setInitial] = useState(() => localInput(value?.since ?? Math.floor(Date.now() / 1000) - 3600));
  const [final, setFinal] = useState(() => localInput(value?.until ?? Math.floor(Date.now() / 1000)));
  const since = Math.floor(new Date(initial).getTime() / 1000);
  const until = Math.floor(new Date(final).getTime() / 1000);
  const valid = Number.isFinite(since) && Number.isFinite(until) && since <= until;
  const style = { background: 'var(--bg)', color: 'var(--t1)', border: '1px solid var(--t3)', borderRadius: 4, padding: '4px 6px', fontSize: 11 };
  return (
    <div style={{ display: 'flex', gap: 8, alignItems: 'center', flexWrap: 'wrap', fontSize: 11 }}>
      <select aria-label="Time mode" style={style} value={editing ? 'range' : 'default'} onChange={e => {
        const range = e.target.value === 'range';
        setEditing(range);
        if (!range) onChange(null);
      }}>
        <option value="default">{defaultLabel}</option>
        <option value="range">Time span</option>
      </select>
      {editing && <>
        <label>Initial <input aria-label="Initial time" type="datetime-local" step="1" style={style} value={initial} onChange={e => setInitial(e.target.value)} /></label>
        <label>Final <input aria-label="Final time" type="datetime-local" step="1" style={style} value={final} onChange={e => setFinal(e.target.value)} /></label>
        <button style={style} disabled={!valid} onClick={() => onChange({ since, until })}>Apply</button>
        <span style={{ color: 'var(--t2)' }} title="Includes every session overlapping the range, including sessions spanning the whole range and touching either endpoint.">
          {valid ? `Overlapping range · ${Intl.DateTimeFormat().resolvedOptions().timeZone}` : 'Initial must be at or before final'}
        </span>
      </>}
    </div>
  );
}
