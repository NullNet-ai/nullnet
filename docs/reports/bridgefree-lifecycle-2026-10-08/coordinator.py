"""Local SSH orchestration; all kernel work executes on the two Linux hosts."""
import argparse
import concurrent.futures as futures
import contextlib
import json
import os
from pathlib import Path
import subprocess
import time

REMOTE = '/root/nn38-bridgefree/docs/reports/bridgefree-lifecycle-2026-10-08/node.py'


class Node:
    def __init__(self, side, variant, lifecycle, slots, workers, run):
        self.side = side
        self.stderr = open(f'/private/tmp/nullnet-bridgefree-20261008/{run}-{side}.stderr', 'w')
        self.process = subprocess.Popen(['ssh', '-T', '-o', 'BatchMode=yes', f'debian@192.168.1.{103+side}',
            'sudo', '-k', '-S', '-p', "''", 'python3', '-u', REMOTE, '--side', str(side), '--variant', variant,
            '--lifecycle', lifecycle, '--slots', str(slots), '--workers', str(workers),
            '--output', f'/root/nn38-bridgefree/results/{run}/node.json'],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self.stderr, text=True, bufsize=1)
        self.process.stdin.write('debian\n')
        self.process.stdin.flush()

    def receive(self):
        line = self.process.stdout.readline()
        if not line:
            raise RuntimeError(('node EOF', self.side, self.process.poll()))
        response = json.loads(line)
        if 'error' in response:
            raise RuntimeError(response)
        return response

    def call(self, message):
        self.process.stdin.write(json.dumps(message)+'\n')
        self.process.stdin.flush()
        return self.receive()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--variant', choices=['bridge', 'redirect'], required=True)
    parser.add_argument('--lifecycle', choices=['pooled', 'unpooled'], required=True)
    parser.add_argument('--mode', choices=['smoke', 'local', 'traffic', 'proof'], default='smoke')
    parser.add_argument('--slots', type=int, default=128)
    parser.add_argument('--workers', type=int, default=128)
    parser.add_argument('--seconds', type=float, default=60)
    parser.add_argument('--trials', type=int, default=3)
    parser.add_argument('--run', required=True)
    args = parser.parse_args()
    nodes = [Node(s, args.variant, args.lifecycle, args.slots, args.workers, args.run) for s in [0, 1]]
    result = {'args': vars(args), 'trials': []}
    out = Path('/private/tmp/nullnet-bridgefree-20261008')/(args.run+'.json')
    def save():
        out.write_text(json.dumps(result, indent=2)+'\n')
    with futures.ThreadPoolExecutor(2) as pool:
        def both(message):
            return list(pool.map(lambda n: n.call(message), nodes))
        def wave(up=(), down=()):
            return both({'op': 'wave', 'up': [[i, os.urandom(32).hex()] for i in up], 'down': list(down)})
        try:
            result['ready'] = list(pool.map(lambda n: n.receive(), nodes))
            print('READY', args.run, flush=True)
            count = min(32, args.slots//2)
            a, b = list(range(count)), list(range(args.slots//2, args.slots//2+count))
            wave(a)
            result['smoke_traffic'] = both({'op': 'traffic', 'ids': a})
            result['smoke_gateway'] = both({'op': 'gateway', 'ids': a})
            wave(down=a)
            both({'op': 'inventory', 'label': 'smoke'})
            both({'op': 'quiet', 'ids': a})
            print('SMOKE_PASSED', args.run, flush=True)
            if args.mode == 'local':
                for c in [32, 64, 256]:
                    if c > args.slots//2:
                        continue
                    for trial in range(args.trials):
                        for accounting, traced, tag in [(False, False, 'plain'), (True, False, 'cpu'), (False, True, 'lock')]:
                            label = f'{args.run}-c{c}-t{trial}-{tag}'
                            row = both({'op': 'benchmark', 'concurrency': c, 'seconds': args.seconds,
                                        'accounting': accounting, 'trace': traced, 'label': label})
                            result['trials'].append({'label': label, 'hosts': row})
                            save()
                            print('RESULT', label, [round(r['cycles_per_second'], 2) for r in row], flush=True)
            if args.mode == 'traffic':
                for trial in range(args.trials):
                    wave(a)
                    before = time.monotonic()
                    cycles, samples = 0, []
                    while time.monotonic()-before < args.seconds:
                        started = time.monotonic()
                        wave(b, a)
                        both({'op': 'traffic', 'ids': b})
                        both({'op': 'gateway', 'ids': b})
                        samples.append(time.monotonic()-started)
                        a, b = b, a
                        cycles += count
                    seconds = time.monotonic()-before
                    start = time.monotonic()
                    wave(down=a)
                    drain = time.monotonic()-start
                    both({'op': 'inventory', 'label': f'traffic-{trial}'})
                    row = {'trial': trial, 'cycles': cycles, 'seconds': seconds, 'cycles_per_second': cycles/seconds,
                           'including_drain_cycles_per_second': cycles/(seconds+drain), 'drain_seconds': drain, 'wave_seconds': samples}
                    result['trials'].append(row)
                    save()
                    print('TRAFFIC_RESULT', {k: v for k, v in row.items() if k != 'wave_seconds'}, flush=True)
            if args.mode == 'proof':
                wave([0, 1])
                result['proof'] = []
                for mode in ['missing', 'wrong-key']:
                    nodes[1].call({'op': 'fault', 'id': 0, 'mode': mode})
                    both({'op': 'traffic', 'ids': [1]})
                    nodes[1].call({'op': 'fault', 'id': 0, 'mode': 'restore'})
                    both({'op': 'traffic', 'ids': [0]})
                    result['proof'].append({'mode': mode, 'unrelated_edge': True, 'recovered': True})
                wave(down=[0, 1])
                both({'op': 'inventory', 'label': 'proof-final'})
            result['completed'] = True
        except Exception as error:
            result['error'] = repr(error)
            raise
        finally:
            result['cleanup'] = []
            for node in nodes:
                try:
                    result['cleanup'].append(node.call({'op': 'finish'}))
                except Exception as error:
                    result['cleanup'].append({'error': repr(error)})
                with contextlib.suppress(Exception):
                    node.process.stdin.close()
                node.process.wait(timeout=120)
                node.stderr.close()
            save()
            if result.get('completed'):
                assert all(r.get('cleaned') and all(r['preservation'].values()) for r in result['cleanup']), result['cleanup']
                print('CLEANUP_PRESERVED', args.run, flush=True)


if __name__ == '__main__':
    main()
