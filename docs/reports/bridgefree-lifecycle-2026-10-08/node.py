"""Matched dedicated-device lifecycles, with host gateways and product reset."""
import argparse
import collections
import concurrent.futures as futures
import contextlib
import ctypes
import hashlib
import importlib.util
import itertools
import json
import os
from pathlib import Path
import resource
import select
import socket
import struct
import subprocess
import sys
import threading
import time
import traceback

REPORTS = Path(__file__).resolve().parents[1]
sys.path[:0] = [str(REPORTS / p) for p in [
    'kernel-parallelism-2026-10-01', 'vxlan-prepared-cross-host-2026-09-28',
    'vxlan-prepared-pool-2026-09-28', 'vxlan-lifecycle-2026-09-23',
    'device-event-bypass-2026-10-05']]
spec = importlib.util.spec_from_file_location('legacy_node', REPORTS / 'kernel-parallelism-2026-10-01/node.py')
legacy = importlib.util.module_from_spec(spec)
spec.loader.exec_module(legacy)
cross, crypto = legacy.cross, legacy.crypto
from netlink import NL, U, attr, info, nested
from prototype import namespace_socket, command, TLS
import trace_lock

BASE = 1970000
cross.BASE, cross.SPI, cross.PORT = BASE, 0x4E380000, 4790
cross.base.GROUP = 0x4E380001
LOCAL = threading.local()
CPU_ON = False
CPU_ROWS = []
MARKER = None
LIBC = ctypes.CDLL(None)


def attrs(data):
    offset = 0
    while offset + 4 <= len(data):
        size, kind = struct.unpack_from('HH', data, offset)
        assert size >= 4 and offset + size <= len(data)
        yield kind & 16383, data[offset+4:offset+size]
        offset += (size + 3) & ~3


def measured(label, function, *args, **kwargs):
    if not CPU_ON and MARKER is None:
        return function(*args, **kwargs)
    label = getattr(LOCAL, 'phase', 'outside') + '/' + label
    before = time.thread_time_ns() if CPU_ON else 0
    if MARKER is not None:
        LOCAL.serial = getattr(LOCAL, 'serial', 0) + 1
        ident = f'{threading.get_native_id()}-{LOCAL.serial}'
        os.write(MARKER, f'NNREQ BEGIN {ident} {label}\n'.encode())
    try:
        return function(*args, **kwargs)
    finally:
        if MARKER is not None:
            os.write(MARKER, f'NNREQ END {ident} {label}\n'.encode())
        if CPU_ON:
            CPU_ROWS.append((label, time.thread_time_ns() - before))


def raw(sock, kind, body, flags=5):
    sock.seq += 1
    sock.s.sendto(struct.pack('IHHII', 16+len(body), kind, flags, sock.seq, 0)+body, (0, 0))
    rows = []
    while True:
        data = sock.s.recv(65536)
        offset = 0
        while offset + 16 <= len(data):
            size, typ, fl, seq, _ = struct.unpack_from('IHHII', data, offset)
            if seq == sock.seq:
                assert not fl & 16, 'interrupted dump'
                payload = data[offset+16:offset+size]
                if typ == 2:
                    error = struct.unpack_from('i', payload)[0]
                    if error:
                        raise RuntimeError(('netlink', kind, error))
                    return rows
                if typ == 3:
                    return rows
                rows.append(payload)
            offset += (size+3) & ~3


