"""Check actual consumer discovery, FD replacement and foreign-filter preservation."""
import json,subprocess,socket,struct,time,select,sys
CHILD=r'''
import ctypes,json,socket,struct,sys,select,time
libc=ctypes.CDLL(None);libc.prctl(15,b'NetworkManager',0,0,0)
s=None
def opened():
 global s
 if s is not None:s.close()
 s=socket.socket(socket.AF_NETLINK,socket.SOCK_RAW,0);s.bind((0,1));s.setblocking(False)
 return {'pid':__import__('os').getpid(),'port':s.getsockname()[0],'cookie':int.from_bytes(s.getsockopt(socket.SOL_SOCKET,57,8),'little')}
print(json.dumps(opened()),flush=True)
for line in sys.stdin:
 op=line.strip()
 if op=='replace':result=opened()
 elif op=='foreign':
  class Ins(ctypes.Structure):_fields_=[('code',ctypes.c_ushort),('jt',ctypes.c_ubyte),('jf',ctypes.c_ubyte),('k',ctypes.c_uint)]
  class Prog(ctypes.Structure):_fields_=[('len',ctypes.c_ushort),('filter',ctypes.POINTER(Ins))]
  ins=Ins(6,0,0,0xffffffff);p=Prog(1,ctypes.pointer(ins))
  assert libc.setsockopt(s.fileno(),1,26,ctypes.byref(p),ctypes.sizeof(p))==0
  result={'foreign_filter':'pass-all classic'}
 elif op=='read':
  result=[];deadline=time.monotonic()+.2
  while time.monotonic()<deadline:
   if select.select([s],[],[],.02)[0]:result.append(s.recv(65536).hex())
 elif op=='quit':break
 else:raise AssertionError(op)
 print(json.dumps(result),flush=True)
'''
child=subprocess.Popen([sys.executable,'-u','-c',CHILD],stdin=subprocess.PIPE,stdout=subprocess.PIPE,text=True)
first=json.loads(child.stdout.readline());records=[]
tx=socket.socket(socket.AF_NETLINK,socket.SOCK_RAW,0);tx.bind((0,0))
def request(op):
 child.stdin.write(op+'\n');child.stdin.flush();return json.loads(child.stdout.readline())
def message(name):
 value=name.encode()+b'\0';attr=struct.pack('HH',4+len(value),3)+value;attr+=b'\0'*(-len(attr)%4)
 body=struct.pack('BBHiII',0,0,0,0x7f000011,0,0)+attr
 return struct.pack('IHHII',16+len(body),16,0,0,0)+body
owned=message('nnv_1960000_s');foreign=message('ensprobe0')
def proof(label,info,filtered):
 request('read');tx.sendto(owned,(info['port'],0));tx.sendto(foreign,(info['port'],0));rows=request('read')
 assert (owned.hex() in rows)==(not filtered),(label,rows)
 assert foreign.hex() in rows,(label,rows)
 records.append({'phase':label,'recipient':info,'owned_dropped':owned.hex() not in rows,'foreign_delivered':True})
try:
 time.sleep(1.6);proof('new consumer discovered',first,True)
 second=request('replace');assert second['cookie']!=first['cookie']
 time.sleep(1.6);proof('replacement socket discovered',second,True)
 request('foreign');time.sleep(1.6);proof('foreign filter preserved',second,False)
finally:
 child.stdin.write('quit\n');child.stdin.flush();child.wait(timeout=3);tx.close()
open('/tmp/nn-layer1-reconcile-proof.json','w').write(json.dumps({'result':'passed','records':records},indent=2)+'\n')
print(json.dumps({'reconciliation':'passed','phases':len(records)}))
