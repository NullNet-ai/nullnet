"""Correlate synchronous requests with actual RTNL acquisitions/releases."""
import contextlib
import io
import json
import os
from pathlib import Path
import resource
import runpy
import signal
import struct
import sys
import threading

REPORTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(REPORTS / 'kernel-parallelism-2026-10-01'))
import node
import local_benchmark
import trace_lock
from netlink import NL
from socket_filter import Filters

OUT = Path(sys.argv[2])
OUT.mkdir(parents=True, exist_ok=True)
Path('/tmp/nn-kernel-estimate').mkdir(exist_ok=True)
trace_lock.OUT = OUT
trace_lock.LOCKS = []
trace_lock.FUNCTIONS = ['rtnl_newlink', 'rtnl_setlink', 'rtnl_dellink',
                        'inet_rtm_newaddr', 'inet_rtm_deladdr',
                        'synchronize_rcu', 'synchronize_rcu_expedited',
                        'synchronize_net', 'rcu_barrier']
LOCAL = threading.local()
marker = None
FILTERS = None


def attrs(data):
    offset = 0
    while offset + 4 <= len(data):
        size, kind = struct.unpack_from('HH', data, offset)
        assert size >= 4 and offset + size <= len(data)
        yield kind & 16383, data[offset + 4:offset + size]
        offset += (size + 3) & ~3


def category(sock, kind, payload):
    protocol = sock.s.proto
    if protocol == 6:
        return 'ipsec_' + str(kind)
    if protocol == 12:
        return 'scoped_conntrack_delete'
    phase = getattr(LOCAL, 'phase', 'batch')
    endpoint = getattr(LOCAL, 'endpoint', None)
    root = getattr(LOCAL, 'root', None)
    ix = endpoint['indices'] if endpoint else {}
    container = sock is not root and endpoint is not None
    if kind == 16:
        data = dict(attrs(payload[16:]))
        name = dict(attrs(data.get(18, b''))).get(1, b'unknown').rstrip(b'\0').decode()
        return 'create_' + name
    if kind == 17:
        return 'delete_vxlan_and_veth_batch'
    if kind == 18:
        name = dict(attrs(payload[16:])).get(3, b'').rstrip(b'\0').decode()
        role = next((key for key, value in endpoint['names'].items() if value == name), 'unknown') if endpoint else 'unknown'
        if phase == 'teardown':
            return 'verify_retained_' + role
        return ('readiness_' if role in ix else 'creation_lookup_') + role
    if kind == 19:
        index, flags = struct.unpack_from('II', payload, 4)
        role = 'container_peer' if container else next((key for key in ['outer', 'transport', 'bridge'] if ix.get(key) == index), 'unknown')
        data = dict(attrs(payload[16:]))
        if 10 in data:
            action = 'detach' if struct.unpack('I', data[10])[0] == 0 else 'attach_and_enable'
        else:
            action = 'enable' if flags & 1 else 'disable'
        return action + '_' + role
    if kind in (20, 21):
        return ('add_' if kind == 20 else 'remove_') + ('container_address' if container else 'bridge_address')
    return {36: 'tc_qdisc', 44: 'tc_filter', 29: 'neighbor_delete'}.get(kind, 'route_' + str(kind))


@contextlib.contextmanager
def request_scope(label):
    if marker is None:
        yield
        return
    serial = getattr(LOCAL, 'serial', 0) + 1
    LOCAL.serial = serial
    ident = f'{threading.get_native_id()}-{serial}'
    os.write(marker, f'NNREQ BEGIN {ident} {label}\n'.encode())
    try:
        yield
    finally:
        os.write(marker, f'NNREQ END {ident} {label}\n'.encode())


original_request = NL.request
def request(self, kind, payload, *args, **kwargs):
    if marker is None:
        return original_request(self, kind, payload, *args, **kwargs)
    label = getattr(LOCAL, 'phase', 'teardown') + '/' + category(self, kind, payload)
    with request_scope(label):
        return original_request(self, kind, payload, *args, **kwargs)
NL.request = request

original_neighbors = node.Route.clear_neighbors
def neighbors(self, index):
    role = 'bridge' if self is getattr(LOCAL, 'root', None) else 'container'
    with request_scope('teardown/' + role + '_neighbor_dump'):
        return original_neighbors(self, index)
node.Route.clear_neighbors = neighbors

