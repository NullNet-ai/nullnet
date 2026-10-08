import argparse, json, os, pathlib, signal, subprocess, sys, time
root=pathlib.Path('/tmp/nullnet-pool-prototype-20260928')
def run(*args):
 return subprocess.check_output(args,text=True)
def inventory():
 containers=json.loads(run('docker','inspect',*run('docker','ps','-q').split()))
 return {'containers':{x['Id']:{'name':x['Name'],'pid':x['State']['Pid'],'started':x['State']['StartedAt'],'networks':x['NetworkSettings']['Networks']} for x in containers},'links':json.loads(run('ip','-j','link','show')),'routes':json.loads(run('ip','-j','route','show','table','all')),'services':run('systemctl','show','nullnet-client','nullnet-server','nullnet-proxy','-p','MainPID','-p','NRestarts')}
names=['nn28-pool-a','nn28-pool-b']
created=[]
before=inventory()
(root/'before.json').write_text(json.dumps(before,indent=2))
try:
 for name in names:
  result=subprocess.run(['docker','inspect',name],capture_output=True)
  assert result.returncode!=0,'fixture already exists'
  run('docker','run','-d','--network','none','--label','nullnet-pool-prototype=20260928','--name',name,'alpine:latest','sleep','infinity')
  created.append(name)
 pids=[run('docker','inspect','-f','{{.State.Pid}}',name).strip() for name in names]
 for pid in pids:
  run('nsenter','-t',pid,'-n','sysctl','-qw','net.ipv6.conf.all.disable_ipv6=1','net.ipv6.conf.default.disable_ipv6=1')
 parser=argparse.ArgumentParser()
 parser.add_argument('--program',choices=['prototype.py','proof.py'],default='prototype.py')
 options,remaining=parser.parse_known_args()
 args=['python3',str(root/options.program),'--pids',*pids,*remaining]
 result=subprocess.run(args)
 if result.returncode: raise RuntimeError(f'prototype exit {result.returncode}')
finally:
 for name in created:
  run('docker','rm','-f',name)
 after=inventory()
 (root/'after.json').write_text(json.dumps(after,indent=2))
 assert before['containers']==after['containers'],'original containers changed'
 assert before['services']==after['services'],'service process changed'
 original={(x['ifindex'],x['ifname']) for x in before['links']}
 final={(x['ifindex'],x['ifname']) for x in after['links']}
 assert original==final,('link inventory differs',original^final)
 assert before['routes']==after['routes'],'route inventory changed'
 print('ORIGINAL_WORKLOADS_AND_NETWORK_INVENTORY_PRESERVED',flush=True)
