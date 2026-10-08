"""Shared LAN lab measurement and Internet-health helpers."""
import concurrent.futures,json,shlex,subprocess,time
from api import STACK
def root(host,args):
 cmd="printf '%s\\n' debian | sudo -S -p '' "+shlex.join(args)
 return subprocess.check_output(['ssh','-o','BatchMode=yes',f'debian@192.168.1.{host}',cmd],text=True,timeout=90)
def nodes(mode,label):
 with concurrent.futures.ThreadPoolExecutor(2) as pool:
  results=list(pool.map(lambda h:json.loads(root(h,['python3','/tmp/measure-node.py',mode,label])),['103','104']))
 return dict(zip(['103','104'],results))
def history():
 program="import sqlite3,json; db=sqlite3.connect('file:/root/nullnet-layer1-20261008/runtime.db?mode=ro',uri=True); print(json.dumps(db.execute(\"select count(*),sum(ended_at is null),sum(ended_at is not null) from sessions where stack='nn-phase1'\").fetchone()))"
 return json.loads(root('104',['python3','-c',program]))
internet_details=[]
def internet():
 def probe(url):
  result=subprocess.run(['curl','-sI','--connect-timeout','3','--max-time','5','-o','/dev/null','-w','%{http_code}',url],capture_output=True,text=True)
  return {'url':url,'returncode':result.returncode,'status':result.stdout,'stderr':result.stderr,'ok':result.returncode==0 and result.stdout=='200'}
 with concurrent.futures.ThreadPoolExecutor(2) as pool:probes=list(pool.map(probe,['https://www.apple.com','https://www.google.com']))
 good=any(p['ok'] for p in probes)
 internet_details.append({'unix':time.time(),'probes':probes,'ok':good})
 return good
