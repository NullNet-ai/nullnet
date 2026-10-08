"""Uninstrumented sustained confirmation with the historical reset scope."""
import contextlib
import io
import json
from pathlib import Path
import runpy
import sys

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import node
from socket_filter import Filters

side = sys.argv[1]
confirm = '--confirm-c64' in sys.argv[2:]
root = HERE.parents[2]/'results'/('confirm-c64' if confirm else 'sustained')
root.mkdir(parents=True, exist_ok=True)
with (root/'discover.json').open('w') as stream, contextlib.redirect_stdout(stream):
    runpy.run_path(str(HERE.parent/'device-event-bypass-2026-10-05/discover.py'))
filters = Filters(json.loads(Path('/tmp/nn-event-sockets.json').read_text()), root/'filters.json')
try:
    filters.apply('all')
    cases = [('pooled', 'redirect')] if confirm else [('pooled', 'redirect'), ('pooled', 'bridge'), ('unpooled', 'redirect'), ('unpooled', 'bridge')]
    for lifecycle, variant in cases:
        run = lifecycle+'-'+variant
        out = root/run
        out.mkdir(exist_ok=True)
        requests = [{'op': 'benchmark', 'concurrency': c, 'seconds': 60,
                     'label': run+f'-c{c}-sustained'} for c in ([64] if confirm else [64, 256] if run == 'pooled-redirect' else [256])]
        requests.append({'op': 'finish'})
        sys.argv = ['node.py', '--side', side, '--variant', variant, '--lifecycle', lifecycle,
                    '--slots', '512', '--workers', '512', '--reset', 'historical', '--output', str(out/'node.json')]
        sys.stdin = io.StringIO(''.join(json.dumps(r)+'\n' for r in requests))
        if hasattr(node.TLS, 'io'):
            for sock in [*node.TLS.io[:2], *node.TLS.io[2], *node.TLS.conntrack]:
                sock.s.close()
            del node.TLS.io
            del node.TLS.conntrack
        with (out/'runner.log').open('w', buffering=1) as log, contextlib.redirect_stdout(log), contextlib.redirect_stderr(log):
            node.main()
        assert 'error' not in json.loads((out/'node.json').read_text())
        assert all(json.loads((out/'node.json.preservation.json').read_text())['checks'].values())
        for request in requests[:-1]:
            data = json.loads((out/(request['label']+'.workload.json')).read_text())
            print('SUSTAINED', request['label'], data['cycles'], round(data['cycles_per_second'], 2), flush=True)
        print('PRESERVED', run, flush=True)
    (root/'done').write_text('complete\n')
finally:
    filters.close()
