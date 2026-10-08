"""Paired-host lifecycle measurement; counted cycles include acknowledged retirement."""
import argparse
import concurrent.futures as futures
import contextlib
import json
import os
from pathlib import Path
import subprocess
import time


class Node:
    def __init__(self, side, args):
        remote = '/tmp/nn29-minimal/vxlan-minimal-dedicated-2026-09-29/node.py'
        self.process = subprocess.Popen(['ssh', '-T', '-o', 'BatchMode=yes', f'debian@192.168.1.{103+side}',
            'sudo', '-k', '-S', '-p', "''", 'python3', '-u', remote, '--side', str(side),
            '--slots', str(args.slots), '--variant', args.variant,
            '--shape', args.shape,
            '--output', f'/tmp/nn29-minimal/{args.variant}-node.json'],
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


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--variant', choices=['bridge', 'redirect'], required=True)
    parser.add_argument('--shape', choices=['cc', 'hc', 'ch', 'hh'], default='cc')
    parser.add_argument('--slots', type=int, default=64)
    parser.add_argument('--seconds', type=float, default=60)
    parser.add_argument('--trials', type=int, default=3)
    parser.add_argument('--turnovers', type=int, default=20)
    parser.add_argument('--proof', action='store_true')
    parser.add_argument('--output', required=True)
    args = parser.parse_args()
    result = {'variant': args.variant, 'shape': args.shape, 'slots': args.slots, 'trials': []}
    nodes = [Node(side, args) for side in [0, 1]]
    with futures.ThreadPoolExecutor(2) as pool:
        def both(message):
            return list(pool.map(lambda node: node.call(message), nodes))
        def activate(ids, down=()):
            return both({'op': 'wave', 'up': [[i, os.urandom(32).hex()] for i in ids], 'down': list(down)})
        def save():
            Path(args.output).write_text(json.dumps(result, indent=2) + '\n')
        try:
            result['ready'] = list(pool.map(lambda node: node.receive(), nodes))
            print('READY', args.variant, flush=True)
            ids = list(range(args.slots))
            activate(ids)
            both({'op': 'traffic', 'ids': ids})
            result['flow_presence_after_smoke'] = both({'op': 'flow-presence'})
            both({'op': 'wave', 'down': ids})
            both({'op': 'inventory', 'label': 'smoke-retired'})
            print('SMOKE_PASSED', flush=True)
            if args.proof:
                result['proof'] = []
                def check(label, value):
                    result['proof'].append({'label': label, 'result': value})
                    print('PROOF', label, flush=True)
                def frames(active, label):
                    both({'op': 'packet-open'})
                    sent = nodes[0].call({'op': 'packet-send', 'active': active})
                    received = nodes[1].call({'op': 'packet-receive', 'active': active})
                    check(label, [sent, received])
                activate([0, 1])
                nodes[0].call({'op': 'wire-open'})
                frames(True, 'frame-parity')
                check('ciphertext-only', nodes[0].call({'op': 'wire-check'}))
                for kind in ['plaintext', 'wrong-vni']:
                    injected = nodes[0].call({'op': 'inject', 'kind': kind})
                    check(kind, [injected, nodes[1].call({'op': 'packet-receive', 'active': False})])
                nodes[0].call({'op': 'replay'})
                check('replay-current-key', nodes[1].call({'op': 'packet-receive', 'active': False}))
                for mode in ['missing', 'wrong-key']:
                    nodes[1].call({'op': 'fault', 'id': 0, 'mode': mode})
                    frames(False, mode)
                    both({'op': 'traffic', 'ids': [1]})
                    check(mode + '-other-edge-survives', True)
                    nodes[1].call({'op': 'fault', 'id': 0, 'mode': 'restore'})
                    frames(True, mode + '-restored')
                both({'op': 'wave', 'down': [0, 1]})
                both({'op': 'inventory', 'label': 'proof-retired'})
                activate([0, 1])
                both({'op': 'packet-open'})
                nodes[0].call({'op': 'replay'})
                check('replay-old-generation', nodes[1].call({'op': 'packet-receive', 'active': False}))
                frames(True, 'new-generation')
                both({'op': 'wave', 'down': [0, 1]})
                both({'op': 'inventory', 'label': 'proof-final'})
            half = args.slots // 2
            for trial in range(0 if args.proof else args.trials):
                active, idle = ids[:half], ids[half:]
                activate(active)
                started = time.monotonic()
                samples = []
                while time.monotonic() - started < args.seconds or len(samples) < args.turnovers * 2:
                    samples.append(activate(idle, active))
                    both({'op': 'traffic', 'ids': idle})
                    active, idle = idle, active
                    if len(samples) % 10 == 0:
                        print('PROGRESS', trial, len(samples), round(time.monotonic() - started, 2), flush=True)
                duration = time.monotonic() - started
                both({'op': 'wave', 'down': active})
                drain = time.monotonic() - started - duration
                both({'op': 'inventory', 'label': f'trial-{trial}'})
                row = {'trial': trial, 'seconds': duration, 'setups': len(samples) * half,
                       'retirements': len(samples) * half, 'complete_cycles_per_second': len(samples) * half / duration,
                       'final_drain_seconds': drain, 'samples': samples}
                result['trials'].append(row)
                save()
                print(json.dumps({k: v for k, v in row.items() if k != 'samples'}), flush=True)
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
                if node.process.returncode:
                    result.setdefault('exit_errors', []).append(node.process.returncode)
                errors = node.process.stderr.read()
                if errors:
                    result.setdefault('stderr', []).append(errors)
            clean = len(result['cleanup']) == 2 and all(
                item.get('cleaned') and all(item.get('preservation', {}).values()) for item in result['cleanup'])
            if not clean or result.get('exit_errors'):
                result['completed'] = False
            save()
            assert clean and not result.get('exit_errors'), 'cleanup/preservation failed; inspect output'


if __name__ == '__main__':
    main()
