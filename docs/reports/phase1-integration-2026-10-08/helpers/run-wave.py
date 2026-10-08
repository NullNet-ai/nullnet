"""Coordinate a complete C256 product wave and record its drain boundary."""
import argparse,concurrent.futures,json,pathlib,shlex,subprocess,time,urllib.request
from api import api,login,STACK
p=argparse.ArgumentParser();p.add_argument('variant');p.add_argument('trial',type=int);p.add_argument('--concurrency',type=int,default=256);p.add_argument('--count',type=int,default=1008);p.add_argument('--services',default='1,2,3,4,5,6');p.add_argument('--path',default='/');p.add_argument('--identities',type=int,default=1008);p.add_argument('--offset',type=int,default=128);a=p.parse_args();label=f'{a.variant}-c{a.concurrency}-t{a.trial}'+(f'-n{a.count}' if a.count!=1008 else '');out=pathlib.Path(__file__).resolve().parents[1]/'measurements';out.mkdir(exist_ok=True)
from lab import root,nodes,history,internet,internet_details
assert not (out/(label+'.json')).exists(), 'Choose a new trial label; existing evidence must be preserved'
login();
registration_deadline=time.monotonic()+30
while not all(service['registered'] for service in api('/api/services/'+STACK)):
 assert time.monotonic()<registration_deadline,'fixture registration timed out'
 time.sleep(.2)
assert not api('/api/graph/'+STACK)['edges'];before_history=history();before=nodes('before',label);assert internet()
args=['python3','/tmp/http-load.py','--count',str(a.count),'--concurrency',str(a.concurrency),'--identities',str(a.identities),'--offset',str(a.offset),'--services',a.services,'--path',a.path,'--output',f'/tmp/nn-layer1-{label}.json']
process=subprocess.Popen(['ssh','-o','BatchMode=yes','debian@192.168.1.103',shlex.join(args)],stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
checks=[];start=time.monotonic()
while process.poll() is None:
 time.sleep(5);checks.append({'elapsed':time.monotonic()-start,'internet':internet(),'probe':internet_details[-1]});print(json.dumps({'label':label,'running_seconds':round(time.monotonic()-start),'internet':checks[-1]['internet']}),flush=True)
 if not checks[-1]['internet']:
  stop="import os,signal; label="+repr('/tmp/nn-layer1-'+label+'.json')+"; [(os.kill(int(p),signal.SIGINT)) for p in os.listdir('/proc') if p.isdigit() and int(p)!=os.getpid() and label.encode() in open('/proc/'+p+'/cmdline','rb').read().split(bytes([0])) and b'/tmp/http-load.py' in open('/proc/'+p+'/cmdline','rb').read().split(bytes([0]))]"
  root('103',['python3','-c',stop]);process.wait(timeout=10)
  (out/(label+'.aborted.json')).write_text(json.dumps({'label':label,'valid_performance_comparison':False,'exclusion_reason':'Internet-health probe failed; workload stopped','internet_checks':checks,'internet_probe_details':internet_details},indent=2)+'\n')
  raise RuntimeError('Internet probe failed; workload stopped')
stdout,stderr=process.communicate();load=json.loads(stdout.strip().splitlines()[-1]);assert process.returncode==0,(load,stderr)
assert load['errors']==0
deadline=time.monotonic()+150
while True:
 after=nodes('poll',label);graph=api('/api/graph/'+STACK)
 if all(row['setups']==a.count and row['retirements']==a.count and not row['remaining_owned_links'] for row in after.values()) and not graph['edges'] and after['104']['server_retirements']==a.count:break
 assert time.monotonic()<deadline, {'label':label,'nodes':after,'graph':graph}
 time.sleep(.5)
after=nodes('after',label);assert all(not n['lifecycle_errors'] and n['before']['all_client_pid']==n['after']['all_client_pid'] for n in after.values());after_history=history()
history_deadline=time.monotonic()+30
while after_history[0]-before_history[0]!=a.count or after_history[1]!=0:
 assert time.monotonic()<history_deadline,(before_history,after_history)
 time.sleep(.2);after_history=history()
last=max(row['last_retirement_unix'] for row in after.values());kernel_complete=last-load['unix_start'];complete=after['104']['last_server_retirement_unix']-load['unix_start'];result={'label':label,'variant':a.variant,'concurrency':a.concurrency,'count':a.count,'load':load,'nodes':after,'history_before':before_history,'history_after':after_history,'graph':graph,'internet_checks':checks,'completion_boundary':'server ID release after endpoint acknowledgements','complete_kernel_cleanup_seconds':kernel_complete,'complete_acknowledged_seconds':complete,'complete_cycles_per_second':a.count/complete,'drain_observed_unix':time.time(),'conservative_complete_cycles_per_second':a.count/(max(row['after']['unix'] for row in after.values())-load['unix_start'])}
(out/(label+'.json')).write_text(json.dumps(result,indent=2)+'\n');print(json.dumps({'label':label,'requests_per_second':load['requests_per_second'],'complete_cycles_per_second':result['complete_cycles_per_second'],'conservative_complete_cycles_per_second':result['conservative_complete_cycles_per_second'],'errors':load['errors'],'history_closed':a.count,'final_edges':len(graph['edges'])}),flush=True)
