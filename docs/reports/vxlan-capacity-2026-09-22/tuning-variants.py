import subprocess,os,pathlib,json,random,resource,time
prefix='nn-bare-docker-';created=[];results=[]
VARIANT={}
def bench(w,s,n,phase,repeat=0):
 env=dict(os.environ,NN_DOCKER_PREFIX=prefix,NN_FORWARD_ONCE='1',NN_PID_CACHE='1',NN_SLOTS=str(s),**VARIANT)
 before=resource.getrusage(resource.RUSAGE_CHILDREN)
 subprocess.run(['bash','/tmp/nullnet-direct-run.sh',str(n),str(w),'nullnet-variant-direct.py'],env=env,check=True,stdout=subprocess.DEVNULL)
 after=resource.getrusage(resource.RUSAGE_CHILDREN)
 r=json.loads(pathlib.Path('/tmp/nn-direct-result.json').read_text())
 assert r['success']==n and not r['errors'],r['errors'][:3]
 for k,v in [('container_veth_peers',n),('vxlan',n),('bridges',n),('xfrm_states',2*n),('xfrm_policies',2*n)]:assert r['inventory'][k]==v,(k,r['inventory'])
 current=json.loads(subprocess.check_output(['docker','inspect']+created,text=True))
 assert {c['Name']:(c['State']['Pid'],c['State']['StartedAt']) for c in current}==original
 for pid,_ in original.values():
  links=json.loads(subprocess.check_output(['nsenter','-t',str(pid),'-n','ip','-j','link','show'],text=True))
  assert not any(x['ifname'].startswith('vd') for x in links)
 r.update(phase=phase,repeat=repeat,rate=n/r['seconds'],cleanup_verified=True,container_continuity_verified=True,child_cpu_including_cleanup=after.ru_utime+after.ru_stime-before.ru_utime-before.ru_stime)
 for field in ['seconds','completed_seconds']:
  vals=sorted(e[field] for e in r['endpoints']);r[field+'_p99']=vals[int(.99*(len(vals)-1))]
 results.append(r);pathlib.Path('/tmp/nn-variant-results.json').write_text(json.dumps(results))
 print(phase,w,s,n,repeat,round(r['rate'],2),flush=True)
try:
 for i in range(12):
  name=prefix+str(i);subprocess.run(['docker','run','-d','--network','none','--name',name,'--label','nullnet-bare-benchmark=20260922','alpine:latest','sleep','infinity'],check=True,stdout=subprocess.DEVNULL);created.append(name)
 original={c['Name']:(c['State']['Pid'],c['State']['StartedAt']) for c in json.loads(subprocess.check_output(['docker','inspect']+created,text=True))}
 variants=[('baseline',{}),('sysctl-once',{'NN_SYSCTL_ONCE':'1'}),('inline-hash',{'NN_SYSCTL_ONCE':'1','NN_HASH_INLINE':'1'}),('namespace-batch',{'NN_SYSCTL_ONCE':'1','NN_HASH_INLINE':'1','NN_NS_BATCH':'1'}),('xfrm-batch',{'NN_SYSCTL_ONCE':'1','NN_HASH_INLINE':'1','NN_NS_BATCH':'1','NN_XFRM_BATCH':'1'})]
 for repeat in range(2):
  order=list(variants);random.Random(200+repeat).shuffle(order)
  for label,flags in order:
   VARIANT=flags;bench(32,8,1000,label,repeat)

finally:
 for name in created:subprocess.run(['docker','rm','-f',name],check=True,stdout=subprocess.DEVNULL)
print('VARIANTS_COMPLETE',flush=True)
