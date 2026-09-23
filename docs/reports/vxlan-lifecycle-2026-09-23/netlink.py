import os, socket, struct, time

U=lambda n:struct.pack('I',n)

def attr(k,v):
 b=struct.pack('HH',len(v)+4,k)+v;return b+b'\0'*((-len(b))%4)

def info(index=0,flags=0,change=0):return struct.pack('BBHiII',0,0,0,index,flags,change)

def nested(kind,data):return attr(18|32768,attr(1,kind.encode()+b'\0')+attr(2|32768,data))

class NL:
 def __init__(self):
  self.s=socket.socket(socket.AF_NETLINK,socket.SOCK_RAW,0);self.s.bind((0,0));self.s.settimeout(120);self.seq=0
 def request(self,kind,payload,flags=5,missing=False):
  self.seq+=1;msg=struct.pack('IHHII',16+len(payload),kind,flags,self.seq,0)+payload;start=time.monotonic();self.s.sendto(msg,(0,0));result=None
  while True:
   data=self.s.recv(65536);off=0
   while off+16<=len(data):
    size,typ,fl,seq,pid=struct.unpack_from('IHHII',data,off)
    if seq==self.seq:
     if typ==2:
      err=struct.unpack_from('i',data,off+16)[0]
      if err and not(missing and err==-19):raise RuntimeError(('netlink',kind,err))
      return result,time.monotonic()-start
     elif typ==16:result=struct.unpack_from('i',data,off+20)[0]
    off+=(size+3)&~3
 def get(self,name):return self.request(18,info()+attr(3,name.encode()+b'\0'),missing=True)[0]
 def create(self,name,kind,data,extra=b''):return self.request(16,info()+attr(3,name.encode()+b'\0')+attr(27,U(0x4e600001))+nested(kind,data)+extra,5|512|1024|(8 if os.environ.get("NN_LINK_ECHO")=="1" else 0))
 def up(self,index,bridge=None):return self.request(19,info(index,1,1)+attr(4,U(1080))+(attr(10,U(bridge)) if bridge else b''))
 def addr(self,index,addr):return self.request(20,struct.pack('BBBBI',2,30,0,0,index)+attr(1,socket.inet_aton(addr))+attr(2,socket.inet_aton(addr)),5|512|1024)
