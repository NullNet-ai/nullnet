"""Exercise the twelve-service cross-host tree through proxy chains or backend triggers."""
import argparse,json,pathlib,time
from api import api,login,STACK
from lab import root,nodes,internet

p=argparse.ArgumentParser();p.add_argument('variant');p.add_argument('mode',choices=['proxy','backend']);a=p.parse_args()
label=a.variant+'-dependencies-'+a.mode;out=pathlib.Path(__file__).resolve().parents[1]/'measurements'
assert not (out/(label+'.json')).exists()
tree={1:[7,2],7:[3,8],2:[9,4],3:[10],8:[5],9:[11],4:[12,6]}
def services(complex):
 rows=[{'name':f'p1-{i:02d}.test','docker_container':f'nn-phase1-{i:02d}','host_ip':'192.168.1.103' if i<=6 else '192.168.1.104','port':9000+i,'timeout':1,'pausable':False} for i in range(1,13)]
 if complex and a.mode=='proxy':rows[0]['proxy_dependencies']=[[f'p1-{n:02d}.test' for n in branch] for branch in [[7,3,10],[7,8,5],[2,9,11],[2,4,12],[2,4,6]]]
 if complex and a.mode=='backend':
  for row in rows:row['backends']=[f'p1-{n:02d}.test' for n in tree.get(int(row['name'][3:5]),[])]
 return rows
def children(complex):
 contents={f'{n:02d}':''.join(f'p1-{c:02d}.test:{9000+c}\n' for c in tree.get(n,[])) if complex else '' for n in range(1,13)}
 code="import pathlib,json; data=json.loads("+repr(json.dumps(contents))+"); base=pathlib.Path('/root/nullnet-layer1-20261008/fixture-web/children'); [(base.joinpath(name).write_text(value)) for name,value in data.items()]"
 for h in ['103','104']:root(h,['python3','-c',code])
login();assert not api('/api/graph/'+STACK)['edges'];assert internet();nodes('before',label)
try:
 children(True);api('/api/service-config/'+STACK,{'services':services(True)})
 result=json.loads(root('103',['python3','/tmp/http-load.py','--count','64','--concurrency','16','--identities','64','--offset','128','--services','1','--path','/tree-all','--output','/tmp/nn-layer1-'+label+'.json']))
 assert result['errors']==0,result
 graph_during=api('/api/graph/'+STACK);assert internet()
finally:
 children(False);api('/api/service-config/'+STACK,{'services':services(False)})
deadline=time.monotonic()+90
while True:
 state=nodes('poll',label);graph=api('/api/graph/'+STACK)
 if not graph['edges'] and all(not n['remaining_owned_links'] for n in state.values()):break
 assert time.monotonic()<deadline,{'graph':graph,'nodes':state}
 time.sleep(.5)
state=nodes('after',label);assert all(not n['lifecycle_errors'] for n in state.values()),state
(out/(label+'.json')).write_text(json.dumps({'label':label,'load':result,'graph_during':graph_during,'graph_after':graph,'nodes':state},indent=2)+'\n')
print(json.dumps({'label':label,'requests_per_second':result['requests_per_second'],'errors':0,'final_edges':len(graph['edges'])}))
