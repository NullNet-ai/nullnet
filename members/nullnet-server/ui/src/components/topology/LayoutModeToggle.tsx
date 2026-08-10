import type { LayoutMode } from './types';

interface Props {
  mode: LayoutMode;
  onChange: (mode: LayoutMode) => void;
}

const OPTIONS: { mode: LayoutMode; label: string }[] = [
  { mode: 'layered', label: 'layered' },
  { mode: 'matrix', label: 'matrix' },
];

// Small segmented control for switching between the available topology
// layouts (layered default, matrix) — styled to match ZoomFrame's existing
// "reset view" overlay button.
export default function LayoutModeToggle({ mode, onChange }: Props) {
  return (
    <div
      style={{
        display: 'flex',
        background: 'rgba(255,255,255,.06)',
        border: '1px solid rgba(255,255,255,.12)',
        borderRadius: 5,
        overflow: 'hidden',
      }}
    >
      {OPTIONS.map(opt => {
        const active = opt.mode === mode;
        return (
          <button
            key={opt.mode}
            onClick={() => onChange(opt.mode)}
            style={{
              background: active ? 'rgba(91,156,246,.25)' : 'transparent',
              border: 'none',
              color: active ? 'rgba(255,255,255,.9)' : 'rgba(255,255,255,.5)',
              fontSize: 10,
              padding: '3px 7px',
              whiteSpace: 'nowrap',
              cursor: 'pointer',
            }}
          >
            {opt.label}
          </button>
        );
      })}
    </div>
  );
}
