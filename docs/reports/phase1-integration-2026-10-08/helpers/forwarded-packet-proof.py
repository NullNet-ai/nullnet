"""Exercise root-forwarded MASQUERADE through a container-bound cross-host endpoint."""
import json,pathlib,sys
from lab import root
ns='nn-phase1-forward';device='nnphf0';peer='nnphf1';source='198.18.254.2';target='10.251.250.3';transport='nnv_4097_s'
def run(*args):return root('103',list(args))
assert ns not in run('ip','netns','list')
rule=['-s',source+'/32','-o',transport,'-m','comment','--comment','nn-phase1-forward-proof','-j','MASQUERADE']
created=False;nat=False
try:
 run('ip','netns','add',ns);created=True
 run('ip','link','add',device,'type','veth','peer','name',peer)
 run('ip','link','set',peer,'netns',ns)
 run('ip','addr','add','198.18.254.1/30','dev',device);run('ip','link','set',device,'up')
 run('ip','netns','exec',ns,'ip','addr','add',source+'/30','dev',peer)
 run('ip','netns','exec',ns,'ip','link','set',peer,'up');run('ip','netns','exec',ns,'ip','link','set','lo','up')
 run('ip','netns','exec',ns,'ip','route','add',target+'/32','via','198.18.254.1')
 run('iptables','-w','-t','nat','-I','POSTROUTING','1',*rule);nat=True
 output=run('ip','netns','exec',ns,'ping','-c','3','-W','1',target)
 assert '0% packet loss' in output,output
 result={'result':'passed','scope':'forwarded source requires MASQUERADE on the VXLAN output interface','source':source,'destination':target,'output':output}
 pathlib.Path(__file__).resolve().parents[1].joinpath('measurements/'+(sys.argv[1] if len(sys.argv)>1 else 'phase1-oct9-forwarded-packets')+'.json').write_text(json.dumps(result,indent=2)+'\n')
 print(json.dumps(result))
finally:
 if nat:run('iptables','-w','-t','nat','-D','POSTROUTING',*rule)
 if created:
  run('ip','netns','del',ns)
