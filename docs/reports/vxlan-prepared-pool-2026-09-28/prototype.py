"""Same-host retained-device experiment; run only against dedicated fixtures."""
import argparse
import concurrent.futures as futures
import hashlib
import json
import os
from pathlib import Path
import socket
import struct
import subprocess
import sys
import threading
import time

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "vxlan-lifecycle-2026-09-23"))
from netlink import NL, U, attr, info, nested
from native_crypto import Genl

GROUP = 0x4E280001
BASE = 190000
TLS = threading.local()


def command(*args):
    result = subprocess.run(args, check=True, capture_output=True, text=True, timeout=120)
    return result.stdout


def namespace_socket(fd, factory):
    original = os.open('/proc/thread-self/ns/net', os.O_RDONLY)
    try:
        os.setns(fd, os.CLONE_NEWNET)
        return factory()
    finally:
        os.setns(original, os.CLONE_NEWNET)
        os.close(original)


class Route(NL):
    def clear_neighbors(self, index):
        self.seq += 1
        body = struct.pack('BBHiHBB', 0, 0, 0, index, 0, 0, 0)
        self.s.sendto(struct.pack('IHHII', 16 + len(body), 30, 1 | 768, self.seq, 0) + body, (0, 0))
        entries = []
        done = False
        while not done:
            data = self.s.recv(65536)
            offset = 0
            while offset + 16 <= len(data):
                size, kind, flags, seq, pid = struct.unpack_from('IHHII', data, offset)
                if seq == self.seq:
                    if flags & 16:
                        raise RuntimeError('interrupted neighbor dump')
                    if kind == 3:
                        done = True
                    elif kind == 2:
                        raise RuntimeError(('neighbor dump', struct.unpack_from('i', data, offset + 16)[0]))
                    elif kind == 28:
                        entry = data[offset + 16:offset + size]
                        if entry[0] in (2, 10) and struct.unpack_from('i', entry, 4)[0] == index:
                            entries.append(entry)
                offset += (size + 3) & ~3
        for entry in entries:
            self.request(29, entry)

    def create(self, name, kind, data=b'', extra=b''):
        return self.request(16, info() + attr(3, name.encode() + b'\0')
                            + attr(27, U(GROUP)) + nested(kind, data) + extra, 5 | 512 | 1024 | 8)

    def configure(self, index, up=False, master=None, extra=b''):
        return self.request(19, info(index, int(up), 1)
                            + (attr(10, U(master)) if master is not None else b'') + extra)

    def address(self, index, address, delete=False):
        return self.request(21 if delete else 20, struct.pack('BBBBI', 2, 29, 0, 0, index)
                            + attr(1, socket.inet_aton(address)) + attr(2, socket.inet_aton(address)),
                            5 if delete else 5 | 512 | 1024)


class Conntrack(NL):
    def __init__(self):
        self.s = socket.socket(socket.AF_NETLINK, socket.SOCK_RAW, 12)
        self.s.bind((0, 0))
        self.s.settimeout(30)
        self.seq = 0

    def delete(self, source, destination, port):
        addresses = attr(1 | 32768, attr(1, socket.inet_aton(source)) + attr(2, socket.inet_aton(destination)))
        protocol = attr(2 | 32768, attr(1, b'\x11') + attr(2, struct.pack('!H', port)) + attr(3, struct.pack('!H', port)))
        try:
            self.request(258, bytes([2, 0, 0, 0]) + attr(1 | 32768, addresses + protocol))
        except RuntimeError as error:
            if error.args != (('netlink', 258, -2),):
                raise


