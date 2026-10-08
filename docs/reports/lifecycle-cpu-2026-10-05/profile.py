"""Attribute synchronous lifecycle work using Linux per-thread CPU accounting."""
import collections
import json
import resource
import struct
import threading
import time

ENABLED = False
ROWS = []
LOCAL = threading.local()


def usage():
    r = resource.getrusage(resource.RUSAGE_THREAD)
    return r.ru_utime, r.ru_stime, r.ru_nvcsw, r.ru_nivcsw


def attrs(data):
    offset = 0
    while offset + 4 <= len(data):
        size, kind = struct.unpack_from('HH', data, offset)
        assert size >= 4
        yield kind & 16383, data[offset + 4:offset + size]
        offset += (size + 3) & ~3


def label(sock, kind, payload):
    protocol = sock.s.proto
    if protocol == 6:
        return getattr(sock, 'step', 'xfrm-' + str(kind))
    if protocol == 12:
        return 'conntrack_delete'
    if kind == 16:
        a = dict(attrs(payload[16:]))
        link = dict(attrs(a.get(18, b''))).get(1, b'unknown').rstrip(b'\0').decode()
        return 'create_' + link
    if kind == 19:
        a = dict(attrs(payload[16:]))
        flags = struct.unpack_from('I', payload, 8)[0]
        return ('enable_attach' if 10 in a else 'enable' if flags & 1 else 'disable')
    return {17: 'delete_link_batch', 18: 'getlink_readiness', 20: 'address_add',
            21: 'address_remove', 36: 'tc_qdisc_add', 44: 'tc_filter_add',
            29: 'neighbor_delete'}.get(kind, 'route-' + str(kind))


def measured(category, fn, *args, **kwargs):
    if not ENABLED:
        return fn(*args, **kwargs)
    before = usage()
    cpu = time.thread_time_ns()
    wall = time.monotonic_ns()
    try:
        return fn(*args, **kwargs)
    finally:
        elapsed = time.monotonic_ns() - wall
        active = time.thread_time_ns() - cpu
        after = usage()
        ROWS.append((getattr(LOCAL, 'phase', 'outside'), category, elapsed, active,
                     after[0] - before[0], after[1] - before[1],
                     after[2] - before[2], after[3] - before[3]))


def install(node):
    from netlink import NL
    original = NL.request

    def request(self, kind, payload, *args, **kwargs):
        if not ENABLED:
            return original(self, kind, payload, *args, **kwargs)
        return measured(label(self, kind, payload), original, self, kind, payload, *args, **kwargs)

    NL.request = request
    neighbors = node.Route.clear_neighbors
    node.Route.clear_neighbors = lambda self, index: measured('neighbor_dump_inclusive', neighbors, self, index)
    for name, phase in [('activate', 'setup'), ('retire_start', 'teardown'), ('retire_finish', 'teardown')]:
        original_method = getattr(node.Minimal, name)

        def wrap(self, *args, _fn=original_method, _phase=phase, **kwargs):
            LOCAL.phase = _phase
            try:
                return measured('lifecycle_inclusive', _fn, self, *args, **kwargs)
            finally:
                LOCAL.phase = 'outside'

        setattr(node.Minimal, name, wrap)


def summarize():
    grouped = collections.defaultdict(list)
    for phase, category, *values in ROWS:
        if category == 'delete_link_batch':
            phase = 'teardown'
        grouped[phase, category].append(values)
    result = {}
    for (phase, category), rows in sorted(grouped.items()):
        wall = sorted(row[0] / 1e6 for row in rows)
        result[phase + '/' + category] = {
            'calls': len(rows), 'wall_sum_ms': sum(wall),
            'wall_p50_ms': wall[len(wall) // 2], 'wall_p95_ms': wall[min(len(wall)-1, int(len(wall)*.95))],
            'cpu_ms': sum(row[1] for row in rows) / 1e6,
            'user_ms': sum(row[2] for row in rows) * 1000,
            'system_ms': sum(row[3] for row in rows) * 1000,
            'voluntary_switches': sum(row[4] for row in rows),
            'involuntary_switches': sum(row[5] for row in rows)}
    return result
