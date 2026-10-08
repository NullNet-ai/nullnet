import ctypes,struct,socket,os
import socket_filter as sf

def pinned(name):
    path=ctypes.create_string_buffer(('/sys/fs/bpf/nullnet-device-events-v1/'+name).encode())
    attribute=ctypes.create_string_buffer(144)
    struct.pack_into('Q',attribute,0,ctypes.addressof(path))
    return sf.bpf(7,attribute)

def update(fd,key,value):
    k=ctypes.create_string_buffer(key);v=ctypes.create_string_buffer(value)
    attribute=ctypes.create_string_buffer(144)
    struct.pack_into('I',attribute,0,fd)
    struct.pack_into('QQQ',attribute,8,ctypes.addressof(k),ctypes.addressof(v),0)
    sf.bpf(2,attribute)

class Program:
    def load_kernel(self,name):return pinned('nullnet_event_route')
def route_program(*args):return Program()
class Filters:
    def __init__(self):
        self.programs={'uevent':pinned('nullnet_event_uevent')}
        self.owned=pinned('EVENT_OWNED');self.counters=pinned('EVENT_COUNTS')
        self.sockets=pinned('EVENT_SOCKETS')
    def apply(self,mode):pass
    def register(self,s):
        cookie=s.getsockopt(socket.SOL_SOCKET,57,8)
        update(self.sockets,cookie,struct.pack('I',s.getsockname()[0]))
    def counts(self):return {'source':'compiled Rust filters; counters saved separately'}
    def close(self):
        for fd in [*self.programs.values(),self.owned,self.counters,self.sockets]:os.close(fd)
