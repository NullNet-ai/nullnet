"""Verify gateway forwarding/SNAT against a routed LAN-only public-address fixture."""
import json,pathlib,subprocess,time
from api import api,login,STACK
from lab import root,internet

out=pathlib.Path(__file__).resolve().parents[1]/'measurements';label='phase1-egress-lan'
assert not (out/(label+'.json')).exists()
ns='nn-phase1-egress';veth='nnpegr0';peer='nnpegr1';target='203.0.113.250'
listener=None;configured=[];results=[]
def run(h,*args):return root(h,list(args))
server="""from http.server import HTTPServer,BaseHTTPRequestHandler
import json,pathlib,os
class Handler(BaseHTTPRequestHandler):
 def do_GET(self):
  with open('/tmp/nn-layer1-egress-received.jsonl','a') as f:f.write(json.dumps({'source':self.client_address[0],'path':self.path})+'\\n')
  body=b'phase1-lan-egress\\n';self.send_response(200);self.send_header('Content-Length',str(len(body)));self.end_headers();self.wfile.write(body)
 def log_message(self,*args):pass
httpd=HTTPServer(('203.0.113.250',22),Handler)
pathlib.Path('/tmp/nn-layer1-egress-listener.pid').write_text(str(os.getpid()))
httpd.serve_forever()
"""
login();assert not api('/api/graph/'+STACK)['edges'];assert internet()
try:
 assert ns not in run('103','ip','netns','list')
 run('103','ip','netns','add',ns);configured.append('namespace')
 run('103','ip','link','add',veth,'type','veth','peer','name',peer)
 run('103','ip','link','set',peer,'netns',ns)
 run('103','ip','addr','add','198.19.255.1/30','dev',veth)
 run('103','ip','link','set',veth,'up')
 run('103','ip','netns','exec',ns,'ip','addr','add','198.19.255.2/30','dev',peer)
 run('103','ip','netns','exec',ns,'ip','link','set',peer,'up')
 run('103','ip','netns','exec',ns,'ip','link','set','lo','up')
 run('103','ip','netns','exec',ns,'ip','addr','add',target+'/32','dev','lo')
 run('103','ip','netns','exec',ns,'ip','route','add','default','via','198.19.255.1')
 run('103','ip','route','add',target+'/32','via','198.19.255.2');configured.append('route103')
 run('104','ip','route','add',target+'/32','via','192.168.1.103');configured.append('route104')
 for direction in ['-i','-o']:
  run('103','iptables','-w','-I','FORWARD','1',direction,veth,'-m','comment','--comment','nn-phase1-egress-proof','-j','ACCEPT');configured.append(direction)
 run('103','python3','-c',"import pathlib; pathlib.Path('/tmp/nn-layer1-egress-received.jsonl').write_text(''); pathlib.Path('/tmp/nn-layer1-egress-listener.pid').unlink(missing_ok=True)")
 cmd=['ip','netns','exec',ns,'python3','-u','-c',server]
 import shlex
 listener=subprocess.Popen(['ssh','-o','BatchMode=yes','debian@192.168.1.103',"printf '%s\\n' debian | sudo -S -p '' "+shlex.join(cmd)],stdout=subprocess.DEVNULL,stderr=subprocess.PIPE,text=True)
 time.sleep(.5)
 for h,name in [('103','nn-phase1-01'),('104','nn-phase1-07')]:
  response=run(h,'docker','exec',name,'wget','-q','-T','20','-O','-',f'http://{target}:22/{name}')
  assert response=='phase1-lan-egress\n',response
  results.append({'host':h,'container':name,'response':response,'graph':api('/api/graph/'+STACK)})
 received=[json.loads(r) for r in run('103','cat','/tmp/nn-layer1-egress-received.jsonl').splitlines()]
 assert len(received)==2 and all(r['source']=='192.168.1.104' for r in received),received
 assert internet()
 (out/(label+'.json')).write_text(json.dumps({'result':'passed','requests':results,'received':received,'scope':'All destinations explicitly routed inside the LAN; observed source proves physical gateway SNAT.'},indent=2)+'\n')
 print(json.dumps({'egress':'passed','gateway_snat_source':'192.168.1.104','cases':2}))
finally:
 if listener is not None:
  run('103','python3','-c',"import os,signal,pathlib,subprocess; p=pathlib.Path('/tmp/nn-layer1-egress-listener.pid'); pid=int(p.read_text()) if p.exists() else None; assert pid is None or subprocess.check_output(['ip','netns','identify',str(pid)],text=True).strip()=='nn-phase1-egress'; os.kill(pid,signal.SIGTERM) if pid else None")
  listener.wait(timeout=5)
 for direction in ['-i','-o']:
  if direction in configured:run('103','iptables','-w','-D','FORWARD',direction,veth,'-m','comment','--comment','nn-phase1-egress-proof','-j','ACCEPT')
 if 'route104' in configured:run('104','ip','route','del',target+'/32','via','192.168.1.103')
 if 'route103' in configured:run('103','ip','route','del',target+'/32','via','198.19.255.2')
 if 'namespace' in configured:run('103','ip','netns','del',ns)
