"""Combine independent uninstrumented, per-request CPU and RTNL measurements."""
import collections
import json
from pathlib import Path
import sys


def cpu_budget(runs):
    cycles = sum(r['cycles'] for r in runs)
    seconds = sum(r['seconds'] for r in runs)
    return {'cycles': cycles, 'seconds': seconds, 'cycles_per_second': cycles / seconds,
            'rates': [r['cycles_per_second'] for r in runs],
            'host_cpu_ms_per_cycle': sum(sum(r['cpu_delta'][i] for i in [0, 1, 2, 5, 6]) for r in runs) * 10 / cycles,
            'process_cpu_ms_per_cycle': sum(r['process_delta']['clock'] for r in runs) / 1e6 / cycles}


result = {}
for directory in sys.argv[1:]:
    root = Path(directory)
    workloads = [json.loads(p.read_text()) for p in sorted(root.glob('retained-*.workload.json'))]
    assert len(workloads) == 30, len(workloads)
    assert json.loads((root / 'metadata.json').read_text())['clock_ticks'] == 100
    preservation = json.loads((root / 'node.json.preservation.json').read_text())
    assert all(preservation['checks'].values())
    host = {'performance': {}, 'cpu': {}, 'locks': {}, 'preservation': preservation['checks']}
    for concurrency in [32, 64, 256]:
        host['performance'][str(concurrency)] = {}
        for mode in ['baseline', 'all']:
            runs = [r for r in workloads if r['concurrency'] == concurrency and r['filter'] == mode and not r['trace'] and not r['accounting_enabled']]
            assert len(runs) == 3
            host['performance'][str(concurrency)][mode] = cpu_budget(runs)
    for concurrency in [64, 256]:
        runs = [r for r in workloads if r['concurrency'] == concurrency and r['accounting_enabled']]
        assert len(runs) == 3
        cycles = sum(r['cycles'] for r in runs)
        rows = collections.defaultdict(lambda: {'requests': 0, 'cpu_ns': 0})
        for run in runs:
            for label, row in run['request_cpu'].items():
                rows[label]['requests'] += row['requests']
                rows[label]['cpu_ns'] += row['cpu_ns']
        for row in rows.values():
            row['requests_per_cycle'] = row['requests'] / cycles
            row['cpu_ms_per_cycle'] = row['cpu_ns'] / 1e6 / cycles
        expected = ['setup/add_container_address', 'setup/add_bridge_address', 'setup/enable_container_peer',
                    'setup/attach_and_enable_outer', 'setup/attach_and_enable_transport', 'setup/enable_bridge',
                    'setup/enable_transport', 'setup/readiness_transport', 'setup/readiness_outer',
                    'setup/readiness_bridge', 'setup/readiness_inner', 'teardown/disable_transport',
                    'teardown/disable_bridge', 'teardown/detach_outer', 'teardown/detach_transport',
                    'teardown/disable_container_peer', 'teardown/bridge_neighbor_dump',
                    'teardown/container_neighbor_dump', 'teardown/remove_bridge_address',
                    'teardown/remove_container_address']
        for label in expected:
            assert rows[label]['requests'] == cycles, (label, rows[label], cycles)
        assert rows['teardown/scoped_conntrack_delete']['requests'] == cycles * 4
        assert not any('create_' in k or 'delete_vxlan' in k for k in rows)
        host['cpu'][str(concurrency)] = {**cpu_budget(runs), 'rows': dict(sorted(rows.items())),
            'setup_request_cpu_ms_per_cycle': sum(r['cpu_ms_per_cycle'] for k, r in rows.items() if k.startswith('setup/')),
            'teardown_request_cpu_ms_per_cycle': sum(r['cpu_ms_per_cycle'] for k, r in rows.items() if k.startswith('teardown/'))}
        traces = [json.loads(p.read_text()) for p in sorted(root.glob(f'retained-c{concurrency}-lock-t*.fine.json'))]
        assert len(traces) == 3
        cycles = sum(r['cycles'] for r in traces)
        holds = collections.defaultdict(lambda: {'requests': 0, 'holds': 0, 'hold_ms': 0, 'wait_ms': 0})
        for trace in traces:
            assert not trace['marker_errors'] and not trace['coarse']['boundary_errors']
            assert not any(trace['coarse']['overruns'].values())
            for label, row in trace['rows'].items():
                holds[label]['requests'] += row['requests']
                holds[label]['holds'] += row['holds']['count']
                holds[label]['hold_ms'] += row['holds']['sum_ms']
                holds[label]['wait_ms'] += row['acquisition_waits']['sum_ms']
        for row in holds.values():
            row['requests_per_cycle'] = row['requests'] / cycles
            row['hold_ms_per_cycle'] = row['hold_ms'] / cycles
        for label in expected:
            assert holds[label]['requests'] == cycles, (label, holds[label], cycles)
        host['locks'][str(concurrency)] = {'cycles': cycles, 'rows': dict(sorted(holds.items())),
            'fixture_hold_ms_per_cycle': sum(r['hold_ms_per_cycle'] for k, r in holds.items() if k != 'other_actor'),
            'setup_hold_ms_per_cycle': sum(r['hold_ms_per_cycle'] for k, r in holds.items() if k.startswith('setup/')),
            'teardown_hold_ms_per_cycle': sum(r['hold_ms_per_cycle'] for k, r in holds.items() if k.startswith('teardown/')),
            'occupancies': [t['coarse']['lock_occupancy'] for t in traces],
            'rates': [t['workload']['cycles_per_second'] for t in traces],
            'rcu_inside_fixture_holds_ms_per_cycle': sum(t['coarse']['rcu_union_inside_hold_ms'].get('fixture', 0) for t in traces) / cycles,
            'trace_integrity': [{'overruns': t['coarse']['overruns'], 'boundary_errors': t['coarse']['boundary_errors'],
                                 'marker_errors': t['marker_errors']} for t in traces]}
    host['total_timed_cycles'] = sum(w['cycles'] for w in workloads)
    host['filter_counts'] = json.loads((root / 'active-filter-counts.json').read_text())
    result[root.name] = host
print(json.dumps(result, indent=2))
