"""Summarize fixed-period host CPU samples, separating idle execution."""
import collections
import json
from pathlib import Path
import re
import subprocess
import sys

data=Path(sys.argv[1])
output=Path(sys.argv[2])
pattern=re.compile(r'^\s*(.*?)\s+(\d+)/(\d+)\s+(\d+\.\d+):\s+(\d+)\s+cpu-clock:\s+\S+\s+(.*?)\s+\((.*?)\)\s*$')
process=subprocess.Popen(['perf','script','--show-task-events','--hide-call-graph','-i',str(data),'-F',
                         'comm,pid,tid,time,period,event,ip,sym,dso'],stdout=subprocess.PIPE,
                         stderr=subprocess.PIPE,text=True)
commands=collections.Counter()
symbols=collections.Counter()
processes=collections.Counter()
phases=collections.defaultdict(collections.Counter)
parents={}
names={}
families=collections.Counter()
rows=json.loads(Path('/tmp/nn-cpu-20261005/phased.json').read_text()) if 'phased' in output.name else []
rows=[r for r in rows if r['sampling']]
unparsed=[]
for line in process.stdout:
    if not line.strip() or line.startswith('#'):
        continue
    fork=re.search(r'PERF_RECORD_FORK\((\d+):(\d+)\):\((\d+):(\d+)\)',line)
    if fork:
        child,tid,parent,ptid=map(int,fork.groups())
        if child!=parent:
            parents[child]=parent
        continue
    rename=re.search(r'PERF_RECORD_COMM(?: exec)?: (.*):(\d+)/(\d+)',line)
    if rename:
        name,pid,tid=rename.groups()
        if pid==tid:
            names[int(pid)]=name
        continue
    if 'PERF_RECORD_' in line:
        continue
    match=pattern.match(line)
    if not match:
        if len(unparsed)<10:
            unparsed.append(line.rstrip())
        continue
    comm,pid,tid,stamp,period,symbol,dso=match.groups()
    period=int(period)
    ancestors=[]
    ancestor=int(pid)
    while ancestor and ancestor not in ancestors:
        ancestors.append(ancestor)
        ancestor=parents.get(ancestor,0)
    if comm=='swapper':
        family='idle'
    elif comm.startswith('nn-est-'):
        family='lifecycle caller'
    elif any(names.get(p,'') in ['systemd-udevd','(udev-worker)'] for p in ancestors) or comm in ['systemd-udevd','(udev-worker)']:
        family='udev and descendants'
    elif comm.startswith(('kworker','ksoftirqd','rcu_','migration/')):
        family='kernel workers'
    else:
        family='other userspace'
    families[family]+=period
    commands[comm]+=period
    processes[pid+'/'+comm]+=period
    symbols[comm+'/'+dso+'/'+symbol]+=period
    for row in rows:
        if row['start_monotonic']<=float(stamp)<row['end_monotonic']:
            phases[row['phase']][comm]+=period
            break
stderr=process.stderr.read()
assert process.wait()==0,stderr
result={'sample_period_units':'nanoseconds','commands_ns':dict(commands),
        'families_ns':dict(families), 'ancestry_source':'PERF_RECORD_FORK and PERF_RECORD_COMM at sample time',
        'processes_ns':dict(processes), 'symbols_ns':dict(symbols.most_common(150)),
        'phases_ns':{k:dict(v) for k,v in phases.items()},'unparsed_examples':unparsed,'stderr':stderr}
output.write_text(json.dumps(result,indent=2)+'\n')
print(json.dumps({'commands_ns':result['commands_ns'],'unparsed_examples':unparsed,'stderr':stderr}))
