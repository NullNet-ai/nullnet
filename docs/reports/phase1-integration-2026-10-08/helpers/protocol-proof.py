"""Verify TCP and UDP proxy ingress to both host placements with exact payloads."""
import json,pathlib,time
from api import api,login
from lab import root,internet

stack='nn-phase1-protocols';label=__import__('sys').argv[1] if len(__import__('sys').argv)>1 else 'phase1-oct9-protocols'
out=pathlib.Path(__file__).resolve().parents[1]/'measurements'/(label+'.json')
assert not out.exists()
server="""import socket,threading
def tcp():
 s=socket.socket();s.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1);s.bind(('0.0.0.0',19124));s.listen(64)
 while True:
  c,_=s.accept()
  with c:
   c.settimeout(10)
   while True:
    data=c.recv(65536)
    if not data:break
    c.sendall(data)
threading.Thread(target=tcp,daemon=True).start()
s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM);s.bind(('0.0.0.0',19124))
while True:
 data,peer=s.recvfrom(65536);s.sendto(data,peer)
"""
login();assert internet();started=[];cases=[];rows=[]
try:
 for h in ['103','104']:
  executable=root(h,['readlink','-f','/usr/bin/python3']).strip()
  root(h,['systemd-run','--unit=nn-phase1-protocol-proof','--collect','/usr/bin/python3','-u','-c',server]);started.append(h)
  for protocol,base in [('tcp',19130),('udp',19140)]:
   port=base+int(h)-103
   rows.append({'name':f'p1-{protocol}-{h}.test','process_path':executable,'host_ip':'192.168.1.'+h,'port':19124,'protocol':protocol,'listen_port':port,'timeout':1,'pausable':False})
 api('/api/service-config/'+stack,{'services':rows})
 deadline=time.monotonic()+40
 while True:
  services=api('/api/services/'+stack)
  if len(services)==4 and all(s['registered'] for s in services):break
  assert time.monotonic()<deadline,services;time.sleep(.2)
 root('104',['python3','-c',"import socket,time; end=time.monotonic()+30\nfor port in [19130,19131]:\n while True:\n  try:s=socket.create_connection(('127.0.0.1',port),timeout=.2);s.close();break\n  except OSError:assert time.monotonic()<end;time.sleep(.2)"])
 for row in rows:
  program="""import socket,json
protocol=PROTOCOL;port=PORT;payload=bytes(range(256))*3+b'phase1-exact-payload'
for i in range(16):
 s=socket.socket(socket.AF_INET,socket.SOCK_STREAM if protocol=='tcp' else socket.SOCK_DGRAM);s.settimeout(10)
 s.connect(('192.168.1.104',port));s.sendall(payload)
 if protocol=='tcp':
  received=b''
  while len(received)<len(payload):
   part=s.recv(len(payload)-len(received));assert part;received+=part
 else:received=s.recv(4096)
 assert received==payload,(protocol,i,len(received));s.close()
print(json.dumps({'protocol':protocol,'messages':16,'bytes_per_message':len(payload),'errors':0}))
""".replace('PROTOCOL',repr(row['protocol'])).replace('PORT',str(row['listen_port']))
  result=json.loads(root('104',['python3','-c',program]));result['target_host']=row['host_ip'];result['request_origin']='gateway host local route into proxy';cases.append(result)
 deadline=time.monotonic()+40
 while api('/api/graph/'+stack)['edges']:
  assert time.monotonic()<deadline,'TCP/UDP graph did not drain';time.sleep(.3)
 assert internet();out.write_text(json.dumps({'label':label,'result':'passed','cases':cases,'graph':api('/api/graph/'+stack)},indent=2)+'\n');print(json.dumps({'label':label,'cases':4,'messages':64,'errors':0}))
finally:
 api('/api/service-config/'+stack,{'services':[]})
 for h in started:root(h,['systemctl','stop','nn-phase1-protocol-proof'])
