"""Isolated persistent-transport experiment; no product runtime replacement."""
import argparse
import ctypes as C
import hashlib
import json
import os
from pathlib import Path
import select
import socket
import struct
import subprocess
import sys
import time
from types import SimpleNamespace

ROOT = Path(__file__).resolve().parent
sys.path[:0] = [str(ROOT.parent / 'vxlan-prepared-cross-host-2026-09-28'),
               str(ROOT.parent / 'vxlan-prepared-pool-2026-09-28'),
               str(ROOT.parent / 'vxlan-lifecycle-2026-09-23')]
import node as cross
from prototype import Route, namespace_socket, command
import native_xfrm as crypto

PORT = 4791
MARK = 0x4E300001
SPI = 0x4E300100
PIN = Path('/sys/fs/bpf/nn29-policy')
cross.PORT = PORT
crypto.UNIQUE_REQID = True
crypto.REPLAY_WINDOW = 4096


def inventory():
    result = cross.original_inventory()
    states = command('ip', 'xfrm', 'state')
    states = '\n'.join(line for line in states.splitlines() if 'lastused' not in line and 'anti-replay context' not in line)
    result['xfrm_hash'] = hashlib.sha256(states.encode()).hexdigest()
    return result


class Bpf:
    def __init__(self):
        self.lib = C.CDLL('libbpf.so.1', use_errno=True)
        signatures = {
            'bpf_object__open_file': (C.c_void_p, [C.c_char_p, C.c_void_p]),
            'bpf_object__load': (C.c_int, [C.c_void_p]),
            'bpf_object__close': (None, [C.c_void_p]),
            'bpf_object__find_map_fd_by_name': (C.c_int, [C.c_void_p, C.c_char_p]),
            'bpf_object__find_program_by_name': (C.c_void_p, [C.c_void_p, C.c_char_p]),
            'bpf_program__pin': (C.c_int, [C.c_void_p, C.c_char_p]),
            'bpf_map_update_elem': (C.c_int, [C.c_int, C.c_void_p, C.c_void_p, C.c_ulonglong]),
            'bpf_map_delete_elem': (C.c_int, [C.c_int, C.c_void_p]),
            'bpf_map_lookup_elem': (C.c_int, [C.c_int, C.c_void_p, C.c_void_p]),
            'bpf_map_get_next_key': (C.c_int, [C.c_int, C.c_void_p, C.c_void_p]),
        }
        for name, (result, args) in signatures.items():
            f = getattr(self.lib, name); f.restype = result; f.argtypes = args
        self.obj = self.lib.bpf_object__open_file(str(ROOT / 'policy.bpf.o').encode(), None)
        assert self.obj and self.obj < 2**63, ('open bpf', C.get_errno())
        assert self.lib.bpf_object__load(self.obj) == 0, ('load bpf', C.get_errno())
        self.maps = {n: self.lib.bpf_object__find_map_fd_by_name(self.obj, n.encode())
                     for n in ['permits', 'identities', 'endpoints', 'config', 'counters']}
        assert all(fd >= 0 for fd in self.maps.values())
        PIN.mkdir()
        for name in ['application', 'transport']:
            prog = self.lib.bpf_object__find_program_by_name(self.obj, name.encode())
            assert prog and self.lib.bpf_program__pin(prog, str(PIN / name).encode()) == 0

    def update(self, name, key, value):
        assert self.lib.bpf_map_update_elem(self.maps[name], key, value, 0) == 0, ('map update', name, C.get_errno())

    def delete(self, name, key):
        rc = self.lib.bpf_map_delete_elem(self.maps[name], key)
        assert rc == 0 or C.get_errno() == 2, ('map delete', name, C.get_errno())

    def counters(self):
        out = []
        for i in range(16):
            value = C.c_uint64()
            assert self.lib.bpf_map_lookup_elem(self.maps['counters'], struct.pack('I', i), C.byref(value)) == 0
            out.append(value.value)
        return out

    def close(self):
        for name in ['application', 'transport']:
            (PIN / name).unlink(missing_ok=True)
        PIN.rmdir()
        self.lib.bpf_object__close(self.obj)