class Route(legacy.Route):
    def __init__(self):
        super().__init__()
        self.s.setsockopt(270, 12, U(1))

    def call(self, label, kind, body, flags=5):
        return measured(label, self.request, kind, body, flags)

    def dump(self, label, kind, body):
        return measured(label, raw, self, kind, body, 1|768)

    def set(self, label, index, up=False, master=None):
        return self.call(label, 19, info(index, int(up), 1) + (attr(10, U(master)) if master is not None else b''))

    def address(self, label, index, ip, delete=False):
        return self.call(label, 21 if delete else 20,
            struct.pack('BBBBI', 2, 29, 0, 0, index)+attr(1, socket.inet_aton(ip))+attr(2, socket.inet_aton(ip)),
            5 if delete else 5|512|1024)

    def link(self, label, name, index=None, idle=False):
        rows = measured(label, raw, self, 18, info()+attr(3, name.encode()+b'\0'))
        assert len(rows) == 1 and len(rows[0]) >= 16
        found, flags = struct.unpack_from('II', rows[0], 4)
        if index is not None:
            assert found == index, (name, found, index)
        if idle:
            assert not flags & 1
            assert not dict(attrs(rows[0][16:])).get(10, b'\0\0\0\0').strip(b'\0')
        return found

    def clear_addresses(self, label, index):
        rows = self.dump(label+'_dump', 22, bytes(4)+U(index))
        for row in rows:
            if struct.unpack_from('I', row, 4)[0] == index:
                self.call(label+'_delete', 21, row)

    def clear_neighbors(self, label, index):
        rows = self.dump(label+'_dump', 30, bytes(12)+attr(8, U(index)))
        for row in rows:
            if row[0] in (2, 10) and struct.unpack_from('I', row, 4)[0] == index:
                try:
                    self.call(label+'_delete', 29, row)
                except RuntimeError as error:
                    if error.args != (('netlink', 29, -2),):
                        raise

    def route(self, label, destination, index, source, delete=False):
        body = struct.pack('BBBBBBBBI', 2, 32, 0, 0, 254, 4, 253, 1, 0)
        body += attr(1, socket.inet_aton(destination))+attr(4, U(index))+attr(7, socket.inet_aton(source))
        return self.call(label, 25 if delete else 24, body, 5 if delete else 5|512|1024)


class Conntrack(legacy.Conntrack):
    def clear(self, label, ips):
        rows = measured(label+'_dump', raw, self, 257, bytes([2, 0, 0, 0]), 1|768)
        matched = 0
        encoded = {socket.inet_aton(ip) for ip in ips}
        for row in rows:
            if row[0] != 2:
                continue
            fields = dict(attrs(row[4:]))
            matches = False
            for kind in (1, 2):
                addresses = dict(attrs(fields.get(kind, b''))).get(1, b'')
                matches |= any(k in (1, 2) and v in encoded for k, v in attrs(addresses))
            if matches:
                body = row[:4]+attr(2|32768, fields[2])+attr(12, fields[12])
                if 18 in fields:
                    body += attr(18, fields[18])
                try:
                    measured(label+'_delete', self.request, 258, body)
                except RuntimeError as error:
                    if error.args != (('netlink', 258, -2),):
                        raise
                matched += 1
        return {'scanned': len(rows), 'deleted': matched}


def tcmsg(index, parent, priority=0, proto=3, handle=0):
    return struct.pack('B3xiIII', 0, index, handle, parent, (priority<<16)|socket.htons(proto))


def mirred(destination):
    parms = struct.pack('IIiiiII', 0, 0, 4, 0, 0, 1, destination)
    return attr(1|32768, attr(1, b'mirred\0')+attr(2|32768, attr(2, parms)))


def u32(n, label, index, ip, proto, priority, mark=None):
    offset = 16 if proto == 0x800 else 24
    selector = struct.pack('BBB x HH hh I', 1, 0, 1, 0, 0, 0, 0, 0)
    selector += struct.pack('!II', 0xffffffff, int.from_bytes(socket.inet_aton(ip), 'big'))+struct.pack('ii', offset, 0)
    options = attr(5, selector)+attr(7|32768, crypto.action('gact', 0))
    if mark is not None:
        options += attr(10, struct.pack('III', mark, 0xffffffff, 0))
    n.call(label, 44, tcmsg(index, 0xfffffff2, priority, proto)+attr(1, b'u32\0')+attr(2|32768, options), 5|512|1024)


def redirect_filters(n, e):
    ix, mark = e['indices'], e['mark']
    n.call('outer_qdisc', 36, tcmsg(ix['outer'], 0xfffffff1, handle=0xffff0000)+attr(1, b'clsact\0'), 5|512|1024)
    for source, destination, authenticated in [(ix['outer'], ix['transport'], False), (ix['transport'], ix['outer'], True)]:
        role = 'transport' if authenticated else 'outer'
        for priority, proto in [(1, 0x800), (2, 0x806)]:
            u32(n, role+'_gateway_pass', source, e['gateway'], proto, priority, mark if authenticated else None)
        if authenticated:
            options = attr(4|32768, mirred(destination))
            n.call('authenticated_redirect', 44, tcmsg(source, 0xfffffff2, 3, handle=mark)+attr(1, b'fw\0')+attr(2|32768, options), 5|512|1024)
            n.call('transport_default_drop', 44, tcmsg(source, 0xfffffff2, 4)+attr(1, b'matchall\0')+attr(2|32768, attr(2|32768, crypto.action('gact', 2))), 5|512|1024)
        else:
            n.call('outer_redirect', 44, tcmsg(source, 0xfffffff2, 3)+attr(1, b'matchall\0')+attr(2|32768, attr(2|32768, mirred(destination))), 5|512|1024)


