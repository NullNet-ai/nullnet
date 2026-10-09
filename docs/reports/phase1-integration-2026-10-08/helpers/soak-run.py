"""Run one fixed-rate fresh-only pilot/soak, supervise resources and audit final drain."""
import argparse,concurrent.futures,json,pathlib,subprocess,time
from api import api,login,wait_registered,STACK
from lab import root,history,internet,internet_details
p=argparse.ArgumentParser();p.add_argument('label');p.add_argument('--rate',type=float,required=True);p.add_argument('--duration',type=float,required=True);p.add_argument('--timeout',type=int,default=60);p.add_argument('--mixed',action='store_true');p.add_argument('--diagnostic',action='store_true');p.add_argument('--tls-ca',default='/tmp/nn-soak-ca.pem');a=p.parse_args()
base=pathlib.Path(__file__).resolve().parents[1];destination=base/'measurements'/(a.label+'.json');assert not destination.exists()
def wan_health():
 code="""import concurrent.futures,json,subprocess,time
def probe(url):
 r=subprocess.run(['curl','-4','-sI','--connect-timeout','3','--max-time','5','-o','/dev/null','-w','%{http_code}',url],capture_output=True,text=True)
 return {'url':url,'returncode':r.returncode,'status':r.stdout,'ok':r.returncode==0 and r.stdout.isdigit() and 200<=int(r.stdout)<500}
with concurrent.futures.ThreadPoolExecutor(2) as pool:rows=list(pool.map(probe,['https://www.apple.com','https://www.google.com']))
print(json.dumps({'unix':time.time(),'origin':'lab103','ip_family':'IPv4','probes':rows,'ok':any(r['ok'] for r in rows)}))
"""
 return json.loads(root('103',['python3','-c',code]))
login();wait_registered();assert not api('/api/graph/'+STACK)['edges'];assert wan_health()['ok']
services=[{'name':f'p1-{i:02d}.test','docker_container':f'nn-phase1-{i:02d}','host_ip':'192.168.1.103' if i<=6 else '192.168.1.104','port':9000+i,'timeout':a.timeout,'pausable':False} for i in range(1,13)]
mixed=None
if a.mixed:
 import importlib.util
 spec=importlib.util.spec_from_file_location('mixed',pathlib.Path(__file__).with_name('mixed-traffic.py'));mixed=importlib.util.module_from_spec(spec);spec.loader.exec_module(mixed)
 for row in services:
  n=int(row['name'][3:5])
  if n in mixed.pairs:row['backends']=[f'p1-{mixed.pairs[n]:02d}.test']
 mixed.names(True)
api('/api/service-config/'+STACK,{'services':services});wait_registered()
before=history();node_unit='nn-soak-watch-'+a.label;client_unit='nn-soak-client-'+a.label;stop_path='/tmp/'+a.label+'.stop';client_output='/tmp/'+a.label+'.jsonl'
initial={};snapshots=[];stop_reason=None;last_print=0;next_internet=0;final=None;started=[];client_started=False;wan_failures=0;mixed_started=False;mixed_results=[]
program="""import pathlib,json
p=pathlib.Path(PATH)
print(p.read_text() if p.exists() else 'null')
"""
def read_json(host,path):return json.loads(root(host,['python3','-c',program.replace('PATH',repr(path))]))
def stop_load(reason):
 global stop_reason
 stop_reason=stop_reason or reason;root('103',['touch',stop_path])
