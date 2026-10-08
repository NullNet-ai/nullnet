"""Balanced baseline/udev/all-listener trials of the same encrypted lifecycle."""
import contextlib
import io
import json
from pathlib import Path
import resource
import runpy
import signal
import sys
import time

phase=sys.argv[2] if len(sys.argv)>2 else 'balanced'
ROOT=Path('/tmp/nn-event-20261005'+('-'+phase if phase!='balanced' else ''))
ROOT.mkdir(exist_ok=True)
sys.path.insert(0,'/tmp/nn-cpu-20261005/docs/reports/kernel-parallelism-2026-10-01')
import node
import local_benchmark
from socket_filter import Filters

soft,hard=resource.getrlimit(resource.RLIMIT_NOFILE)
resource.setrlimit(resource.RLIMIT_NOFILE,(min(65536,hard),hard))
snapshots=[]
original_cpu=local_benchmark.cpu
original_benchmark=local_benchmark.benchmark
filters=None
results=[]


def cpu():
    r=resource.getrusage(resource.RUSAGE_SELF)
    snapshots.append({'clock':time.process_time_ns(),'user':r.ru_utime,'system':r.ru_stime})
    return original_cpu()


def benchmark(experiment,request):
    node.command('udevadm','settle','--timeout=60')
    filters.seed()
    filters.apply(request['filter'])
    time.sleep(.5)
    before=filters.counts()
    snapshots.clear()
    result=original_benchmark(experiment,request)
    result['process_delta']={k:snapshots[-1][k]-snapshots[0][k] for k in snapshots[0]}
    result['filter']=request['filter']
    result['drop_delta']={k:v-before[k] for k,v in filters.counts().items()}
    result['slots']=experiment.args.slots
    results.append(result)
    (ROOT/'results.json').write_text(json.dumps(results,indent=2)+'\n')
    return result


local_benchmark.cpu=cpu
local_benchmark.benchmark=benchmark
side=sys.argv[1]
requests=[{'op':'benchmark','concurrency':256,'seconds':6,'mode':'mixed','trace':False,
           'filter':'baseline','label':'events-warmup-c256'}]
for concurrency in [64,256]:
    for trial,mode in enumerate(['baseline','udev','all','all','udev','baseline']):
        requests.append({'op':'benchmark','concurrency':concurrency,'seconds':15,'mode':'mixed','trace':False,
                         'filter':mode,'label':f'events-c{concurrency}-t{trial}-{mode}'})
requests.append({'op':'benchmark','concurrency':256,'seconds':60,'mode':'mixed','trace':False,
                 'filter':'all','label':'events-all-sustained-c256'})
if side=='0':
    requests.append({'op':'benchmark','concurrency':256,'seconds':15,'mode':'mixed','trace':False,'perf':True,
                     'filter':'all','label':'events-all-sampled-c256'})
requests.append({'op':'benchmark','concurrency':256,'seconds':3,'mode':'mixed','trace':False,
                 'filter':'baseline','label':'events-restore-c256'})
requests.append({'op':'finish'})
if phase=='sustained':
    requests=[{'op':'benchmark','concurrency':256,'seconds':duration,'mode':'mixed','trace':False,
               'filter':'all','label':label,**({'perf':True} if sampled else {})}
              for duration,label,sampled in [(6,'events-filtered-warmup',False),
                    (60,'events-filtered-sustained',False)]+([(15,'events-filtered-sampled',True)] if side=='0' else [])]
    requests.append({'op':'finish'})
if phase=='final':
    requests=[{'op':'benchmark','concurrency':c,'seconds':15,'mode':'mixed','trace':False,
               'filter':'all','label':f'events-final-c{c}'} for c in [64,256]]
    if side=='0':
        requests.append({'op':'benchmark','concurrency':256,'seconds':8,'mode':'mixed','trace':True,
                         'filter':'all','label':'events-final-lock-trace'})
    requests.append({'op':'finish'})
original_cleanup=node.Minimal.cleanup
def cleanup(experiment):
    if phase=='balanced':
        filters.apply('baseline')
    return original_cleanup(experiment)
node.Minimal.cleanup=cleanup
def interrupted(signum,frame):
    raise KeyboardInterrupt
signal.signal(signal.SIGTERM,interrupted)
with (ROOT/'runner.log').open('w',buffering=1) as log,contextlib.redirect_stdout(log),contextlib.redirect_stderr(log):
    try:
        runpy.run_path('/tmp/nn-event-discover.py')
        inventory=json.loads(Path('/tmp/nn-event-sockets.json').read_text())
        (ROOT/'socket-inventory.json').write_text(json.dumps(inventory,indent=2)+'\n')
        filters=Filters(inventory,ROOT/'filters.json')
        if phase!='balanced':
            filters.apply('all')
        sys.stdin=io.StringIO(''.join(json.dumps(r)+'\n' for r in requests))
        sys.argv=['node.py','--side',side,'--slots','512','--workers','512','--variant','bridge','--output',str(ROOT/'node.json')]
        node.main()
        assert 'error' not in json.loads((ROOT/'node.json').read_text())
        assert all(json.loads((ROOT/'node.json.preservation.json').read_text())['checks'].values())
        (ROOT/'done').write_text('complete\n')
    finally:
        if filters:
            filters.close()