def install_lifecycle(target, methods):
    for method, phase in methods:
        original = getattr(target, method)
        def wrap(self, slot, *args, _fn=original, _phase=phase, **kwargs):
            LOCAL.phase, LOCAL.endpoint, LOCAL.root = _phase, slot['sides'][0], self.io()[0]
            try:
                return _fn(self, slot, *args, **kwargs)
            finally:
                LOCAL.phase, LOCAL.endpoint, LOCAL.root = 'outside', None, None
        setattr(target, method, wrap)

original_start, original_stop = trace_lock.trace_start, trace_lock.trace_stop
def start(label, pid):
    global marker
    result = original_start(label, pid)
    trace_lock.write(trace_lock.INSTANCE / 'tracing_on', 0)
    trace_lock.write(trace_lock.INSTANCE / 'events/sched/sched_switch/enable', 0)
    trace_lock.write(trace_lock.INSTANCE / 'events/sched/sched_switch/trigger', '!stacktrace')
    trace_lock.write(trace_lock.INSTANCE / 'buffer_size_kb', 65536)
    marker = os.open(trace_lock.INSTANCE / 'trace_marker', os.O_WRONLY)
    trace_lock.write(trace_lock.INSTANCE / 'trace', '')
    meta = json.loads((OUT / 'trace-meta.json').read_text())
    import time
    meta['start'] = time.monotonic()
    (OUT / 'trace-meta.json').write_text(json.dumps(meta))
    trace_lock.write(trace_lock.INSTANCE / 'tracing_on', 1)
    return result

def stop(label, pid):
    global marker
    os.close(marker)
    marker = None
    trace_lock.write(trace_lock.INSTANCE / 'tracing_on', 0)
    trace_lock.write(trace_lock.INSTANCE / 'events/sched/sched_switch/trigger', 'stacktrace if prev_comm == "nn-est-worker" || prev_comm == "nn-est-main"')
    return original_stop(label, pid)
trace_lock.trace_start, trace_lock.trace_stop = start, stop

original_benchmark = local_benchmark.benchmark
def benchmark(experiment, request):
    result = original_benchmark(experiment, request)
    (OUT / (request['label'] + '.workload.json')).write_text(json.dumps(result, indent=2) + '\n')
    return result
local_benchmark.benchmark = benchmark

def interrupted(signum, frame):
    raise KeyboardInterrupt


def run(requests, side):
    global FILTERS
    soft, hard = resource.getrlimit(resource.RLIMIT_NOFILE)
    resource.setrlimit(resource.RLIMIT_NOFILE, (min(65536, hard), hard))
    signal.signal(signal.SIGTERM, interrupted)
    with (OUT / 'runner.log').open('w', buffering=1) as log, contextlib.redirect_stdout(log), contextlib.redirect_stderr(log):
        try:
            runpy.run_path(str(Path(__file__).with_name('discover.py')))
            inventory = json.loads(Path('/tmp/nn-event-sockets.json').read_text())
            (OUT / 'socket-inventory.json').write_text(json.dumps(inventory, indent=2) + '\n')
            FILTERS = Filters(inventory, OUT / 'filters.json')
            FILTERS.apply('all')
            sys.stdin = io.StringIO(''.join(json.dumps(r) + '\n' for r in requests))
            sys.argv = ['node.py', '--side', side, '--slots', '512', '--workers', '512',
                        '--variant', 'bridge', '--output', str(OUT / 'node.json')]
            node.main()
            assert 'error' not in json.loads((OUT / 'node.json').read_text())
            assert all(json.loads((OUT / 'node.json.preservation.json').read_text())['checks'].values())
            FILTERS.save()
            (OUT / 'active-filter-counts.json').write_text(json.dumps(FILTERS.counts(), indent=2) + '\n')
            (OUT / 'done').write_text('complete\n')
        finally:
            if FILTERS:
                FILTERS.close()


if __name__ == '__main__':
    install_lifecycle(node.Minimal, [('activate', 'setup'), ('retire_start', 'teardown'), ('retire_finish', 'teardown')])
    requests = []
    for trial in range(3):
        for traced in [False, True]:
            requests.append({'op': 'benchmark', 'concurrency': 256, 'seconds': 8,
                             'mode': 'mixed', 'trace': traced,
                             'label': f'fine-lock-t{trial}-trace{int(traced)}'})
    requests.append({'op': 'finish'})
    run(requests, sys.argv[1])
