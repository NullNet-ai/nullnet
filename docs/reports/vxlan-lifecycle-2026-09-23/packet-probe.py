import json,socket,struct,sys,time
mode,iface,path=sys.argv[1:4];s=socket.socket(socket.AF_PACKET,socket.SOCK_RAW,socket.htons(3));s.bind((iface,0));s.settimeout(.2)
if mode in ('capture','capture_esp'):
 deadline=time.monotonic()+5;frames=[]
 while time.monotonic()<deadline:
  try:data,addr=s.recvfrom(65536)
  except TimeoutError:continue
  if addr[2]==4 and ((mode=='capture' and data[12:14]==b'\x88\xe5') or (mode=='capture_esp' and len(data)>34 and data[12:14]==b'\x08\x00' and data[23]==50)):
   frames.append(data.hex());break
 open(path,'w').write(json.dumps(frames))
elif mode=='replay':
 frames=json.load(open(path));assert frames
 for frame in frames:s.send(bytes.fromhex(frame))
elif mode=='listen':
 deadline=time.monotonic()+3;count=0
 while time.monotonic()<deadline:
  try:data=s.recv(65536)
  except TimeoutError:continue
  if b'NN23_ISOLATION_PROBE' in data:count+=1
 open(path,'w').write(json.dumps({'received':count}))
elif mode=='inject':
 tags=json.loads(path);dest=sys.argv[4];src='10.223.0.1';payload=b'NN23_ISOLATION_PROBE'
 udp=struct.pack('!HHHH',45454,45454,len(payload)+8,0)+payload
 ip=struct.pack('!BBHHHBBH4s4s',0x45,0,len(udp)+20,0,0,64,17,0,socket.inet_aton(src),socket.inet_aton(dest));words=struct.unpack('!10H',ip);summ=sum(words);summ=(summ&65535)+(summ>>16);summ=(summ&65535)+(summ>>16);ip=ip[:10]+struct.pack('!H',(~summ)&65535)+ip[12:]
 eth=b'\xff'*6+b'\x02\x23\xaa\xbb\xcc\xdd'
 for tag in tags:eth+=struct.pack('!HH',0x8100,tag)
 eth+=struct.pack('!H',0x0800)
 for _ in range(3):s.send(eth+ip+udp)
