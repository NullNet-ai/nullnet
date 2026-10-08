"""Measure encrypted reused edges, then require acknowledged final retirement."""
import argparse,json,pathlib,subprocess,time
from api import api,login,STACK
from lab import root,nodes,history,internet

p=argparse.ArgumentParser();p.add_argument('variant');p.add_argument('--trials',type=int,default=3);a=p.parse_args()
out=pathlib.Path(__file__).resolve().parents[1]/'measurements'
label=a.variant+'-warm-c256';assert not (out/(label+'.json')).exists()
def configure(timeout):
 services=[{'name':f'p1-{i:02d}.test','docker_container':f'nn-phase1-{i:02d}','host_ip':'192.168.1.103' if i<=6 else '192.168.1.104','port':9000+i,'timeout':timeout,'pausable':False} for i in range(1,13)]
 api('/api/service-config/'+STACK,{'services':services})
def load(count,suffix):
 result=json.loads(root('103',['python3','/tmp/http-load.py','--count',str(count),'--concurrency','256','--identities','252','--offset','128','--output','/tmp/nn-layer1-'+label+'-'+suffix+'.json']))
 assert result['errors']==0,result
 assert internet(),'Internet-health probe failed'
 return result
login();assert not api('/api/graph/'+STACK)['edges'];assert internet()
before_history=history();nodes('before',label);trials=[]
try:
 configure(0);seed=load(252,'seed');seed_nodes=nodes('poll',label)
 assert all(n['setups']==252 and n['retirements']==0 for n in seed_nodes.values())
 for trial in range(1,a.trials+1):
  trials.append(load(10080,str(trial)))
  current=nodes('poll',label)
  assert all(n['setups']==252 and n['retirements']==0 and not n['lifecycle_errors'] for n in current.values()),current
  print(json.dumps({'label':label,'trial':trial,'requests_per_second':trials[-1]['requests_per_second'],'errors':0}),flush=True)
finally:configure(1)
deadline=time.monotonic()+90
while True:
 current=nodes('poll',label);graph=api('/api/graph/'+STACK)
 if all(n['setups']==252 and n['retirements']==252 and not n['remaining_owned_links'] for n in current.values()) and current['104']['server_retirements']==252 and not graph['edges']:break
 assert time.monotonic()<deadline,current
 time.sleep(.5)
after=nodes('after',label);after_history=history()
assert after_history[0]-before_history[0]==252 and after_history[1]==0
assert all(n['before']['all_client_pid']==n['after']['all_client_pid'] for n in after.values())
(out/(label+'.json')).write_text(json.dumps({'label':label,'seed':seed,'trials':trials,'nodes':after,'history_before':before_history,'history_after':after_history,'graph':graph},indent=2)+'\n')