def ip(side, index):
    return f'10.230.{side}.{index+1}'


def endpoint(number, side):
    return number // 8 if side == 0 else number % 8


def tuple_key(number, direction):
    return socket.inet_aton(ip(direction, endpoint(number, direction))) + socket.inet_aton(ip(1-direction, endpoint(number, 1-direction))) + struct.pack('!HH', 31000+number, 31000+number) + struct.pack('I', 17)


class Experiment:
    def __init__(self, side):
        self.args = SimpleNamespace(side=side)
        self.side = side
        self.local = f'198.18.30.{103+side}'
        self.remote = f'198.18.30.{104-side}'
        self.infrastructure, self.rules, self.fixtures, self.links, self.targets = [], [], [], [], []
        self.policy = False
        self.xstates = []
        self.xpolicy = False
        self.bpf = None
        self.sockets = {}
        self.raw = {}
        self.entries = []
        self.generations = {}
        self.capture = None
        self.capture_file = None
        self.crypto_key = None
        self.inbound_present = True

    def attach(self, dev, program):
        command('tc', 'qdisc', 'add', 'dev', dev, 'clsact')
        command('tc', 'filter', 'add', 'dev', dev, 'ingress', 'bpf', 'da', 'pinned', str(PIN/program))

    def prepare(self, key):
        start = time.monotonic()
        self.crypto_key = key
        assert not PIN.exists()
        Cross = cross.Cross
        Cross.setup_underlay(self)
        self.bpf = Bpf()
        name = 'nnp30vx'
        assert Route().get(name) is None
        command('ip', 'link', 'add', name, 'type', 'vxlan', 'external', 'local', self.local, 'dstport', str(PORT), 'nolearning')
        self.links.append(name)
        command('ip', 'link', 'set', name, 'mtu', '1080')
        transport = Route().get(name)
        self.attach(name, 'transport')
        config = struct.pack('IIII', int.from_bytes(socket.inet_aton(self.local), 'big'), int.from_bytes(socket.inet_aton(self.remote), 'big'), transport, MARK)
        self.bpf.update('config', struct.pack('I', 0), config)
        x = crypto.Xfrm()
        for direction in [self.side, 1-self.side]:
            a,b = (self.local,self.remote) if direction == self.side else (self.remote,self.local)
            derived = hashlib.sha256((key + str(direction)).encode()).hexdigest()
            x.state(a,b,SPI+direction,derived,MARK,inbound=direction != self.side)
            self.xstates.append((a,b,SPI+direction,direction != self.side))
        x.policy(self.local,self.remote,SPI+self.side,PORT,MARK,1)
        self.xpolicy = True
        x.s.close()
        for i in range(8):
            fixture = f'nnp30-{self.side}-{i}'
            assert subprocess.run(['docker','inspect',fixture],capture_output=True).returncode != 0
            command('docker','run','-d','--network','none','--label','nullnet-prototype=policy-20260929','--name',fixture,'alpine:latest','sleep','infinity')
            self.fixtures.append(fixture)
            pid = int(command('docker','inspect','-f','{{.State.Pid}}',fixture))
            fd = os.open(f'/proc/{pid}/ns/net',os.O_RDONLY); self.targets.append(fd)
            outer, inner = f'nnp30-{i}', 'nnpolicy'
            command('ip','link','add',outer,'type','veth','peer','name',f'nnp30i-{i}','netns',str(pid))
            self.links.append(outer)
            ns = ['nsenter','-t',str(pid),'-n']
            command(*ns,'ip','link','set',f'nnp30i-{i}','name',inner)
            command(*ns,'ip','link','set',inner,'mtu','1080')
            command('ip','link','set',outer,'mtu','1080')
            command(*ns,'ip','addr','add',ip(self.side,i)+'/32','dev',inner)
            ix = Route().get(outer)
            self.bpf.update('identities',struct.pack('I',ix),socket.inet_aton(ip(self.side,i)))
            details = json.loads(command(*ns,'ip','-j','link','show','dev',inner))[0]
            mac = bytes.fromhex(details['address'].replace(':',''))
            self.bpf.update('endpoints',socket.inet_aton(ip(self.side,i)),struct.pack('I',ix)+mac+b'\0\0')
            self.entries.append({'pid':pid,'fd':fd,'index':ix,'mac':mac.hex()})
            self.attach(outer,'application')
            command('ip','link','set',outer,'up')
            command(*ns,'ip','link','set',inner,'up')
            command(*ns,'ip','route','add',f'10.230.{1-self.side}.0/24','dev',inner)
            for j in range(8):
                command(*ns,'ip','neigh','add',ip(1-self.side,j),'lladdr','02:30:00:00:00:01','nud','permanent','dev',inner)
            Route().get(outer)
            namespace_socket(fd, lambda: Route().get(inner))
            def packet():
                s=socket.socket(socket.AF_PACKET,socket.SOCK_RAW,socket.htons(3));s.bind((inner,0));return s
            self.raw[i] = namespace_socket(fd,packet)
        command('ip','link','set',name,'up'); Route().get(name)
        for n in range(64):
            fd=self.targets[endpoint(n,self.side)]
            s=namespace_socket(fd,lambda:socket.socket(socket.AF_INET,socket.SOCK_DGRAM))
            s.bind((ip(self.side,endpoint(n,self.side)),31000+n));s.setblocking(False);self.sockets[n]=s
        return {'ready':True,'seconds':time.monotonic()-start,'kernel':os.uname().release,'containers':8,'links':17,'source':hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),'bpf_source':hashlib.sha256((ROOT/'policy.bpf.c').read_bytes()).hexdigest()}

    def set_policy(self, ids, generation, transmit, lease_ms=2000):
        assert 0 < generation < 2**24
        deadline=time.monotonic_ns()+lease_ms*1000000
        start=time.monotonic_ns()
        for n in ids:
            for direction in [0,1]:
                out=self.entries[endpoint(n,self.side)]['index'] if direction != self.side else 0
                value=struct.pack('IIQII',generation,out,deadline,int(transmit),0)
                self.bpf.update('permits',tuple_key(n,direction),value)
            self.generations[n]=generation
        return {'policy_ns':time.monotonic_ns()-start,'entries':2*len(ids)}

    def retire(self, ids):
        start=time.monotonic_ns()
        for n in ids:
            for direction in [0,1]:self.bpf.delete('permits',tuple_key(n,direction))
        return {'retire_ns':time.monotonic_ns()-start,'entries':2*len(ids)}

    def send(self, ids, label):
        for n in ids:
            data=struct.pack('!4sIII',b'NP30',n,self.generations.get(n,0),label)
            self.sockets[n].sendto(data,(ip(1-self.side,endpoint(n,1-self.side)),31000+n))

    def receive(self, ids, label, timeout=3):
        pending=set(ids); deadline=time.monotonic()+timeout
        while pending:
            remaining=deadline-time.monotonic()
            assert remaining > 0, ('receive timeout',sorted(pending),self.bpf.counters())
            ready,_,_=select.select([self.sockets[n] for n in pending],[],[],remaining)
            for s in ready:
                n=s.getsockname()[1]-31000;data,peer=s.recvfrom(2048)
                assert data == struct.pack('!4sIII',b'NP30',n,self.generations[n],label),(n,data.hex(),label)
                assert peer==(ip(1-self.side,endpoint(n,1-self.side)),31000+n),peer
                pending.remove(n)
        return {'received':len(ids)}

    def quiet(self, wait=0):
        ready,_,_=select.select(list(self.sockets.values()),[],[],wait)
        assert not ready,('unexpected application delivery',[(s.getsockname(),s.recvfrom(2048)[0].hex()) for s in ready])
        return {'delivered':0,'counters':self.bpf.counters()}

    def frame(self,n,source=None,label=999):
        src=socket.inet_aton(source or ip(self.side,endpoint(n,self.side)));dst=socket.inet_aton(ip(1-self.side,endpoint(n,1-self.side)))
        payload=struct.pack('!4sIII',b'NP30',n,self.generations.get(n,0),label)
        udp=struct.pack('!HHHH',31000+n,31000+n,8+len(payload),0)+payload
        header=struct.pack('!BBHHHBBH4s4s',0x45,0,20+len(udp),0,0,64,17,0,src,dst)
        checksum=sum(struct.unpack('!10H',header));checksum=(checksum&65535)+(checksum>>16);checksum=(checksum&65535)+(checksum>>16)
        header=header[:10]+struct.pack('!H',(~checksum)&65535)+header[12:]
        return b'\x02\x30\0\0\0\x01'*2+b'\x08\0'+header+udp

    def inject(self,n,generation,source=None,plaintext=False):
        s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM)
        s.bind((self.local,0))
        if not plaintext:s.setsockopt(socket.SOL_SOCKET,36,MARK)
        packet=b'\x08\0\0\0'+struct.pack('!I',generation<<8)+self.frame(n,source)
        try:s.sendto(packet,(self.remote,PORT));blocked=False
        except PermissionError:blocked=True
        finally:s.close()
        return {'injected':1,'local_blocked':blocked}

    def handle(self,r):
        op=r['op']; ids=r.get('ids',list(range(64)))
        if op=='prepare':return self.set_policy(ids,r['generation'],False,r.get('lease_ms',2000))
        if op=='commit':return self.set_policy(ids,r['generation'],True,r.get('lease_ms',2000))
        if op=='retire':return self.retire(ids)
        if op=='traffic':self.send(ids,r['label']);return self.receive(ids,r['label'])
        if op=='send':self.send(ids,r.get('label',999));return {'sent':len(ids)}
        if op=='denied':
            before=self.bpf.counters()[2];self.send(ids,999)
            deadline=time.monotonic()+2
            while self.bpf.counters()[2] < before+len(ids):
                assert time.monotonic()<deadline,('deny counter timeout',self.bpf.counters())
            return {'denied':len(ids),'counters':self.bpf.counters()}
        if op=='quiet':return self.quiet(r.get('wait',0))
        if op=='counters':return self.bpf.counters()
        if op=='inject':return self.inject(r.get('id',0),r['generation'],r.get('source'),r.get('plaintext',False))
        if op=='spoof':
            self.raw[r.get('endpoint',1)].send(self.frame(r.get('id',0),r.get('source')));return {'sent':1}
        if op=='inventory':
            key=C.create_string_buffer(16)
            rc=self.bpf.lib.bpf_map_get_next_key(self.bpf.maps['permits'],None,key)
            assert rc<0 and C.get_errno()==2, ('permission map not empty',rc,C.get_errno())
            links=json.loads(command('ip','-j','link'))
            return {'permissions':0,'links':[(l['ifindex'],l['ifname']) for l in links if l['ifname'].startswith('nnp30')]}
        if op=='xfrm-counters':
            return {k:int(v) for k,v in (line.split() for line in Path('/proc/net/xfrm_stat').read_text().splitlines())}
        if op=='fault':
            x=crypto.Xfrm(); direction=1-self.side
            if self.inbound_present:x.state(self.remote,self.local,SPI+direction,'',MARK,inbound=True,delete=True)
            self.inbound_present=False
            if r['mode']!='missing':
                key=hashlib.sha256((self.crypto_key+str(direction)).encode()).hexdigest() if r['mode']=='restore' else os.urandom(32).hex()
                x.state(self.remote,self.local,SPI+direction,key,MARK,inbound=True);self.inbound_present=True
            x.s.close();return {'fault':r['mode']}
        if op=='replay':
            data=Path('/tmp/nn29-policy/wire.pcap').read_bytes()
            endian='<' if data[:4]==bytes.fromhex('d4c3b2a1') else '>'
            assert data[:4] in [bytes.fromhex('d4c3b2a1'),bytes.fromhex('a1b2c3d4')]
            offset=24;frames=[]
            while offset<len(data):
                _,_,size,_=struct.unpack_from(endian+'IIII',data,offset);offset+=16
                frame=data[offset:offset+size];offset+=size
                if frame[12:14]==b'\x08\0' and frame[23]==50 and frame[26:30]==socket.inet_aton(self.local):frames.append(frame)
            assert frames
            raw=socket.socket(socket.AF_PACKET,socket.SOCK_RAW,socket.htons(3));raw.bind(('ens18',0))
            for frame in frames:raw.send(frame)
            raw.close();return {'replayed':len(frames)}
        if op=='writer-exit':
            child=os.fork()
            if child==0:
                self.set_policy(ids,r['generation'],True,r['lease_ms']);os._exit(0)
            _,status=os.waitpid(child,0);assert status==0
            for n in ids:self.generations[n]=r['generation']
            return {'writer_exited':True,'lease_ms':r['lease_ms']}
        if op=='capture':
            self.capture_file=open('/tmp/nn29-policy/capture.log','w')
            self.capture=subprocess.Popen(['tcpdump','--immediate-mode','-i','ens18','-U','-w','/tmp/nn29-policy/wire.pcap','host',self.local,'and','host',self.remote],stdout=subprocess.DEVNULL,stderr=self.capture_file)
            deadline=time.monotonic()+3
            while 'listening on' not in Path('/tmp/nn29-policy/capture.log').read_text():
                assert time.monotonic()<deadline; time.sleep(.01)
            return {'capturing':True}
        if op=='capture-stop':
            self.capture.send_signal(2);self.capture.wait(timeout=5);self.capture=None;self.capture_file.close();self.capture_file=None
            text=command('tcpdump','-nn','-r','/tmp/nn29-policy/wire.pcap')
            assert 'ESP(' in text and 'UDP' not in text,text
            return {'esp_packets':text.count('ESP('),'plaintext_udp':0}
        raise ValueError(op)

    def cleanup(self):
        errors=[]
        def attempt(fn):
            try:fn()
            except Exception as e:errors.append(repr(e))
        if self.capture:attempt(lambda:self.capture.terminate());attempt(lambda:self.capture.wait(timeout=5))
        for s in list(self.sockets.values())+list(self.raw.values()):s.close()
        if self.bpf:attempt(lambda:self.retire(list(range(64))))
        for name in reversed(self.links):attempt(lambda name=name:command('ip','link','del',name))
        if self.bpf:attempt(self.bpf.close)
        x=crypto.Xfrm()
        if self.xpolicy:attempt(lambda:x.policy(self.local,self.remote,SPI+self.side,PORT,MARK,1,True))
        for a,b,spi,inbound in self.xstates:
            if not inbound or self.inbound_present:attempt(lambda a=a,b=b,spi=spi,inbound=inbound:x.state(a,b,spi,'',MARK,inbound=inbound,delete=True))
        x.s.close()
        if self.policy:attempt(lambda:command('ip','xfrm','policy','delete','src',self.remote,'dst',self.local,'proto','udp','dport',str(PORT),'dir','in'))
        for rule in reversed(self.rules):attempt(lambda rule=rule:command(*rule))
        for cmd in reversed(self.infrastructure):attempt(lambda cmd=cmd:command(*cmd))
        for fixture in self.fixtures:attempt(lambda fixture=fixture:command('docker','rm','-f',fixture))
        for address in [self.local,self.remote]:
            for field in ['--orig-src','--orig-dst']:
                attempt(lambda address=address,field=field:cross.run(['conntrack','-D','-f','ipv4',field,address],(0,1)))
        for fd in self.targets:os.close(fd)
        assert not errors,errors


def main():
    p=argparse.ArgumentParser();p.add_argument('--side',type=int,required=True);args=p.parse_args()
    before=inventory();experiment=Experiment(args.side);result={}
    def emit(value):print(json.dumps(value),flush=True)
    try:
        initial=json.loads(sys.stdin.readline());emit(experiment.prepare(initial['key']))
        for line in sys.stdin:
            request=json.loads(line)
            if request['op']=='finish':break
            try:emit(experiment.handle(request))
            except Exception as e:emit({'error':repr(e)})
    except Exception as e:
        result['error']=repr(e);emit(result)
    finally:
        try:experiment.cleanup()
        except Exception as e:result['cleanup_error']=repr(e)
        after=inventory();result['preservation']={k:before[k]==after[k] for k in before}
        result['cleaned']='cleanup_error' not in result and all(result['preservation'].values())
        Path(f'/tmp/nn29-policy/final-{args.side}.json').write_text(json.dumps(result,indent=2));emit(result)

if __name__=='__main__':main()