class Experiment(cross.Cross):
    def __init__(self, args):
        super().__init__(args)
        self.local = f'198.18.38.{103+args.side}'
        self.remote = f'198.18.38.{104-args.side}'
        self.result['kind'] = args.lifecycle+'-'+args.variant+'-full-reset-gateway'
        self.result['source_sha256'] = hashlib.sha256(Path(__file__).read_bytes()).hexdigest()
        self.flow_rows = []
        self.pidfd = os.pidfd_open(args.pids[0])

    def validate_namespace(self):
        process_poll = select.poll()
        process_poll.register(self.pidfd, select.POLLIN)
        assert not process_poll.poll(0), 'container generation exited'
        a, b = os.fstat(self.targets[0]), os.stat(f'/proc/{self.args.pids[0]}/ns/net')
        assert (a.st_dev, a.st_ino) == (b.st_dev, b.st_ino)

    def endpoint_io(self, slot):
        if self.args.reset == 'historical':
            n, _, target = self.io()
            return n, target[0], TLS.conntrack[0], TLS.conntrack[1]
        e = slot['sides'][0]
        if 'io' not in e:
            def open_sockets():
                return (Route(), namespace_socket(self.targets[0], Route), Conntrack(), namespace_socket(self.targets[0], Conntrack))
            e['io'] = measured('open_endpoint_sockets', open_sockets)
        return e['io']

    def io(self):
        if not hasattr(TLS, 'io'):
            LIBC.prctl(15, b'nn-est-main' if threading.current_thread() is threading.main_thread() else b'nn-est-worker', 0, 0, 0)
            TLS.io = (Route(), crypto.Xfrm(), [namespace_socket(self.targets[0], Route)])
            TLS.conntrack = [Conntrack(), namespace_socket(self.targets[0], Conntrack)]
        return TLS.io

    def prepare(self):
        begin = time.monotonic()
        self.setup_underlay()
        n = self.io()[0]
        side = self.args.side
        for number in range(self.args.slots):
            ident = BASE+number
            role = 's' if side == 0 else 'c'
            names = {'bridge': f'br_{ident}_{role}', 'outer': f'ns_{ident}_{role}-o', 'inner': f'ns_{ident}_{role}-in',
                     'transport': f'nnv_{ident}_{role}', 'macsec': f'nnv_{ident}_{role}'}
            assert all(n.get(name) is None for name in set(names.values()))
            group = 0x53000000 | ((number//(self.args.slots//2))<<1) | side
            e = {'names': names, 'indices': {}, 'side': side, 'number': number, 'mark': 0x4E800000|ident, 'group': group}
            for key, offset in [('ip', 1+side*2), ('peer_ip', 3-side*2), ('gateway', 2+side*2), ('peer_gateway', 4-side*2)]:
                e[key] = socket.inet_ntoa(struct.pack('!I', 0x0A000000+ident*8+offset))
            e['socket'] = namespace_socket(self.targets[0], lambda: socket.socket(socket.AF_INET, socket.SOCK_DGRAM))
            e['socket'].bind(('0.0.0.0', 31000+number))
            e['socket'].settimeout(3)
            slot = {'id': ident, 'generation': 0, 'active': False, 'sides': [e], 'group': group}
            self.slots.append(slot)
            if self.args.variant == 'bridge':
                e['indices']['bridge'] = self.create_link(n, 'prepare_bridge', names['bridge'], 'bridge', extra=attr(4, U(1080)))
                self.owned.append(names['bridge'])
            if self.args.lifecycle == 'pooled':
                self.devices(slot)
        self.result['preparation_seconds'] = time.monotonic()-begin
        self.inventory('prepared')

    def create_link(self, n, label, name, kind, data=b'', extra=b'', group=None):
        body = info()+attr(3, name.encode()+b'\0')+attr(27, U(group or cross.base.GROUP))+nested(kind, data)+extra
        index = n.call(label, 16, body, 5|512|1024|8)[0]
        return index if index is not None else n.link(label+'_lookup', name)

    def devices(self, slot):
        n, _, targets = self.io()
        e = slot['sides'][0]
        ix, names = e['indices'], e['names']
        peer = info()+attr(3, names['inner'].encode()+b'\0')+attr(4, U(1080))+attr(28, U(self.targets[0]))
        ix['outer'] = self.create_link(n, 'create_veth', names['outer'], 'veth', attr(1|32768, peer), attr(4, U(1080)), e['group'])
        self.owned.append(names['outer'])
        ix['inner'] = targets[0].link('lookup_container_peer', names['inner'])
        vxlan = attr(1, U(slot['id']))+attr(4, socket.inet_aton(self.local))+attr(2, socket.inet_aton(self.remote))
        vxlan += attr(15, struct.pack('!H', cross.PORT))+attr(7, b'\0')
        ix['transport'] = ix['macsec'] = self.create_link(n, 'create_vxlan', names['transport'], 'vxlan', vxlan, attr(4, U(1080)), e['group'])
        self.owned.append(names['transport'])
        transport = ix['transport']
        n.call('transport_qdisc', 36, tcmsg(transport, 0xfffffff1, handle=0xffff0000)+attr(1, b'clsact\0'), 5|512|1024)
        n.call('transport_egress_mark', 44, tcmsg(transport, 0xfffffff3, 1)+attr(1, b'matchall\0')+attr(2|32768,
            attr(2|32768, crypto.action('skbedit', 3, attr(5, U(e['mark']))+attr(8, U(0xffffffff))))), 5|512|1024)
        if self.args.variant == 'redirect':
            redirect_filters(n, e)
        else:
            n.call('transport_authenticated_accept', 44, tcmsg(transport, 0xfffffff2, 1, handle=e['mark'])+attr(1, b'fw\0')+
                attr(2|32768, attr(4|32768, crypto.action('gact', 0))), 5|512|1024)
            n.call('transport_default_drop', 44, tcmsg(transport, 0xfffffff2, 2)+attr(1, b'matchall\0')+
                attr(2|32768, attr(2|32768, crypto.action('gact', 2))), 5|512|1024)
        self.endpoint_io(slot)

    def associations(self, x, e, key=None):
        number, mark, side = e['number'], e['mark'], self.args.side
        if key is None:
            measured('remove_ipsec_policy', x.policy, self.local, self.remote, cross.SPI+number*2+side, cross.PORT, mark, 1, True)
            for direction in [side, 1-side]:
                a, b = (self.local, self.remote) if direction == side else (self.remote, self.local)
                measured('remove_ipsec_state', x.state, a, b, cross.SPI+number*2+direction, '', mark, inbound=direction!=side, delete=True)
        else:
            for direction in [side, 1-side]:
                a, b = (self.local, self.remote) if direction == side else (self.remote, self.local)
                derived = hashlib.sha256((self.keys[number]+str(direction)).encode()).hexdigest()
                measured('install_ipsec_state', x.state, a, b, cross.SPI+number*2+direction, derived, mark, inbound=direction!=side)
            measured('install_ipsec_policy', x.policy, self.local, self.remote, cross.SPI+number*2+side, cross.PORT, mark, 1)

    def activate(self, slot):
        LOCAL.phase = 'setup'
        begin = time.monotonic()
        if self.args.reset == 'full':
            measured('validate_namespace', self.validate_namespace)
        x = self.io()[1]
        e, ix = slot['sides'][0], slot['sides'][0]['indices']
        assert not slot['active']
        slot['generation'] += 1
        if self.args.lifecycle == 'unpooled':
            self.devices(slot)
        n, target, _, _ = self.endpoint_io(slot)
        self.associations(x, e, True)
        target.address('add_container_address', ix['inner'], e['ip'])
        target.set('enable_container', ix['inner'], True)
        gateway = ix['bridge'] if self.args.variant == 'bridge' else ix['outer']
        n.address('add_gateway_address', gateway, e['gateway'])
        if self.args.variant == 'bridge':
            n.set('attach_enable_outer', ix['outer'], True, ix['bridge'])
            n.set('attach_enable_transport', ix['transport'], True, ix['bridge'])
            n.set('enable_bridge', ix['bridge'], True)
        else:
            n.set('enable_outer', ix['outer'], True)
            n.set('enable_transport', ix['transport'], True)
            for key in ['peer_ip', 'peer_gateway']:
                n.route('add_'+key+'_route', e[key], ix['transport'], e['gateway'])
        n.set('enable_transport_again', ix['transport'], True)
        for role in ['transport', 'outer']+(['bridge'] if self.args.variant == 'bridge' else []):
            n.link('readiness_'+role, e['names'][role], ix[role])
        target.link('readiness_container', e['names']['inner'], ix['inner'])
        slot['active'] = True
        return {'seconds': time.monotonic()-begin}

    def clear_flows(self, slot):
        self.io()
        e = slot['sides'][0]
        results = []
        _, _, root_ct, target_ct = self.endpoint_io(slot)
        if self.args.reset == 'historical':
            for ct in [root_ct, target_ct]:
                for a, b in [(e['ip'], e['peer_ip']), (e['peer_ip'], e['ip'])]:
                    measured('exact_conntrack_delete', ct.delete, a, b, 31000+e['number'])
            return
        for ct, label, ips in [(root_ct, 'root_conntrack', [e['ip'], e['gateway']]),
                              (target_ct, 'container_conntrack', [e['ip']])]:
            results.append(ct.clear(label, ips))
        self.flow_rows.extend(results)

    def revoke(self, slot):
        LOCAL.phase = 'teardown'
        start = time.monotonic()
        n, target, _, _ = self.endpoint_io(slot)
        x = self.io()[1]
        e, ix = slot['sides'][0], slot['sides'][0]['indices']
        if self.args.lifecycle == 'pooled' and self.args.variant == 'redirect':
            for key in ['peer_ip', 'peer_gateway']:
                n.route('remove_'+key+'_route', e[key], ix['transport'], e['gateway'], True)
        n.set('disable_transport', ix['transport'])
        if self.args.lifecycle == 'pooled':
            if self.args.variant == 'bridge':
                n.set('disable_bridge', ix['bridge'])
                n.set('detach_disable_transport', ix['transport'], master=0)
                n.set('detach_disable_outer', ix['outer'], master=0)
            else:
                n.set('disable_outer', ix['outer'])
            target.set('disable_container', ix['inner'])
        self.associations(x, e)
        return start

    def reset(self, slot, start):
        LOCAL.phase = 'teardown'
        n, target, _, _ = self.endpoint_io(slot)
        e, ix = slot['sides'][0], slot['sides'][0]['indices']
        if self.args.lifecycle == 'pooled':
            for role in (['bridge'] if self.args.variant == 'bridge' else ['outer', 'transport']):
                n.clear_neighbors('root_'+role+'_neighbors', ix[role])
                if self.args.reset == 'full':
                    n.clear_addresses('root_'+role+'_addresses', ix[role])
                elif role != 'transport':
                    n.address('remove_gateway_address', ix[role], e['gateway'], True)
            target.clear_neighbors('container_neighbors', ix['inner'])
            if self.args.reset == 'full':
                target.clear_addresses('container_addresses', ix['inner'])
                for role in ['transport', 'outer']+(['bridge'] if self.args.variant == 'bridge' else []):
                    n.link('idle_'+role, e['names'][role], ix[role], True)
                target.link('idle_container', e['names']['inner'], ix['inner'], True)
            else:
                target.address('remove_container_address', ix['inner'], e['ip'], True)
        else:
            for role in ['transport', 'outer', 'inner', 'macsec']:
                ix.pop(role, None)
            if self.args.variant == 'bridge':
                n.set('disable_bridge', ix['bridge'])
                n.clear_neighbors('root_bridge_neighbors', ix['bridge'])
                if self.args.reset == 'full':
                    n.clear_addresses('root_bridge_addresses', ix['bridge'])
                else:
                    n.address('remove_gateway_address', ix['bridge'], e['gateway'], True)
                n.link('idle_bridge', e['names']['bridge'], ix['bridge'], True)
            elif self.args.reset == 'full':
                for role in ['transport', 'outer']:
                    assert measured('verify_deleted_'+role, n.get, e['names'][role]) is None
        slot['active'] = False
        self.clear_flows(slot)
        if self.args.lifecycle == 'unpooled' and self.args.reset == 'full':
            for sock in slot['sides'][0].pop('io'):
                measured('close_endpoint_socket', sock.s.close)
        return {'seconds': time.monotonic()-start}

    def wave(self, request):
        jobs = []
        downs = [self.slots[i] for i in request.get('down', [])]
        ups = []
        for number, key in request.get('up', []):
            self.keys[number] = key
            ups.append(self.slots[number])
        for down, up in itertools.zip_longest(downs, ups):
            if down is not None:
                jobs.append(('down', down, self.pool.submit(self.revoke, down)))
            if up is not None:
                jobs.append(('up', up, self.pool.submit(self.activate, up)))
        pending = [(slot, future.result()) for kind, slot, future in jobs if kind == 'down']
        if downs and self.args.lifecycle == 'unpooled':
            LOCAL.phase = 'teardown'
            groups = {s['group'] for s in downs}
            assert {s['id'] for s in self.slots if s['active'] and s['group'] in groups} == {s['id'] for s in downs}
            for group in groups:
                self.io()[0].call('batch_delete_veth_vxlan', 17, info()+attr(27, U(group)))
        resets = [self.pool.submit(self.reset, slot, start) for slot, start in pending]
        return {'up': [f.result() for kind, _, f in jobs if kind == 'up'], 'down': [f.result() for f in resets]}

    def inventory(self, label):
        prefixes = [[], ['nsenter', '-t', str(self.args.pids[0]), '-n']]
        names = {v for slot in self.slots for v in slot['sides'][0]['names'].values()}
        links = [row for prefix in prefixes for row in json.loads(command(*prefix, 'ip', '-j', '-d', 'addr')) if row['ifname'] in names]
        expected = self.args.slots*((4 if self.args.variant == 'bridge' else 3) if self.args.lifecycle == 'pooled' else int(self.args.variant == 'bridge'))
        assert len(links) == expected, (label, len(links), expected)
        assert all('UP' not in row['flags'] and not row.get('master') and not row.get('addr_info') for row in links), label
        indices = {row['ifname']: row['ifindex'] for row in links}
        if hasattr(self, 'stable_indices'):
            assert indices == self.stable_indices
        self.stable_indices = indices
        for prefix in prefixes:
            neighbors = [row for row in json.loads(command(*prefix, 'ip', '-j', 'neigh')) if row.get('dev') in names]
            assert not neighbors, (label, neighbors)
            ips = {e[key] for s in self.slots for e in s['sides'] for key in ['ip', 'gateway']}
            listing = command(*prefix, 'conntrack', '-L', '-f', 'ipv4', *(['-p', 'udp'] if self.args.reset == 'historical' else []))
            matching = [row for row in listing.splitlines() if any(f'src={ip} ' in row or f'dst={ip} ' in row for ip in ips)]
            if self.args.reset == 'historical':
                matching = [row for row in matching if any(
                    (f"src={s['sides'][0]['ip']} " in row or f"dst={s['sides'][0]['ip']} " in row)
                    and f"sport={31000+s['sides'][0]['number']} dport={31000+s['sides'][0]['number']} " in row
                    for s in self.slots)]
            assert not matching, (label, prefix, matching)
        assert self.local not in command('ip', 'xfrm', 'state'), label
        assert f'src {self.local}/32 dst {self.remote}/32' not in command('ip', 'xfrm', 'policy'), label
        self.result.setdefault('inventories', []).append({'label': label, 'links': len(links), 'flows': 0, 'neighbors': 0, 'addresses': 0, 'sas': 0})
        self.save()

    def gateway_traffic(self, numbers):
        for number in numbers:
            e = self.slots[number]['sides'][0]
            root = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
            root.bind((e['gateway'], 41000+number))
            root.settimeout(3)
            try:
                payload = b'gateway'+os.urandom(16)
                e['socket'].sendto(payload, (e['gateway'], 41000+number))
                received, peer = root.recvfrom(4096)
                assert received == payload and peer[0] == e['ip']
                root.sendto(payload, peer)
                assert e['socket'].recvfrom(4096)[0] == payload
            finally:
                root.close()
        return {'gateway_roundtrips': len(numbers)}

    def cleanup(self):
        self.pool.shutdown(wait=True)
        n, x, _ = self.io()
        for slot in self.slots:
            e = slot['sides'][0]
            if slot['active']:
                with contextlib.suppress(Exception):
                    self.associations(x, e)
            for role in ['transport', 'outer', 'bridge']:
                index = n.get(e['names'][role])
                if index is not None:
                    n.request(17, info(index))
            self.clear_flows(slot)
            if self.args.reset == 'full':
                for sock in e.pop('io'):
                    sock.s.close()
            e['socket'].close()
        if self.policy:
            command('ip', 'xfrm', 'policy', 'delete', 'src', self.remote, 'dst', self.local, 'proto', 'udp', 'dport', str(cross.PORT), 'dir', 'in')
        if self.args.reset == 'historical':
            ips = [e[key] for s in self.slots for e in s['sides'] for key in ['ip', 'gateway']]
            for ct in TLS.conntrack:
                ct.clear('final_owned_conntrack', ips)
        for action in reversed(self.rules+self.infrastructure):
            cross.run(action)
        for a, b in [(self.local, self.remote), (self.remote, self.local)]:
            cross.run(['conntrack', '-D', '-f', 'ipv4', '--orig-src', a, '--orig-dst', b], (0, 1))
        self.result['flow_cleanup'] = {'dumps': len(self.flow_rows), 'scanned': sum(r['scanned'] for r in self.flow_rows), 'deleted': sum(r['deleted'] for r in self.flow_rows)}
        self.save()
        os.close(self.pidfd)


def cpu():
    return [int(v) for v in Path('/proc/stat').read_text().splitlines()[0].split()[1:]]


def trace_start(label, out):
    global MARKER
    trace_lock.OUT = out
    trace_lock.LOCKS = []
    trace_lock.FUNCTIONS = ['rtnl_newlink', 'rtnl_setlink', 'rtnl_dellink', 'inet_rtm_newaddr', 'inet_rtm_deladdr', 'synchronize_rcu', 'synchronize_rcu_expedited']
    trace_lock.trace_start(label, os.getpid())
    trace_lock.write(trace_lock.INSTANCE/'tracing_on', 0)
    trace_lock.write(trace_lock.INSTANCE/'events/sched/sched_switch/enable', 0)
    trace_lock.write(trace_lock.INSTANCE/'events/sched/sched_switch/trigger', '!stacktrace')
    trace_lock.write(trace_lock.INSTANCE/'buffer_size_kb', 65536)
    MARKER = os.open(trace_lock.INSTANCE/'trace_marker', os.O_WRONLY)
    trace_lock.write(trace_lock.INSTANCE/'trace', '')
    trace_lock.write(trace_lock.INSTANCE/'tracing_on', 1)


def trace_stop(label):
    global MARKER
    trace_lock.write(trace_lock.INSTANCE/'tracing_on', 0)
    os.close(MARKER)
    MARKER = None
    trace_lock.write(trace_lock.INSTANCE/'events/sched/sched_switch/trigger', 'stacktrace if prev_comm == "nn-est-worker" || prev_comm == "nn-est-main"')
    trace_lock.trace_stop(label, os.getpid())


def benchmark(exp, request):
    global CPU_ON
    count = request['concurrency']
    a, b = list(range(count)), list(range(exp.args.slots//2, exp.args.slots//2+count))
    def wave(up=(), down=()):
        return exp.wave({'up': [[i, os.urandom(32).hex()] for i in up], 'down': list(down)})
    wave(a)
    command('udevadm', 'settle', '--timeout=60')
    out, label = Path(exp.args.output).parent, request['label']
    if request.get('trace'):
        trace_start(label, out)
    CPU_ROWS.clear()
    CPU_ON = request.get('accounting', False)
    before, process_before, started = cpu(), time.process_time_ns(), time.monotonic()
    waves, latencies = 0, []
    try:
        while time.monotonic()-started < request['seconds']:
            begin = time.monotonic()
            wave(b, a)
            latencies.append(time.monotonic()-begin)
            a, b = b, a
            waves += 1
        seconds = time.monotonic()-started
        after, process_after = cpu(), time.process_time_ns()
    finally:
        CPU_ON = False
        if request.get('trace'):
            trace_stop(label)
    started_drain = time.monotonic()
    wave(down=a)
    drain = time.monotonic()-started_drain
    exp.inventory(label)
    cycles = count*waves
    grouped = collections.defaultdict(lambda: {'requests': 0, 'cpu_ns': 0})
    for name, ns in CPU_ROWS:
        grouped[name]['requests'] += 1
        grouped[name]['cpu_ns'] += ns
    result = {'label': label, 'cycles': cycles, 'seconds': seconds, 'cycles_per_second': cycles/seconds,
              'concurrency': count, 'workers': exp.args.workers, 'cpus': os.cpu_count(), 'kernel': os.uname().release,
              'cpu_delta': [v-u for u, v in zip(before, after)], 'process_cpu_ns': process_after-process_before,
              'final_drain_seconds': drain, 'wave_seconds': latencies,
              'request_cpu': {k: {**v, 'cpu_ms_per_cycle': v['cpu_ns']/1e6/cycles} for k, v in sorted(grouped.items())},
              'accounting': request.get('accounting', False), 'trace': request.get('trace', False)}
    (out/(label+'.workload.json')).write_text(json.dumps(result, indent=2)+'\n')
    return {k: v for k, v in result.items() if k not in ['wave_seconds', 'request_cpu']}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--side', type=int, required=True)
    parser.add_argument('--variant', choices=['bridge', 'redirect'], required=True)
    parser.add_argument('--lifecycle', choices=['unpooled', 'pooled'], required=True)
    parser.add_argument('--slots', type=int, default=128)
    parser.add_argument('--workers', type=int, default=128)
    parser.add_argument('--output', required=True)
    parser.add_argument('--reset', choices=['historical', 'full'], default='historical')
    args = parser.parse_args()
    args.trace, args.attachment = False, 'container'
    resource.setrlimit(resource.RLIMIT_NOFILE, (65536, 65536))
    Path(args.output).parent.mkdir(parents=True, exist_ok=True)
    before = legacy.original_inventory()
    fixture = 'nn38-bridgefree'
    exp = None
    created = False
    try:
        assert subprocess.run(['docker', 'inspect', fixture], capture_output=True).returncode != 0
        command('docker', 'run', '-d', '--network', 'none', '--label', 'nullnet-prototype=20261008', '--name', fixture, 'alpine:latest', 'sleep', 'infinity')
        created = True
        args.pids = [int(command('docker', 'inspect', '-f', '{{.State.Pid}}', fixture))]
        exp = Experiment(args)
        exp.prepare()
        cross.emit({'ready': True, 'pid': os.getpid(), 'preparation_seconds': exp.result['preparation_seconds']})
        for line in sys.stdin:
            request = json.loads(line)
            op = request['op']
            if op == 'finish':
                break
            if op == 'benchmark':
                result = benchmark(exp, request)
            elif op == 'wave':
                result = exp.wave(request)
            elif op == 'traffic':
                list(exp.pool.map(exp.traffic, request['ids']))
                result = {'bidirectional_deliveries': len(request['ids'])*2}
            elif op == 'gateway':
                result = exp.gateway_traffic(request['ids'])
            elif op == 'inventory':
                exp.inventory(request['label'])
                result = {'idle': args.slots}
            elif op == 'quiet':
                result = exp.quiet(request['ids'])
            elif op == 'fault':
                result = exp.fault(request['id'], request['mode'])
            elif op == 'diagnostics':
                result = exp.diagnostics()
            else:
                result = exp.packets(request)
            cross.emit(result)
    except Exception as error:
        traceback.print_exc(file=sys.stderr)
        if exp:
            exp.result['error'] = repr(error)
            exp.save()
        cross.emit({'error': repr(error)})
    finally:
        try:
            if exp:
                exp.cleanup()
        finally:
            if created:
                command('docker', 'rm', '-f', fixture)
            after = legacy.original_inventory()
            checks = {k: before[k] == after[k] for k in before}
            Path(args.output+'.preservation.json').write_text(json.dumps({'before': before, 'after': after, 'checks': checks}, indent=2)+'\n')
            cross.emit({'cleaned': True, 'preservation': checks})
            assert all(checks.values()), checks


if __name__ == '__main__':
    main()
