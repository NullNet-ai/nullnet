"""Check live-edge recovery and preserve Docker processes, links and routes."""
import argparse,json,pathlib,time
from api import api,login,STACK
from lab import root,internet

p=argparse.ArgumentParser();p.add_argument('variant');a=p.parse_args()
out=pathlib.Path(__file__).resolve().parents[1]/'measurements';label=a.variant+'-restart'
assert not (out/(label+'.json')).exists()
snapshot_code="""import json,re,subprocess
def run(*args):return json.loads(subprocess.check_output(args))
containers=run('docker','inspect',*subprocess.check_output(['docker','ps','-q'],text=True).split())
links=run('ip','-j','link');routes=run('ip','-j','route','show','table','main')
owned={l['ifname'] for l in links if re.match(r'^(br_\\d+_[sc]|nnv_\\d+_[sc]|vxlan-ns_\\d+_[sc]|ns_\\d+_[sc]-(out|o)|veth-\\d+-[sc]|macsec-\\d+-?[sc])$',l['ifname'])}
links=[l for l in links if l['ifname'] not in owned];routes=[r for r in routes if r.get('dev') not in owned]
print(json.dumps({'containers':{c['Id']:{'name':c['Name'],'pid':c['State']['Pid']} for c in containers},'links':{l['ifname']:l['ifindex'] for l in links},'routes':routes}))
"""
def snapshot():return {h:json.loads(root(h,['python3','-c',snapshot_code])) for h in ['103','104']}
def configure(timeout):
 rows=[{'name':f'p1-{i:02d}.test','docker_container':f'nn-phase1-{i:02d}','host_ip':'192.168.1.103' if i<=6 else '192.168.1.104','port':9000+i,'timeout':timeout,'pausable':False} for i in range(1,13)]
 api('/api/service-config/'+STACK,{'services':rows})
def request(suffix):
 r=json.loads(root('103',['python3','/tmp/http-load.py','--tls-ca','/tmp/nn-soak-ca.pem','--count','12','--concurrency','1','--identities','12','--services','1,7,2,8,3,9,4,10,5,11,6,12','--offset','128','--output','/tmp/nn-layer1-'+label+'-'+suffix+'.json']))
 assert r['errors']==0,r
 return r
def registered():
 deadline=time.monotonic()+60
 while True:
  try:
   services=api('/api/services/'+STACK)
   if len(services)==12 and all(s['registered'] for s in services):return
  except Exception:pass
  assert time.monotonic()<deadline,'registration did not recover'
  time.sleep(.5)
login();assert not api('/api/graph/'+STACK)['edges'];assert internet();before=snapshot();results=[]
(out/(label+'-before.json')).write_text(json.dumps(before,indent=2)+'\n')
try:
 configure(0);request('seed')
 for host,unit in [('103','nullnet-client'),('104','nullnet-client'),('104','nullnet-server')]:
  graph_before=api('/api/graph/'+STACK);assert graph_before['edges']
  root(host,['systemctl','restart',unit]);registered();login()
  result=request(host+'-'+unit);assert internet()
  current=snapshot()
  assert all(current[h]['containers']==before[h]['containers'] for h in before),'application containers changed'
  assert all(all(current[h]['links'].get(n)==index for n,index in before[h]['links'].items()) for h in before),'pre-existing link changed'
  results.append({'host':host,'unit':unit,'load':result,'graph_before':graph_before,'graph_after':api('/api/graph/'+STACK)})
finally:configure(1)
deadline=time.monotonic()+90
while api('/api/graph/'+STACK)['edges']:
 assert time.monotonic()<deadline,'final graph did not drain';time.sleep(.5)
after=snapshot();assert after==before,(before,after)
(out/(label+'.json')).write_text(json.dumps({'label':label,'before':before,'after':after,'restarts':results},indent=2)+'\n')
print(json.dumps({'label':label,'restarts':len(results),'application_pids_preserved':True,'links_routes_preserved':True}))
