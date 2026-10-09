"""Switch only the isolated lab overrides; never execute the old .103 checkout."""
import argparse,concurrent.futures,json,time
from api import api,login,STACK
from lab import root

p=argparse.ArgumentParser();p.add_argument('variant',choices=['phase1-fresh-egress','phase1-fresh-port','phase1-fresh-only','baseline','policy-admission','phase1-ready','phase1-oct9','phase1-oct9-cleanup','phase1-oct9-ingress','phase1-oct9-workers8','phase1-oct9-probe','phase1-oct9-batched']);a=p.parse_args()
login();assert not api('/api/graph/'+STACK)['edges']
for h in ['103','104']:root(h,['systemctl','stop','nullnet-client'])
current='phase1-ready' if a.variant.startswith('phase1-oct9') else a.variant
versions={'nullnet-server':'baseline-ack' if a.variant=='baseline' else current,'nullnet-proxy':'final' if a.variant=='baseline' else current}
program="""import pathlib,subprocess,hashlib,json
versions=VERSIONS
for unit,variant in versions.items():
 p=pathlib.Path('/etc/systemd/system/'+unit+'.service.d/99-nn-layer1.conf');s=p.read_text();prefix='ExecStart=/root/nullnet-layer1-20261008/artifacts/';assert prefix in s
 binary=pathlib.Path('/root/nullnet-layer1-20261008/artifacts')/variant/unit;assert binary.is_file()
 p.write_text('\\n'.join('ExecStart='+str(binary) if line.startswith(prefix) else line for line in s.split('\\n')))
 print(json.dumps({'unit':unit,'variant':variant,'sha256':hashlib.sha256(binary.read_bytes()).hexdigest()}))
subprocess.run(['systemctl','daemon-reload'],check=True)
for unit in versions:subprocess.run(['systemctl','restart',unit],check=True)
""".replace('VERSIONS',repr(versions))
print(root('104',['python3','-c',program]),flush=True)
client_program="""import pathlib,subprocess,hashlib,json
variant=VARIANT
binary=pathlib.Path('/root/nullnet-layer1-20261008/artifacts')/variant/'nullnet-client';assert binary.is_file()
p=pathlib.Path('/etc/systemd/system/nullnet-client.service.d/99-nn-layer1.conf');prefix='ExecStart=/root/nullnet-layer1-20261008/artifacts/';s=p.read_text();assert prefix in s
p.write_text('\\n'.join('ExecStart='+str(binary) if line.startswith(prefix) else line for line in s.split('\\n')))
subprocess.run(['systemctl','daemon-reload'],check=True);subprocess.run(['systemctl','start','nullnet-client'],check=True)
print(json.dumps({'variant':variant,'sha256':hashlib.sha256(binary.read_bytes()).hexdigest()}))
""".replace('VARIANT',repr(a.variant))
with concurrent.futures.ThreadPoolExecutor(2) as pool:
 for r in pool.map(lambda h:root(h,['python3','-c',client_program]),['103','104']):print(r,flush=True)
deadline=time.monotonic()+60
while True:
 services=api('/api/services/'+STACK)
 if len(services)==12 and all(s['registered'] for s in services):break
 assert time.monotonic()<deadline,'fixture registration failed'
 time.sleep(.5)

# Agent registration can precede the proxy's initial TLS reconnect.
while True:
 try:
  root('104',['python3','-c',"import socket,sys; s=socket.socket(); s.settimeout(.5); sys.exit(bool(s.connect_ex(('127.0.0.1',80))))"]);break
 except Exception:
  assert time.monotonic()<deadline,'proxy listener did not become ready';time.sleep(.5)
