"""Coordinate two real hosts; SSH control and packet round trips are timed."""
import argparse
import concurrent.futures as futures
import contextlib
import json
import os
from pathlib import Path
import subprocess
import time


class Node:
    def __init__(self, side, slots, trace):
        self.process = subprocess.Popen(['ssh', '-T', '-o', 'BatchMode=yes', f'debian@192.168.1.{103+side}',
                                         'sudo', '-k', '-S', '-p', "''", 'python3', '-u', '/tmp/nn28-cross/node.py',
                                         '--side', str(side), '--slots', str(slots), '--output', '/tmp/nn28-cross/node.json'] + (['--trace'] if trace else []),
                                        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, bufsize=1)
        self.process.stdin.write('debian\n')
        self.process.stdin.flush()

    def receive(self):
        line = self.process.stdout.readline()
        if not line:
            raise RuntimeError(self.process.stderr.read())
        result = json.loads(line)
        if 'error' in result:
            raise RuntimeError(result)
        return result

    def call(self, message):
        self.process.stdin.write(json.dumps(message) + '\n')
        self.process.stdin.flush()
        return self.receive()


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--slots', type=int, default=64)
    parser.add_argument('--seconds', type=float, default=60)
    parser.add_argument('--trials', type=int, default=3)
    parser.add_argument('--turnovers', type=int, default=20)
    parser.add_argument('--output', required=True)
    parser.add_argument('--proof', action='store_true')
    parser.add_argument('--trace', action='store_true')
    args = parser.parse_args()
    nodes = [Node(side, args.slots, args.trace) for side in [0, 1]]
    result = {'trials': [], 'slots': args.slots}
    def save():
        Path(args.output).write_text(json.dumps(result, indent=2) + '\n')
    with futures.ThreadPoolExecutor(2) as pool:
        def both(message):
            return list(pool.map(lambda node: node.call(message), nodes))
        def activate(ids, down=()):
            return both({'op': 'wave', 'up': [[i, os.urandom(32).hex()] for i in ids], 'down': list(down)})
        try:
            result['ready'] = list(pool.map(lambda node: node.receive(), nodes))
            print('PREPARED_BOTH', flush=True)
            ids = list(range(args.slots))
            both({'op': 'quiet', 'ids': ids})
            activate(ids)
            both({'op': 'traffic', 'ids': ids})
            both({'op': 'wave', 'down': ids})
            both({'op': 'inventory', 'label': 'after-smoke'})
            both({'op': 'quiet', 'ids': ids})
            print('TRAFFIC_AND_RESET_PASSED', flush=True)
            if args.proof:
                assert args.slots >= 2
                result['proof'] = []
                def check(label, response):
                    result['proof'].append({'label': label, 'result': response})
                    print('PROOF', label, flush=True)
                both({'op': 'packet-open'})
                def frames(active, label):
                    both({'op': 'packet-open'})
                    sent = nodes[0].call({'op': 'packet-send', 'active': active})
                    received = nodes[1].call({'op': 'packet-receive', 'active': active})
                    check(label, [sent, received])
                frames(False, 'prepared-idle')
                activate([0, 1])
                nodes[0].call({'op': 'wire-open'})
                frames(True, 'active-frame-parity')
                check('ciphertext-only', nodes[0].call({'op': 'wire-check'}))
                nodes[0].call({'op': 'replay'})
                check('current-key-replay-rejected', nodes[1].call({'op': 'packet-receive', 'active': False}))
                for kind in ['plaintext', 'wrong-vni']:
                    injection = nodes[0].call({'op': 'inject', 'kind': kind})
                    check(kind + '-rejected', [injection, nodes[1].call({'op': 'packet-receive', 'active': False})])
                for mode in ['missing', 'wrong-key']:
                    nodes[1].call({'op': 'fault', 'id': 0, 'mode': mode})
                    frames(False, mode + '-rejected')
                    both({'op': 'traffic', 'ids': [1]})
                    check(mode + '-unrelated-slot-survived', True)
                    nodes[1].call({'op': 'fault', 'id': 0, 'mode': 'restore'})
                    frames(True, mode + '-recovered')
                both({'op': 'wave', 'down': [0, 1]})
                both({'op': 'inventory', 'label': 'proof-reset'})
                frames(False, 'reset-idle')
                activate([0, 1])
                both({'op': 'packet-open'})
                nodes[0].call({'op': 'replay'})
                check('old-key-replay-rejected', nodes[1].call({'op': 'packet-receive', 'active': False}))
                frames(True, 'new-generation-frame-parity')
                both({'op': 'wave', 'down': [0, 1]})
                both({'op': 'inventory', 'label': 'proof-final-reset'})
                frames(False, 'final-idle')
            half = args.slots // 2
            for trial in range(0 if args.proof else args.trials):
                active, idle = ids[:half], ids[half:]
                activate(active)
                started = time.monotonic()
                waves = 0
                samples = []
                while time.monotonic() - started < args.seconds or waves < args.turnovers * 2:
                    samples.append(activate(idle, active))
                    both({'op': 'traffic', 'ids': idle})
                    active, idle = idle, active
                    waves += 1
                    if waves % 20 == 0:
                        print('PROGRESS', trial, waves, time.monotonic() - started, flush=True)
                duration = time.monotonic() - started
                both({'op': 'wave', 'down': active})
                both({'op': 'inventory', 'label': f'after-trial-{trial}'})
                row = {'trial': trial, 'seconds': duration, 'waves': waves, 'setups': waves * half,
                       'retirements': waves * half, 'setups_per_second': waves * half / duration,
                       'retirements_per_second': waves * half / duration, 'endpoint_samples': samples}
                result['trials'].append(row)
                save()
                print(json.dumps({k: v for k, v in row.items() if k != 'endpoint_samples'}), flush=True)
            result['completed'] = True
        except Exception as error:
            result['error'] = repr(error)
            with contextlib.suppress(Exception):
                result['diagnostics'] = both({'op': 'diagnostics'})
            raise
        finally:
            result['cleanup'] = []
            for node in nodes:
                try:
                    result['cleanup'].append(node.call({'op': 'finish'}))
                except Exception as error:
                    result['cleanup'].append({'error': str(error)})
                with contextlib.suppress(BrokenPipeError):
                    node.process.stdin.close()
                node.process.wait(timeout=120)
            save()
