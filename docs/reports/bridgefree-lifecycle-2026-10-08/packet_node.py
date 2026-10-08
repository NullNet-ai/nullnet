"""Additional encrypted packet and host-gateway proofs, outside CPU timing."""
import os
import select
import socket
import struct
import time
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))
import node


class PacketExperiment(node.Experiment):
    def packets(self, request):
        op = request['op']
        if op == 'keys':
            return {'keys': [os.urandom(32).hex() for _ in request['ids']]}
        if op == 'host-open':
            for number in request.get('ids', [0]):
                e = self.slots[number]['sides'][0]
                root = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
                root.bind((e['gateway'], 41000+number))
                root.settimeout(3)
                e['root_socket'] = root
            return {'host_sockets': len(request.get('ids', [0]))}
        if op == 'host-close':
            for slot in self.slots:
                sock = slot['sides'][0].pop('root_socket', None)
                if sock:
                    sock.close()
            return {'closed': True}
        if op == 'host-traffic':
            number = request.get('id', 0)
            e = self.slots[number]['sides'][0]
            root = e['root_socket']
            payload = b'root-gateway'+struct.pack('I', self.args.side)+os.urandom(16)
            root.sendto(payload, (e['peer_gateway'], 41000+number))
            data, peer = root.recvfrom(4096)
            assert data.startswith(b'root-gateway') and peer[0] == e['peer_gateway']
            return {'host_to_host_deliveries': 1}
        if op == 'send':
            return self.send(request['id'])
        if op == 'owned-flows-clear':
            ips = [e[key] for s in self.slots for e in s['sides'] for key in ['ip', 'gateway']]
            self.io()
            return [ct.clear('proof_owned_flows', ips) for ct in node.TLS.conntrack]
        if op == 'metadata':
            return {'kernel': os.uname().release, 'conntrack_buckets': Path('/proc/sys/net/netfilter/nf_conntrack_buckets').read_text().strip()}
        if op == 'wire-check':
            packets = []
            deadline = time.monotonic()+1
            while time.monotonic() < deadline:
                if not select.select([self.wire], [], [], .02)[0]:
                    continue
                frame, _ = self.wire.recvfrom(65536)
                if frame[12:14] == b'\x08\x00' and frame[26:30] == socket.inet_aton(self.local) and frame[30:34] == socket.inet_aton(self.remote):
                    packets.append(frame)
            assert packets and all(f[23] == 50 for f in packets), ('unencrypted underlay', [f[23] for f in packets])
            self.ciphertext = packets
            return {'esp_packets': len(packets), 'plaintext': 0}
        return super().packets(request)

    def cleanup(self):
        self.packets({'op': 'host-close'})
        for sock in [self.packet, self.wire]:
            if sock:
                sock.close()
        return super().cleanup()


node.Experiment = PacketExperiment
node.main()
