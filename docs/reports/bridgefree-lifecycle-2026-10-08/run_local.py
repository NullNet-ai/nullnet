"""Sequential local endpoint trials, invoked separately on each Linux host."""
import argparse
import contextlib
import io
import json
from pathlib import Path
import runpy
import sys

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import node


def main():
    p = argparse.ArgumentParser()
    p.add_argument('--side', required=True)
    p.add_argument('--seconds', type=float, default=8)
    p.add_argument('--trials', type=int, default=3)
    p.add_argument('--slots', type=int, default=512)
    p.add_argument('--workers', type=int, default=512)
    p.add_argument('--filter', choices=['baseline', 'all'], default='all')
    p.add_argument('--pilot', action='store_true')
    p.add_argument('--reset', choices=['historical', 'full'], default='historical')
    args = p.parse_args()
    root = HERE.parents[2]/'results'/(('pilot-' if args.pilot else '')+args.reset)
    root.mkdir(parents=True, exist_ok=True)
    filters = None
    try:
        if args.filter == 'all':
            from socket_filter import Filters
            with (root/'discover.json').open('w') as stream, contextlib.redirect_stdout(stream):
                runpy.run_path(str(HERE.parent/'device-event-bypass-2026-10-05/discover.py'))
            inventory = json.loads(Path('/tmp/nn-event-sockets.json').read_text())
            filters = Filters(inventory, root/'filters.json')
            filters.apply('all')
        for lifecycle, variant in [('unpooled', 'bridge'), ('unpooled', 'redirect'), ('pooled', 'bridge'), ('pooled', 'redirect')]:
            run = lifecycle+'-'+variant
            out = root/run
            out.mkdir(exist_ok=True)
            requests = []
            for c in ([8] if args.pilot else [64, 256]):
                for trial in range(1 if args.pilot else args.trials):
                    for cpu, traced, tag in [(False, False, 'plain'), (True, False, 'cpu'), (False, True, 'lock')]:
                        requests.append({'op': 'benchmark', 'concurrency': c, 'seconds': args.seconds if tag == 'plain' else min(4, args.seconds),
                                         'accounting': cpu, 'trace': traced, 'label': f'{run}-c{c}-t{trial}-{tag}'})
            requests.append({'op': 'finish'})
            sys.argv = ['node.py', '--side', args.side, '--variant', variant, '--lifecycle', lifecycle,
                        '--slots', str(16 if args.pilot else args.slots), '--workers', str(16 if args.pilot else args.workers), '--output', str(out/'node.json')]
            sys.argv += ['--reset', args.reset]
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
            for r in requests[:-1]:
                data = json.loads((out/(r['label']+'.workload.json')).read_text())
                print('RESULT', r['label'], round(data['cycles_per_second'], 2), flush=True)
                if r['trace']:
                    from fine_lock_analysis import fine
                    analysis = fine(out/(r['label']+'.trace.gz'))
                    print('RTNL', r['label'], round(analysis['fixture_hold_ms_per_cycle'], 4), flush=True)
            print('PRESERVED', run, flush=True)
        (root/'done').write_text('complete\n')
    finally:
        if filters:
            filters.save()
            filters.close()


if __name__ == '__main__':
    main()
