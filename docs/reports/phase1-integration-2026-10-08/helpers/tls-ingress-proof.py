"""Use owned self-signed fixtures to verify trusted HTTPS ingress and payloads."""
import json,pathlib,time
from api import api,login,wait_registered,STACK
from lab import root,internet

label=__import__('sys').argv[1] if len(__import__('sys').argv)>1 else 'phase1-oct9-tls-ingress';out=pathlib.Path(__file__).resolve().parents[1]/'measurements'/(label+'.json')
assert not out.exists()
login();wait_registered();assert not api('/api/graph/'+STACK)['edges'];assert internet()
created=[];results=[];names=['p1-01.test','p1-07.test'];reuse='--reuse' in __import__('sys').argv[2:]
try:
 for name in names:
  if reuse:continue
  code="""import pathlib,subprocess
name=NAME;base=pathlib.Path('/root/nullnet-layer1-20261008/run/server/certs')/name
assert not base.exists();base.mkdir(parents=True)
subprocess.run(['openssl','req','-x509','-newkey','rsa:2048','-nodes','-keyout',str(base/'privkey.pem'),'-out',str(base/'fullchain.pem'),'-subj','/CN='+name,'-addext','subjectAltName=DNS:'+name,'-days','1'],check=True,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
(base/'privkey.pem').chmod(0o600)
""".replace('NAME',repr(name))
  root('104',['python3','-c',code]);created.append(name)
 deadline=time.monotonic()+30
 while not set(names).issubset({c['domain'] for c in api('/api/certificates')}):
  assert time.monotonic()<deadline,'TLS fixtures not loaded';time.sleep(.2)
 for name in names:
  code="""import ssl,socket,time,json
name=NAME;context=ssl.create_default_context(cafile='/root/nullnet-layer1-20261008/run/server/certs/'+name+'/fullchain.pem')
deadline=time.monotonic()+20
while True:
 try:
  raw=socket.create_connection(('192.168.1.104',443),timeout=3);s=context.wrap_socket(raw,server_hostname=name);break
 except OSError:assert time.monotonic()<deadline;time.sleep(.2)
s.sendall(('GET /payload.bin HTTP/1.1\\r\\nHost: '+name+'\\r\\nConnection: close\\r\\n\\r\\n').encode());data=b''
while True:
 chunk=s.recv(65536)
 if not chunk:break
 data+=chunk
header,body=data.split(b'\\r\\n\\r\\n',1);assert header.startswith(b'HTTP/1.1 200'),header
assert body==bytes(range(256))*4096
print(json.dumps({'host':name,'tls_version':s.version(),'certificate_hostname_verified':True,'bytes':len(body),'status':200}));s.close()
""".replace('NAME',repr(name))
  results.append(json.loads(root('104',['python3','-c',code])))
 deadline=time.monotonic()+30
 while api('/api/graph/'+STACK)['edges']:
  assert time.monotonic()<deadline;time.sleep(.2)
 assert internet();out.write_text(json.dumps({'label':label,'result':'passed','cases':results,'graph':api('/api/graph/'+STACK)},indent=2)+'\n');print(json.dumps({'label':label,'result':'passed','cases':2}))
finally:
 for name in created:
  root('104',['python3','-c',"import pathlib; base=pathlib.Path('/root/nullnet-layer1-20261008/run/server/certs')/"+repr(name)+"; assert {p.name for p in base.iterdir()}<= {'fullchain.pem','privkey.pem','privkey.enc'}; [p.unlink() for p in base.iterdir()]; base.rmdir()"])