class Experiment:
    def __init__(self, args):
        self.args = args
        self.root = os.open('/proc/self/ns/net', os.O_RDONLY)
        self.targets = [os.open(f'/proc/{pid}/ns/net', os.O_RDONLY) for pid in args.pids]
        self.family = None
        self.slots = []
        self.owned = []
        self.result = {'kind': 'same-host-retained-devices', 'slots': args.slots,
                       'workers': args.workers, 'attachment': args.attachment,
                       'source_sha256': hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
                       'kernel': os.uname().release, 'trials': []}

    def io(self):
        if not hasattr(TLS, 'io'):
            TLS.io = (Route(), Genl(), [namespace_socket(fd, Route) for fd in self.targets])
            TLS.conntrack = [Conntrack()] + [namespace_socket(fd, Conntrack) for fd in self.targets]
        return TLS.io

    def prepare(self):
        n = Route()
        g = Genl()
        self.family = g.family()
        g.s.close()
        begin = time.monotonic()
        for number in range(self.args.slots):
            ident = BASE + number
            slot = {'id': ident, 'generation': 0, 'active': False, 'sides': []}
            self.slots.append(slot)
            for side in range(2):
                role = 's' if side == 0 else 'c'
                names = {'bridge': f'br_{ident}_{role}', 'outer': f'ns_{ident}_{role}-o',
                         'inner': f'ns_{ident}_{role}-in', 'transport': f'veth-{ident}-{role}',
                         'macsec': f'macsec-{ident}-{role}'}
                assert all(len(name) < 16 for name in names.values())
                for name in names.values():
                    if n.get(name) is not None:
                        raise RuntimeError(f'refusing existing device {name}')
                mac = bytes([2, 0x28, (ident >> 16) & 255, (ident >> 8) & 255, ident & 255, side + 1])
                entry = {'names': names, 'mac': mac, 'indices': {}, 'side': side}
                slot['sides'].append(entry)
                entry['indices']['bridge'] = n.create(names['bridge'], 'bridge', extra=attr(1, mac) + attr(4, U(1080)))[0]
                self.owned.append(names['bridge'])
                peer = info() + attr(3, names['inner'].encode() + b'\0') + attr(4, U(1080)) + attr(27, U(GROUP))
                entry['indices']['outer'] = n.create(names['outer'], 'veth', attr(1 | 32768, peer), attr(4, U(1080)))[0]
                self.owned.append(names['outer'])
                entry['indices']['inner'] = n.get(names['inner'])
                entry['ip'] = socket.inet_ntoa(struct.pack('!I', 0x0A000000 + ident * 8 + 1 + side * 2))
                entry['gateway'] = socket.inet_ntoa(struct.pack('!I', 0x0A000000 + ident * 8 + 2 + side * 2))
                entry['socket'] = namespace_socket(self.targets[side], lambda: socket.socket(socket.AF_INET, socket.SOCK_DGRAM))
                entry['socket'].bind(('0.0.0.0', 31000 + number))
                entry['socket'].settimeout(2)
            a, b = slot['sides']
            peer = info() + attr(3, b['names']['transport'].encode() + b'\0') + attr(1, b['mac']) + attr(4, U(1112)) + attr(27, U(GROUP))
            n.create(a['names']['transport'], 'veth', attr(1 | 32768, peer), attr(1, a['mac']) + attr(4, U(1112)))
            self.owned.append(a['names']['transport'])
            for side, entry in enumerate(slot['sides']):
                parent = n.get(entry['names']['transport'])
                entry['indices']['transport'] = parent
                settings = attr(2, struct.pack('!H', 1)) + attr(4, struct.pack('Q', 0x0080C20001000002))
                settings += attr(5, U(128)) + attr(7, b'\x01') + attr(8, b'\x01') + attr(12, b'\x01') + attr(13, b'\x02')
                n.create(entry['names']['macsec'], 'macsec', settings, attr(5, U(parent)) + attr(4, U(1080)))
                entry['indices']['macsec'] = n.get(entry['names']['macsec'])
                entry['sci'] = slot['sides'][1-side]['mac'] + b'\0\x01'
                gen = Genl()
                gen.call(self.family, 1, attr(1, U(entry['indices']['macsec']))
                         + attr(2 | 32768, attr(1, entry['sci']) + attr(2, b'\x01')))
                gen.s.close()
        if self.args.attachment == 'container':
            for slot in self.slots:
                for side, entry in enumerate(slot['sides']):
                    n.configure(entry['indices']['inner'], extra=attr(28, U(self.targets[side])))
                    target = namespace_socket(self.targets[side], Route)
                    entry['indices']['inner'] = target.get(entry['names']['inner'])
                    target.s.close()
        n.s.close()
        self.io()
        for slot in self.slots:
            self.clear_flows(slot)
        self.result['preparation_seconds'] = time.monotonic() - begin
        self.inventory('prepared')

    def clear_flows(self, slot):
        a, b = slot['sides']
        for conntrack in TLS.conntrack:
            conntrack.delete(a['ip'], b['ip'], 31000 + slot['id'] - BASE)
            conntrack.delete(b['ip'], a['ip'], 31000 + slot['id'] - BASE)

    def associations(self, g, entry, key=None):
        base = attr(1, U(entry['indices']['macsec']))
        rx = attr(2 | 32768, attr(1, entry['sci']))
        if key is None:
            sa = attr(3 | 32768, attr(1, b'\0'))
            inactive = attr(3 | 32768, attr(1, b'\0') + attr(2, b'\0'))
            g.call(self.family, 6, base + inactive)
            g.call(self.family, 9, base + rx + inactive)
            g.call(self.family, 5, base + sa)
            g.call(self.family, 8, base + rx + sa)
        else:
            sa = attr(3 | 32768, attr(1, b'\0') + attr(2, b'\x01') + attr(3, U(1))
                      + attr(4, key) + attr(5, hashlib.sha256(key).digest()[:16]))
            g.call(self.family, 4, base + sa)
            g.call(self.family, 7, base + rx + sa)

    def activate(self, slot):
        start = time.monotonic()
        n, g, endpoints = self.io()
        assert not slot['active']
        slot['generation'] += 1
        key = os.urandom(32)
        stages = {}
        last = start
        for side, entry in enumerate(slot['sides']):
            idx = entry['indices']
            if self.args.attachment == 'move':
                n.configure(idx['inner'], extra=attr(28, U(self.targets[side])))
                idx['inner'] = endpoints[side].get(entry['names']['inner'])
            endpoints[side].address(idx['inner'], entry['ip'])
            endpoints[side].configure(idx['inner'], True)
            n.address(idx['bridge'], entry['gateway'])
        stages['attachment_addresses'] = time.monotonic() - last
        last = time.monotonic()
        for entry in slot['sides']:
            self.associations(g, entry, key)
        stages['fresh_sas'] = time.monotonic() - last
        last = time.monotonic()
        for entry in slot['sides']:
            idx = entry['indices']
            n.configure(idx['outer'], True, idx['bridge'])
            n.configure(idx['macsec'], True, idx['bridge'])
            n.configure(idx['bridge'], True)
        for entry in slot['sides']:
            n.configure(entry['indices']['transport'], True)
        stages['attach_enable'] = time.monotonic() - last
        last = time.monotonic()
        # Single-link GET synchronizes deferred activation, lower links first.
        synchronized = set()
        for kind in ['transport', 'macsec', 'outer', 'bridge', 'inner']:
            for side, entry in enumerate(slot['sides']):
                target = endpoints[side] if kind == 'inner' else n
                name, index = entry['names'][kind], entry['indices'][kind]
                if name not in synchronized:
                    assert target.get(name) == index
                    synchronized.add(name)
        stages['readiness'] = time.monotonic() - last
        slot['active'] = True
        return {'seconds': time.monotonic() - start, 'stages': stages}

    def traffic(self, slot):
        a, b = slot['sides']
        payload = struct.pack('!II', slot['id'], slot['generation']) + os.urandom(32)
        port = 31000 + slot['id'] - BASE
        a['socket'].sendto(payload, (b['ip'], port))
        received, sender = b['socket'].recvfrom(2048)
        assert received == payload and sender[0] == a['ip']
        b['socket'].sendto(payload, sender)
        received, sender = a['socket'].recvfrom(2048)
        assert received == payload and sender[0] == b['ip']

    def retire(self, slot):
        start = time.monotonic()
        n, g, endpoints = self.io()
        assert slot['active']
        stages = {}
        for entry in slot['sides']:
            n.configure(entry['indices']['transport'])
        for side, entry in enumerate(slot['sides']):
            idx = entry['indices']
            n.configure(idx['bridge'])
            n.configure(idx['outer'], master=0)
            n.configure(idx['macsec'], master=0)
            endpoints[side].configure(idx['inner'])
        stages['revoke_detach'] = time.monotonic() - start
        last = time.monotonic()
        for entry in slot['sides']:
            self.associations(g, entry)
        stages['delete_sas'] = time.monotonic() - last
        last = time.monotonic()
        self.clear_flows(slot)
        stages['conntrack'] = time.monotonic() - last
        last = time.monotonic()
        for side, entry in enumerate(slot['sides']):
            idx = entry['indices']
            endpoints[side].clear_neighbors(idx['inner'])
            n.clear_neighbors(idx['bridge'])
            endpoints[side].address(idx['inner'], entry['ip'], True)
            if self.args.attachment == 'move':
                endpoints[side].configure(idx['inner'], extra=attr(28, U(self.root)))
                idx['inner'] = n.get(entry['names']['inner'])
            n.address(idx['bridge'], entry['gateway'], True)
        slot['active'] = False
        stages['reclaim_addresses'] = time.monotonic() - last
        return {'seconds': time.monotonic() - start, 'stages': stages}

    def inventory(self, label):
        prefixes = [[]] + [['nsenter', '-t', str(pid), '-n'] for pid in self.args.pids]
        links = [link for prefix in prefixes for link in json.loads(command(*prefix, 'ip', '-j', '-d', 'link', 'show'))]
        names = {name for slot in self.slots for e in slot['sides'] for name in e['names'].values()}
        owned = [link for link in links if link['ifname'] in names]
        assert len(owned) == len(names), (label, len(owned), len(names))
        assert all('UP' not in link['flags'] and 'master' not in link for link in owned), label
        addresses = [x for prefix in prefixes for x in json.loads(command(*prefix, 'ip', '-j', 'addr', 'show')) if x['ifname'] in names]
        assert all(not x['addr_info'] for x in addresses), label
        neighbors = [x for prefix in prefixes for x in json.loads(command(*prefix, 'ip', '-j', 'neigh', 'show')) if x.get('dev') in names]
        assert not neighbors, (label, neighbors)
        fdb = [x for x in json.loads(command('bridge', '-j', 'fdb', 'show')) if x.get('dev') in names]
        assert all('permanent' in x.get('state', []) for x in fdb), (label, fdb)
        mdb = [x for x in json.loads(command('bridge', '-j', 'mdb', 'show')) if x.get('dev') in names]
        assert not any(x.get('mdb') for x in mdb), (label, mdb)
        crypto = command('ip', 'macsec', 'show')
        stable_names = {e['names'][kind] for s in self.slots for e in s['sides'] for kind in ('bridge', 'outer', 'transport', 'macsec')}
        stable_indices = {link['ifname']: link['ifindex'] for link in owned if link['ifname'] in stable_names}
        if hasattr(self, 'stable_indices'):
            assert self.stable_indices == stable_indices, 'retained root devices changed'
        self.stable_indices = stable_indices
        owned_ips = {e['ip'] for s in self.slots for e in s['sides']}
        for prefix in prefixes:
            listing = command(*prefix, 'conntrack', '-L', '-f', 'ipv4', '-p', 'udp')
            assert not any(f'src={ip} ' in listing or f'dst={ip} ' in listing for ip in owned_ips), 'stale conntrack'
        own_crypto = []
        capture = False
        for line in crypto.splitlines():
            if line and not line[0].isspace():
                capture = any(f": {e['names']['macsec']}:" in line for s in self.slots for e in s['sides'])
            if capture:
                own_crypto.append(line)
                assert 'PN ' not in line, (label, 'residual SA')
        self.result.setdefault('inventories', []).append({'label': label, 'links': len(owned),
                                                         'addresses': 0, 'neighbors': 0,
                                                         'dynamic_fdb': 0, 'sas': 0})

    def negative(self, slot, label):
        a, b = slot['sides']
        a['socket'].settimeout(.1)
        b['socket'].settimeout(.1)
        delivered = False
        try:
            try:
                a['socket'].sendto(b'negative', (b['ip'], 31000 + slot['id'] - BASE))
                b['socket'].recvfrom(2048)
                delivered = True
            except (socket.timeout, OSError):
                pass
            assert not delivered, label
            self.result.setdefault('negative_checks', []).append(label)
        finally:
            a['socket'].settimeout(2)
            b['socket'].settimeout(2)

    def capture(self, slot):
        with socket.socket(socket.AF_PACKET, socket.SOCK_RAW, socket.htons(3)) as tap:
            tap.bind((slot['sides'][0]['names']['transport'], 0))
            tap.settimeout(2)
            self.traffic(slot)
            while True:
                frame, address = tap.recvfrom(4096)
                if address[2] == 4 and frame[12:14] == b'\x88\xe5' and len(frame) >= 100:
                    return frame

    def replay(self, slot, frame, label):
        receiver = slot['sides'][1]['socket']
        with socket.socket(socket.AF_PACKET, socket.SOCK_RAW, socket.htons(3)) as tap:
            tap.bind((slot['sides'][0]['names']['transport'], 0))
            for _ in range(20):
                assert tap.send(frame) == len(frame)
        receiver.settimeout(.2)
        try:
            try:
                receiver.recvfrom(2048)
            except socket.timeout:
                pass
            else:
                raise AssertionError(label)
        finally:
            receiver.settimeout(2)
        self.traffic(slot)
        self.result.setdefault('negative_checks', []).append(label)

    def run(self):
        self.prepare()
        print('PREPARED', self.args.slots, flush=True)
        self.negative(self.slots[0], 'idle-before-first-lease')
        with futures.ThreadPoolExecutor(max_workers=self.args.workers) as pool:
            list(pool.map(self.activate, self.slots))
            list(pool.map(self.traffic, self.slots))
            n, g, endpoints = self.io()
            victim = self.slots[0]
            ciphertext = self.capture(victim)
            for _ in range(130):
                self.traffic(victim)
            self.replay(victim, ciphertext, 'same-key-replay-outside-128-packet-window-20-frames')
            self.associations(g, victim['sides'][1])
            self.negative(victim, 'missing-receive-crypto')
            self.traffic(self.slots[1])
            self.associations(g, victim['sides'][1], os.urandom(32))
            self.negative(victim, 'wrong-receive-key')
            self.traffic(self.slots[1])
            for side, entry in enumerate(victim['sides']):
                for route, index in [(endpoints[side], entry['indices']['inner']), (n, entry['indices']['bridge'])]:
                    route.request(28, struct.pack('BBHiHBB', 2, 0, 0, index, 128, 0, 1)
                                  + attr(1, socket.inet_aton('192.0.2.99')) + attr(2, bytes.fromhex('022800000099')), 5 | 512 | 1024)
            list(pool.map(self.retire, self.slots))
            self.inventory('after-smoke-reset')
            self.negative(self.slots[0], 'idle-after-lease')
            self.activate(victim)
            self.replay(victim, ciphertext, 'previous-lease-key-replay-20-frames')
            self.retire(victim)
            self.inventory('after-replay-reset')
            self.save()
            print('ISOLATION_AND_RESET_PASSED', flush=True)
            half = len(self.slots) // 2
            for trial in range(self.args.trials):
                active, idle = self.slots[:half], self.slots[half:]
                list(pool.map(self.activate, active))
                samples = {'setup': [], 'retire': []}
                started = time.monotonic()
                waves = 0
                while time.monotonic() - started < self.args.seconds or waves < self.args.turnovers * 2:
                    jobs = []
                    for old, new in zip(active, idle):
                        jobs.append(('retire', pool.submit(self.retire, old)))
                        jobs.append(('setup', pool.submit(self.activate, new)))
                    for kind, task in jobs:
                        samples[kind].append(task.result())
                    list(pool.map(self.traffic, idle))
                    active, idle = idle, active
                    waves += 1
                    if waves % 10 == 0:
                        print('PROGRESS', trial, waves, time.monotonic() - started, flush=True)
                duration = time.monotonic() - started
                list(pool.map(self.retire, active))
                self.inventory(f'after-trial-{trial}')
                record = {'trial': trial, 'seconds': duration, 'waves': waves,
                          'setups': len(samples['setup']), 'retirements': len(samples['retire']),
                          'setup_per_second': len(samples['setup']) / duration,
                          'retire_per_second': len(samples['retire']) / duration,
                          'samples': samples}
                self.result['trials'].append(record)
                print(json.dumps({k: v for k, v in record.items() if k != 'samples'}), flush=True)
                self.save()

    def save(self):
        Path(self.args.output).write_text(json.dumps(self.result, indent=2) + '\n')

    def cleanup(self):
        started = time.monotonic()
        n = Route()
        errors = []
        self.io()
        for slot in self.slots:
            if len(slot['sides']) == 2 and all('ip' in e for e in slot['sides']):
                try:
                    self.clear_flows(slot)
                except Exception as error:
                    errors.append(('conntrack', str(error)))
        for name in reversed(self.owned):
            try:
                index = n.get(name)
                if index is not None:
                    n.request(17, info(index))
            except Exception as error:
                errors.append((name, str(error)))
        n.s.close()
        self.result['destruction_seconds'] = time.monotonic() - started
        self.result['cleanup_errors'] = errors
        for slot in self.slots:
            for entry in slot['sides']:
                if 'socket' in entry:
                    entry['socket'].close()
        for fd in self.targets + [self.root]:
            os.close(fd)
        self.save()
        if errors:
            raise RuntimeError(errors)


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--pids', nargs=2, type=int, required=True)
    parser.add_argument('--attachment', choices=['move', 'container'], default='move')
    parser.add_argument('--slots', type=int, default=64)
    parser.add_argument('--workers', type=int, default=32)
    parser.add_argument('--trials', type=int, default=3)
    parser.add_argument('--seconds', type=float, default=60)
    parser.add_argument('--turnovers', type=int, default=20)
    parser.add_argument('--output', required=True)
    args = parser.parse_args()
    assert args.slots >= 2 and args.slots % 2 == 0
    experiment = Experiment(args)
    try:
        experiment.run()
        experiment.result['completed'] = True
    except Exception as error:
        experiment.result['error'] = repr(error)
        raise
    finally:
        experiment.cleanup()
