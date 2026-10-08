"""Run isolated fixtures; never restart or modify the installed product."""
import contextlib
import io
import json
import os
from pathlib import Path
import resource
import runpy
import sys

ROOT = Path('/tmp/nn-cpu-20261005')
TARGET = ROOT / 'docs/reports/kernel-parallelism-2026-10-01'
sys.path.insert(0, str(TARGET))
import node
import local_benchmark
import profile as accounting

soft, hard = resource.getrlimit(resource.RLIMIT_NOFILE)
resource.setrlimit(resource.RLIMIT_NOFILE, (min(65536, hard), hard))
accounting.install(node)
original_benchmark = local_benchmark.benchmark


def benchmark(experiment, request):
    accounting.ROWS.clear()
    accounting.ENABLED = request.get('accounting', False)
    try:
        result = original_benchmark(experiment, request)
    finally:
        accounting.ENABLED = False
    result['accounting'] = accounting.summarize()
    result['ticks'] = os.sysconf('SC_CLK_TCK')
    (ROOT / (request['label'] + '.cpu.json')).write_text(json.dumps(result, indent=2) + '\n')
    return result


local_benchmark.benchmark = benchmark
requests = []
for count in [1, 64, 256]:
    for trial in range(3):
        for enabled in [False, True]:
            requests.append({'op': 'benchmark', 'concurrency': count, 'seconds': 10,
                             'mode': 'separate', 'trace': False, 'accounting': enabled,
                             'label': f'c{count}-t{trial}-cpu{int(enabled)}'})
requests.append({'op': 'finish'})
with (ROOT / 'runner.log').open('w', buffering=1) as log, contextlib.redirect_stdout(log), contextlib.redirect_stderr(log):
    try:
        Path('/tmp/nn-kernel-estimate').mkdir(exist_ok=True)
        sys.stdin = io.StringIO(''.join(json.dumps(r) + '\n' for r in requests))
        sys.argv = ['node.py', '--side', sys.argv[1], '--slots', '512', '--workers', '512',
                    '--variant', 'bridge', '--output', str(ROOT / 'node.json')]
        node.main()
        (ROOT / 'done').write_text('complete\n')
    except BaseException:
        import traceback
        traceback.print_exc()
        (ROOT / 'failed').write_text('failed\n')
        raise
