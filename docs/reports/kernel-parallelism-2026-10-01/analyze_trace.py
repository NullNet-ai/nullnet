import collections
import gzip
import json
from pathlib import Path
import re
import statistics
import sys

LOCKS = {'rtnl_lock', 'rtnl_lock_killable', 'rtnl_trylock'}
OPS = {'rtnl_newlink', 'rtnl_setlink', 'rtnl_dellink', 'inet_rtm_newaddr', 'inet_rtm_deladdr'}
RCU = {'synchronize_rcu', 'synchronize_rcu_expedited', 'synchronize_net', 'rcu_barrier'}
PATTERN = re.compile(r'^\s*(.*)-(\d+)\s+\[\d+\].*?\s(\d+\.\d+):\s+(\w+):\s*(.*)$')

def describe(values):
    values = sorted(values)
    if not values:
        return {'count':0, 'sum_ms':0}
    return {'count':len(values), 'sum_ms':sum(values)*1000,
            'mean_ms':statistics.mean(values)*1000,
            'p50_ms':values[len(values)//2]*1000,
            'p95_ms':values[min(len(values)-1,int(len(values)*.95))]*1000,
            'max_ms':max(values)*1000}

def overlap_union(intervals, start, end):
    intervals = sorted((max(a,start),min(b,end)) for a,b in intervals if b>start and a<end)
    total, right = 0, start
    for a,b in intervals:
        total += max(0,b-max(a,right))
        right = max(right,b)
    return total

def analyze(path):
    meta = json.loads(path.with_name(path.name.replace('.trace.gz','.trace-meta.json')).read_text())
    tids = set(meta['tids'])
    generic = 'mutex_address' in meta
    pending = {}
    holds = {}
    ops = {}
    stacks = collections.defaultdict(list)
    durations = collections.defaultdict(list)
    waits = collections.defaultdict(list)
    intervals = []
    rcu = collections.defaultdict(list)
    errors = collections.Counter()
    commands = {}
    with gzip.open(path,'rt') as stream:
        for line in stream:
            match = PATTERN.match(line)
            if not match:
                continue
            comm, pid, stamp, event, rest = match.groups()
            pid, stamp = int(pid), float(stamp)
            commands[pid] = comm.strip()
            owner = 'fixture' if pid in tids else 'other'
            if generic and event == 'mutex_in':
                pending[pid] = (stamp,'mutex_lock')
                continue
            if generic and event == 'mutex_out':
                if pid in pending:
                    start,_ = pending.pop(pid)
                    waits[owner].append(stamp-start)
                    if pid in holds:
                        errors['double_acquire'] += 1
                    holds[pid] = (stamp,ops.get(pid,'other'))
                continue
            if event == 'release':
                if pid in holds:
                    start, operation = holds.pop(pid)
                    intervals.append((start,stamp,pid,operation,owner))
                else:
                    errors['release_without_start'] += 1
                continue
            if event.endswith('_in'):
                function = event[:-3]
                if function in LOCKS and generic:
                    continue
                if function in LOCKS:
                    pending[pid] = (stamp,function)
                else:
                    stacks[pid,function].append(stamp)
                    if function in OPS:
                        ops[pid] = function
                        if pid in holds:
                            holds[pid] = (holds[pid][0],function)
            elif event.endswith('_out'):
                function = event[:-4]
                if function in LOCKS and generic:
                    continue
                if function in LOCKS:
                    retval = re.search(r'retval=(-?\d+)',rest)
                    successful = function == 'rtnl_lock' or (retval and ((function == 'rtnl_trylock' and int(retval[1]) != 0) or (function == 'rtnl_lock_killable' and int(retval[1]) == 0)))
                    if pid in pending:
                        start, _ = pending.pop(pid)
                        waits[owner].append(stamp-start)
                    else:
                        errors['lock_return_without_entry'] += 1
                    if successful:
                        if pid in holds:
                            errors['double_acquire'] += 1
                        holds[pid] = (stamp,ops.get(pid,'other'))
                elif stacks[pid,function]:
                    start = stacks[pid,function].pop()
                    durations[owner,function].append(stamp-start)
                    if function in RCU:
                        rcu[pid].append((start,stamp))
                    if function in OPS:
                        ops.pop(pid,None)
                else:
                    errors['function_return_without_entry'] += 1
    grouped = collections.defaultdict(list)
    hold_rcu = collections.Counter()
    actors = collections.defaultdict(list)
    for start,end,pid,operation,owner in intervals:
        grouped[owner,operation].append(end-start)
        actors[commands[pid]].append(end-start)
        hold_rcu[owner] += overlap_union(rcu[pid],start,end)
    total_hold = sum(end-start for start,end,*_ in intervals)
    wall = meta['stop']-meta['start']
    overruns = {cpu: int(re.search(r'^overrun:\s*(\d+)',stats,re.M)[1]) for cpu,stats in meta['stats'].items()}
    result = {'label':meta['label'], 'seconds':wall, 'fixture_tids':len(tids),
              'all_hold_ms':total_hold*1000, 'lock_occupancy':total_hold/wall,
              'waits':{k:describe(v) for k,v in waits.items()},
              'holds':{f'{a}/{b}':describe(v) for (a,b),v in grouped.items()},
              'functions':{f'{a}/{b}':describe(v) for (a,b),v in durations.items()},
              'rcu_union_inside_hold_ms':{k:v*1000 for k,v in hold_rcu.items()},
              'actors':{k:describe(v) for k,v in actors.items()},
              'overruns':overruns, 'boundary_errors':dict(errors),
              'incomplete_holds':len(holds), 'kprobe_profile':meta['kprobe_profile']}
    path.with_name(path.name.replace('.trace.gz','.analysis.json')).write_text(json.dumps(result,indent=2)+'\n')
    return result

if __name__ == '__main__':
    for name in sys.argv[1:]:
        r=analyze(Path(name))
        print(r['label'],round(r['lock_occupancy'],3),r['overruns'],r['boundary_errors'])
