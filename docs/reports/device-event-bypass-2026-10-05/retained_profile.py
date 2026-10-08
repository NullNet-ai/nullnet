"""CPU and RTNL attribution for the verified retained cross-host fixture."""
import collections
import json
from pathlib import Path
import resource
import sys
import time

import fine_lock_profile as fine
from netlink import NL

side = sys.argv[1]
OUT = fine.OUT
fine.node.Minimal = fine.node.cross.Cross
fine.install_lifecycle(fine.node.Minimal, [('activate', 'setup'), ('retire', 'teardown')])
cpu_enabled = False
want_cpu = False
rows = []
snapshots = []
original_cpu = fine.local_benchmark.cpu


def cpu():
    global cpu_enabled
    usage = resource.getrusage(resource.RUSAGE_SELF)
    snapshots.append({'clock': time.process_time_ns(), 'user': usage.ru_utime, 'system': usage.ru_stime})
    result = original_cpu()
    # Only requests inside the benchmark's timed window enter the CPU table.
    cpu_enabled = want_cpu if len(snapshots) == 1 else False
    return result
fine.local_benchmark.cpu = cpu


def measured(label, function, *args, **kwargs):
    before = time.thread_time_ns()
    try:
        return function(*args, **kwargs)
    finally:
        rows.append((label, time.thread_time_ns() - before))


def request(self, kind, payload, *args, **kwargs):
    if not cpu_enabled:
        return fine.request(self, kind, payload, *args, **kwargs)
    label = getattr(fine.LOCAL, 'phase', 'outside') + '/' + fine.category(self, kind, payload)
    return measured(label, fine.original_request, self, kind, payload, *args, **kwargs)
NL.request = request


def neighbors(self, index):
    if not cpu_enabled:
        return fine.neighbors(self, index)
    role = 'bridge' if self is getattr(fine.LOCAL, 'root', None) else 'container'
    return measured('teardown/' + role + '_neighbor_dump', fine.original_neighbors, self, index)
fine.node.Route.clear_neighbors = neighbors


def benchmark(experiment, request):
    global want_cpu, cpu_enabled
    fine.FILTERS.apply(request['filter'])
    before = fine.FILTERS.counts()
    rows.clear()
    snapshots.clear()
    want_cpu = request.get('accounting', False)
    cpu_enabled = False
    try:
        result = fine.original_benchmark(experiment, request)
    finally:
        cpu_enabled = False
    result['process_delta'] = {k: snapshots[-1][k] - snapshots[0][k] for k in snapshots[0]}
    result['filter'] = request['filter']
    result['accounting_enabled'] = want_cpu
    result['drop_delta'] = {k: v - before[k] for k, v in fine.FILTERS.counts().items()}
    grouped = collections.defaultdict(lambda: {'requests': 0, 'cpu_ns': 0})
    for label, elapsed in rows:
        grouped[label]['requests'] += 1
        grouped[label]['cpu_ns'] += elapsed
    result['request_cpu'] = {k: {**v, 'cpu_ms_per_cycle': v['cpu_ns'] / 1e6 / result['cycles']} for k, v in sorted(grouped.items())}
    (OUT / (request['label'] + '.workload.json')).write_text(json.dumps(result, indent=2) + '\n')
    return result
fine.local_benchmark.benchmark = benchmark

requests = []
for concurrency in [32, 64, 256]:
    for trial in range(3):
        modes = ['baseline', 'all'] if trial % 2 == 0 else ['all', 'baseline']
        for mode in modes:
            requests.append({'op': 'benchmark', 'concurrency': concurrency, 'seconds': 8,
                             'mode': 'mixed', 'trace': False, 'filter': mode,
                             'label': f'retained-c{concurrency}-t{trial}-{mode}'})
for concurrency in [64, 256]:
    for trial in range(3):
        requests.append({'op': 'benchmark', 'concurrency': concurrency, 'seconds': 8,
                         'mode': 'mixed', 'trace': False, 'filter': 'all', 'accounting': True,
                         'label': f'retained-c{concurrency}-cpu-t{trial}'})
        requests.append({'op': 'benchmark', 'concurrency': concurrency, 'seconds': 8,
                         'mode': 'mixed', 'trace': True, 'filter': 'all',
                         'label': f'retained-c{concurrency}-lock-t{trial}'})
requests.append({'op': 'finish'})
fine.run(requests, side)
