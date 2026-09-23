import json,pathlib,subprocess,time,sys,os
ROOT=pathlib.Path('/tmp/nn23');ROOT.mkdir(exist_ok=True)
def run(args,**kw):
 p=subprocess.run(args,text=True,capture_output=True,**kw)
 if p.returncode:raise RuntimeError((args,p.stdout,p.stderr))
 return p.stdout
def snapshot():
 names=run(['docker','ps','-q']).split()
 containers=json.loads(run(['docker','inspect']+names)) if names else []
 return {'containers':{c['Name']:{'pid':c['State']['Pid'],'start':c['State']['StartedAt'],'networks':c['NetworkSettings']['Networks']} for c in containers},'links':json.loads(run(['ip','-j','addr'])),'routes':json.loads(run(['ip','-j','route'])),'units':run(['systemctl','list-units','--state=running','--no-legend','nullnet*'])}
created=[];before=snapshot();(ROOT/'before.json').write_text(json.dumps(before));pilots='pilot' in sys.argv;pooled='pooled' in sys.argv
if pooled:os.environ['NN_POOLED']='1'
current='current' in sys.argv
if current:os.environ['NN_CURRENT']='1'
native='native' in sys.argv
if native:os.environ.update(NN_NATIVE_NS='1',NN_NATIVE_MACSEC='1')
try:
 for i in range(12):
  name=f'nn23-bench-{i}';run(['docker','run','-d','--network','none','--name',name,'--label','nullnet-benchmark=20260923','alpine:latest','sleep','infinity']);created.append(name)
 t=time.monotonic();pids=[c['State']['Pid'] for c in json.loads(run(['docker','inspect']+created))];discovery_seconds=time.monotonic()-t;pathlib.Path('/tmp/nn23-pids.json').write_text(json.dumps(pids))
 cases=[('same_macsec','separate','current'),('same_macsec','batch','current'),('unique_ipsec','batch','current'),('unique_ipsec','batch','exact'),('shared_ipsec','batch','exact'),('vxlan_macsec','batch','exact')]
 if pooled:cases=[('same_macsec','batch','exact'),('shared_ipsec','batch','exact'),('vxlan_macsec','batch','exact')]
 if 'same' in sys.argv:cases=[('same_macsec','batch','exact')]
 if 'marked' in sys.argv:cases=[('marked_ipsec','batch','exact')]
 if current:cases=[('same_macsec','separate','current'),('unique_ipsec','separate','current')]
 sweep='sweep' in sys.argv
 cases=[(*c,int(os.environ.get('NN_WORKERS','32')),int(os.environ.get('NN_DELETE_BATCH','128'))) for c in cases]
 if sweep:
  cases=[(m,'batch','exact',w,1000) for w in [1,4,8,16,32] for m in ['same_macsec','vxlan_macsec']]+[(m,'batch','exact',8,d) for d in [16,64,128,256] for m in ['same_macsec','vxlan_macsec']]
 for repeat in range(1 if pilots or sweep else 3):
  for mode,batch,delete,workers,deletebatch in (cases if repeat%2==0 else list(reversed(cases))):
   os.environ.update(NN_WORKERS=str(workers),NN_DELETE_BATCH=str(deletebatch))
   name=(os.environ.get('NN_LABEL','')+'-' if os.environ.get('NN_LABEL') else '')+('sweep-' if sweep else '')+('current-' if current else '')+('native-' if native else '')+('pooled-' if pooled else '')+f'{mode}-{batch}-{delete}-{repeat}'+(f'-w{workers}-d{deletebatch}' if sweep else '');run(['ip','netns','add','nn23-base'])
   try:
    run(['ip','-n','nn23-base','link','add','dummy0','type','dummy']);run(['ip','-n','nn23-base','addr','add','192.0.2.1/24','dev','dummy0']);run(['ip','-n','nn23-base','link','set','dummy0','up'])
    out=run(['python3','/tmp/nn23/lifecycle.py',mode,'16' if pilots else os.environ.get('NN_N','1000'),batch,delete,str(ROOT/(name+'.json'))]);record=json.loads((ROOT/(name+'.json')).read_text());record['discovery_seconds']=discovery_seconds if not current else 0;(ROOT/(name+'.json')).write_text(json.dumps(record));print(out,flush=True)
   finally:run(['ip','netns','delete','nn23-base'])
finally:
 for name in created:run(['docker','rm','-f',name])
 after=snapshot();(ROOT/'after.json').write_text(json.dumps(after));assert before['containers']==after['containers'],'Original containers changed'
 print('CONTINUITY_OK',flush=True)
