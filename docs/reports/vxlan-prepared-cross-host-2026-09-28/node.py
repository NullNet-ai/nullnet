"""Root-namespace cross-host pool endpoint, controlled over a private SSH pipe."""
import argparse
import errno
import concurrent.futures as futures
import hashlib
import itertools
import json
import os
from pathlib import Path
import re
import select
import signal
import socket
import struct
import subprocess
import sys
import time

sys.path[:0] = [str(Path(__file__).resolve().parents[1] / 'vxlan-prepared-pool-2026-09-28'),
               str(Path(__file__).resolve().parents[1] / 'vxlan-lifecycle-2026-09-23')]
import prototype as base
from prototype import Experiment, Route, Conntrack, namespace_socket, command, TLS
from netlink import attr, U, info
import native_xfrm as crypto

base.GROUP = 0x4E290001
crypto.UNIQUE_REQID = True
crypto.REPLAY_WINDOW = 4096
BASE = 1950000
PORT = 4790
SPI = 0x4E290000


def run(args, acceptable=(0,)):
    result = subprocess.run(args, capture_output=True, text=True, timeout=60)
    if result.returncode not in acceptable:
        raise RuntimeError((args[:3], result.returncode, result.stderr))
    return result.stdout


def original_inventory():
    containers = json.loads(command('docker', 'inspect', *command('docker', 'ps', '-q').split()))
    states = command('ip', 'xfrm', 'state')
    return {'containers': {c['Id']: {'pid': c['State']['Pid'], 'started': c['State']['StartedAt'],
                                    'networks': c['NetworkSettings']['Networks']} for c in containers},
            'links': [(l['ifindex'], l['ifname']) for l in json.loads(command('ip', '-j', 'link'))],
            'routes': json.loads(command('ip', '-j', 'route', 'show', 'table', 'all')),
            'services': command('systemctl', 'show', 'nullnet-client', 'nullnet-server', 'nullnet-proxy', '-p', 'MainPID', '-p', 'NRestarts'),
            'xfrm_hash': hashlib.sha256(states.encode()).hexdigest(),
            'policy_hash': hashlib.sha256(command('ip', 'xfrm', 'policy').encode()).hexdigest(),
            'iptables_hash': hashlib.sha256('\n'.join(re.sub(r'\[\d+:\d+\]', '[counters]', line)
                for line in command('iptables-save').splitlines() if not line.startswith('#')).encode()).hexdigest()}


