"""Scoped, reversible filtering before host netlink consumers are awakened."""
import ctypes
import json
import os
from pathlib import Path
import socket
import struct

LIBC=ctypes.CDLL(None,use_errno=True)
LIBC.syscall.restype=ctypes.c_long


def syscall(number,*args):
    result=LIBC.syscall(number,*args)
    if result<0:
        raise OSError(ctypes.get_errno(),os.strerror(ctypes.get_errno()))
    return result


def bpf(command,attribute):
    return syscall(321,command,ctypes.byref(attribute),ctypes.sizeof(attribute))


def map_create(kind,key_size,value_size,entries,name):
    attribute=ctypes.create_string_buffer(144)
    struct.pack_into('IIII',attribute,0,kind,key_size,value_size,entries)
    attribute[28:28+len(name)]=name.encode()
    return bpf(0,attribute)


def map_update(fd,key,value):
    key_buffer=ctypes.create_string_buffer(struct.pack('I',key))
    value_buffer=ctypes.create_string_buffer(struct.pack('I',value))
    attribute=ctypes.create_string_buffer(144)
    struct.pack_into('I',attribute,0,fd)
    struct.pack_into('QQQ',attribute,8,ctypes.addressof(key_buffer),ctypes.addressof(value_buffer),0)
    bpf(2,attribute)


class Program:
    def __init__(self):
        self.instructions=[]
        self.labels={}
        self.fixups=[]
        self.serial=0

    def emit(self,code,dst=0,src=0,offset=0,immediate=0):
        self.instructions.append([code,dst|(src<<4),offset,immediate])

    def mark(self,name):
        assert name not in self.labels,name
        self.labels[name]=len(self.instructions)

    def unique(self,prefix):
        self.serial+=1
        return prefix+str(self.serial)

    def jump(self,target,code=0x05,dst=0,immediate=0,src=0):
        if immediate>=2**31 and code&7==5 and code!=0x05 and not src:
            code+=1
        self.fixups.append((len(self.instructions),target))
        self.emit(code,dst,src,immediate=immediate)

    def load(self,size,offset,indirect=False):
        self.emit(({4:0x20,2:0x28,1:0x30}[size]+(0x20 if indirect else 0)),src=7 if indirect else 0,immediate=offset)

    def move(self,dst,value,register=False):
        self.emit(0xbf if register else 0xb7,dst,value if register else 0,immediate=0 if register else value)

    def map_fd(self,dst,fd):
        self.emit(0x18,dst,1,immediate=fd)
        self.emit(0)

    def bytes(self):
        for index,label in self.fixups:
            self.instructions[index][2]=self.labels[label]-index-1
        return b''.join(struct.pack('BBhi',code,registers,offset,
            immediate if immediate<2**31 else immediate-2**32) for code,registers,offset,immediate in self.instructions)

    def load_kernel(self,name):
        raw=self.bytes()
        instructions=ctypes.create_string_buffer(raw)
        license_buffer=ctypes.create_string_buffer(b'GPL\0')
        log=ctypes.create_string_buffer(1024*1024)
        attribute=ctypes.create_string_buffer(144)
        struct.pack_into('IIQQIIQ',attribute,0,1,len(raw)//8,ctypes.addressof(instructions),
            ctypes.addressof(license_buffer),1,len(log),ctypes.addressof(log))
        attribute[48:48+len(name)]=name.encode()
        try:
            return bpf(5,attribute)
        except OSError:
            Path('/tmp/nn-event-verifier.log').write_bytes(log.value)
            raise


def compare_bytes(p,data,offset,fail):
    while data:
        size=4 if len(data)>=4 else 2 if len(data)>=2 else 1
        p.load(size,offset,True)
        p.jump(fail,0x55,immediate=int.from_bytes(data[:size],'big'))
        data=data[size:]
        offset+=size


