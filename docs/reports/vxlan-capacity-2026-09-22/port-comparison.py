import socket,struct,time,json,sys,subprocess,resource,statistics,os
U=lambda n:struct.pack('I',n)
def attr(k,v):
 b=struct.pack('HH',len(v)+4,k)+v
 return b+b'\0'*((-len(b))%4)
def message(kind,seq,payload,flags=1|4):
 return struct.pack('IHHII',16+len(payload),kind,flags,seq,0)+payload
IF=lambda:struct.pack('BBHiII',0,0,0,0,1,0xffffffff)
SHARED=False
class NL:
 def __init__(self):
  self.s=socket.socket(socket.AF_NETLINK,socket.SOCK_RAW,0);self.s.bind((0,0));self.s.settimeout(30);self.s.setsockopt(socket.SOL_SOCKET,socket.SO_RCVBUF,4*1024*1024);self.seq=0
 def transact(self,payloads,kind=16):
  pending={};packet=b'';started=time.perf_counter()
  for payload in payloads:
   self.seq+=1;pending[self.seq]=started;packet+=message(kind,self.seq,payload,1|4|(512|1024 if kind==16 else 0))
  self.s.sendto(packet,(0,0));rows=[]
  while pending:
   data=self.s.recv(4*1024*1024);off=0
   while off+16<=len(data):
    size,typ,flags,seq,pid=struct.unpack_from('IHHII',data,off)
    if typ==2 and seq in pending:
     err=struct.unpack_from('i',data,off+16)[0];rows.append((err,(time.perf_counter()-pending.pop(seq))*1000))
    off+=(size+3)&~3
  return rows
 def create(self,ids):
  payload=[]
  for i in ids:
   info=attr(1,U(100000+i))+attr(3,U(2))+attr(4,socket.inet_aton('192.0.2.1'))+attr(2,socket.inet_aton('192.0.2.2'))+attr(15,struct.pack('!H',(4789 if SHARED else 20000+i)))
   payload.append(IF()+attr(3,('vb%06d'%i).encode()+b'\0')+attr(27,U(0x4e500001))+attr(18|32768,attr(1,b'vxlan\0')+attr(2|32768,info)))
  return self.transact(payload)
 def delete(self):return self.transact([IF()+attr(27,U(0x4e500001))],17)
def cpu():
 r=resource.getrusage(resource.RUSAGE_SELF);return r.ru_utime+r.ru_stime
def run(n,batch):
 nl=NL();before=cpu();start=time.perf_counter();rows=[]
 for first in range(0,n,batch):rows+=nl.create(range(first,min(first+batch,n)))
 elapsed=time.perf_counter()-start;used=cpu()-before
 links=json.loads(subprocess.check_output(['ip','-j','-d','link','show','type','vxlan'],text=True))
 assert len(links)==n,(len(links),n,rows[:5])
 ports={x["linkinfo"]["info_data"]["port"] for x in links}
 vnis={x["linkinfo"]["info_data"]["id"] for x in links}
 assert len(vnis)==n and len(ports)==(1 if SHARED else n),(len(vnis),ports)
 startdel=time.perf_counter();deleted=nl.delete();cleanup=time.perf_counter()-startdel
 remaining=json.loads(subprocess.check_output(['ip','-j','link','show','type','vxlan'],text=True));assert not remaining,remaining
 times=sorted(t for e,t in rows);return {'n':n,'batch':batch,'seconds':elapsed,'rate':n/elapsed,'ack_mean_ms':statistics.mean(times),'ack_p99_ms':times[min(len(times)-1,int(len(times)*.99))],'errors':sum(e!=0 for e,t in rows),'delete_seconds':cleanup,'delete_errors':sum(e!=0 for e,t in deleted),'residual':len(remaining),'userspace_cpu_seconds':used}
if __name__=='__main__':
 results=[]
 for repeat in range(3):
  for SHARED in ([False,True] if repeat%2==0 else [True,False]):
   r=run(1000,32);r.update(shared_port=SHARED,repeat=repeat,unique_vnis=1000,unique_interfaces=1000);results.append(r);print(json.dumps(r),flush=True)
   open('/tmp/nullnet-port-comparison.json','w').write(json.dumps(results))
