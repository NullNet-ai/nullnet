"""Whole-process accounting and sampling of the original concurrent workload."""
import contextlib
import io
import json
import os
from pathlib import Path
import resource
import sys
import time

ROOT=Path('/tmp/nn-cpu-20261005')
sys.path.insert(0,str(ROOT/'docs/reports/kernel-parallelism-2026-10-01'))
import node
import local_benchmark

soft,hard=resource.getrlimit(resource.RLIMIT_NOFILE)
resource.setrlimit(resource.RLIMIT_NOFILE,(min(65536,hard),hard))
original_cpu=local_benchmark.cpu
original_benchmark=local_benchmark.benchmark
snapshots=[]


def cpu():
    r=resource.getrusage(resource.RUSAGE_SELF)
    snapshots.append({'clock':time.process_time_ns(),'user':r.ru_utime,'system':r.ru_stime,
                      'voluntary':r.ru_nvcsw,'involuntary':r.ru_nivcsw})
    return original_cpu()


def benchmark(experiment,request):
    snapshots.clear()
    result=original_benchmark(experiment,request)
    result['process_delta']={k:snapshots[-1][k]-snapshots[0][k] for k in snapshots[0]}
    (ROOT/(request['label']+'.concurrent.json')).write_text(json.dumps(result,indent=2)+'\n')
    return result


local_benchmark.cpu=cpu
local_benchmark.benchmark=benchmark
side=sys.argv[1]
requests=[]
for count in [64,256]:
    for mode in ['separate','mixed']:
        requests.append({'op':'benchmark','concurrency':count,'seconds':12,'mode':mode,
            'trace':False,'label':f'process-{mode}-c{count}'})
if side=='0':
    requests.append({'op':'benchmark','concurrency':256,'seconds':12,'mode':'mixed',
        'trace':False,'perf':True,'label':'sampled-mixed-c256'})
    requests.append({'op':'benchmark','concurrency':256,'seconds':5,'mode':'mixed',
        'trace':True,'label':'locks-mixed-c256'})
requests.append({'op':'finish'})
with (ROOT/'concurrent.log').open('w',buffering=1) as log,contextlib.redirect_stdout(log),contextlib.redirect_stderr(log):
    sys.stdin=io.StringIO(''.join(json.dumps(r)+'\n' for r in requests))
    sys.argv=['node.py','--side',side,'--slots','512','--workers','512','--variant','bridge','--output',str(ROOT/'concurrent-node.json')]
    node.main()
    (ROOT/'concurrent.done').write_text('complete\n')