class Cross(Experiment):
    def __init__(self, args):
        super().__init__(args)
        self.local = f'198.18.28.{103 + args.side}'
        self.remote = f'198.18.28.{104 - args.side}'
        self.keys = {}
        self.rules = []
        self.infrastructure = []
        self.policy = False
        self.packet = None
        self.wire = None
        self.ciphertext = []
        self.faults = {}
        self.capture_process = None
        self.endpoint_capture = None
        self.last_traffic = {}
        self.trace_path = None
        self.pool = futures.ThreadPoolExecutor(args.workers)
        self.result['kind'] = 'cross-host-retained-vxlan-ipsec'
        self.result['cross_source_sha256'] = hashlib.sha256(Path(__file__).read_bytes()).hexdigest()

    def io(self):
        if not hasattr(TLS, 'io'):
            TLS.io = (Route(), crypto.Xfrm(), [namespace_socket(self.targets[0], Route)])
            TLS.conntrack = [Conntrack(), namespace_socket(self.targets[0], Conntrack)]
        return TLS.io

    def setup_underlay(self):
        command('ip', 'addr', 'add', self.local + '/32', 'dev', 'lo')
        self.infrastructure.append(['ip', 'addr', 'del', self.local + '/32', 'dev', 'lo'])
        command('ip', 'route', 'add', self.remote + '/32', 'via', f'192.168.1.{104-self.args.side}', 'dev', 'ens18')
        self.infrastructure.append(['ip', 'route', 'del', self.remote + '/32', 'via', f'192.168.1.{104-self.args.side}', 'dev', 'ens18'])
        maps = json.loads(command('bpftool', '-j', 'map', 'show'))
        peers = [m for m in maps if m.get('name') == 'PEERS']
        assert len(peers) == 1
        key = [f'{byte:02x}' for byte in socket.inet_aton(self.remote)[::-1]]
        command('bpftool', 'map', 'update', 'id', str(peers[0]['id']), 'key', 'hex', *key, 'value', 'hex', '01', 'noexist')
        self.infrastructure.append(['bpftool', 'map', 'delete', 'id', str(peers[0]['id']), 'key', 'hex', *key])
        for chain, direction, source, destination in [('INPUT', 'in', self.remote, self.local), ('OUTPUT', 'out', self.local, self.remote)]:
            rule = ['-s', source, '-d', destination, '-p', 'udp', '--dport', str(PORT), '-m', 'policy', '--dir', direction,
                    '--pol', 'none', '-m', 'comment', '--comment', 'nn28-cross-prototype', '-j', 'DROP']
            command('iptables', '-w', '2', '-I', chain, '1', *rule)
            self.rules.append(['iptables', '-w', '2', '-D', chain, *rule])
        command('ip', 'xfrm', 'policy', 'add', 'src', self.remote, 'dst', self.local, 'proto', 'udp', 'dport', str(PORT),
                'dir', 'in', 'tmpl', 'src', self.remote, 'dst', self.local, 'proto', 'esp', 'mode', 'transport')
        self.policy = True

    def prepare(self):
        begin = time.monotonic()
        self.setup_underlay()
        n, x, endpoints = self.io()
        target = endpoints[0]
        side = self.args.side
        for number in range(self.args.slots):
            ident = BASE + number
            role = 's' if side == 0 else 'c'
            names = {'bridge': f'br_{ident}_{role}', 'outer': f'ns_{ident}_{role}-o',
                     'inner': f'ns_{ident}_{role}-in', 'transport': f'nnv_{ident}_{role}', 'macsec': f'nnv_{ident}_{role}'}
            assert all(len(name) < 16 for name in names.values())
            for name in set(names.values()):
                assert n.get(name) is None, ('existing device', name)
            entry = {'names': names, 'indices': {}, 'side': side, 'number': number}
            slot = {'id': ident, 'generation': 0, 'active': False, 'sides': [entry]}
            self.slots.append(slot)
            indices = entry['indices']
            indices['bridge'] = n.create(names['bridge'], 'bridge', extra=attr(4, U(1080)))[0]
            self.owned.append(names['bridge'])
            peer = info() + attr(3, names['inner'].encode() + b'\0') + attr(4, U(1080)) + attr(28, U(self.targets[0]))
            indices['outer'] = n.create(names['outer'], 'veth', attr(1 | 32768, peer), attr(4, U(1080)))[0]
            self.owned.append(names['outer'])
            indices['inner'] = target.get(names['inner'])
            vxlan = attr(1, U(ident)) + attr(4, socket.inet_aton(self.local)) + attr(2, socket.inet_aton(self.remote))
            vxlan += attr(15, struct.pack('!H', PORT)) + attr(7, b'\0')
            n.create(names['transport'], 'vxlan', vxlan, attr(4, U(1080)))
            self.owned.append(names['transport'])
            indices['transport'] = indices['macsec'] = n.get(names['transport'])
            entry['mark'] = 0x4E800000 | ident
            crypto.tc_install(n, indices['transport'], entry['mark'])
            for key, offset in [('ip', 1 + side * 2), ('peer_ip', 3 - side * 2), ('gateway', 2 + side * 2)]:
                entry[key] = socket.inet_ntoa(struct.pack('!I', 0x0A000000 + ident * 8 + offset))
            entry['socket'] = namespace_socket(self.targets[0], lambda: socket.socket(socket.AF_INET, socket.SOCK_DGRAM))
            entry['socket'].bind(('0.0.0.0', 31000 + number))
            entry['socket'].settimeout(3)
            self.clear_flows(slot)
        self.result['preparation_seconds'] = time.monotonic() - begin
        self.inventory('prepared')
        if self.args.trace:
            self.trace_path = Path('/sys/kernel/tracing/instances/nn28-pool')
            self.trace_path.mkdir()
            (self.trace_path / 'trace_clock').write_text('mono')
            (self.trace_path / 'buffer_size_kb').write_text('4096')
            (self.trace_path / 'events/net/net_dev_queue/filter').write_text('name ~ "ns_195*" || name ~ "nnv_195*"')
            (self.trace_path / 'events/net/net_dev_queue/enable').write_text('1')
            (self.trace_path / 'events/skb/kfree_skb/filter').write_text('reason == 55')
            (self.trace_path / 'events/skb/kfree_skb/enable').write_text('1')
            self.capture_log = open(self.args.output + '.tcpdump.log', 'w')
            self.capture_process = subprocess.Popen(['tcpdump', '-i', 'any', '-s', '0', '-U', '-w', self.args.output + '.pcap',
                'host', self.local, 'or', 'host', self.remote, 'or', 'net', '10.238.0.0/16'], stderr=self.capture_log)
            self.endpoint_log = open(self.args.output + '.endpoint.log', 'w')
            self.endpoint_capture = subprocess.Popen(['nsenter', '-t', str(self.args.pids[0]), '-n',
                'tcpdump', '-i', 'any', '-s', '0', '-U', '-w', self.args.output + '.endpoint.pcap',
                'net', '10.238.0.0/16'], stderr=self.endpoint_log)
            time.sleep(.2)

    def diagnostics(self):
        if self.trace_path:
            (self.trace_path / 'tracing_on').write_text('0')
            Path(self.args.output + '.kernel-trace').write_text((self.trace_path / 'trace').read_text())
        states = command('ip', '-s', 'xfrm', 'state')
        self.result['diagnostics'] = {
            'xfrm_counters': Path('/proc/net/xfrm_stat').read_text(),
            'states': '\n'.join(line for line in states.splitlines() if not any(word in line for word in ['aead ', 'auth ', 'enc '])),
            'links': json.loads(command('ip', '-j', '-s', 'link')),
            'neighbors': json.loads(command('ip', '-j', 'neigh')),
            'endpoint_neighbors': json.loads(command('nsenter', '-t', str(self.args.pids[0]), '-n', 'ip', '-j', 'neigh')),
            'endpoint_links': json.loads(command('nsenter', '-t', str(self.args.pids[0]), '-n', 'ip', '-j', '-s', '-d', 'link')),
            'endpoint_addresses': json.loads(command('nsenter', '-t', str(self.args.pids[0]), '-n', 'ip', '-j', 'addr')),
            'endpoint_routes': json.loads(command('nsenter', '-t', str(self.args.pids[0]), '-n', 'ip', '-j', 'route', 'show', 'table', 'all')),
            'endpoint_snmp': command('nsenter', '-t', str(self.args.pids[0]), '-n', 'cat', '/proc/net/snmp'),
            'last_traffic': self.last_traffic,
            'fdb': json.loads(command('bridge', '-j', 'fdb', 'show'))}
        self.save()
        return {'saved': True}

    def associations(self, x, entry, key=None):
        number, mark = entry['number'], entry['mark']
        side = self.args.side
        if key is None:
            x.policy(self.local, self.remote, SPI + number * 2 + side, PORT, mark, 1, True)
            for direction in [side, 1-side]:
                a, b = (self.local, self.remote) if direction == side else (self.remote, self.local)
                x.state(a, b, SPI + number * 2 + direction, '', mark, inbound=direction != side, delete=True)
        else:
            for direction in [side, 1-side]:
                a, b = (self.local, self.remote) if direction == side else (self.remote, self.local)
                derived = hashlib.sha256((self.keys[number] + str(direction)).encode()).hexdigest()
                x.state(a, b, SPI + number * 2 + direction, derived, mark, inbound=direction != side)
            x.policy(self.local, self.remote, SPI + number * 2 + side, PORT, mark, 1)

    def clear_flows(self, slot):
        entry = slot['sides'][0]
        for conntrack in TLS.conntrack:
            conntrack.delete(entry['ip'], entry['peer_ip'], 31000 + entry['number'])
            conntrack.delete(entry['peer_ip'], entry['ip'], 31000 + entry['number'])

    def inventory(self, label):
        super().inventory(label)
        names = {name for slot in self.slots for e in slot['sides'] for name in e['names'].values()}
        fdb = [entry for entry in json.loads(command('bridge', '-j', 'fdb', 'show')) if entry.get('ifname', entry.get('dev')) in names]
        assert all('permanent' in entry.get('state', []) for entry in fdb), (label, 'stale FDB', fdb)
        states = command('ip', 'xfrm', 'state')
        assert f'src {self.local} ' not in states and f'dst {self.local}\n' not in states, 'idle SA remains'

    def traffic(self, number):
        slot = self.slots[number]
        entry = slot['sides'][0]
        payload = b'NN28' + struct.pack('!IIB', slot['id'], slot['generation'], self.args.side) + os.urandom(32)
        record = {'generation': slot['generation'], 'send_start': time.time(), 'monotonic': time.monotonic()}
        self.last_traffic[number] = record
        entry['socket'].sendto(payload, (entry['peer_ip'], 31000 + number))
        record['send_done'] = time.time()
        try:
            data, peer = entry['socket'].recvfrom(4096)
        except TimeoutError:
            raise RuntimeError(('traffic timeout', number, slot['generation'], self.args.side)) from None
        assert data[:13] == b'NN28' + struct.pack('!IIB', slot['id'], slot['generation'], 1-self.args.side)
        assert peer[0] == entry['peer_ip']
        record['receive_done'] = time.time()

    def wave(self, request):
        jobs = []
        for down, up in itertools.zip_longest(request.get('down', []), request.get('up', [])):
            for kind, item in [('down', down), ('up', up)]:
                if item is None:
                    continue
                if kind == 'up':
                    number, key = item
                    self.keys[number] = key
                    jobs.append((kind, self.pool.submit(self.activate, self.slots[number])))
                else:
                    jobs.append((kind, self.pool.submit(self.retire, self.slots[item])))
        return {kind: [future.result() for label, future in jobs if kind == label] for kind in ['up', 'down']}

    def quiet(self, numbers):
        sockets = [self.slots[number]['sides'][0]['socket'] for number in numbers]
        ready, _, _ = select.select(sockets, [], [], .3)
        received = []
        for sock in ready:
            try:
                data, _ = sock.recvfrom(4096)
                received.append(len(data))
            except OSError:
                pass
        assert not received, ('unexpected delivery', received)
        return {'delivered': 0}

    def fault(self, number, mode):
        entry = self.slots[number]['sides'][0]
        x = self.io()[1]
        direction = 1-self.args.side
        if self.faults.get(number) != 'missing':
            x.state(self.remote, self.local, SPI + number * 2 + direction, '', entry['mark'], inbound=True, delete=True)
        if mode != 'missing':
            key = hashlib.sha256((self.keys[number] + str(direction)).encode()).hexdigest() if mode == 'restore' else os.urandom(32).hex()
            x.state(self.remote, self.local, SPI + number * 2 + direction, key, entry['mark'], inbound=True)
        self.faults[number] = mode
        return {'fault': mode}

    def packets(self, request):
        from proof import frames
        op = request['op']
        entry = self.slots[0]['sides'][0]
        if op == 'packet-open':
            if self.packet:
                self.packet.close()
            def create():
                sock = socket.socket(socket.AF_PACKET, socket.SOCK_RAW, socket.htons(3))
                sock.bind((entry['names']['inner'], 0))
                sock.setsockopt(263, 8, 1)
                return sock
            self.packet = namespace_socket(self.targets[0], create)
            return {'opened': True}
        if op == 'packet-send':
            sent = 0
            for frame in frames():
                try:
                    self.packet.send(frame)
                    sent += 1
                except OSError as error:
                    if error.errno != errno.ENETDOWN or request['active']:
                        raise
            return {'sent': sent}
        if op == 'packet-receive':
            found = []
            deadline = time.monotonic() + (3 if request['active'] else .3)
            while time.monotonic() < deadline:
                ready, _, _ = select.select([self.packet], [], [], .05)
                if not ready:
                    continue
                try:
                    data, ancillary, _, address = self.packet.recvmsg(4096, 1024)
                except OSError as error:
                    if not request['active'] and error.errno == errno.ENETDOWN:
                        break
                    raise
                if b'NN28_FRAME_' not in data or address[2] == 4:
                    continue
                for level, kind, value in ancillary:
                    if level == 263 and kind == 8:
                        status, _, _, _, _, tci, tpid = struct.unpack('IIIHHHH', value[:20])
                        if status & 16:
                            data = data[:12] + struct.pack('!HH', tpid if status & 64 else 0x8100, tci) + data[12:]
                found.append(data)
                if request['active'] and len(found) == len(frames()):
                    break
            assert sorted(found) == sorted(frames()) if request['active'] else not found, ('frame delivery', len(found))
            return {'received': len(found)}
        if op == 'wire-open':
            self.wire = socket.socket(socket.AF_PACKET, socket.SOCK_RAW, socket.htons(3))
            self.wire.bind(('ens18', 0))
            return {'opened': True}
        if op == 'wire-check':
            packets = []
            while select.select([self.wire], [], [], .1)[0]:
                frame, _ = self.wire.recvfrom(65536)
                if frame[12:14] == b'\x08\x00' and frame[26:30] == socket.inet_aton(self.local) and frame[30:34] == socket.inet_aton(self.remote):
                    packets.append(frame)
            assert packets and all(frame[23] == 50 for frame in packets), ('unencrypted underlay', [f[23] for f in packets])
            self.ciphertext = packets
            return {'esp_packets': len(packets), 'plaintext': 0}
        if op == 'replay':
            assert self.ciphertext
            for frame in self.ciphertext:
                self.wire.send(frame)
            return {'replayed': len(self.ciphertext)}
        if op == 'inject':
            sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
            blocked = 0
            try:
                sock.bind((self.local, 0))
                if request['kind'] == 'wrong-vni':
                    sock.setsockopt(socket.SOL_SOCKET, 36, self.slots[1]['sides'][0]['mark'])
                header = b'\x08\0\0\0' + struct.pack('!I', BASE << 8)
                for frame in frames():
                    if request['kind'] == 'plaintext':
                        udp = struct.pack('!HHHH', 45000, PORT, 16 + len(frame), 0) + header + frame
                        ip = struct.pack('!BBHHHBBH4s4s', 0x45, 0, 20 + len(udp), 0, 0, 64, 17, 0,
                                         socket.inet_aton(self.local), socket.inet_aton(self.remote))
                        checksum = sum(struct.unpack('!10H', ip))
                        checksum = (checksum & 65535) + (checksum >> 16)
                        checksum = (checksum & 65535) + (checksum >> 16)
                        ip = ip[:10] + struct.pack('!H', (~checksum) & 65535) + ip[12:]
                        try:
                            self.wire.send(self.ciphertext[0][:14] + ip + udp)
                        except OSError as error:
                            if error.errno != errno.ENOBUFS:
                                raise
                            blocked += 1
                    else:
                        sock.sendto(header + frame, (self.remote, PORT))
            finally:
                sock.close()
            return {'attempted': len(frames()), 'local_drops': blocked}
        raise ValueError(op)

    def send(self, number):
        entry = self.slots[number]['sides'][0]
        for _ in range(20):
            entry['socket'].sendto(b'NN28-negative', (entry['peer_ip'], 31000 + number))
        return {'sent': 20}

    def cleanup(self):
        self.pool.shutdown(wait=True)
        if self.capture_process:
            self.capture_process.send_signal(signal.SIGINT)
            self.capture_process.wait(timeout=10)
            self.capture_log.close()
            self.endpoint_capture.send_signal(signal.SIGINT)
            self.endpoint_capture.wait(timeout=10)
            self.endpoint_log.close()
        if self.trace_path:
            (self.trace_path / 'tracing_on').write_text('0')
            (self.trace_path / 'events/net/net_dev_queue/enable').write_text('0')
            (self.trace_path / 'events/skb/kfree_skb/enable').write_text('0')
            self.trace_path.rmdir()
        for sock in [self.packet, self.wire]:
            if sock:
                sock.close()
        errors = []
        n, x, _ = self.io()
        for slot in self.slots:
            entry = slot['sides'][0]
            if 'transport' not in entry['indices']:
                continue
            n.configure(entry['indices']['transport'])
            self.clear_flows(slot)
            try:
                x.policy(self.local, self.remote, SPI + entry['number'] * 2 + self.args.side, PORT, entry['mark'], 1, True)
            except RuntimeError as error:
                if error.args[0][-1] != -2:
                    errors.append(str(error))
            for direction in [self.args.side, 1-self.args.side]:
                a, b = (self.local, self.remote) if direction == self.args.side else (self.remote, self.local)
                try:
                    x.state(a, b, SPI + entry['number'] * 2 + direction, '', entry['mark'], inbound=direction != self.args.side, delete=True)
                except RuntimeError as error:
                    if error.args[0][-1] not in (-2, -3):
                        errors.append(str(error))
        if self.policy:
            command('ip', 'xfrm', 'policy', 'delete', 'src', self.remote, 'dst', self.local, 'proto', 'udp', 'dport', str(PORT), 'dir', 'in')
        try:
            super().cleanup()
        finally:
            for rule in reversed(self.rules):
                run(rule)
            for action in reversed(self.infrastructure):
                run(action)
            for a, b in [(self.local, self.remote), (self.remote, self.local)]:
                run(['conntrack', '-D', '-f', 'ipv4', '--orig-src', a, '--orig-dst', b], (0, 1))
        assert not errors, errors


