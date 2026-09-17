import { Link } from 'react-router-dom';
import Layout from '../components/Layout';
import { useApi } from '../hooks/useApi';
import RefreshStatus from '../components/RefreshStatus';
import { useStack } from '../StackContext';
import type { NodeJson, ServiceJson } from '../types';

interface SessionCounts {
  active: number;
  busiest_services: { service: string; count: number }[];
  allowed: number;
  blocked: number;
  allowed_by_direction: { ingress: number; egress: number };
  blocked_by_direction: { ingress: number; egress: number };
  egress: number;
  ingress: number;
  backend: number;
}

function SessionCountCard({ kind, counts }: { kind: 'active' | 'allowed' | 'blocked'; counts: SessionCounts | null }) {
  const total = counts?.[kind];
  const directions = kind === 'active' ? counts : counts?.[`${kind}_by_direction`];
  const parts = [
    { direction: 'ingress', label: 'Ingress', count: directions?.ingress ?? 0, color: 'var(--amber)' },
    { direction: 'egress', label: 'Egress', count: directions?.egress ?? 0, color: 'var(--purple)' },
    ...(kind === 'active' ? [{ direction: 'backend', label: 'Backend', count: counts?.backend ?? 0, color: 'var(--t0)' }] : []),
  ];
  const sum = parts.reduce((value, part) => value + part.count, 0);
  const link = kind === 'active' ? '/sessions?status=active' : `/sessions?policy=${kind}`;
  const point = (angle: number, radius = 62) => `${70 + radius * Math.cos(angle)},${70 + radius * Math.sin(angle)}`;
  return <div className="stat glass policy-card">
    <div className="policy-summary">
      <Link to={link} className="policy-total">
        <div className="stat-label">{kind === 'active' ? 'Active' : kind === 'allowed' ? 'Allowed' : 'Blocked'} sessions</div>
        <div className="stat-value" style={{ color: kind === 'blocked' ? 'var(--red)' : 'var(--green)' }}>{total ?? '—'}</div>
        <div className="stat-sub">{kind === 'active' ? 'all directions' : 'all retained history'}</div>
      </Link>

    </div>
    <div className="policy-chart">
    <svg viewBox="0 0 140 140" className="policy-pie" aria-label={`${kind} sessions by direction`}>
      {sum === 0 && <><circle cx="70" cy="70" r="54" fill="none" stroke="var(--gb)" strokeWidth="16" /><text x="70" y="70" textAnchor="middle" dominantBaseline="middle" fill="var(--t2)" fontSize="11">{directions ? 'No sessions' : 'Loading…'}</text></>}
      {parts.map((part, index) => {
        if (part.count === 0 || sum === 0) return null;
        const start = -Math.PI / 2 + (parts.slice(0, index).reduce((value, previous) => value + previous.count, 0) / sum * Math.PI * 2);
        const end = start + part.count / sum * Math.PI * 2;
        const mid = (start + end) / 2;
        return <Link key={part.direction} to={`${link}&direction=${part.direction}`} className="policy-slice"
          aria-label={`${kind} ${part.label}: ${part.count} (${Math.round(part.count / sum * 100)}%)`}>
          <title>{`${part.label}: ${part.count} (${Math.round(part.count / sum * 100)}%)`}</title>
          <path d={`M${point(start)} A62,62 0 0 1 ${point(mid)} A62,62 0 0 1 ${point(end)} L${point(end, 46)} A46,46 0 0 0 ${point(mid, 46)} A46,46 0 0 0 ${point(start, 46)} Z`} fill={part.color} />
        </Link>;
      })}
    </svg>
      <div className="policy-legend">
        {parts.map(part => <Link key={part.direction} to={`${link}&direction=${part.direction}`}>
          <span className="dot" style={{ background: part.color }} />{part.label}
          <span style={{ color: part.color }}>{directions ? part.count : '—'}</span>
        </Link>)}
      </div>
    </div>
  </div>;
}

