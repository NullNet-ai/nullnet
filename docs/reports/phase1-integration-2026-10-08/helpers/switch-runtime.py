"""Switch only the isolated lab overrides; never execute the old .103 checkout."""
import argparse,concurrent.futures,json,time
from api import api,login,STACK
from lab import root

p=argparse.ArgumentParser();p.add_argument('variant',choices=['baseline','policy-admission','phase1-ready']);a=p.parse_args()
login();assert not api('/api/graph/'+STACK)['edges']
for h in ['103','104']:root(h,['systemctl','stop','nullnet-client'])
versions={'nullnet-server':'baseline-ack' if a.variant=='baseline' else a.variant,'nullnet-proxy':'final' if a.variant=='baseline' else a.variant}
program="""import pathlib,subprocess,hashlib,json
versions=VERSIONS
for unit,variant in versions.items():
 p=pathlib.Path('/run/systemd/system/'+unit+'.service.d/99-nn-layer1.conf');s=p.read_text();prefix='ExecStart=/root/nullnet-layer1-20261008/artifacts/';assert prefix in s
 binary=pathlib.Path('/root/nullnet-layer1-20261008/artifacts')/variant/unit;assert binary.is_file()
 p.write_text('\\n'.join('ExecStart='+str(binary) if line.startswith(prefix) else line for line in s.split('\\n')))
 print(json.dumps({'unit':unit,'variant':variant,'sha256':hashlib.sha256(binary.read_bytes()).hexdigest()}))
subprocess.run(['systemctl','daemon-reload'],check=True)
for unit in versions:subprocess.run(['systemctl','restart',unit],check=True)
""".replace('VERSIONS',repr(versions))
print(root('104',['python3','-c',program]),flush=True)
with concurrent.futures.ThreadPoolExecutor(2) as pool:
 for r in pool.map(lambda h:root(h,['python3','/tmp/nn-layer1-switch-client.py',a.variant]),['103','104']):print(r,flush=True)
deadline=time.monotonic()+60
while True:
 services=api('/api/services/'+STACK)
 if len(services)==12 and all(s['registered'] for s in services):break
 assert time.monotonic()<deadline,'fixture registration failed'
 time.sleep(.5)

# Agent registration can precede the proxy's initial TLS reconnect.
while True:
 try:
  root('104',['python3','-c',"import socket; socket.create_connection(('127.0.0.1',80),timeout=.5).close()"]);break
 except Exception:
  assert time.monotonic()<deadline,'proxy listener did not become ready';time.sleep(.5)
