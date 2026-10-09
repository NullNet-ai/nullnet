"""Prove equal-priority source routes and exact teardown in an owned namespace."""
import json,pathlib,subprocess
from lab import root
code='''import subprocess,json
name='nn-egress-rule-proof'
def run(*args):return subprocess.check_output(args,text=True,stderr=subprocess.STDOUT)
def ip(*args):return run('ip','-n',name,*args)
run('ip','netns','add',name)
try:
 for dev,src,table in [('nne0','172.31.254.2',14097),('nne1','172.31.254.3',2107151)]:
  ip('link','add',dev,'type','dummy');ip('link','set',dev,'up');ip('addr','add',src+'/32','dev',dev)
  ip('route','add','default','dev',dev,'table',str(table))
  ip('rule','add','from',src,'to','10.0.0.0/8','lookup','main','priority','1001','protocol','242')
  ip('rule','add','from',src,'lookup',str(table),'priority','1015','protocol','242')
 ip('route','add','10.0.0.0/8','dev','nne0')
 before=run('ip','-n',name,'-N','rule','show')
 routes=[ip('route','get','9.9.9.9','from',src) for src in ['172.31.254.2','172.31.254.3']]
 internal=ip('route','get','10.9.9.9','from','172.31.254.3')
 ip('rule','del','from','172.31.254.2','lookup','14097','priority','1015','protocol','242')
 ip('rule','del','from','172.31.254.2','to','10.0.0.0/8','lookup','main','priority','1001','protocol','242')
 survivor=ip('route','get','9.9.9.9','from','172.31.254.3')
 assert 'nne0' in routes[0] and 'nne1' in routes[1] and 'nne1' in survivor and 'nne0' in internal
 assert 'proto 242' in before and 'lookup 254' in before
 print(json.dumps({'rules':before,'routes':routes,'internal_bypass':internal,'survivor_after_exact_delete':survivor}))
finally:run('ip','netns','del',name)
'''
results={h:json.loads(root(h,['python3','-c',code])) for h in ['103','104']}
destination=pathlib.Path(__file__).resolve().parents[1]/'measurements'/'egress-shared-priority-kernel-proof-242.json';assert not destination.exists();destination.write_text(json.dumps(results,indent=2)+'\n');print(json.dumps(results))