function DashboardCards({ stack }: { stack: string }) {
  const path = encodeURIComponent(stack);
  const { data: counts, error: countsError, updatedAt: countsUpdated } = useApi<SessionCounts>(`/api/sessions/${path}/count?include_policy=true&include_busiest=true`, 5000);
  const { data: services, error: servicesError, updatedAt: servicesUpdated } = useApi<ServiceJson[]>(`/api/services/${path}`, 5000);
  const { data: nodes, error: nodesError, updatedAt: nodesUpdated } = useApi<NodeJson[]>(`/api/nodes/${path}`, 5000);
  const registered = services?.filter(service => service.registered).length;
  const unregistered = services ? services.length - registered! : 0;
  const cards = [
    { label: 'Services', value: registered, to: '/services', color: 'var(--green)', sub: services ? (services.length === 0 ? 'no services configured' : unregistered > 0 ? `${unregistered} unregistered` : 'all services registered') : 'registered services', subColor: unregistered > 0 ? 'var(--red)' : undefined, total: services?.length },
    { label: 'Nodes', value: nodes?.length, to: '/nodes', color: 'var(--green)', sub: 'connected' },
  ];
  const updatedAt = countsUpdated != null && servicesUpdated != null && nodesUpdated != null
    ? Math.min(countsUpdated, servicesUpdated, nodesUpdated) : null;
  return <Layout page="dashboard" topbarRight={<RefreshStatus updatedAt={updatedAt} failed={!!(countsError || servicesError || nodesError)} />}><div className="content">
    {(countsError || servicesError || nodesError) && <div className="modal-err" role="alert">Could not refresh dashboard: {countsError || servicesError || nodesError}</div>}
    {[{ label: 'Active sessions', cards: [], className: 'dashboard-active' },
      { label: 'Session policy', cards: [], className: 'dashboard-policy' },
      { label: 'Infrastructure', cards, className: 'dashboard-infrastructure' }].map(group =>
    <section key={group.label} aria-label={group.label} className="dashboard-section">
      <h2 className="dashboard-section-title">{group.label}</h2>
      <div className={`stats dashboard-stats ${group.className}`}>
      {group.label === 'Active sessions' && <>
        <SessionCountCard kind="active" counts={counts} />
        <div className="stat glass">
          <div className="stat-label">Busiest services · active sessions</div>
          {!counts ? <div className="stat-sub">Loading…</div> : counts.busiest_services.length === 0
            ? <div className="stat-sub">No active sessions</div>
            : <ol className="busiest-services">
              {counts.busiest_services.map(service => <li key={service.service}>
                <Link to={`/sessions?status=active&service=${encodeURIComponent(service.service)}`}>
                  <span title={service.service}>{service.service}</span><span aria-hidden="true">·</span><strong>{service.count}</strong>
                </Link>
              </li>)}
            </ol>}
        </div>
      </>}
      {group.label === 'Session policy' && <>
        <SessionCountCard kind="allowed" counts={counts} />
        <SessionCountCard kind="blocked" counts={counts} />
      </>}
      {group.cards.map(card => <Link key={card.label} to={card.to} className="stat glass stat-link">
        <div className="stat-label">{card.label}</div>
        <div className="stat-value" style={{ color: card.color }}>
          {card.value ?? '—'}{card.total != null && <span className="denom">/{card.total}</span>}
        </div>
        <div className="stat-sub" style={{ color: card.subColor }}>{card.sub}</div>
        {card.total != null && card.total > 0 && <div className="registration-bar"
          role="meter" aria-label="Registered services" aria-valuemin={0} aria-valuemax={card.total} aria-valuenow={registered}
          aria-valuetext={`${registered} of ${card.total} services registered`}>
          <div style={{ width: `${registered! / card.total * 100}%` }} />
        </div>}
      </Link>)}
      </div>
    </section>)}
  </div></Layout>;
}

export default function Dashboard() {
  const { stack } = useStack();
  return <DashboardCards key={stack} stack={stack} />;
}
