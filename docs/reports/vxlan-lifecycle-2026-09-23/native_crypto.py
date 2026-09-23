"""Linux 6.12/iproute2-source-matched MACsec netlink prototype."""
import os,socket,struct,time
FAMILY=None
METRIC=None
from netlink import NL,U,attr

def attrs(data):
 off=0
 while off+4<=len(data):
  size,kind=struct.unpack_from('HH',data,off);yield kind&16383,data[off+4:off+size];off+=(size+3)&~3

class Genl:
 def __init__(self):
  self.s=socket.socket(socket.AF_NETLINK,socket.SOCK_RAW,16);self.s.bind((0,0));self.s.settimeout(30);self.seq=0
 def call(self,family,cmd,payload,version=1):
  t=time.monotonic();r=self._call(family,cmd,payload,version)
  if METRIC:METRIC(cmd,time.monotonic()-t)
  return r
 def _call(self,family,cmd,payload,version=1):
  self.seq+=1;body=struct.pack('BBH',cmd,version,0)+payload
  self.s.sendto(struct.pack('IHHII',16+len(body),family,5,self.seq,0)+body,(0,0));reply=None
  while True:
   data=self.s.recv(65536);off=0
   while off+16<=len(data):
    size,typ,flags,seq,pid=struct.unpack_from('IHHII',data,off)
    if seq==self.seq:
     if typ==2:
      err=struct.unpack_from('i',data,off+16)[0]
      if err:raise RuntimeError(('genl',cmd,err))
      return reply
     reply=data[off+20:off+size]
    off+=(size+3)&~3
 def family(self):return struct.unpack('H',dict(attrs(self.call(16,3,attr(2,b'macsec\0'),2)))[1])[0]

def install(nl,name,parent,peer,key,family=None,replay=False):
 data=attr(2,struct.pack('!H',1))+attr(4,struct.pack('Q',0x0080c20001000002))+attr(7,b'\x01')
 if replay:data+=attr(5,U(128))+attr(12,b'\x01')+attr(13,b'\x02')
 nl.create(name,'macsec',data,attr(5,U(nl.get(parent))))
 index=nl.get(name);g=Genl()
 try:
  family=family or FAMILY or g.family();base=attr(1,U(index));sci=bytes.fromhex(peer.replace(':',''))+b'\0\x01'
  sa=attr(3|32768,attr(1,b'\0')+attr(2,b'\x01')+attr(3,U(1))+attr(4,bytes.fromhex(key))+attr(5,b'\0'*16))
  g.call(family,4,base+sa)
  g.call(family,1,base+attr(2|32768,attr(1,sci)+attr(2,b'\x01')))
  g.call(family,7,base+attr(2|32768,attr(1,sci))+sa)
 finally:g.s.close()
 return index

def configure_namespace(pid,name,address,namespace=None,default_route=False):
 old=os.open('/proc/thread-self/ns/net',os.O_RDONLY);target=os.open(namespace or f'/proc/{pid}/ns/net',os.O_RDONLY)
 try:
  os.setns(target,os.CLONE_NEWNET);n=NL()
 finally:os.setns(old,os.CLONE_NEWNET);os.close(old);os.close(target)
 try:
  index=n.get(name);n.addr(index,address);n.up(index)
  if default_route:
   gateway=socket.inet_ntoa(struct.pack('!I',struct.unpack('!I',socket.inet_aton(address))[0]-1))
   n.request(24,struct.pack('BBBBBBBBI',2,0,0,0,254,3,0,1,0)+attr(4,U(index))+attr(5,socket.inet_aton(gateway)),5|512|1024)
 finally:n.s.close()
