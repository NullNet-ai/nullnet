"""Shared LAN lab measurement and Internet-health helpers."""
import concurrent.futures,json,re,shlex,subprocess,time
from api import STACK
def root(host,args):
 cmd="printf '%s\\n' debian | sudo -S -p '' "+shlex.join(args)
 return subprocess.check_output(['ssh','-o','BatchMode=yes',f'debian@192.168.1.{host}',cmd],text=True,timeout=90)
def nodes(mode,label):
 with concurrent.futures.ThreadPoolExecutor(2) as pool:
  results=list(pool.map(lambda h:json.loads(root(h,['python3','/tmp/measure-node.py',mode,label])),['103','104']))
 if mode!='before':
  fixture_ids={r['id'] for row in results for r in row['records']['setups'] if r['docker'].startswith('nn-phase1-')}
  foreign_ids={r['id'] for row in results for r in row['records']['setups'] if r['docker']!='none' and not r['docker'].startswith('nn-phase1-')}
  assert not fixture_ids.intersection(foreign_ids),'Recycled ID spans fixture and foreign generations; explicit generation attribution required'
  for row in results:
   row['unscoped_counts']={k:row[k] for k in ['setups','retirements','server_retirements']}
   for key in ['setups','retirements','server_retirements']:
    row['records'][key]=[r for r in row['records'][key] if r['id'] in fixture_ids]
    row[key]=len(row['records'][key])
   row['last_retirement_unix']=max((r['unix'] for r in row['records']['retirements']),default=None)
   row['last_server_retirement_unix']=max((r['unix'] for r in row['records']['server_retirements']),default=None)
   row['remaining_owned_links']=[r for r in row['remaining_owned_links'] if int(re.search(r'\d+',r['name'])[0]) in fixture_ids]
   row['scope']='Phase 1 fixture IDs; full unscoped records preserved on each host'
   del row['records']
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
