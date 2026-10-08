"""Attribute non-overlapping RTNL holds to marked lifecycle requests."""
import collections
import gzip
import json
from pathlib import Path
import re
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'kernel-parallelism-2026-10-01'))
from analyze_trace import PATTERN, describe, analyze


def fine(path):
    meta = json.loads(path.with_name(path.name.replace('.trace.gz', '.trace-meta.json')).read_text())
    overruns = {cpu: int(re.search(r'^overrun:\s*(\d+)', stats, re.M)[1]) for cpu, stats in meta['stats'].items()}
    assert not any(overruns.values()), overruns
    tids = set(meta['tids'])
    scopes, pending, held = {}, {}, {}
    counts = collections.Counter()
    intervals = collections.defaultdict(list)
    waits = collections.defaultdict(list)
    errors = collections.Counter()
    with gzip.open(path, 'rt') as stream:
        for line in stream:
            match = PATTERN.match(line)
            if not match:
                continue
            _, tid, stamp, event, rest = match.groups()
            tid, stamp = int(tid), float(stamp)
            if event == 'tracing_mark_write' and rest.startswith('NNREQ '):
                _, action, ident, label = rest.split()
                if action == 'BEGIN':
                    if tid in scopes:
                        errors['nested_request'] += 1
                    scopes[tid] = (ident, label)
                    counts[label] += 1
                elif scopes.pop(tid, None) != (ident, label):
                    errors['request_end_mismatch'] += 1
                if action == 'END' and tid in held:
                    errors['request_ended_with_lock'] += 1
            elif event == 'mutex_in':
                pending[tid] = stamp
            elif event == 'mutex_out' and tid in pending:
                wait = stamp - pending.pop(tid)
                ident, label = scopes.get(tid, ('none', 'unattributed_fixture' if tid in tids else 'other_actor'))
                if tid in held:
                    errors['double_acquire'] += 1
                held[tid] = (stamp, ident, label)
                waits[label].append(wait)
            elif event == 'release':
                if tid not in held:
                    errors['release_without_acquire'] += 1
                    continue
                start, ident, label = held.pop(tid)
                if ident != 'none' and scopes.get(tid) != (ident, label):
                    errors['hold_crossed_request'] += 1
                intervals[label].append(stamp - start)
    coarse = analyze(path)
    workload = json.loads(path.with_name(path.name.replace('.trace.gz', '.workload.json')).read_text())
    cycles = workload['cycles']
    rows = {}
    for label in sorted(set(counts) | set(intervals)):
        hold = describe(intervals[label])
        rows[label] = {'requests': counts[label], 'holds': hold,
                       'hold_ms_per_cycle': hold['sum_ms'] / cycles,
                       'acquisition_waits': describe(waits[label])}
    result = {'cycles': cycles, 'workload': workload, 'rows': rows,
              'fixture_hold_ms_per_cycle': sum(r['holds']['sum_ms'] for k, r in rows.items() if k != 'other_actor') / cycles,
              'coarse': coarse, 'marker_errors': dict(errors), 'incomplete_scopes': len(scopes),
              'incomplete_holds': len(held)}
    assert not errors and not scopes and not held, (dict(errors), len(scopes), len(held))
    assert not coarse['boundary_errors'] and coarse['incomplete_holds'] == 0, coarse['boundary_errors']
    assert not any(coarse['overruns'].values()), coarse['overruns']
    assert abs(sum(r['holds']['sum_ms'] for r in rows.values()) - coarse['all_hold_ms']) < 1e-5
    for line in coarse['kprobe_profile'].splitlines():
        if re.search(r'(mutex_in|mutex_out|/release)\s', line):
            assert int(line.split()[-1]) == 0, line
    path.with_name(path.name.replace('.trace.gz', '.fine.json')).write_text(json.dumps(result, indent=2) + '\n')
    return result


if __name__ == '__main__':
    for name in sys.argv[1:]:
        result = fine(Path(name))
        print(name, result['cycles'], result['fixture_hold_ms_per_cycle'])
