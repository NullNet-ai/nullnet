"""Same-host counterpart: create and destroy dedicated encrypted edges on demand."""
import argparse
import concurrent.futures as futures
import json
import hashlib
import os
from pathlib import Path
import socket
import struct
import subprocess
import time

from node import create, redirect, original_inventory, cross, batch_wave
from prototype import Experiment, Route, namespace_socket, command
from netlink import attr, U, info
import native_crypto

BASE = 197000


class DirectMacsec:
    def __init__(self, root, target, fd, parent):
        self.root, self.target, self.fd, self.parent = root, target, fd, parent

    def get(self, name):
        return (self.root if name == self.parent else self.target).get(name)

    def create(self, name, kind, data, extra):
        return self.root.create(name, kind, data, extra + attr(28, U(self.fd)))


class Same(Experiment):
    def __init__(self, args):
        super().__init__(args)
        self.result['kind'] = 'same-host-on-demand-' + args.variant
        self.result['source_sha256'] = hashlib.sha256(Path(__file__).read_bytes()).hexdigest()

    def observe_window_replay(self, slot, frame):
        receiver = slot['sides'][1]['socket']
        with socket.socket(socket.AF_PACKET, socket.SOCK_RAW, socket.htons(3)) as tap:
            tap.bind((slot['sides'][0]['names']['transport'], 0))
            for _ in range(20):
                tap.send(frame)
        received = 0
        receiver.settimeout(.2)
        try:
            while True:
                receiver.recvfrom(2048)
                received += 1
        except socket.timeout:
            pass
        finally:
            receiver.settimeout(3)
        self.result['within_128_packet_window_replay'] = {'injected': 20, 'received': received}

    def prepare(self):
        n, gen, _ = self.io()
        self.family = gen.family()
        for number in range(self.args.slots):
            ident = BASE + number
            slot = {'id': ident, 'generation': 0, 'active': False, 'sides': [],
                    'group': 0x53000000 | (number // (self.args.slots // 2))}
            self.slots.append(slot)
            for side in range(2):
                role = 's' if side == 0 else 'c'
                names = {'bridge': f'br_{ident}_{role}', 'outer': f'ns_{ident}_{role}-o',
                         'inner': f'ns_{ident}_{role}-in', 'transport': f'veth-{ident}-{role}',
                         'macsec': f'macsec-{ident}-{role}'}
                if self.args.variant == 'direct':
                    names['inner'] = names['macsec']
                for name in names.values():
                    assert len(name) < 16 and n.get(name) is None
                e = {'names': names, 'indices': {}, 'side': side,
                     'mac': bytes([2, 0x29, 3, number >> 8, number & 255, side + 1])}
                slot['sides'].append(e)
                e['ip'] = socket.inet_ntoa(struct.pack('!I', 0x0A000000 + ident * 8 + 1 + side * 2))
                e['gateway'] = socket.inet_ntoa(struct.pack('!I', 0x0A000000 + ident * 8 + 2 + side * 2))
                e['socket'] = namespace_socket(self.targets[side], lambda: socket.socket(socket.AF_INET, socket.SOCK_DGRAM))
                e['socket'].bind(('0.0.0.0', 31000 + number))
                e['socket'].settimeout(3)
                if self.args.variant == 'bridge':
                    e['indices']['bridge'] = create(n, names['bridge'], 'bridge', extra=attr(4, U(1080)))
                    self.owned.append(names['bridge'])
        self.inventory('prepared')

    def activate(self, slot):
        start = last = time.monotonic()
        n, _, targets = self.io()
        a, b = slot['sides']
        assert not slot['active']
        slot['generation'] += 1
        key = os.urandom(32).hex()
        peer = info() + attr(3, b['names']['transport'].encode() + b'\0') + attr(1, b['mac'])
        peer += attr(4, U(1112)) + attr(27, U(slot['group']))
        a['indices']['transport'] = create(n, a['names']['transport'], 'veth', attr(1 | 32768, peer),
                                            attr(1, a['mac']) + attr(4, U(1112)), slot['group'])
        b['indices']['transport'] = n.get(b['names']['transport'])
        stages = {}
        for side, e in enumerate(slot['sides']):
            names, ix = e['names'], e['indices']
            if self.args.variant == 'direct':
                continue
            peer = info() + attr(3, names['inner'].encode() + b'\0') + attr(4, U(1080)) + attr(28, U(self.targets[side]))
            ix['outer'] = create(n, names['outer'], 'veth', attr(1 | 32768, peer), attr(4, U(1080)), slot['group'])
            ix['inner'] = targets[side].get(names['inner'])
        stages['create_links'] = time.monotonic() - last
        last = time.monotonic()
        for side, e in enumerate(slot['sides']):
            peer_mac = ':'.join(f'{x:02x}' for x in slot['sides'][1-side]['mac'])
            def install():
                route = DirectMacsec(n, targets[side], self.targets[side], e['names']['transport']) if self.args.variant == 'direct' else n
                return native_crypto.install(route, e['names']['macsec'], e['names']['transport'],
                                             peer_mac, key, family=self.family, replay=True)
            e['indices']['macsec'] = namespace_socket(self.targets[side], install) if self.args.variant == 'direct' else install()
            if self.args.variant == 'direct':
                e['indices']['inner'] = e['indices']['macsec']
        stages['crypto'] = time.monotonic() - last
        last = time.monotonic()
        for side, e in enumerate(slot['sides']):
            ix = e['indices']
            targets[side].address(ix['inner'], e['ip'])
            if self.args.variant == 'direct':
                pass
            elif self.args.variant == 'redirect':
                redirect(n, ix['outer'], ix['macsec'])
                redirect(n, ix['macsec'], ix['outer'])
                n.configure(ix['outer'], True)
                n.configure(ix['macsec'], True)
            else:
                n.address(ix['bridge'], e['gateway'])
                n.configure(ix['outer'], True, ix['bridge'])
                n.configure(ix['macsec'], True, ix['bridge'])
                n.configure(ix['bridge'], True)
            n.configure(ix['transport'], True)
            targets[side].configure(ix['inner'], True)
        stages['connect_address_enable'] = time.monotonic() - last
        last = time.monotonic()
        kinds = ['transport'] if self.args.variant == 'direct' else ['transport', 'macsec', 'outer']
        for kind in kinds + (['bridge'] if self.args.variant == 'bridge' else []):
            for e in slot['sides']:
                assert n.get(e['names'][kind]) == e['indices'][kind]
        for side, e in enumerate(slot['sides']):
            assert targets[side].get(e['names']['inner']) == e['indices']['inner']
        stages['readiness'] = time.monotonic() - last
        slot['active'] = True
        return {'seconds': time.monotonic() - start, 'stages': stages}

    def retire_start(self, slot):
        start = time.monotonic()
        n, _, _ = self.io()
        for e in slot['sides']:
            n.configure(e['indices']['transport'])
        revoked = time.monotonic()
        return {'start': start, 'stages': {'revoke': revoked - start}}

    def retire_finish(self, slot, row, deleted):
        n = self.io()[0]
        finish_start = time.monotonic()
        self.clear_flows(slot)
        for e in slot['sides']:
            for kind in ['transport', 'macsec', 'outer', 'inner']:
                e['indices'].pop(kind, None)
            if self.args.variant == 'bridge':
                index = e['indices']['bridge']
                n.configure(index)
                n.clear_neighbors(index)
                n.address(index, e['gateway'], True)
                assert n.get(e['names']['bridge']) == index
        slot['active'] = False
        row['stages'].update(delete_batch=deleted, flows_bridge_reset=time.monotonic() - finish_start)
        return {'seconds': time.monotonic() - row['start'], 'stages': row['stages']}

    def inventory(self, label):
        prefixes = [[]] + [['nsenter', '-t', str(pid), '-n'] for pid in self.args.pids]
        names = {name for s in self.slots for e in s['sides'] for name in e['names'].values()}
        links = [x for prefix in prefixes for x in json.loads(command(*prefix, 'ip', '-j', 'addr')) if x['ifname'] in names]
        assert len(links) == (2 * self.args.slots if self.args.variant == 'bridge' else 0), label
        assert all('UP' not in x['flags'] and not x.get('addr_info') for x in links), label
        ips = {e['ip'] for s in self.slots for e in s['sides']}
        for prefix in prefixes:
            flows = command(*prefix, 'conntrack', '-L', '-f', 'ipv4', '-p', 'udp')
            assert not any(f'src={ip} ' in flows or f'dst={ip} ' in flows for ip in ips), label
        self.result.setdefault('inventories', []).append({'label': label, 'retained_bridges': len(links), 'flows': 0})

    def cleanup(self):
        n, _, _ = self.io()
        for slot in self.slots:
            for e in slot['sides']:
                for kind in ['transport', 'outer']:
                    index = n.get(e['names'][kind])
                    if index is not None:
                        n.request(17, info(index))
        super().cleanup()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--variant', choices=['bridge', 'redirect', 'direct'], required=True)
    parser.add_argument('--shape', choices=['cc', 'hc', 'ch'], default='cc')
    parser.add_argument('--slots', type=int, default=64)
    parser.add_argument('--workers', type=int, default=32)
    parser.add_argument('--trials', type=int, default=3)
    parser.add_argument('--seconds', type=float, default=60)
    parser.add_argument('--turnovers', type=int, default=20)
    parser.add_argument('--output', required=True)
    args = parser.parse_args()
    args.attachment = 'container'
    assert args.shape == 'cc' or args.variant == 'direct'
    cross.base.BASE = BASE
    before = original_inventory()
    created = []
    experiment = None
    try:
        args.pids = []
        for side in range(2):
            name = f'nn29-minimal-same-{side}'
            assert subprocess.run(['docker', 'inspect', name], capture_output=True).returncode != 0
            command('docker', 'run', '-d', '--network', 'none', '--label', 'nullnet-prototype=20260929',
                    '--name', name, 'alpine:latest', 'sleep', 'infinity')
            created.append(name)
            args.pids.append(int(command('docker', 'inspect', '-f', '{{.State.Pid}}', name)))
            if args.shape[side] == 'h':
                args.pids[side] = os.getpid()
        experiment = Same(args)
        experiment.prepare()
        experiment.negative(experiment.slots[0], 'absent-before-creation')
        ids = list(range(args.slots))
        with futures.ThreadPoolExecutor(args.workers) as pool:
            list(pool.map(experiment.activate, experiment.slots))
            list(pool.map(experiment.traffic, experiment.slots))
            from proof import exchange, exchange_sockets, packet_socket
            exchange(experiment, True, 'active-frame-parity')
            ciphertext = experiment.capture(experiment.slots[0])
            experiment.observe_window_replay(experiment.slots[0], ciphertext)
            for _ in range(130):
                experiment.traffic(experiment.slots[0])
            experiment.replay(experiment.slots[0], ciphertext, 'current-key-replay-outside-128-packet-window')
            raw = socket.socket(socket.AF_PACKET, socket.SOCK_RAW, socket.htons(3))
            raw.bind((experiment.slots[0]['sides'][0]['names']['transport'], 0))
            receiver = packet_socket(experiment, experiment.slots[0]['sides'][1])
            try:
                exchange_sockets(experiment, [raw, receiver], False, 'plaintext-parent-injection')
            finally:
                raw.close()
                receiver.close()
            first = experiment.slots[0]['sides'][0]['names']['macsec']
            prefix = ['nsenter', '-t', str(args.pids[0]), '-n'] if args.variant == 'direct' else []
            command(*prefix, 'ip', 'macsec', 'set', first, 'tx', 'sa', '0', 'off')
            exchange(experiment, False, 'inactive-key')
            experiment.traffic(experiment.slots[1])
            command(*prefix, 'ip', 'macsec', 'set', first, 'tx', 'sa', '0', 'on')
            exchange(experiment, True, 'restored-key')
            batch_wave(experiment, [], experiment.slots, pool)
            experiment.inventory('smoke-retired')
            experiment.negative(experiment.slots[0], 'absent-after-retirement')
            list(pool.map(experiment.activate, experiment.slots))
            experiment.replay(experiment.slots[0], ciphertext, 'previous-generation-replay')
            batch_wave(experiment, [], experiment.slots, pool)
            experiment.inventory('replay-retired')
            print('SMOKE_PASSED', flush=True)
            for trial in range(args.trials):
                active, idle = ids[:args.slots//2], ids[args.slots//2:]
                list(pool.map(experiment.activate, [experiment.slots[i] for i in active]))
                samples = []
                start = time.monotonic()
                while time.monotonic() - start < args.seconds or len(samples) < args.turnovers * 2:
                    samples.append(batch_wave(experiment, [experiment.slots[i] for i in idle],
                                              [experiment.slots[i] for i in active], pool))
                    list(pool.map(experiment.traffic, [experiment.slots[i] for i in idle]))
                    active, idle = idle, active
                    if len(samples) % 10 == 0:
                        print('PROGRESS', trial, len(samples), round(time.monotonic() - start, 2), flush=True)
                duration = time.monotonic() - start
                batch_wave(experiment, [], [experiment.slots[i] for i in active], pool)
                drain = time.monotonic() - start - duration
                experiment.inventory(f'trial-{trial}')
                row = {'trial': trial, 'seconds': duration, 'setups': len(samples) * len(active),
                       'retirements': len(samples) * len(active), 'complete_cycles_per_second': len(samples) * len(active) / duration,
                       'final_drain_seconds': drain, 'samples': samples}
                experiment.result['trials'].append(row)
                experiment.save()
                print(json.dumps({k: v for k, v in row.items() if k != 'samples'}), flush=True)
        experiment.result['completed'] = True
    finally:
        try:
            if experiment:
                experiment.cleanup()
        finally:
            for name in created:
                command('docker', 'rm', '-f', name)
            after = original_inventory()
            checks = {key: before[key] == after[key] for key in before}
            Path(args.output + '.preservation.json').write_text(json.dumps({'before': before, 'after': after, 'checks': checks}, indent=2))
            print(json.dumps({'preservation': checks}), flush=True)
            assert all(checks.values()), checks


if __name__ == '__main__':
    main()
