import Layout from '../components/Layout';
import { useApiText } from '../hooks/useApi';
import { useStack } from '../StackContext';

export default function Config() {
  const { stack } = useStack();
  const { text, loading, error } = useApiText(`/api/config/${stack}`);

  return (
    <Layout page="config">
      <div className="content">
        <div className="page-title">Configuration</div>
        <div className="page-sub">Raw service configuration for stack <span style={{ fontFamily: "'JetBrains Mono',monospace", color: 'var(--t1)' }}>{stack}</span></div>

        {loading && <div style={{ color: 'var(--t2)', fontSize: 12 }}>Loading…</div>}

        {error && (
          <div style={{ color: 'var(--red)', fontSize: 12 }}>
            Failed to load config: {error}
          </div>
        )}

        {text && (
          <pre className="cfg-pre">{text}</pre>
        )}
      </div>
    </Layout>
  );
}
