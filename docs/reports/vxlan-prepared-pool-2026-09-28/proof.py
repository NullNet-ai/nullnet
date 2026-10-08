"""Packet-level checks using the same retained-device implementation as timing."""
import argparse
import errno
import hashlib
import json
import select
import socket
import struct
import subprocess
import time
from pathlib import Path

from prototype import Experiment, namespace_socket, command


TAGS = [[], [(0x8100, 1)], [(0x8100, 2)], [(0x8100, 4094)],
        [(0x8100, 0xa001)], [(0x88a8, 2)], [(0x8100, 1), (0x8100, 2)],
        [(0x88a8, 2), (0x8100, 4094)]]


def packet_socket(experiment, entry):
    def create():
        sock = socket.socket(socket.AF_PACKET, socket.SOCK_RAW, socket.htons(3))
        sock.bind((entry['names']['inner'], 0))
        sock.setsockopt(263, 8, 1)
        return sock
    return namespace_socket(experiment.targets[entry['side']], create)


def frames():
    result = []
    for index, tags in enumerate(TAGS):
        result.append(b'\xff' * 6 + bytes.fromhex('0228aabbccdd')
                      + b''.join(struct.pack('!HH', *tag) for tag in tags)
                      + b'\x88\xb5NN28_FRAME_' + bytes([index]) + b'x' * 64)
    ethernet = b'\xff' * 6 + bytes.fromhex('0228aabbccdd')
    payload = b'NN28_FRAME_' + b'x' * 80
    arp = struct.pack('!HHBBH', 1, 0x0800, 6, 4, 1) + ethernet[6:12] + socket.inet_aton('192.0.2.1') + b'\0' * 6 + socket.inet_aton('192.0.2.2')
    result.append(ethernet + b'\x08\x06' + arp + payload)
    udp = struct.pack('!HHHH', 32000, 32000, len(payload) + 8, 0) + payload
    ip = struct.pack('!BBHHHBBH4s4s', 0x45, 0, len(udp) + 20, 0, 0, 64, 17, 0,
                     socket.inet_aton('192.0.2.1'), socket.inet_aton('192.0.2.2'))
    checksum = sum(struct.unpack('!10H', ip))
    checksum = (checksum & 65535) + (checksum >> 16)
    checksum = (checksum & 65535) + (checksum >> 16)
    ip = ip[:10] + struct.pack('!H', (~checksum) & 65535) + ip[12:]
    result.append(ethernet + b'\x08\x00' + ip + udp)
    ip6 = struct.pack('!IHBB16s16s', 0x60000000, len(payload), 59, 64,
                      socket.inet_pton(socket.AF_INET6, '2001:db8::1'), socket.inet_pton(socket.AF_INET6, '2001:db8::2'))
    result.append(ethernet + b'\x86\xdd' + ip6 + payload)
    marker = b'NN28_FRAME_'
    result.append(ethernet + b'\x88\xb5' + marker + b'x' * (1080 - len(marker)))
    return result


def exchange_sockets(experiment, sockets, active, label):
    expected = frames()
    sent = 0
    for frame in expected:
        try:
            sockets[0].send(frame)
            sent += 1
        except OSError as error:
            if active or error.errno != errno.ENETDOWN:
                raise
    found = []
    deadline = time.monotonic() + (2 if active else .2)
    while time.monotonic() < deadline and (not active or len(found) < len(expected)):
        ready, _, _ = select.select([sockets[1]], [], [], .05)
        if not ready:
            continue
        try:
            data, ancillary, flags, address = sockets[1].recvmsg(4096, 1024)
        except OSError as error:
            if not active and error.errno == errno.ENETDOWN:
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
    assert (sorted(found) == sorted(expected)) if active else not found, (label, len(found))
    experiment.result.setdefault('frame_checks', []).append({'label': label, 'sent': sent,
                                                            'received': len(found), 'expected': len(expected) if active else 0})


def exchange(experiment, active, label):
    sockets = [packet_socket(experiment, entry) for entry in experiment.slots[0]['sides']]
    try:
        exchange_sockets(experiment, sockets, active, label)
    finally:
        for sock in sockets:
            sock.close()


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--pids', nargs=2, type=int, required=True)
    parser.add_argument('--output', required=True)
    args = parser.parse_args()
    args.slots, args.workers, args.attachment = 2, 2, 'container'
    experiment = Experiment(args)
    experiment.result['proof_sha256'] = hashlib.sha256(Path(__file__).read_bytes()).hexdigest()
    try:
        experiment.prepare()
        slot = experiment.slots[0]
        exchange(experiment, False, 'prepared-idle')
        for generation in range(3):
            experiment.activate(slot)
            exchange(experiment, True, f'active-{generation}')
            for entry in slot['sides']:
                command('bridge', 'fdb', 'add', '02:28:ff:00:00:99', 'dev', entry['names']['outer'], 'master', 'static')
                command('bridge', 'mdb', 'add', 'dev', entry['names']['bridge'], 'port', entry['names']['outer'], 'grp', '239.28.0.1', 'permanent')
            experiment.retire(slot)
            experiment.inventory(f'reset-{generation}')
            listing = command('bridge', '-j', 'fdb', 'show')
            assert '02:28:ff:00:00:99' not in listing, 'seeded FDB survived reset'
            exchange(experiment, False, f'reset-idle-{generation}')
        experiment.result['completed'] = True
        print('FRAME_DELIVERY_AND_IDLE_ISOLATION_PASSED', flush=True)
    except Exception as error:
        experiment.result['error'] = repr(error)
        raise
    finally:
        try:
            for prefix in [[]] + [['nsenter', '-t', str(pid), '-n'] for pid in args.pids]:
                for selectors in [('ipv4', '192.0.2.1', '192.0.2.2'), ('ipv6', '2001:db8::1', '2001:db8::2')]:
                    family, source, destination = selectors
                    result = subprocess.run(prefix + ['conntrack', '-D', '-f', family, '--orig-src', source, '--orig-dst', destination], capture_output=True, text=True)
                    assert result.returncode == 0 or (result.returncode == 1 and '0 flow entries have been deleted' in result.stderr), result.stderr
        finally:
            experiment.cleanup()
