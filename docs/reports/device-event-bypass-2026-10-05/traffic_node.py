"""Run the existing encrypted fixture with scoped listeners disabled."""
import contextlib
import json
from pathlib import Path
import runpy
import signal
import sys

sys.path.insert(0,'/tmp/nn-cpu-20261005/docs/reports/kernel-parallelism-2026-10-01')
import node
from socket_filter import Filters

ROOT=Path('/tmp/nn-event-20261005')
with (ROOT/'traffic-discovery.log').open('w') as log,contextlib.redirect_stdout(log):
    runpy.run_path('/tmp/nn-event-discover.py')
filters=Filters(json.loads(Path('/tmp/nn-event-sockets.json').read_text()),ROOT/'traffic-filters.json')
original_cleanup=node.Minimal.cleanup
def cleanup(experiment):
    filters.apply('baseline')
    return original_cleanup(experiment)
node.Minimal.cleanup=cleanup
def interrupted(signum,frame):
    raise KeyboardInterrupt
signal.signal(signal.SIGTERM,interrupted)
try:
    filters.apply('all')
    node.main()
    output=Path(sys.argv[sys.argv.index('--output')+1])
    assert 'error' not in json.loads(output.read_text())
    assert all(json.loads(Path(str(output)+'.preservation.json').read_text())['checks'].values())
finally:
    filters.close()
