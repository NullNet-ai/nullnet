import re,subprocess,os,pathlib,json,random,resource,time
prefix='nn-bare-docker-';created=[];results=[]
VARIANT={}
def bench(w,s,n,phase,repeat=0):
 env=dict(os.environ,NN_DOCKER_PREFIX=prefix,NN_FORWARD_ONCE='1',NN_PID_CACHE='1',NN_SLOTS=str(s),**VARIANT)
 before=resource.getrusage(resource.RUSAGE_CHILDREN)
 subprocess.run(['bash','/tmp/nullnet-direct-run.sh',str(n),str(w),'nullnet-equivalence-direct.py'],env=env,check=True,stdout=subprocess.DEVNULL)
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
 results.append(r);pathlib.Path('/tmp/nn-equivalence-results.json').write_text(json.dumps(results))
 print(phase,w,s,n,repeat,round(r['rate'],2),flush=True)
try:
 for i in range(12):
  name=prefix+str(i);subprocess.run(['docker','run','-d','--network','none','--name',name,'--label','nullnet-bare-benchmark=20260922','alpine:latest','sleep','infinity'],check=True,stdout=subprocess.DEVNULL);created.append(name)
 original={c['Name']:(c['State']['Pid'],c['State']['StartedAt']) for c in json.loads(subprocess.check_output(['docker','inspect']+created,text=True))}
 for label,flags in [('baseline',{}),('optimized',{'NN_SYSCTL_ONCE':'1','NN_HASH_INLINE':'1','NN_NS_BATCH':'1','NN_XFRM_BATCH':'1'})]:
  VARIANT=dict(flags,NN_FINGERPRINT='1');bench(8,8,16,label)
  pathlib.Path('/tmp/nn-config-'+label+'.json').write_text(pathlib.Path('/tmp/nn-config.json').read_text())
 a=json.loads(pathlib.Path('/tmp/nn-config-baseline.json').read_text())
 b=json.loads(pathlib.Path('/tmp/nn-config-optimized.json').read_text())
 def normalize(config):
  for key in ['state','policy']:
   raw=re.sub(r'(?m)^\s*(?:lastused |anti-replay context:).*$\n?', '',config[key])
   config[key]=sorted('src '+part for part in re.split(r'(?m)^src ',raw)[1:])
  collections=[config['host_links'],config['host_addresses']]+list(config['containers'].values())
  for links in collections:
   links.sort(key=lambda x:x['ifname'])
   for link in links:
    if 'flags' in link:link['flags']=sorted(f for f in link['flags'] if f not in ['LOWER_UP','NO-CARRIER'])
    if 'linkinfo' in link:
     info=link['linkinfo'];data=info.get('info_data',{})
     keep=['id','remote','local','port','learning','udpcsum','forward_delay','hello_time','max_age','ageing_time','stp_state','priority','vlan_filtering','vlan_protocol']
     link['linkinfo']={'info_kind':info['info_kind'],'info_data':{k:v for k,v in data.items() if k in keep}}
  return config
 a=normalize(a);b=normalize(b)
 pathlib.Path('/tmp/nn-config-baseline-normalized.json').write_text(json.dumps(a,sort_keys=True))
 pathlib.Path('/tmp/nn-config-optimized-normalized.json').write_text(json.dumps(b,sort_keys=True))
 assert a==b,'Normalized kernel configurations differ; inspect snapshots'
 print('CONFIGURATIONS_IDENTICAL',flush=True)
 VARIANT={'NN_SYSCTL_ONCE':'1','NN_HASH_INLINE':'1','NN_NS_BATCH':'1','NN_XFRM_BATCH':'1'}
 for repeat in range(2):
  for workers in ([8,32] if repeat==0 else [32,8]):bench(workers,8,1000,'optimized-confirm' if workers==8 else 'optimized-control',repeat)


finally:
 for name in created:subprocess.run(['docker','rm','-f',name],check=True,stdout=subprocess.DEVNULL)
print('EQUIVALENCE_COMPLETE',flush=True)
