"""Inject per-edge encrypted/plain VXLAN packets into coordinated Rust test endpoints."""
import json,pathlib,socket,struct,sys,time,subprocess
mode=sys.argv[1]
def checksum(b):
 if len(b)%2:b+=b'\0'
 total=sum(struct.unpack('!%dH'%(len(b)//2),b));total=(total>>16)+(total&65535);total+=(total>>16)
 return (~total)&65535
def packet(subnet,label):
 payload=label.encode();udp=struct.pack('!HHHH',45554,45555,8+len(payload),0)+payload
 ip=struct.pack('!BBHHHBBH4s4s',0x45,0,20+len(udp),123,0,64,17,0,socket.inet_aton(f'10.251.{subnet}.1'),socket.inet_aton(f'10.251.{subnet}.3'))
 ip=ip[:10]+struct.pack('!H',checksum(ip))+ip[12:]
 return b'\xff'*6+b'\x02\x00\x00\x00\x00\x01\x08\x00'+ip+udp
if mode=='listen':
 subnet=int(sys.argv[2]);s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM);s.bind((f'10.251.{subnet}.3',45555));s.settimeout(.2)
 pathlib.Path('/tmp/nn-layer1-security-ready-'+str(subnet)).write_text('ok');rows=[];end=time.monotonic()+4
 while time.monotonic()<end:
  try:rows.append(s.recv(4096).decode())
  except socket.timeout:pass
 pathlib.Path('/tmp/nn-layer1-security-'+str(subnet)+'.json').write_text(json.dumps(rows));print(json.dumps(rows))
elif mode=='send':
 device=json.loads(subprocess.check_output(['ip','-j','route','get','192.168.1.104'],text=True))[0]['dev']
 capture=socket.socket(socket.AF_PACKET,socket.SOCK_RAW,socket.htons(0x0003));capture.bind((device,0));capture.settimeout(.1)
 for vni,subnet,mark,label in [(4097,250,0,'plain-A'),(4103,251,0,'plain-B'),(4103,251,4097,'wrong-edge-A-to-B'),(4097,250,4103,'wrong-edge-B-to-A'),(4097,250,4097,'encrypted-A'),(4103,251,4103,'encrypted-B')]:
  s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM)
  if mark:s.setsockopt(socket.SOL_SOCKET,socket.SO_MARK,0x4e800000|mark)
  s.bind(('192.168.1.103',0));s.sendto(b'\x08\0\0\0'+vni.to_bytes(3,'big')+b'\0'+packet(subnet,label),('192.168.1.104',4791));s.close()
 captured=None;end=time.monotonic()+1
 while time.monotonic()<end:
  try:
   frame,address=capture.recvfrom(65535);ip=frame[14:];ihl=(ip[0]&15)*4
   if address[2]==socket.PACKET_OUTGOING and ip[9]==50 and int.from_bytes(ip[ihl:ihl+4],'big')==(0x4e000000|(4097<<1)):
    captured=ip[:int.from_bytes(ip[2:4],'big')]
  except socket.timeout:pass
 assert captured is not None,'did not capture fixture ESP packet'
 raw=socket.socket(socket.AF_INET,socket.SOCK_RAW,socket.IPPROTO_RAW)
 raw.sendto(captured,('192.168.1.104',0))
 damaged=bytearray(captured);ihl=(damaged[0]&15)*4
 damaged[ihl+4:ihl+8]=(int.from_bytes(damaged[ihl+4:ihl+8],'big')+1000).to_bytes(4,'big');damaged[-1]^=1
 raw.sendto(damaged,('192.168.1.104',0))
 print('sent plaintext, wrong-edge, correct encrypted, replay and damaged-authentication packets')
