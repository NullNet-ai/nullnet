import subprocess,os,pathlib,json
prefix='nn-bare-docker-';created=[]
try:
 for i in range(12):
  name=prefix+str(i)
  subprocess.run(['docker','run','-d','--network','none','--name',name,'--label','nullnet-bare-benchmark=20260922','alpine:latest','sleep','infinity'],check=True,stdout=subprocess.DEVNULL);created.append(name)
 before=json.loads(subprocess.check_output(['docker','inspect']+created,text=True));original={c['Name']:(c['State']['Pid'],c['State']['StartedAt']) for c in before}
 for label,once,cached in [('full',False,False),('no-iptables',True,False),('no-iptables-no-pid',True,True)]:
  env=dict(os.environ,NN_DOCKER_PREFIX=prefix,NN_FORWARD_ONCE='1' if once else '0',NN_PID_CACHE='1' if cached else '0')
  subprocess.run(['bash','/tmp/nullnet-direct-run.sh','1000','32'],env=env,check=True)
  r=json.loads(pathlib.Path('/tmp/nn-direct-result.json').read_text())
  assert r['success']==1000 and not r['errors'],r['errors'][:3]
  assert r['inventory']['container_veth_peers']==1000 and r['inventory']['docker_pid_lookups']==(0 if cached else 1000),r['inventory']
  for k,v in [('vxlan',1000),('bridges',1000),('xfrm_states',2000),('xfrm_policies',2000)]:assert r['inventory'][k]==v,(k,r['inventory'])
  after=json.loads(subprocess.check_output(['docker','inspect']+created,text=True));assert {c['Name']:(c['State']['Pid'],c['State']['StartedAt']) for c in after}==original
  for pid,started in original.values():
   links=json.loads(subprocess.check_output(['nsenter','-t',str(pid),'-n','ip','-j','link','show'],text=True))
   assert not any(x['ifname'].startswith('vd') for x in links),links
  assert r['sysctl_per_endpoint'] is True and r['pid_cached']==cached
  r['container_continuity_verified']=True;r['residual_container_veths']=0
  pathlib.Path('/tmp/nn-docker-final-'+label+'.json').write_text(json.dumps(r))
  print('DOCKER_VARIANT_DONE',label,flush=True)
finally:
 for name in created:subprocess.run(['docker','rm','-f',name],check=True,stdout=subprocess.DEVNULL)
print('DOCKER_FULL_COMPLETE',flush=True)