def emit(value):
    print(json.dumps(value), flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--side', type=int, choices=[0, 1], required=True)
    parser.add_argument('--slots', type=int, default=64)
    parser.add_argument('--workers', type=int, default=32)
    parser.add_argument('--output', required=True)
    parser.add_argument('--trace', action='store_true')
    args = parser.parse_args()
    args.attachment = 'container'
    before = original_inventory()
    fixture = 'nn28-cross-pool'
    created = False
    experiment = None
    try:
        existing = subprocess.run(['docker', 'inspect', fixture], capture_output=True)
        assert existing.returncode != 0, 'fixture already exists'
        command('docker', 'run', '-d', '--network', 'none', '--label', 'nullnet-pool-prototype=20260928-cross', '--name', fixture, 'alpine:latest', 'sleep', 'infinity')
        created = True
        args.pids = [int(command('docker', 'inspect', '-f', '{{.State.Pid}}', fixture))]
        experiment = Cross(args)
        experiment.prepare()
        emit({'ready': True, 'preparation_seconds': experiment.result['preparation_seconds']})
        for line in sys.stdin:
            request = json.loads(line)
            op = request['op']
            if op == 'finish':
                break
            if op == 'wave':
                response = experiment.wave(request)
            elif op == 'traffic':
                list(experiment.pool.map(experiment.traffic, request['ids']))
                response = {'traffic': len(request['ids'])}
            elif op == 'inventory':
                experiment.inventory(request['label'])
                response = {'idle': args.slots}
            elif op == 'quiet':
                response = experiment.quiet(request['ids'])
            elif op == 'fault':
                response = experiment.fault(request['id'], request['mode'])
            elif op == 'send':
                response = experiment.send(request['id'])
            elif op == 'diagnostics':
                response = experiment.diagnostics()
            elif op.startswith('packet-') or op.startswith('wire-') or op in ['replay', 'inject']:
                response = experiment.packets(request)
            else:
                raise ValueError(op)
            emit(response)
    except Exception as error:
        if experiment:
            experiment.result['error'] = repr(error)
            experiment.diagnostics()
        emit({'error': repr(error)})
        for line in sys.stdin:
            if json.loads(line)['op'] == 'finish':
                break
            emit({'diagnostics_saved': True})
        raise
    finally:
        try:
            if experiment:
                experiment.cleanup()
        finally:
            if created:
                command('docker', 'rm', '-f', fixture)
            after = original_inventory()
            preservation = {key: before[key] == after[key] for key in before}
            Path(args.output + '.preservation.json').write_text(json.dumps({'before': before, 'after': after, 'checks': preservation}, indent=2))
            assert all(preservation.values()), preservation
            emit({'cleaned': True, 'preservation': preservation})