def owned_name(p,yes,no,descendants=False):
    # R7 points at the interface name; legacy OVS VLAN ports are excluded.
    choices=[(b'nnp_',None),(b'nnb_',None),(b'nnv_',None),(b'br_',3),(b'ns_',3),(b'vxlan-ns_',9)]
    for prefix,digit in choices:
        following=p.unique('name_next')
        compare_bytes(p,prefix,0,following)
        if digit is not None:
            p.load(1,digit,True)
            p.jump(following,0xa5,immediate=ord('0'))
            p.jump(following,0x25,immediate=ord('9'))
        p.jump(yes)
        p.mark(following)
    for prefix,separator in [(b'veth-',b'-'),(b'macsec-',b'')]:
        following=p.unique('name_next')
        compare_bytes(p,prefix,0,following)
        p.load(1,len(prefix),True)
        p.jump(following,0xa5,immediate=ord('0'))
        p.jump(following,0x25,immediate=ord('9'))
        for offset in range(len(prefix)+1,15-len(separator)):
            suffix_check=p.unique('suffix_check')
            next_position=p.unique('position')
            p.load(1,offset,True)
            p.jump(suffix_check,0xa5,immediate=ord('0'))
            p.jump(next_position,0xb5,immediate=ord('9'))
            p.mark(suffix_check)
            if separator:
                p.jump(following,0x55,immediate=ord('-'))
                p.load(1,offset+1,True)
            suffix=p.unique('suffix')
            p.jump(suffix,0x15,immediate=ord('s'))
            p.jump(following,0x55,immediate=ord('c'))
            p.mark(suffix)
            p.load(1,offset+1+len(separator),True)
            p.jump(yes,0x15,immediate=0)
            if descendants:
                p.jump(yes,0x15,immediate=ord('/'))
            p.jump(following)
            p.mark(next_position)
        p.mark(following)
    p.jump(no)


def counter(p,fd,number):
    p.emit(0x62,10,offset=-16,immediate=number)
    p.map_fd(1,fd)
    p.move(2,10,True)
    p.emit(0x07,2,immediate=-16)
    p.emit(0x85,immediate=1)
    skip=p.unique('counter_missing')
    p.jump(skip,0x15,immediate=0)
    p.emit(0x79,1,0,0)
    p.emit(0x07,1,immediate=1)
    p.emit(0x7b,0,1,0)
    p.mark(skip)


def finish(p,counters,kinds):
    p.mark('allow')
    p.move(0,0xffffffff)
    p.emit(0x95)
    for kind,number in [('uevent',0),('link',1),('address',2),('route',3),('neighbor',4)]:
        if kind not in kinds:
            continue
        p.mark('drop_'+kind)
        counter(p,counters,number)
        p.move(0,0)
        p.emit(0x95)


def uevent_program(counters):
    p=Program()
    p.move(6,1,True)
    p.emit(0x61,0,6,0)
    p.jump('allow',0xa5,immediate=40)
    p.load(4,0)
    actions={b'add@':4,b'remo':7,b'chan':7,b'move':5,b'onli':7,b'offl':8,b'bind':5,b'unbi':7}
    for action,length in actions.items():
        p.jump('action_'+action.decode(),0x15,immediate=int.from_bytes(action,'big'))
    p.jump('allow')
    for action,length in actions.items():
        p.mark('action_'+action.decode())
        p.move(7,length)
        p.jump('path')
    p.mark('path')
    compare_bytes(p,b'/devices/virtual/net/',0,'allow')
    p.emit(0x07,7,immediate=21)
    owned_name(p,'drop_uevent','allow',descendants=True)
    finish(p,counters,{'uevent'})
    return p


def lookup_owned(p,owned,kind):
    p.emit(0x63,10,8,-4)
    p.map_fd(1,owned)
    p.move(2,10,True)
    p.emit(0x07,2,immediate=-4)
    p.emit(0x85,immediate=1)
    p.jump('drop_'+kind,0x55,immediate=0)
    p.jump('allow')


def route_program(owned,counters,recipient=0x4e123499):
    p=Program()
    p.move(6,1,True)
    p.emit(0x61,0,6,0)
    p.jump('allow',0xa5,immediate=32)
    p.move(9,0,True)
    p.load(4,0)
    p.emit(0xdc,0,immediate=32)
    p.jump('allow',0x5d,src=9)
    p.load(4,12)
    p.jump('allow',0x15,immediate=socket.htonl(recipient))
    p.jump('multicast',0x55,immediate=0)
    p.load(4,8)
    p.jump('allow',0x55,immediate=0)
    p.mark('multicast')
    p.load(2,6)
    p.jump('allow',0x45,immediate=socket.htons(2))
    p.load(2,4)
    for types,label in [((16,17),'link'),((20,21),'address'),((24,25),'route'),((28,29),'neighbor')]:
        for value in types:
            p.jump(label,0x15,immediate=socket.htons(value))
    p.jump('allow')
    p.mark('link')
    p.load(2,34)
    p.jump('allow',0x55,immediate=socket.htons(3))
    p.load(4,20)
    p.move(8,0,True)
    p.move(7,36)
    owned_name(p,'owned_link','foreign_link')
    p.mark('owned_link')
    p.emit(0x63,10,8,-4)
    p.emit(0x62,10,offset=-8,immediate=1)
    p.map_fd(1,owned)
    p.move(2,10,True)
    p.emit(0x07,2,immediate=-4)
    p.move(3,10,True)
    p.emit(0x07,3,immediate=-8)
    p.move(4,0)
    p.emit(0x85,immediate=2)
    p.jump('drop_link')
    p.mark('foreign_link')
    p.emit(0x63,10,8,-4)
    p.map_fd(1,owned)
    p.move(2,10,True)
    p.emit(0x07,2,immediate=-4)
    p.emit(0x85,immediate=3)
    p.jump('allow')
    for kind in ['address','neighbor']:
        p.mark(kind)
        p.load(4,20)
        p.move(8,0,True)
        lookup_owned(p,owned,kind)
    p.mark('route')
    p.move(7,28)
    for _ in range(32):
        p.load(2,0,True)
        p.emit(0xdc,0,immediate=16)
        p.jump('allow',0xa5,immediate=4)
        p.move(8,0,True)
        p.load(2,2,True)
        p.jump('route_oif',0x15,immediate=socket.htons(4))
        p.jump('allow',0x15,immediate=socket.htons(9))
        p.emit(0x07,8,immediate=3)
        p.emit(0x57,8,immediate=-4)
        p.emit(0x0f,7,8)
        p.emit(0x61,0,6,0)
        p.jump('allow',0x3d,dst=7,src=0)
    p.jump('allow')
    p.mark('route_oif')
    p.load(4,4,True)
    p.move(8,0,True)
    lookup_owned(p,owned,'route')
    finish(p,counters,{'link','address','route','neighbor'})
    return p


