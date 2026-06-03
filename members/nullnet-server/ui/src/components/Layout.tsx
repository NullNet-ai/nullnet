import { NavLink } from 'react-router-dom';
import { useStack } from '../StackContext';
import { useApi } from '../hooks/useApi';
import type { SessionJson } from '../types';

type Page = 'dashboard' | 'services' | 'nodes' | 'sessions' | 'pool' | 'config' | 'events';

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
      { id: 'topology', icon: '⬡', label: 'Topology', to: null },
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
      { id: 'config', icon: '⚙', label: 'Config', to: '/config' },
    ],
  },
];

export default function Layout({ page, topbarRight, children }: Props) {
  const { stack, setStack, editing, setEditing } = useStack();
  const { data: sessions } = useApi<SessionJson[]>('/api/sessions', 5000);

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
          <div className="foot-stack">
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
              <span className="foot-stack-name" title="Click to change stack" style={{ cursor: 'pointer' }} onClick={() => setEditing(true)}>{stack}</span>
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
