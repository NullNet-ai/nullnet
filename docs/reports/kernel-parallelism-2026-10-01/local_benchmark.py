import json
import os
import shutil
import subprocess
from pathlib import Path
import time
import trace_lock

def cpu():
    return [int(v) for v in Path('/proc/stat').read_text().splitlines()[0].split()[1:]]

def benchmark(experiment, request):
    subprocess.run(['udevadm','settle','--timeout=60'],check=True)
    count = request['concurrency']
    mode = request.get('mode', 'mixed')
    traced = request.get('trace', False)
    a, b = list(range(count)), list(range(experiment.args.slots//2,experiment.args.slots//2+count))
    def wave(up=(), down=()):
        start = time.monotonic()
        result = experiment.wave({'up':[[i,os.urandom(32).hex()] for i in up], 'down':list(down)})
        return {'seconds':time.monotonic()-start, 'result':result}
    if mode == 'mixed':
        wave(a)
    if traced:
        trace_lock.trace_start(request['label'], os.getpid())
    profile = None
    profile_log = None
    if request.get('perf') and shutil.which('perf'):
        profile_log = open('/tmp/nn-kernel-estimate/'+request['label']+'.perf.log','w')
        profile = subprocess.Popen(['perf','record','-a','-e','cpu-clock','-F','99','-g','-o',
            '/tmp/nn-kernel-estimate/'+request['label']+'.perf.data'],stdout=profile_log,stderr=profile_log)
    cpu_before = cpu()
    started = time.monotonic()
    samples = []
    setup_cpu = [0]*10
    retire_cpu = [0]*10
    try:
        while time.monotonic()-started < request.get('seconds',5):
            if mode == 'mixed':
                samples.append(wave(b,a))
                a,b=b,a
            else:
                before = cpu()
                up = wave(a)
                middle = cpu()
                down = wave(down=a)
                after = cpu()
                setup_cpu = [v+b-a for v,a,b in zip(setup_cpu,before,middle)]
                retire_cpu = [v+b-a for v,a,b in zip(retire_cpu,middle,after)]
                samples.append({'up':up, 'down':down})
        seconds=time.monotonic()-started
        cpu_after=cpu()
    finally:
        if profile:
            profile.send_signal(2)
            profile.wait(timeout=30)
            profile_log.close()
        if traced:
            trace_lock.trace_stop(request['label'], os.getpid())
    drain = wave(down=a) if mode == 'mixed' else None
    experiment.inventory(request['label'])
    result={'label':request['label'], 'mode':mode, 'concurrency':count, 'trace':traced,
            'seconds':seconds,'cycles':count*len(samples), 'cycles_per_second':count*len(samples)/seconds,
            'samples':samples, 'final_drain':drain, 'cpu_delta':[b-a for a,b in zip(cpu_before,cpu_after)],
            'cpus':os.cpu_count(),'kernel':os.uname().release,
            'setup_cpu_delta':setup_cpu, 'retire_cpu_delta':retire_cpu}
    Path('/tmp/nn-kernel-estimate/'+request['label']+'.benchmark.json').write_text(json.dumps(result,indent=2)+'\n')
    return {k:v for k,v in result.items() if k not in ['samples','final_drain']}
