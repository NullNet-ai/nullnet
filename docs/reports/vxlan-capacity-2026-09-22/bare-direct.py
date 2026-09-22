import socket,struct,time,json,os,subprocess,concurrent.futures,threading,pathlib,sys
N=int(sys.argv[1]);WORKERS=int(sys.argv[2]);SLOTS=threading.Semaphore(8);BASE='nn-direct-base';metrics=[];lock=threading.Lock()
os.setns(os.open('/var/run/netns/'+BASE,os.O_RDONLY),os.CLONE_NEWNET)
U=lambda n:struct.pack('I',n)
def attr(k,v):
 b=struct.pack('HH',len(v)+4,k)+v;return b+b'\0'*((-len(b))%4)
def info(index=0,flags=0,change=0):return struct.pack('BBHiII',0,0,0,index,flags,change)
def nested(kind,data):return attr(18|32768,attr(1,kind.encode()+b'\0')+attr(2|32768,data))
class NL:
 def __init__(self):
  self.s=socket.socket(socket.AF_NETLINK,socket.SOCK_RAW,0);self.s.bind((0,0));self.s.settimeout(120);self.seq=0
 def request(self,kind,payload,flags=5,missing=False):
  self.seq+=1;msg=struct.pack('IHHII',16+len(payload),kind,flags,self.seq,0)+payload;start=time.monotonic();self.s.sendto(msg,(0,0));result=None
  while True:
   data=self.s.recv(65536);off=0
   while off+16<=len(data):
    size,typ,fl,seq,pid=struct.unpack_from('IHHII',data,off)
    if seq==self.seq:
     if typ==2:
      err=struct.unpack_from('i',data,off+16)[0]
      if err and not(missing and err==-19):raise RuntimeError(('netlink',kind,err))
      return result,time.monotonic()-start
     elif typ==16:result=struct.unpack_from('i',data,off+20)[0]
    off+=(size+3)&~3
 def get(self,name):return self.request(18,info()+attr(3,name.encode()+b'\0'),missing=True)[0]
 def create(self,name,kind,data,extra=b''):return self.request(16,info()+attr(3,name.encode()+b'\0')+attr(27,U(0x4e600001))+nested(kind,data)+extra,5|512|1024)
 def up(self,index,bridge=None):return self.request(19,info(index,1,1)+attr(4,U(1080))+(attr(10,U(bridge)) if bridge else b''))
 def addr(self,index,addr):return self.request(20,struct.pack('BBBBI',2,30,0,0,index)+attr(1,socket.inet_aton(addr))+attr(2,socket.inet_aton(addr)),5|512|1024)
def cmd(args,input=None):
 start=time.monotonic()
 with SLOTS:
  entered=time.monotonic();r=subprocess.run(args,input=input,stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=120)
  if r.returncode:raise RuntimeError((args[:3],r.returncode,r.stderr.decode()[:200]))
 return entered-start,time.monotonic()-entered,r.stdout
KEY='11'*32
salt=subprocess.check_output(['sha256sum'],input=KEY.encode()).decode()[:8];aead='0x'+KEY+salt
def setup(i):
 nl=NL();ns='nn-dir-%04d'%i;vout='vd%04do'%i;vin='vd%04di'%i;br='bd%04d'%i;vx='xd%04d'%i;stages={};last=time.monotonic();begin=last
 def mark(s):
  nonlocal last
  now=time.monotonic();stages[s]=now-last;last=now
 cmd(['ip','netns','add',ns]);mark('namespace')
 nl.get(vout);fd=os.open('/var/run/netns/'+ns,os.O_RDONLY)
 peer=info()+attr(3,vin.encode()+b'\0')+attr(4,U(1080))+attr(28,U(fd))
 nl.create(vout,'veth',attr(1|32768,peer),attr(4,U(1080)));os.close(fd);mark('veth')
 base=0x0afe0000+i*4;bridge=socket.inet_ntoa(struct.pack('!I',base+1));endpoint=socket.inet_ntoa(struct.pack('!I',base+2));prefix=['nsenter','--net=/var/run/netns/'+ns,'--']
 cmd(prefix+['ip','addr','add',endpoint+'/30','dev',vin]);cmd(prefix+['ip','link','set',vin,'up']);cmd(prefix+['ip','route','add','default','via',bridge]);mark('namespace_config')
 nl.get(br);nl.create(br,'bridge',b'');bi=nl.get(br);nl.addr(bi,bridge);nl.up(bi);oi=nl.get(vout);nl.up(oi,bi);mark('bridge_and_attach')
 for name in ['ms%04ds'%i,'ms%04dc'%i,'vs%04ds'%i,vx]:nl.get(name)
 data=attr(1,U(100000+i))+attr(4,socket.inet_aton('192.0.2.1'))+attr(2,socket.inet_aton('192.0.2.2'))+attr(15,struct.pack('!H',20000+i))
 nl.create(vx,'vxlan',data);vi=nl.get(vx);nl.up(vi,bi);mark('vxlan')
 cmd(['sha256sum'],KEY.encode());spi=str(101000+i)
 for local,remote,direction in [('192.0.2.1','192.0.2.2','out'),('192.0.2.2','192.0.2.1','in')]:
  cmd(['ip','xfrm','state','add','src',local,'dst',remote,'proto','esp','spi',spi,'aead','rfc4106(gcm(aes))',aead,'128','mode','transport'])
  cmd(['ip','xfrm','policy','add','src',local,'dst',remote,'proto','udp','dport',str(20000+i),'dir',direction,'tmpl','src',local,'dst',remote,'proto','esp','spi',spi,'mode','transport'])
 mark('ipsec')
 if os.environ.get('NN_FORWARD_ONCE')!='1':
  cmd(['sysctl','-w','net.ipv4.ip_forward=1']);cmd(['iptables','-P','FORWARD','ACCEPT'])
 mark('forwarding')
 return {'i':i,'seconds':time.monotonic()-begin,'stages':stages}
if __name__=='__main__':
 if os.environ.get('NN_FORWARD_ONCE')=='1':
  cmd(['sysctl','-w','net.ipv4.ip_forward=1']);cmd(['iptables','-P','FORWARD','ACCEPT'])
 start=time.monotonic();errors=[]
 with concurrent.futures.ThreadPoolExecutor(max_workers=WORKERS) as ex:
  futures={ex.submit(setup,i):i for i in range(N)}
  for f in concurrent.futures.as_completed(futures):
   try:metrics.append(f.result())
   except Exception as e:errors.append({'i':futures[f],'error':str(e)})
 elapsed=time.monotonic()-start
 inventory={'links':len(json.loads(subprocess.check_output(['ip','-j','link','show'],text=True))),'vxlan':len(json.loads(subprocess.check_output(['ip','-j','link','show','type','vxlan'],text=True))),'bridges':len(json.loads(subprocess.check_output(['ip','-j','link','show','type','bridge'],text=True))),'xfrm_states':sum(l.startswith('src ') for l in subprocess.check_output(['ip','xfrm','state'],text=True).splitlines()),'xfrm_policies':sum(l.startswith('src ') for l in subprocess.check_output(['ip','xfrm','policy'],text=True).splitlines())}
 result={'inventory':inventory,'n':N,'workers':WORKERS,'seconds':elapsed,'success':len(metrics),'errors':errors,'endpoints':metrics}
 pathlib.Path('/tmp/nn-direct-result.json').write_text(json.dumps(result));print(json.dumps({k:v for k,v in result.items() if k!='endpoints'}),flush=True)