class Filters:
    def __init__(self,inventory,output):
        self.output=Path(output)
        self.inventory=inventory
        self.sockets=[]
        self.attached=set()
        self.mode='baseline'
        self.owned=map_create(9,4,4,65536,'nn_evt_owned')
        self.counters=map_create(6,4,8,5,'nn_evt_counts')
        self.programs={'uevent':uevent_program(self.counters).load_kernel('nn_evt_uevent')}
        for row in inventory['sockets']:
            if row['protocol']==15 and not row['groups']&1:
                continue
            assert row['original_filter_instructions']==0,('preserve original filter',row)
            pidfd=syscall(434,row['pid'],0)
            try:
                duplicate=syscall(438,pidfd,row['fd'],0)
            finally:
                os.close(pidfd)
            s=socket.socket(fileno=duplicate)
            assert os.fstat(s.fileno()).st_ino==row['inode']
            self.sockets.append((row,s))
            if row['protocol']==0:
                self.programs[row['inode']]=route_program(self.owned,self.counters,row['portid']).load_kernel('nn_evt_route')
        self.seed()
        self.save()

    def seed(self):
        import re
        import subprocess
        pattern=re.compile(r'^(?:nnp_|nnb_|nnv_|br_[0-9]|ns_[0-9]|vxlan-ns_[0-9]|veth-[0-9]+-[sc]$|macsec-[0-9]+[sc]$)')
        for link in json.loads(subprocess.check_output(['ip','-j','link'],text=True)):
            if pattern.match(link['ifname']):
                map_update(self.owned,socket.htonl(link['ifindex']),1)

    def apply(self,mode):
        assert mode in ('baseline','udev','all')
        for row,s in self.sockets:
            enable=mode=='all' or mode=='udev' and row['protocol']==15
            if enable:
                program=self.programs['uevent' if row['protocol']==15 else row['inode']]
                s.setsockopt(socket.SOL_SOCKET,50,struct.pack('I',program))
                self.attached.add(row['inode'])
            elif row['inode'] in self.attached:
                s.setsockopt(socket.SOL_SOCKET,27,struct.pack('I',0))
                self.attached.remove(row['inode'])
        self.mode=mode
        self.save()

    def counts(self):
        key=ctypes.create_string_buffer(4)
        value=ctypes.create_string_buffer(os.cpu_count()*8)
        results=[]
        for i in range(5):
            struct.pack_into('I',key,0,i)
            attribute=ctypes.create_string_buffer(144)
            struct.pack_into('I',attribute,0,self.counters)
            struct.pack_into('QQ',attribute,8,ctypes.addressof(key),ctypes.addressof(value))
            bpf(1,attribute)
            results.append(sum(struct.unpack_from('Q',value,j*8)[0] for j in range(os.cpu_count())))
        return dict(zip(['uevent','link','address','route','neighbor'],results))

    def save(self):
        self.output.write_text(json.dumps({'mode':self.mode,'sockets':[r for r,s in self.sockets],
                                          'counts':self.counts()},indent=2)+'\n')

    def close(self):
        self.apply('baseline')
        for row,s in self.sockets:
            s.close()
        for fd in [*self.programs.values(),self.owned,self.counters]:
            os.close(fd)
