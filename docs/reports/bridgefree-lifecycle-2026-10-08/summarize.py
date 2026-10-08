"""Validate accepted runs and retain additive CPU and RTNL operation budgets."""
import collections
import hashlib
import json
from pathlib import Path
import statistics
import sys

HERE = Path(__file__).resolve().parent


def aggregate_cpu(runs):
    total = sum(r['cycles'] for r in runs)
    rows = collections.defaultdict(lambda: {'requests': 0, 'cpu_ns': 0})
    for run in runs:
        for label, row in run['request_cpu'].items():
            rows[label]['requests'] += row['requests']
            rows[label]['cpu_ns'] += row['cpu_ns']
    return {'cycles': total, 'rows': {k: {**v, 'cpu_ms_per_cycle': v['cpu_ns']/1e6/total} for k, v in sorted(rows.items())},
            'request_cpu_ms_per_cycle': sum(v['cpu_ns'] for v in rows.values())/1e6/total}


def aggregate_locks(runs):
    total = sum(r['cycles'] for r in runs)
    rows = collections.defaultdict(lambda: {'requests': 0, 'hold_ms': 0, 'holds': 0})
    for run in runs:
        assert not run['marker_errors'] and not run['incomplete_scopes'] and not run['incomplete_holds']
        assert not run['coarse']['boundary_errors'] and not any(run['coarse']['overruns'].values())
        for label, row in run['rows'].items():
            rows[label]['requests'] += row['requests']
            rows[label]['hold_ms'] += row['holds']['sum_ms']
            rows[label]['holds'] += row['holds']['count']
    return {'cycles': total, 'rows': {k: {**v, 'hold_ms_per_cycle': v['hold_ms']/total} for k, v in sorted(rows.items())},
            'fixture_hold_ms_per_cycle': sum(v['hold_ms'] for k, v in rows.items() if k != 'other_actor')/total,
            'traces': [{'cycles': r['cycles'], 'marker_errors': r['marker_errors'], 'incomplete_holds': r['incomplete_holds'],
                        'overruns': r['coarse']['overruns'], 'kprobe_profile': r['coarse']['kprobe_profile']} for r in runs]}


def main():
    inputs = [Path(p) for p in sys.argv[1:]]
    assert len(inputs) == 2
    summary = {'date': '2026-10-08', 'scope': 'historical-reset local cross-host endpoints, dedicated per-lease IPsec', 'hosts': {}}
    for host, root in zip(['103', '104'], inputs):
        result = {'conditions': {}}
        for lifecycle in ['unpooled', 'pooled']:
            for variant in ['bridge', 'redirect']:
                condition = lifecycle+'-'+variant
                directory = root/'results'/'historical'/condition
                node = json.loads((directory/'node.json').read_text())
                assert 'error' not in node
                preservation = json.loads((directory/'node.json.preservation.json').read_text())
                assert all(preservation['checks'].values())
                item = {'source_sha256': node['source_sha256'], 'preparation_seconds': node['preparation_seconds'],
                        'preservation': preservation['checks'], 'concurrency': {}}
                for c in [64, 256]:
                    plain, cpu, lock = [], [], []
                    for trial in range(3):
                        prefix = condition+f'-c{c}-t{trial}'
                        for tag, destination in [('plain', plain), ('cpu', cpu)]:
                            row = json.loads((directory/(prefix+'-'+tag+'.workload.json')).read_text())
                            assert row['cycles'] > 0 and row['seconds'] >= (8 if tag == 'plain' else 4)
                            destination.append(row)
                        lock.append(json.loads((directory/(prefix+'-lock.fine.json')).read_text()))
                    cycles = sum(r['cycles'] for r in plain)
                    seconds = sum(r['seconds'] for r in plain)
                    ticks = sum(sum(r['cpu_delta'][i] for i in [0, 1, 2, 5, 6]) for r in plain)
                    item['concurrency'][str(c)] = {
                        'performance': {'cycles': cycles, 'seconds': seconds, 'cycles_per_second': cycles/seconds,
                            'median_cycles_per_second': statistics.median(r['cycles_per_second'] for r in plain),
                            'range': [min(r['cycles_per_second'] for r in plain), max(r['cycles_per_second'] for r in plain)],
                            'host_cpu_ms_per_cycle': ticks*10/cycles,
                            'process_cpu_ms_per_cycle': sum(r['process_cpu_ns'] for r in plain)/1e6/cycles,
                            'drain_seconds': [r['final_drain_seconds'] for r in plain]},
                        'cpu': aggregate_cpu(cpu), 'locks': aggregate_locks(lock)}
                    for kind in ['cpu', 'locks']:
                        rows = item['concurrency'][str(c)][kind]['rows']
                        count = item['concurrency'][str(c)][kind]['cycles']
                        assert rows['teardown/exact_conntrack_delete']['requests'] == count*4
                        for key in ['setup/install_ipsec_state', 'teardown/remove_ipsec_state']:
                            assert rows[key]['requests'] == count*2
                        for key in ['setup/install_ipsec_policy', 'teardown/remove_ipsec_policy', 'setup/add_gateway_address']:
                            assert rows[key]['requests'] == count
                        if lifecycle == 'pooled':
                            assert not any('redirect' in key or 'qdisc' in key or 'gateway_pass' in key for key in rows)
                        if variant == 'redirect':
                            assert rows['setup/add_peer_ip_route']['requests'] == count
                            assert rows['setup/add_peer_gateway_route']['requests'] == count
                            if lifecycle == 'pooled':
                                assert rows['teardown/remove_peer_ip_route']['requests'] == count
                                assert rows['teardown/remove_peer_gateway_route']['requests'] == count
                result['conditions'][condition] = item
        result['comparisons'] = {}
        for lifecycle in ['unpooled', 'pooled']:
            result['comparisons'][lifecycle] = {}
            for c in ['64', '256']:
                b = result['conditions'][lifecycle+'-bridge']['concurrency'][c]
                r = result['conditions'][lifecycle+'-redirect']['concurrency'][c]
                result['comparisons'][lifecycle][c] = {
                    'rate_gain_percent': (r['performance']['cycles_per_second']/b['performance']['cycles_per_second']-1)*100,
                    'cpu_reduction_percent': (1-r['performance']['host_cpu_ms_per_cycle']/b['performance']['host_cpu_ms_per_cycle'])*100,
                    'rtnl_reduction_percent': (1-r['locks']['fixture_hold_ms_per_cycle']/b['locks']['fixture_hold_ms_per_cycle'])*100}
        summary['hosts'][host] = result
    proof = HERE/'packet-results.json'
    if proof.exists():
        summary['packet_proofs'] = json.loads(proof.read_text())
    summary['source_hashes'] = {p.name: hashlib.sha256(p.read_bytes()).hexdigest() for p in HERE.glob('*.py')}
    (HERE/'summary.json').write_text(json.dumps(summary, indent=2)+'\n')
    for host, data in summary['hosts'].items():
        print(host, json.dumps(data['comparisons'], indent=2))


if __name__ == '__main__':
    main()
