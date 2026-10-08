"""Reproduce the fine-grained lock table from the two extracted archives."""
import collections
import json
from pathlib import Path
import sys

result = {}
for directory in sys.argv[1:]:
    root = Path(directory)
    runs = [json.loads(p.read_text()) for p in sorted(root.glob('fine-lock-t*-trace1.fine.json'))]
    assert len(runs) == 3
    cycles = sum(r['cycles'] for r in runs)
    rows = collections.defaultdict(lambda: {'requests': 0, 'holds': 0, 'hold_ms': 0, 'wait_ms': 0})
    for run in runs:
        assert not run['marker_errors'] and run['incomplete_scopes'] == run['incomplete_holds'] == 0
        for label, row in run['rows'].items():
            target = rows[label]
            target['requests'] += row['requests']
            target['holds'] += row['holds']['count']
            target['hold_ms'] += row['holds']['sum_ms']
            target['wait_ms'] += row['acquisition_waits']['sum_ms']
    for row in rows.values():
        row['requests_per_cycle'] = row['requests'] / cycles
        row['hold_ms_per_cycle'] = row['hold_ms'] / cycles
        row['mean_hold_ms'] = row['hold_ms'] / row['holds'] if row['holds'] else None
    for name in ['setup/create_veth', 'setup/create_vxlan', 'setup/attach_and_enable_outer',
                 'setup/attach_and_enable_transport', 'setup/enable_bridge',
                 'setup/enable_container_peer', 'teardown/disable_transport',
                 'teardown/disable_bridge', 'setup/add_bridge_address',
                 'setup/add_container_address', 'teardown/remove_bridge_address']:
        assert rows[name]['requests'] == cycles, (name, rows[name], cycles)
    assert rows['teardown/delete_vxlan_and_veth_batch']['requests'] * 256 == cycles
    workloads = [json.loads(p.read_text()) for p in sorted(root.glob('fine-lock-t*-trace0.workload.json'))]
    assert len(workloads) == 3
    preservation = json.loads((root / 'node.json.preservation.json').read_text())
    assert all(preservation['checks'].values())
    host = preservation['before']['services']
    result[root.name] = {'cycles': cycles, 'rows': dict(sorted(rows.items())),
        'fixture_hold_ms_per_cycle': sum(v['hold_ms_per_cycle'] for k, v in rows.items() if k != 'other_actor'),
        'setup_hold_ms_per_cycle': sum(v['hold_ms_per_cycle'] for k, v in rows.items() if k.startswith('setup/')),
        'teardown_hold_ms_per_cycle': sum(v['hold_ms_per_cycle'] for k, v in rows.items() if k.startswith('teardown/')),
        'trace_lock_occupancy': [r['coarse']['lock_occupancy'] for r in runs],
        'untraced_rates': [r['cycles_per_second'] for r in workloads],
        'traced_rates': [r['workload']['cycles_per_second'] for r in runs],
        'preservation': preservation['checks'],
        'drop_counts': json.loads((root / 'active-filter-counts.json').read_text()),
        'trace_integrity': [{'overruns': r['coarse']['overruns'], 'boundary_errors': r['coarse']['boundary_errors'],
                             'marker_errors': r['marker_errors']} for r in runs]}
print(json.dumps(result, indent=2))
