import { NavLink } from 'react-router-dom';
import { useStack } from '../StackContext';
import { useApi } from '../hooks/useApi';
import type { SessionJson } from '../types';
import { useRef, useState, useEffect } from 'react';

type Page = 'dashboard' | 'services' | 'nodes' | 'sessions' | 'pool' | 'config' | 'certificates' | 'events';

interface Props {
  page: Page;
  topbarRight?: React.ReactNode;
  children: React.ReactNode;
}

const NAV = [
  {
    group: 'Overview',
    items: [
      { id: 'dashboard', icon: '⊞', label: 'Dashboard', to: '/' },
    ],
  },
  {
    group: 'State',
    items: [
      { id: 'services', icon: '◈', label: 'Services', to: '/services' },
      { id: 'sessions', icon: '⌾', label: 'Sessions', to: '/sessions', live: true },
      { id: 'nodes', icon: '◉', label: 'Nodes', to: '/nodes' },
      { id: 'pool', icon: '▦', label: 'Pool', to: '/pool' },
    ],
  },
  {
    group: 'Ops',
    items: [
      { id: 'events', icon: '≡', label: 'Events', to: '/events' },
      { id: 'certificates', icon: '⛨', label: 'Certificates', to: '/certificates' },
      { id: 'config', icon: '⚙', label: 'Config', to: '/config' },
    ],
  },
];

export default function Layout({ page, topbarRight, children }: Props) {
  const { stack, setStack, editing, setEditing } = useStack();
  const { data: sessions } = useApi<SessionJson[]>(`/api/sessions/${stack}`, 5000);
  const { data: availableStacks } = useApi<string[]>('/api/stacks', 10000);
  const [dropdownOpen, setDropdownOpen] = useState(false);
  const dropdownRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!dropdownOpen) return;
    function onClickOutside(e: MouseEvent) {
      if (dropdownRef.current && !dropdownRef.current.contains(e.target as Node)) {
        setDropdownOpen(false);
      }
    }
    document.addEventListener('mousedown', onClickOutside);
    return () => document.removeEventListener('mousedown', onClickOutside);
  }, [dropdownOpen]);

  const sessionCount = sessions?.length ?? null;

  return (
    <>
      <nav className="sidebar">
        <div className="logo">
          <div className="logo-name">Nullnet</div>
          <div className="logo-sub">Control Plane</div>
        </div>

        <div className="nav">
          {NAV.map(group => (
            <div key={group.group} className="nav-group">
              <div className="nav-group-label">{group.group}</div>
              {group.items.map(item => {
                if (!item.to) {
                  return (
                    <span key={item.id} className="nav-a disabled">
                      <span className="nav-icon">{item.icon}</span>
                      {item.label}
                    </span>
                  );
                }
                return (
                  <NavLink
                    key={item.id}
                    to={item.to}
                    end={item.to === '/'}
                    className={({ isActive }) => 'nav-a' + (isActive ? ' active' : '')}
                  >
                    <span className="nav-icon">{item.icon}</span>
                    {item.label}
                    {item.id === 'sessions' && sessionCount !== null && (
                      <span className={'nav-count' + ('live' in item && item.live ? ' live' : '')}>{sessionCount}</span>
                    )}
                  </NavLink>
                );
              })}
            </div>
          ))}
        </div>

        <div className="foot">
          <div className="foot-row"><span className="dot dot-g"></span>HTTP · 8080</div>
          <div className="foot-stack" ref={dropdownRef} style={{ position: 'relative' }}>
            <span>stack:</span>
            {editing ? (
              <input
                defaultValue={stack}
                autoFocus
                onBlur={e => { setStack(e.target.value.trim() || stack); setEditing(false); }}
                onKeyDown={e => {
                  if (e.key === 'Enter') { setStack((e.target as HTMLInputElement).value.trim() || stack); setEditing(false); }
                  if (e.key === 'Escape') setEditing(false);
                }}
              />
            ) : (
              <span
                className="foot-stack-name"
                title="Click to select or type a stack"
                style={{ cursor: 'pointer', display: 'flex', alignItems: 'center', gap: 3 }}
                onClick={() => setDropdownOpen(o => !o)}
                onDoubleClick={() => { setDropdownOpen(false); setEditing(true); }}
              >
                {stack}
                <span style={{ fontSize: 8, color: 'var(--t3)', lineHeight: 1 }}>▾</span>
              </span>
            )}
            {dropdownOpen && availableStacks && availableStacks.length > 0 && (
              <div style={{
                position: 'absolute',
                bottom: '100%',
                left: 0,
                marginBottom: 4,
                background: 'var(--surface, #1a1d24)',
                border: '1px solid rgba(255,255,255,.12)',
                borderRadius: 6,
                padding: '4px 0',
                minWidth: 140,
                zIndex: 100,
                boxShadow: '0 4px 16px rgba(0,0,0,.5)',
              }}>
                {availableStacks.map(s => (
                  <div
                    key={s}
                    onClick={() => { setStack(s); setDropdownOpen(false); }}
                    style={{
                      padding: '5px 12px',
                      fontSize: 11,
                      cursor: 'pointer',
                      color: s === stack ? 'var(--accent, #5b9cf6)' : 'rgba(255,255,255,.75)',
                      background: s === stack ? 'rgba(91,156,246,.08)' : 'transparent',
                      fontFamily: "'JetBrains Mono', monospace",
                    }}
                    onMouseEnter={e => { if (s !== stack) (e.target as HTMLElement).style.background = 'rgba(255,255,255,.05)'; }}
                    onMouseLeave={e => { (e.target as HTMLElement).style.background = s === stack ? 'rgba(91,156,246,.08)' : 'transparent'; }}
                  >
                    {s}
                  </div>
                ))}
                <div
                  style={{
                    padding: '5px 12px',
                    fontSize: 10,
                    cursor: 'pointer',
                    color: 'var(--t3)',
                    borderTop: '1px solid rgba(255,255,255,.06)',
                    marginTop: 2,
                  }}
                  onClick={() => { setDropdownOpen(false); setEditing(true); }}
                  onMouseEnter={e => { (e.target as HTMLElement).style.color = 'rgba(255,255,255,.5)'; }}
                  onMouseLeave={e => { (e.target as HTMLElement).style.color = 'var(--t3)'; }}
                >
                  type custom…
                </div>
              </div>
            )}
          </div>
        </div>
      </nav>

      <div className="main">
        <div className="topbar">
          <div className="topbar-path">
            <span>nullnet</span>
            <span style={{ color: 'var(--t3)' }}>·</span>
            <span className="pg">{page.charAt(0).toUpperCase() + page.slice(1)}</span>
          </div>
          <div className="topbar-right">
            {topbarRight}
            <span className="pill">{stack}</span>
          </div>
        </div>
        {children}
      </div>
    </>
  );
}
