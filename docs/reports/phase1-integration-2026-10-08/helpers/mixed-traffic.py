"""Owned lab traffic: six cross-host backends plus six SNAT-verified egress streams."""
import json,pathlib,subprocess,time
from lab import root
pairs={1:10,2:11,3:12,7:4,8:5,9:6}
address='198.18.240.103'
receiver_rules=[['-s','192.168.1.104','-d',address,'-o','nn-soak-rx','-p','tcp','--dport','80','-j','ACCEPT'],['-i','nn-soak-rx','-s',address,'-d','192.168.1.104','-m','conntrack','--ctstate','ESTABLISHED,RELATED','-j','ACCEPT']]
receiver="""from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
class Handler(BaseHTTPRequestHandler):
 protocol_version='HTTP/1.1'
 def do_GET(self):
  body=('source='+self.client_address[0]+'\\n').encode();self.send_response(200);self.send_header('Content-Length',str(len(body)));self.end_headers();self.wfile.write(body)
 def log_message(self,*args):pass
ThreadingHTTPServer(('198.18.240.103',80),Handler).serve_forever()
"""
def names(enabled):
 code="""import pathlib,subprocess
for n in NUMBERS:
 name='nn-phase1-%02d'%n
 path=pathlib.Path(subprocess.check_output(['docker','inspect','--format','{{.HostsPath}}',name],text=True).strip())
 text=''.join(line+'\\n' for line in path.read_text().splitlines() if '# nn-phase1-bootstrap' not in line)
 if ENABLED:text+=''.join(('192.168.1.103' if i<=6 else '192.168.1.104')+' p1-%02d.test # nn-phase1-bootstrap\\n'%i for i in range(1,13))
 with path.open('w') as f:f.write(text)
"""
 for host,numbers in [('103',range(1,7)),('104',range(7,13))]:root(host,['python3','-c',code.replace('NUMBERS',repr(list(numbers))).replace('ENABLED',repr(enabled))])
def start(label,duration):
 helper=pathlib.Path(__file__).with_name('soak-outbound.py')
 for host in ['103','104']:subprocess.run(['scp',str(helper),f'debian@192.168.1.{host}:/tmp/soak-outbound.py'],check=True)
 assert not root('103',['ip','-o','addr','show','to',address+'/32']).strip(),'owned receiver address already exists'
 root('103',['ip','netns','add','nn-soak-receiver'])
 root('103',['ip','link','add','nn-soak-rx','type','veth','peer','name','nn-soak-in','netns','nn-soak-receiver'])
 root('103',['ip','addr','add','198.18.241.1/30','dev','nn-soak-rx'])
 root('103',['ip','link','set','nn-soak-rx','up'])
 for args in [['addr','add','198.18.241.2/30','dev','nn-soak-in'],['link','set','nn-soak-in','up'],['link','set','lo','up'],['addr','add',address+'/32','dev','lo'],['route','add','default','via','198.18.241.1']]:root('103',['ip','-n','nn-soak-receiver']+args)
 root('103',['ip','route','add',address+'/32','via','198.18.241.2','proto','186'])
 root('104',['ip','route','add',address+'/32','via','192.168.1.103','proto','186'])
 for rule in receiver_rules:root('103',['iptables','-w','-I','FORWARD','1']+rule)
 root('103',['python3','-c',"from pathlib import Path;Path('/tmp/nn-soak-receiver.py').write_text("+repr(receiver)+")"])
 root('103',['systemd-run','--unit=nn-mixed-receiver-'+label,'--collect','--property=RuntimeMaxSec='+str(int(duration+300)),'/usr/bin/ip','netns','exec','nn-soak-receiver','/usr/bin/python3','/tmp/nn-soak-receiver.py'])
 for n,target in pairs.items():
  host='103' if n<=6 else '104';container=f'nn-phase1-{n:02d}'
  metadata=json.loads(root(host,['docker','inspect','--format','{"pid":{{.State.Pid}},"hosts":{{json .HostsPath}}}',container]))
  for kind in kinds(n):
   args=['--name',f'p1-{target:02d}.test' if kind=='backend' else address,'--port',str(9000+target if kind=='backend' else 80),'--expected',f'node={target:02d}' if kind=='backend' else 'source=192.168.1.104','--rate','20' if kind=='backend' else '10','--duration',str(duration+150)]
   if kind=='backend':args+=['--hosts-file',metadata['hosts']]
   root(host,['systemd-run','--unit='+unit(label,n,kind),'--collect','--property=RuntimeMaxSec='+str(int(duration+180)),'/usr/bin/nsenter','--net','--target',str(metadata['pid']),'/usr/bin/python3','/tmp/soak-outbound.py']+args)
 time.sleep(10)
 for n in pairs:
  host='103' if n<=6 else '104'
  for kind in kinds(n):
   text=root(host,['journalctl','-u',unit(label,n,kind),'-o','cat','--no-pager'])
   samples=[]
   for line in text.splitlines():
    try:samples.append(json.loads(line))
    except json.JSONDecodeError:pass
   assert samples and all('failure' not in r and r.get('errors',0)==0 for r in samples) and samples[-1]['requests']>0,(n,kind,samples)
def kinds(n):return ['backend','egress']
def unit(label,n,kind):return f'nn-mixed-{label}-{n:02d}-{kind}'
def stop(label,out):
 records=[]
 for n in pairs:
  host='103' if n<=6 else '104'
  for kind in kinds(n):
   name=unit(label,n,kind)
   try:root(host,['systemctl','stop',name])
   except subprocess.CalledProcessError:pass
   journal=root(host,['journalctl','-u',name,'-o','cat','--no-pager'])
   (out/f'{label}-{n:02d}-{kind}.jsonl').write_text(journal)
   rows=[]
   for line in journal.splitlines():
    try:rows.append(json.loads(line))
    except json.JSONDecodeError:pass
   records.append({'source':n,'kind':kind,'samples':rows})
 try:root('103',['systemctl','stop','nn-mixed-receiver-'+label])
 except subprocess.CalledProcessError:pass
 for rule in receiver_rules:root('103',['iptables','-w','-D','FORWARD']+rule)
 root('104',['ip','route','del',address+'/32','via','192.168.1.103','proto','186'])
 root('103',['ip','route','del',address+'/32','via','198.18.241.2','proto','186'])
 root('103',['ip','link','del','nn-soak-rx'])
 root('103',['ip','netns','del','nn-soak-receiver']);names(False)
 return records