try:
 for h in ['103','104']:
  initial[h]=root(h,['systemctl','show','nullnet-client','--property=ExecStart','--value']);assert '/phase1-fresh-egress/' in initial[h],initial[h]
  root(h,['systemd-run','--unit='+node_unit,'--collect','--property=RuntimeMaxSec='+str(int(a.duration+300)), '/usr/bin/python3','/tmp/soak-node.py','--label',a.label,'--duration',str(a.duration+280),'--stop-file',stop_path]);started.append(h)
 ready_deadline=time.monotonic()+20
 while True:
  initialized=[read_json(h,'/root/nullnet-layer1-20261008/evidence/'+a.label+'/status.json') for h in ['103','104']]
  if all(initialized):
   assert all(n['owned_root_links']==0 and n['counts']['setups']+n['counts'].get('setup_failures',sum('setup FAILED' in f.get('message','') for f in n['failures']))==n['counts']['retirements']==0 for n in initialized),initialized
   break
  assert time.monotonic()<ready_deadline,'observers did not initialize';time.sleep(.5)
 if mixed:
  seed_code="import asyncio,importlib.util,ssl,json;spec=importlib.util.spec_from_file_location('transport','/tmp/http-load.py');t=importlib.util.module_from_spec(spec);spec.loader.exec_module(t);context=ssl.create_default_context(cafile='/tmp/nn-soak-ca.pem')\nasync def run():\n rows=await asyncio.gather(*(t.http_request(n,'198.19.255.'+str(n),n,tls_context=context) for n in range(1,13)));print(json.dumps(rows));assert all(r['ok'] for r in rows),rows\nasyncio.run(run())"
  seed=json.loads(root('103',['python3','-c',seed_code]))
  mixed_started=True;mixed.start(a.label,a.duration)
 root('103',['systemd-run','--unit='+client_unit,'--uid=debian','--collect','--property=LimitNOFILE=524288','--property=RuntimeMaxSec='+str(int(a.duration+150)), '/usr/bin/python3','/tmp/soak-client.py','--rate',str(a.rate),'--duration',str(a.duration),'--output',client_output,'--stop-file',stop_path,'--tls-ca',a.tls_ca]+(['--diagnostic'] if a.diagnostic else []));client_started=True
 start=time.monotonic();deadline=start+a.duration+260
 while True:
  with concurrent.futures.ThreadPoolExecutor(2) as pool:
   node_rows=dict(zip(['103','104'],pool.map(lambda h:read_json(h,'/root/nullnet-layer1-20261008/evidence/'+a.label+'/status.json'),['103','104'])))
  workload=read_json('103',client_output.replace('.jsonl','.status.json'));final=read_json('103',client_output.replace('.jsonl','.summary.json'))
  now=time.monotonic();row={'unix':time.time(),'elapsed':now-start,'nodes':node_rows,'workload':workload};snapshots.append(row)
  for h,node in node_rows.items():
   if node and node['stop_reason']:stop_load(h+': '+node['stop_reason'])
  if workload and workload['stop_reason']:stop_reason=stop_reason or workload['stop_reason']
  if now>=next_internet:
   probe=wan_health();row['internet_probe']=probe
   wan_failures=0 if probe['ok'] else wan_failures+1;next_internet=now+(60 if probe['ok'] else 10)
   if wan_failures>=3:stop_load('WAN probes failed three consecutive checks from the lab host')
  if now-last_print>=60:
   print(json.dumps({'label':a.label,'elapsed':round(now-start),'client':workload,'nodes':{h:({'backlog':n['completed_not_retired'],'links':n['owned_root_links'],'client_fds':n['resources']['nullnet-client']['fds'],'disk_free_bytes':n['disk_free_bytes']} if n else None) for h,n in node_rows.items()}}),flush=True);last_print=now
  if final:break
  if now>deadline:stop_load('workload deadline exceeded');raise RuntimeError('workload did not finish')
  time.sleep(10)
 if final['stop_reason']:stop_reason=stop_reason or final['stop_reason']
 if mixed_started:
  mixed_results=mixed.stop(a.label,base/'measurements');mixed_started=False
 drain_deadline=time.monotonic()+a.timeout+150
 while True:
  graph=api('/api/graph/'+STACK)
  with concurrent.futures.ThreadPoolExecutor(2) as pool:
   last=dict(zip(['103','104'],pool.map(lambda h:read_json(h,'/root/nullnet-layer1-20261008/evidence/'+a.label+'/status.json'),['103','104'])))
  if not graph['edges'] and all(n and n['owned_root_links']==0 and n['counts']['setups']+n['counts'].get('setup_failures',sum('setup FAILED' in f.get('message','') for f in n['failures']))==n['counts']['retirements'] for n in last.values()):break
  assert time.monotonic()<drain_deadline,('cleanup did not finish',last,graph)
  time.sleep(5)
 after=history();assert after[1]==0,after
 result={'label':a.label,'offered_clients_per_second':a.rate,'arrival_duration_seconds':a.duration,'service_idle_timeout_seconds':a.timeout,'workload':final,'node_final':last,'history_before':before,'history_after':after,'graph_after':graph,'snapshots':snapshots,'diagnostic':a.diagnostic,'status':('degraded' if final['counters'].get('latency_exceedances',0) else 'completed') if stop_reason is None else 'stopped','stop_reason':stop_reason,'runtime':initial,'mixed_traffic':mixed_results,'scope':'single-hop trusted HTTPS ingress across 12 running fixtures; alternating same-host/cross-host; encrypted fresh lifecycle; persistent HTTPS sessions; 75% one request,20%20 requests over10s,5%240 requests over120s; Poisson arrivals; 65520 source identities; 1% 1MiB responses and 99%8-byte responses; response-limited sessions skip overdue half-second slots while new-client arrivals remain open-loop; no endpoint retention'}
 destination.write_text(json.dumps(result,indent=2)+'\n')
 if stop_reason is None:
  if mixed:
   assert len(mixed_results)==12 and all(r['samples'] and r['samples'][-1].get('final') and r['samples'][-1]['errors']==0 and r['samples'][-1]['requests']>0 for r in mixed_results),mixed_results
  assert final['counters']['errors']==0
  assert final['counters']['clients_started']==final['counters']['clients_finished']
  assert after[0]-before[0]>=final['counters']['clients_started'],(before,after,final)
  assert last['104']['counts']['server_releases']==after[0]-before[0],(last,after,before)
 destination.write_text(json.dumps(result,indent=2)+'\n');print(json.dumps({'label':a.label,'status':result['status'],'stop_reason':stop_reason,'clients':final['counters']['clients_started'],'requests':final['counters']['requests'],'errors':final['counters']['errors'],'closed_sessions':after[0]-before[0],'final_edges':len(graph['edges'])}),flush=True)
finally:
 if mixed_started:mixed.stop(a.label,base/'measurements')
 if client_started:
  try:root('103',['systemctl','stop',client_unit])
  except subprocess.CalledProcessError:pass
 for h in started:
  try:root(h,['systemctl','stop',node_unit])
  except subprocess.CalledProcessError:pass
  path='/root/nullnet-layer1-20261008/evidence/'+a.label+'/samples.jsonl'
  try:(base/'measurements'/(a.label+'-'+h+'-samples.jsonl')).write_text(root(h,['cat',path]))
  except subprocess.CalledProcessError:pass
if stop_reason or final['counters'].get('latency_exceedances',0):raise SystemExit(1)
