"""Fixed Linux UAPI layouts for the IPv4 transport-mode experiment."""
import hashlib,socket,struct,time
METRIC=None
UNIQUE_REQID=False
REPLAY_WINDOW=128
from netlink import NL,U,attr

def address(a):return socket.inet_aton(a)+b'\0'*12
def selector(a,b,port):return address(b)+address(a)+struct.pack('!HHHH',port,65535,0,0)+struct.pack('HBBB3xiI',2,32,32,17,0,0)
def lifetime():return struct.pack('QQQQQQQQ',*[2**64-1]*4,*[0]*4)
def xid(b,spi):return address(b)+struct.pack('!I',spi)+b'\x32\0\0\0'
class Xfrm(NL):
 def __init__(self):
  self.s=socket.socket(socket.AF_NETLINK,socket.SOCK_RAW,6);self.s.bind((0,0));self.s.settimeout(30);self.seq=0
 def request(self,*args,**kwargs):
  t=time.monotonic();r=super().request(*args,**kwargs)
  if METRIC:METRIC(getattr(self,"step","IPsec update"),time.monotonic()-t)
  return r
 def state(self,a,b,spi,key,mark,inbound=False,delete=False):
  self.step="IPsec state "+("in" if inbound else "out")+(" delete" if delete else "add")
  if delete:
   body=address(b)+struct.pack('!I',spi)+struct.pack('HBx',2,50)
   return self.request(17,body+(attr(21,struct.pack('II',mark,0xffffffff)) if not inbound else b''))
  body=b'\0'*56+xid(b,spi)+address(a)+lifetime()+b'\0'*32+b'\0'*12+struct.pack('IIHBBB7x',0,mark if UNIQUE_REQID else 0,2,0,0,0)
  assert len(body)==224,len(body)
  rawkey=bytes.fromhex(key+hashlib.sha256(key.encode()).hexdigest()[:8])
  body+=attr(18,b'rfc4106(gcm(aes))'.ljust(64,b'\0')+struct.pack('II',288,128)+rawkey)
  body+=attr(23,struct.pack('IIIIII',(REPLAY_WINDOW+31)//32,0,0,0,0,REPLAY_WINDOW)+b'\0'*(((REPLAY_WINDOW+31)//32)*4))
  body+=attr(29,U(mark))+attr(30,U(0xffffffff)) if inbound else attr(21,struct.pack('II',mark,0xffffffff))
  return self.request(16,body,5|512|1024)
 def policy(self,a,b,spi,port,mark,direction,delete=False):
  self.step="IPsec policy "+("in" if direction==0 else "out")+(" delete" if delete else "add")
  sel=selector(a,b,port);assert len(sel)==56
  if delete:return self.request(20,sel+struct.pack('IB3x',0,direction)+attr(21,struct.pack('II',mark,0xffffffff)))
  body=sel+lifetime()+b'\0'*32+struct.pack('IIBBBB4x',0,0,direction,0,0,0);assert len(body)==168
  template=xid(b,spi)+struct.pack('H2x',2)+address(a)+struct.pack('IBBBxIII',mark if UNIQUE_REQID else 0,0,0,0,0xffffffff,0xffffffff,0xffffffff);assert len(template)==64
  return self.request(19,body+attr(5,template)+attr(21,struct.pack('II',mark,0xffffffff)),5|512|1024)

def tcmsg(index,handle,parent,priority=0):return struct.pack('B3xiIII',0,index,handle,parent,(priority<<16)|socket.htons(3))
def action(kind,code,extra=b''):
 return attr(1|32768,attr(1,kind.encode()+b'\0')+attr(2|32768,attr(2,struct.pack('IIiii',0,0,code,0,0))+extra))
def tc_install(nl,index,mark):
 nl.request(36,tcmsg(index,0xffff0000,0xfffffff1)+attr(1,b'clsact\0'),5|512|1024)
 nl.request(44,tcmsg(index,0,0xfffffff3,1)+attr(1,b'matchall\0')+attr(2|32768,attr(2|32768,action('skbedit',3,attr(5,U(mark))+attr(8,U(0xffffffff))))),5|512|1024)
 nl.request(44,tcmsg(index,mark,0xfffffff2,1)+attr(1,b'fw\0')+attr(2|32768,attr(4|32768,action('gact',0))),5|512|1024)
 nl.request(44,tcmsg(index,0,0xfffffff2,2)+attr(1,b'matchall\0')+attr(2|32768,attr(2|32768,action('gact',2))),5|512|1024)

def install(nl,index,i,local,remote,key,port=4789,side=0,shared_inbound=False):
 mark=1000+i;tc_install(nl,index,mark);x=Xfrm()
 try:
  for direction in [side,1-side]:
   a,b=(local,remote) if direction==side else (remote,local);spi=11000+i*2+direction;dk=hashlib.sha256((key+str(direction)).encode()).hexdigest()
   x.state(a,b,spi,dk,mark,inbound=direction!=side)
   if not shared_inbound or direction==side:x.policy(a,b,spi,port,mark,1 if direction==side else 0)
 finally:x.s.close()

def remove(i,local,remote,port=4789,side=0,shared_inbound=False):
 x=Xfrm();mark=1000+i
 try:
  for direction in [side,1-side]:
   a,b=(local,remote) if direction==side else (remote,local);spi=11000+i*2+direction
   if not shared_inbound or direction==side:x.policy(a,b,spi,port,mark,1 if direction==side else 0,True)
   x.state(a,b,spi,'',mark,inbound=direction!=side,delete=True)
 finally:x.s.close()
