"""On-demand bridge versus dedicated TC cross-connect; isolated lab fixtures only."""
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
import time
import traceback

REPORTS = Path(__file__).resolve().parents[1]
sys.path[:0] = [str(REPORTS / 'vxlan-prepared-cross-host-2026-09-28'),
               str(REPORTS / 'vxlan-prepared-pool-2026-09-28'),
               str(REPORTS / 'vxlan-lifecycle-2026-09-23')]
import importlib.util
spec = importlib.util.spec_from_file_location('cross_node', REPORTS / 'vxlan-prepared-cross-host-2026-09-28/node.py')
cross = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cross)
from prototype import Route, Conntrack, namespace_socket, command, TLS
from netlink import attr, U, info, nested
import native_xfrm as crypto

cross.BASE = 1960000
cross.SPI = 0x4E2A0000
cross.base.GROUP = 0x4E2A0001


def original_inventory():
    result = cross.original_inventory()
    states = command('ip', 'xfrm', 'state')
    stable = '\n'.join(line for line in states.splitlines()
                       if not line.strip().startswith(('lastused ', 'anti-replay context:')))
    result['xfrm_hash'] = hashlib.sha256(stable.encode()).hexdigest()
    return result


def create(n, name, kind, data=b'', extra=b'', group=None):
    index = n.request(16, info() + attr(3, name.encode() + b'\0')
                     + attr(27, U(group or cross.base.GROUP)) + nested(kind, data) + extra,
                     5 | 512 | 1024 | 8)[0]
    return index if index is not None else n.get(name)


def redirect(n, source, destination, mark=None):
    parameters = struct.pack('IIiiiII', 0, 0, 4, 0, 0, 1, destination)
    action = attr(1 | 32768, attr(1, b'mirred\0')
                  + attr(2 | 32768, attr(2, parameters)))
    if mark is None:
        n.request(36, crypto.tcmsg(source, 0xffff0000, 0xfffffff1)
                  + attr(1, b'clsact\0'), 5 | 512 | 1024)
        body = attr(1, b'matchall\0') + attr(2 | 32768, attr(2 | 32768, action))
    else:
        body = attr(1, b'fw\0') + attr(2 | 32768, attr(4 | 32768, action))
    n.request(44, crypto.tcmsg(source, mark or 0, 0xfffffff2, 1) + body,
              5 | 1024 | (256 if mark is not None else 512))


def batch_wave(experiment, ups, downs, pool):
    selected = {s['id'] for s in downs}
    groups = {s['group'] for s in downs}
    assert {s['id'] for s in experiment.slots if s['active'] and s['group'] in groups} == selected
    jobs = []
    import itertools
    for down, up in itertools.zip_longest(downs, ups):
        if down is not None:
            jobs.append(('down', down, pool.submit(experiment.retire_start, down)))
        if up is not None:
            jobs.append(('up', up, pool.submit(experiment.activate, up)))
    pending = [(slot, future.result()) for kind, slot, future in jobs if kind == 'down']
    n = experiment.io()[0]
    begin = time.monotonic()
    for group in groups:
        n.request(17, info() + attr(27, U(group)))
    deleted = time.monotonic() - begin
    finish = [pool.submit(experiment.retire_finish, slot, row, deleted) for slot, row in pending]
    return {'up': [f.result() for kind, _, f in jobs if kind == 'up'],
            'down': [f.result() for f in finish], 'delete_batch_seconds': deleted}


