import sys,time,json,threading
sys.path.insert(0,'/tmp')
from nullnet_capacity_bare import NL,attr,U,IF
# Use a second group so deleting the previous cohort cannot touch new interfaces.
class NextNL(NL):
 def transact(self,payloads,kind=16):
  if kind==16:payloads=[p.replace(attr(27,U(0x4e500001)),attr(27,U(0x4e500002))) for p in payloads]
  return super().transact(payloads,kind)
a=NL()
for i in range(0,1000,128):assert all(e==0 for e,t in a.create(range(i,min(i+128,1000))))
result={};started=threading.Event()
def retire():
 n=NL();started.set();t=time.perf_counter();r=n.delete();result['delete_seconds']=time.perf_counter()-t;result['delete_errors']=sum(e!=0 for e,v in r)
worker=threading.Thread(target=retire);worker.start();started.wait();time.sleep(.01)
b=NextNL();begin=time.perf_counter();rows=[]
for i in range(1000,2000,128):rows+=b.create(range(i,min(i+128,2000)))
result.update(create_seconds=time.perf_counter()-begin,create_errors=sum(e!=0 for e,t in rows));result['create_per_second']=1000/result['create_seconds'];worker.join()
b.transact([IF()+attr(27,U(0x4e500002))],17)
result['remaining']=json.loads(__import__('subprocess').check_output(['ip','-j','link','show','type','vxlan'],text=True));print(json.dumps(result),flush=True);open('/tmp/nullnet-capacity-overlap.json','w').write(json.dumps(result))
