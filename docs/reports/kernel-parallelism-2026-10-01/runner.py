import contextlib
import io
import json
import os
from pathlib import Path
import runpy
import resource
import shutil
import signal
import subprocess
import sys
import time

OUT = Path('/tmp/nn-kernel-estimate')
TARGET = OUT / 'docs/reports/vxlan-minimal-dedicated-2026-09-29'
side = sys.argv[1]
soft,hard = resource.getrlimit(resource.RLIMIT_NOFILE)
resource.setrlimit(resource.RLIMIT_NOFILE,(min(65536,hard),hard))
(OUT/'run-config.json').write_text(json.dumps({'nofile_before':[soft,hard], 'nofile_after':resource.getrlimit(resource.RLIMIT_NOFILE),'cpus':os.cpu_count(),'kernel':os.uname().release,'workers':512,'slots':512,'cpuinfo':Path('/proc/cpuinfo').read_text(),'meminfo':Path('/proc/meminfo').read_text()},indent=2))
if (OUT/'runner.log').exists():
    shutil.copyfile(OUT/'runner.log',OUT/'runner-extended.log')

def cpu():
    return [int(v) for v in Path('/proc/stat').read_text().splitlines()[0].split()[1:]]

with (OUT / 'runner.log').open('w', buffering=1) as log, contextlib.redirect_stdout(log), contextlib.redirect_stderr(log):
    try:
        for path in Path('/proc').glob('[0-9]*/cmdline'):
            try:
                args = path.read_bytes().split(b'\0')
                if b'/tmp/bootstrap.py' in args:
                    os.kill(int(path.parent.name), signal.SIGINT)
            except (FileNotFoundError, ProcessLookupError, PermissionError):
                pass
        deadline = time.monotonic()+90
        while subprocess.run(['docker','inspect','nn29-minimal'],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL).returncode == 0:
            assert time.monotonic()<deadline, 'previous measurement fixture has not drained'
            time.sleep(1)
        for name in ['node.py','local_benchmark.py','trace_lock.py']:
            shutil.copyfile('/tmp/'+name,TARGET/name)
        before = cpu()
        start = time.monotonic()
        time.sleep(5)
        after = cpu()
        (OUT/'idle-cpu.json').write_text(json.dumps({'seconds':time.monotonic()-start,
            'delta':[b-a for a,b in zip(before,after)],'cpus':os.cpu_count(),'ticks':os.sysconf('SC_CLK_TCK')},indent=2))
        requests = []
        for trial in range(2):
            for count in [64,128,256]:
                requests.append({'op':'benchmark','concurrency':count,'seconds':10,'mode':'mixed',
                    'trace':False,'label':f'warm-mixed-c{count}-t{trial}'})
        for count in [1,8,64,256]:
            requests.append({'op':'benchmark','concurrency':count,'seconds':8,'mode':'separate',
                'trace':False,'label':f'warm-separate-c{count}'})
        for count in [1,64,256]:
            requests.append({'op':'benchmark','concurrency':count,'seconds':5,'mode':'mixed',
                'trace':True,'label':f'focused-mixed-c{count}'})
        requests.append({'op':'finish'})
        sys.stdin = io.StringIO(''.join(json.dumps(r)+'\n' for r in requests))
        sys.argv = ['node.py','--side',side,'--slots','512','--workers','512','--variant','bridge','--output',str(OUT/'node.json')]
        sys.path.insert(0,str(TARGET))
        runpy.run_path(str(TARGET/'node.py'),run_name='__main__')
        if shutil.which('perf'):
            for data in OUT.glob('extended*.perf.data'):
                with data.with_suffix('.report.txt').open('w') as report:
                    subprocess.run(['perf','report','--stdio','--no-children','--sort','comm,dso,symbol','-i',str(data)],stdout=report,stderr=report)
        (OUT/'runner.done').write_text('completed\n')
    except BaseException as error:
        import traceback
        traceback.print_exc()
        (OUT/'runner.failed').write_text(repr(error)+'\n')
        raise
