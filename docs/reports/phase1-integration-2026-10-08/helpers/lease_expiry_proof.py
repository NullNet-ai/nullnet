"""Verify that installed compiled filters pass events when the controller stops renewing."""
import json,os,signal,socket,select,struct,subprocess,time
from compiled_filters import pinned,update
pid=int(subprocess.check_output(['systemctl','show','nullnet-client','--property=MainPID','--value']))
rx=socket.socket(socket.AF_NETLINK,socket.SOCK_RAW,2);rx.bind((0,0))
rx.setsockopt(socket.SOL_SOCKET,50,struct.pack('I',pinned('nullnet_event_route')))
update(pinned('EVENT_SOCKETS'),rx.getsockopt(socket.SOL_SOCKET,57,8),struct.pack('I',rx.getsockname()[0]))
tx=socket.socket(socket.AF_NETLINK,socket.SOCK_RAW,2);tx.bind((0,0))
name=b'nnv_1960000_s\0';a=struct.pack('HH',4+len(name),3)+name;a+=b'\0'*(-len(a)%4)
body=struct.pack('BBHiII',0,0,0,0x7f000022,0,0)+a
packet=struct.pack('IHHII',16+len(body),16,0,0,0)+body
def delivered():
 tx.sendto(packet,(rx.getsockname()[0],0))
 if select.select([rx],[],[],.15)[0]:assert rx.recv(8192)==packet;return True
 return False
records=[]
assert not delivered();records.append({'phase':'active controller','delivered':False})
try:
 os.kill(pid,signal.SIGSTOP);time.sleep(3.5)
 assert delivered();records.append({'phase':'expired lease, controller stopped','delivered':True})
finally:os.kill(pid,signal.SIGCONT)
time.sleep(1.3);assert not delivered();records.append({'phase':'renewal resumed','delivered':False})
result={'result':'passed','records':records}
open('/tmp/nn-layer1-lease-expiry-proof.json','w').write(json.dumps(result,indent=2)+'\n')
print(json.dumps(result))
