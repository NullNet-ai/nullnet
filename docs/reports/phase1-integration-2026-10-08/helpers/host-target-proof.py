"""Verify encrypted ingress to ordinary host listeners on both sides of the proxy."""
import json,pathlib,subprocess,time,ssl,urllib.request
from api import api,login
from lab import root,internet
label=__import__('sys').argv[1] if len(__import__('sys').argv)>1 else 'phase1-oct9-host-targets';stack='nn-phase1-host-targets';out=pathlib.Path(__file__).resolve().parents[1]/'measurements'/(label+'.json')
assert not out.exists()
login();assert internet();started=[];results=[]
try:
 rows=[]
 for h in ['103','104']:
  executable=root(h,['readlink','-f','/usr/bin/python3']).strip()
  root(h,['systemd-run','--unit=nn-phase1-host-proof','--collect','/usr/bin/python3','-m','http.server','19123','--bind','0.0.0.0','--directory','/root/nullnet-layer1-20261008/fixture-web']);started.append(h)
  rows.append({'name':'p1-host-'+h+'.test','process_path':executable,'host_ip':'192.168.1.'+h,'port':19123,'timeout':1,'pausable':False})
 api('/api/service-config/'+stack,{'services':rows})
 deadline=time.monotonic()+40
 while True:
  state=api('/api/services/'+stack)
  if len(state)==2 and all(s['registered'] for s in state):break
  assert time.monotonic()<deadline,state;time.sleep(.2)
 for h in ['103','104']:
  program="import urllib.request,json; r=urllib.request.urlopen(urllib.request.Request('http://192.168.1.104/payload.bin',headers={'Host':'p1-host-"+h+".test'}),timeout=20); body=r.read(); assert body==bytes(range(256))*4096; print(json.dumps({'status':r.status,'bytes':len(body)}))"
  result=json.loads(root('103',['python3','-c',program]));result.update({'target_host':h,'placement':'same-host' if h=='104' else 'cross-host'});results.append(result)
 deadline=time.monotonic()+30
 while api('/api/graph/'+stack)['edges']:
  assert time.monotonic()<deadline;time.sleep(.2)
 assert internet();out.write_text(json.dumps({'label':label,'result':'passed','cases':results,'graph':api('/api/graph/'+stack)},indent=2)+'\n');print(json.dumps({'label':label,'cases':len(results),'result':'passed'}))
finally:
 api('/api/service-config/'+stack,{'services':[]})
 for h in started:root(h,['systemctl','stop','nn-phase1-host-proof'])
