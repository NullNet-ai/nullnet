"""Protocol and independent-traffic checks on the same policy datapath."""
import importlib.util
from pathlib import Path
import select
import socket
import struct
import threading
import time

spec=importlib.util.spec_from_file_location('policy_node',Path(__file__).with_name('node.py'))
policy=importlib.util.module_from_spec(spec);spec.loader.exec_module(policy)


class Stress(policy.Experiment):
    def __init__(self,side):
        super().__init__(side)
        self.tcp=None;self.listener=None;self.bg=None;self.bg_thread=None
        self.bg_stop=threading.Event();self.bg_result={'exchanges':0,'errors':[]}

    def keys(self,port,proto):
        for direction in [0,1]:
            yield direction, socket.inet_aton(policy.ip(direction,0))+socket.inet_aton(policy.ip(1-direction,0))+struct.pack('!HH',port,port)+struct.pack('I',proto)

    def permission(self,port,proto,delete=False):
        for direction,key in self.keys(port,proto):
            if delete:self.bpf.delete('permits',key)
            else:
                value=struct.pack('IIQII',16000000+port,self.entries[0]['index'] if direction!=self.side else 0,time.monotonic_ns()+300_000_000_000,1,0)
                self.bpf.update('permits',key,value)

    def tcp_exchange(self):
        self.tcp.sendall(b'policy-tcp-'+bytes([self.side]))
        data=b''
        while len(data)<12:
            chunk=self.tcp.recv(12-len(data));assert chunk, 'TCP closed during exchange';data+=chunk
        assert data==b'policy-tcp-'+bytes([1-self.side]),data
        return {'tcp_received':len(data)}

    def background(self):
        try:
            sequence=0
            while not self.bg_stop.is_set():
                if self.side==0:
                    payload=struct.pack('!Q',sequence)
                    self.bg.sendto(payload,(policy.ip(1,0),32001))
                    data,_=self.bg.recvfrom(1024);assert data==payload
                    sequence+=1;self.bg_result['exchanges']+=1
                    self.bg_stop.wait(.001)
                else:
                    try:data,peer=self.bg.recvfrom(1024)
                    except TimeoutError:continue
                    self.bg.sendto(data,peer);self.bg_result['exchanges']+=1
        except Exception as e:self.bg_result['errors'].append(repr(e))

    def stop_background(self):
        self.bg_stop.set()
        if self.bg_thread:self.bg_thread.join(timeout=3);assert not self.bg_thread.is_alive()
        if self.bg:self.bg.close();self.bg=None
        self.permission(32001,17,True)
        return self.bg_result

    def handle(self,r):
        op=r['op']
        if op=='tcp-open':
            self.permission(32000,6)
            s=policy.namespace_socket(self.targets[0],lambda:socket.socket(socket.AF_INET,socket.SOCK_STREAM))
            s.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1);s.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEPORT,1)
            s.setsockopt(socket.SOL_SOCKET,socket.SO_LINGER,struct.pack('ii',1,0))
            s.bind((policy.ip(self.side,0),32000));s.settimeout(3)
            if self.side==1:s.listen();self.listener=s
            else:self.tcp=s
            return {'ready':True}
        if op=='tcp-connect':
            if self.side==0:self.tcp.connect((policy.ip(1,0),32000))
            else:self.tcp,_=self.listener.accept();self.tcp.settimeout(3)
            return self.tcp_exchange()
        if op=='tcp-revoke':self.permission(32000,6,True);return {'revoked':True}
        if op=='tcp-send-retired':self.tcp.sendall(b'revoked');return {'attempted':7}
        if op=='tcp-quiet':
            ready,_,_=select.select([self.tcp],[],[],.1)
            assert not ready,('revoked TCP delivered',self.tcp.recv(1024));return {'delivered':0}
        if op=='tcp-close':
            self.tcp.setsockopt(socket.SOL_SOCKET,socket.SO_LINGER,struct.pack('ii',1,0));self.tcp.close();self.tcp=None
            if self.listener:self.listener.close();self.listener=None
            return {'closed':True}
        if op=='background-start':
            self.permission(32001,17)
            self.bg=policy.namespace_socket(self.targets[0],lambda:socket.socket(socket.AF_INET,socket.SOCK_DGRAM))
            self.bg.bind((policy.ip(self.side,0),32001));self.bg.settimeout(2 if self.side==0 else .1)
            self.bg_thread=threading.Thread(target=self.background);self.bg_thread.start();return {'started':True}
        if op=='background-stop':return self.stop_background()
        return super().handle(r)

    def cleanup(self):
        if self.bg_thread:self.stop_background()
        if self.tcp:self.tcp.close()
        if self.listener:self.listener.close()
        if self.bpf:self.permission(32000,6,True)
        super().cleanup()


policy.Experiment=Stress
if __name__=='__main__':policy.main()