class Minimal(cross.Cross):
    def __init__(self, args):
        super().__init__(args)
        self.local = f'198.18.29.{103 + args.side}'
        self.remote = f'198.18.29.{104 - args.side}'
        self.result['kind'] = 'on-demand-' + args.variant
        self.result['shape'] = args.shape
        self.result['source_sha256'] = hashlib.sha256(Path(__file__).read_bytes()).hexdigest()

    def prepare(self):
        start = time.monotonic()
        self.setup_underlay()
        n, _, _ = self.io()
        side = self.args.side
        for number in range(self.args.slots):
            ident = cross.BASE + number
            role = 's' if side == 0 else 'c'
            names = {'bridge': f'br_{ident}_{role}', 'outer': f'ns_{ident}_{role}-o',
                     'inner': f'ns_{ident}_{role}-in', 'transport': f'nnv_{ident}_{role}',
                     'macsec': f'nnv_{ident}_{role}'}
            assert all(len(name) < 16 for name in names.values())
            for name in set(names.values()):
                assert n.get(name) is None, ('existing device', name)
            group = 0x52000000 | ((number // (self.args.slots // 2)) << 1) | side
            entry = {'names': names, 'indices': {}, 'side': side, 'number': number,
                     'mark': 0x4E800000 | ident, 'group': group, 'host': self.args.shape[side] == 'h'}
            if entry['host']:
                names['inner'] = names['transport']
            self.slots.append({'id': ident, 'generation': 0, 'active': False, 'sides': [entry], 'group': group})
            for key, offset in [('ip', 1 + side * 2 + int(entry['host'])),
                                ('peer_ip', 3 - side * 2 + int(self.args.shape[1-side] == 'h')),
                                ('gateway', 2 + side * 2)]:
                entry[key] = socket.inet_ntoa(struct.pack('!I', 0x0A000000 + ident * 8 + offset))
            entry['socket'] = namespace_socket(self.targets[0], lambda: socket.socket(socket.AF_INET, socket.SOCK_DGRAM))
            entry['socket'].bind(('0.0.0.0', 31000 + number))
            entry['socket'].settimeout(3)
            if self.args.variant == 'bridge':
                entry['indices']['bridge'] = create(n, names['bridge'], 'bridge', extra=attr(4, U(1080)))
                self.owned.append(names['bridge'])
        self.result['preparation_seconds'] = time.monotonic() - start
        self.inventory('prepared')

    def activate(self, slot):
        start = last = time.monotonic()
        stages = {}
        n, x, endpoints = self.io()
        target = endpoints[0]
        e = slot['sides'][0]
        names, ix = e['names'], e['indices']
        assert not slot['active']
        slot['generation'] += 1
        if not e['host']:
            peer = info() + attr(3, names['inner'].encode() + b'\0') + attr(4, U(1080)) + attr(28, U(self.targets[0]))
            ix['outer'] = create(n, names['outer'], 'veth', attr(1 | 32768, peer), attr(4, U(1080)), e['group'])
            ix['inner'] = target.get(names['inner'])
        vxlan = attr(1, U(slot['id'])) + attr(4, socket.inet_aton(self.local)) + attr(2, socket.inet_aton(self.remote))
        vxlan += attr(15, struct.pack('!H', cross.PORT)) + attr(7, b'\0')
        ix['transport'] = ix['macsec'] = create(n, names['transport'], 'vxlan', vxlan, attr(4, U(1080)), e['group'])
        if e['host']:
            ix['inner'] = ix['transport']
        stages['create_links'] = time.monotonic() - last
        last = time.monotonic()
        crypto.tc_install(n, ix['transport'], e['mark'])
        self.associations(x, e, True)
        stages['crypto'] = time.monotonic() - last
        last = time.monotonic()
        target.address(ix['inner'], e['ip'])
        if e['host']:
            n.configure(ix['transport'], True)
        elif self.args.variant == 'redirect':
            redirect(n, ix['outer'], ix['transport'])
            redirect(n, ix['transport'], ix['outer'], e['mark'])
            n.configure(ix['outer'], True)
            n.configure(ix['transport'], True)
        else:
            n.address(ix['bridge'], e['gateway'])
            n.configure(ix['outer'], True, ix['bridge'])
            n.configure(ix['transport'], True, ix['bridge'])
            n.configure(ix['bridge'], True)
        target.configure(ix['inner'], True)
        stages['connect_address_enable'] = time.monotonic() - last
        last = time.monotonic()
        kinds = ['transport'] if e['host'] else ['transport', 'outer']
        for kind in kinds + (['bridge'] if self.args.variant == 'bridge' else []):
            assert n.get(names[kind]) == ix[kind]
        assert target.get(names['inner']) == ix['inner']
        stages['readiness'] = time.monotonic() - last
        slot['active'] = True
        return {'seconds': time.monotonic() - start, 'stages': stages}

    def wave(self, request):
        ups = []
        for number, key in request.get('up', []):
            self.keys[number] = key
            ups.append(self.slots[number])
        return batch_wave(self, ups, [self.slots[i] for i in request.get('down', [])], self.pool)

    def retire_start(self, slot):
        start = time.monotonic()
        n, x, _ = self.io()
        e = slot['sides'][0]
        ix = e['indices']
        n.configure(ix['transport'])
        revoked = time.monotonic()
        self.associations(x, e)
        encrypted = time.monotonic()
        return {'start': start, 'stages': {'revoke': revoked - start, 'crypto_remove': encrypted - revoked}}

    def retire_finish(self, slot, row, deleted):
        n = self.io()[0]
        e = slot['sides'][0]
        ix = e['indices']
        for kind in ['outer', 'inner', 'transport', 'macsec']:
            ix.pop(kind, None)
        finish_start = time.monotonic()
        self.clear_flows(slot)
        if self.args.variant == 'bridge':
            n.configure(ix['bridge'])
            n.clear_neighbors(ix['bridge'])
            n.address(ix['bridge'], e['gateway'], True)
            assert n.get(e['names']['bridge']) == ix['bridge']
        slot['active'] = False
        row['stages'].update(delete_batch=deleted, flows_bridge_reset=time.monotonic() - finish_start)
        return {'seconds': time.monotonic() - row['start'], 'stages': row['stages']}

    def inventory(self, label):
        root = json.loads(command('ip', '-j', '-d', 'addr'))
        target = json.loads(command('nsenter', '-t', str(self.args.pids[0]), '-n', 'ip', '-j', '-d', 'addr'))
        names = {v for s in self.slots for v in s['sides'][0]['names'].values()}
        owned = [link for link in root + target if link['ifname'] in names]
        assert len(owned) == (self.args.slots if self.args.variant == 'bridge' else 0), (label, owned)
        assert all('UP' not in link['flags'] and not link.get('addr_info') for link in owned), label
        states = command('ip', 'xfrm', 'state')
        assert self.local not in states, 'residual SA'
        policies = command('ip', 'xfrm', 'policy')
        assert f'src {self.local}/32 dst {self.remote}/32' not in policies, 'residual outbound policy'
        for prefix in [[], ['nsenter', '-t', str(self.args.pids[0]), '-n']]:
            flows = command(*prefix, 'conntrack', '-L', '-f', 'ipv4', '-p', 'udp')
            assert not any((f"src={s['sides'][0]['ip']} " in row or f"dst={s['sides'][0]['ip']} " in row)
                           and f"sport={31000+s['sides'][0]['number']} dport={31000+s['sides'][0]['number']} " in row
                           for row in flows.splitlines() for s in self.slots)
        self.result.setdefault('inventories', []).append({'label': label, 'retained_bridges': len(owned), 'sas': 0, 'flows': 0})
        self.save()

    def cleanup(self):
        self.pool.shutdown(wait=True)
        n, x, _ = self.io()
        errors = []
        for slot in self.slots:
            e = slot['sides'][0]
            for kind in ['transport', 'outer']:
                index = n.get(e['names'][kind])
                if index is not None:
                    n.request(17, info(index))
            try:
                self.clear_flows(slot)
            except Exception as error:
                errors.append(str(error))
            for direction in [self.args.side, 1-self.args.side]:
                a, b = (self.local, self.remote) if direction == self.args.side else (self.remote, self.local)
                try:
                    x.state(a, b, cross.SPI + e['number'] * 2 + direction, '', e['mark'], inbound=direction != self.args.side, delete=True)
                except RuntimeError as error:
                    if error.args[0][-1] not in (-2, -3):
                        errors.append(str(error))
            try:
                x.policy(self.local, self.remote, cross.SPI + e['number'] * 2 + self.args.side, cross.PORT, e['mark'], 1, True)
            except RuntimeError as error:
                if error.args[0][-1] != -2:
                    errors.append(str(error))
        if self.policy:
            command('ip', 'xfrm', 'policy', 'delete', 'src', self.remote, 'dst', self.local,
                    'proto', 'udp', 'dport', str(cross.PORT), 'dir', 'in')
        for sock in [self.packet, self.wire]:
            if sock:
                sock.close()
        cross.Experiment.cleanup(self)
        for action in reversed(self.rules + self.infrastructure):
            cross.run(action)
        for a, b in [(self.local, self.remote), (self.remote, self.local)]:
            cross.run(['conntrack', '-D', '-f', 'ipv4', '--orig-src', a, '--orig-dst', b], (0, 1))
        for ip in sorted({e[key] for slot in self.slots for e in slot['sides'] for key in ['ip', 'gateway']}):
            for field in ['--orig-src', '--orig-dst']:
                cross.run(['conntrack', '-D', '-f', 'ipv4', field, ip], (0, 1))
        assert not errors, errors


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--side', type=int, choices=[0, 1], required=True)
    parser.add_argument('--slots', type=int, default=64)
    parser.add_argument('--workers', type=int, default=32)
    parser.add_argument('--variant', choices=['bridge', 'redirect'], required=True)
    parser.add_argument('--shape', choices=['cc', 'hc', 'ch', 'hh'], default='cc')
    parser.add_argument('--output', required=True)
    args = parser.parse_args()
    args.trace = False
    args.attachment = 'container'
    assert args.shape == 'cc' or args.variant == 'redirect'
    before = original_inventory()
    fixture = 'nn29-minimal'
    created = False
    experiment = None
    try:
        assert subprocess.run(['docker', 'inspect', fixture], capture_output=True).returncode != 0
        command('docker', 'run', '-d', '--network', 'none', '--label', 'nullnet-prototype=20260929',
                '--name', fixture, 'alpine:latest', 'sleep', 'infinity')
        created = True
        args.pids = [int(command('docker', 'inspect', '-f', '{{.State.Pid}}', fixture))]
        if args.shape[args.side] == 'h':
            args.pids = [os.getpid()]
        experiment = Minimal(args)
        experiment.prepare()
        cross.emit({'ready': True, 'variant': args.variant})
        for line in sys.stdin:
            request = json.loads(line)
            op = request['op']
            if op == 'finish':
                break
            if op == 'wave':
                result = experiment.wave(request)
            elif op == 'traffic':
                list(experiment.pool.map(experiment.traffic, request['ids']))
                result = {'traffic': len(request['ids'])}
            elif op == 'inventory':
                experiment.inventory(request['label'])
                result = {'idle': args.slots}
            elif op == 'quiet':
                result = experiment.quiet(request['ids'])
            elif op == 'flow-presence':
                ips = {s['sides'][0]['ip'] for s in experiment.slots}
                result = {}
                for label, prefix in [('root', []), ('application', ['nsenter', '-t', str(args.pids[0]), '-n'])]:
                    rows = command(*prefix, 'conntrack', '-L', '-f', 'ipv4', '-p', 'udp').splitlines()
                    result[label] = sum(any(f'src={ip} ' in row or f'dst={ip} ' in row for ip in ips) for row in rows)
            elif op == 'fault':
                result = experiment.fault(request['id'], request['mode'])
            elif op == 'diagnostics':
                result = experiment.diagnostics()
            else:
                result = experiment.packets(request)
            cross.emit(result)
    except Exception as error:
        traceback.print_exc(file=sys.stderr)
        if experiment:
            experiment.result['error'] = repr(error)
            experiment.diagnostics()
        cross.emit({'error': repr(error)})
        for line in sys.stdin:
            if json.loads(line)['op'] == 'finish':
                break
            cross.emit({'diagnostics_saved': True})
    finally:
        try:
            if experiment:
                experiment.cleanup()
        finally:
            if created:
                command('docker', 'rm', '-f', fixture)
            after = original_inventory()
            checks = {key: before[key] == after[key] for key in before}
            Path(args.output + '.preservation.json').write_text(json.dumps({'before': before, 'after': after, 'checks': checks}, indent=2))
            cross.emit({'cleaned': True, 'preservation': checks})
            assert all(checks.values()), checks


if __name__ == '__main__':
    main()
