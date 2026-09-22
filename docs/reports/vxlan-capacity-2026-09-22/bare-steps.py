import importlib.util,sys,concurrent.futures,subprocess,time,json,pathlib,os,socket,struct
spec=importlib.util.spec_from_file_location('direct','/tmp/nullnet-direct.py');d=importlib.util.module_from_spec(spec);spec.loader.exec_module(d)
N=d.N;W=d.WORKERS;indices={};results=[]
def apply(label,fn,ops=1):
 start=time.monotonic();errors=[]
 with concurrent.futures.ThreadPoolExecutor(max_workers=W) as pool:
  fs={pool.submit(fn,i):i for i in range(N)}
  for f in concurrent.futures.as_completed(fs):
   try:f.result()
   except Exception as e:errors.append({'i':fs[f],'error':str(e)})
 elapsed=time.monotonic()-start;r={'step':label,'endpoints':N,'operations_per_endpoint':ops,'seconds':elapsed,'endpoints_per_second':N/elapsed,'operations_per_second':N*ops/elapsed,'errors':errors};results.append(r);print(json.dumps(r),flush=True);pathlib.Path('/tmp/nn-bare-steps.json').write_text(json.dumps(results))
 if errors:raise RuntimeError(errors[:3])
def names(i):return 'nn-dir-%04d'%i,'vd%04do'%i,'vd%04di'%i,'bd%04d'%i,'xd%04d'%i
def addr(i):
 b=0x0afe0000+i*4;return socket.inet_ntoa(struct.pack('!I',b+1)),socket.inet_ntoa(struct.pack('!I',b+2))
def veth(i):
 ns,out,inside,br,vx=names(i);fd=os.open('/var/run/netns/'+ns,os.O_RDONLY);peer=d.info()+d.attr(3,inside.encode()+b'\0')+d.attr(4,d.U(1080))+d.attr(28,d.U(fd));nl=d.NL();nl.create(out,'veth',d.attr(1|32768,peer),d.attr(4,d.U(1080)));os.close(fd)
def nsconf(i,which):
 ns,out,inside,br,vx=names(i);bridge,endpoint=addr(i);prefix=['nsenter','--net=/var/run/netns/'+ns,'--']
 args={'address':['ip','addr','add',endpoint+'/30','dev',inside],'up':['ip','link','set',inside,'up'],'route':['ip','route','add','default','via',bridge]}[which];d.cmd(prefix+args)
def createbridge(i):d.NL().create(names(i)[3],'bridge',b'')
def lookup(i):
 ns,out,inside,br,vx=names(i);nl=d.NL();indices[i]={'br':nl.get(br),'out':nl.get(out)}
def vxlan(i):
 ns,out,inside,br,vx=names(i);data=d.attr(1,d.U(100000+i))+d.attr(4,socket.inet_aton('192.0.2.1'))+d.attr(2,socket.inet_aton('192.0.2.2'))+d.attr(15,struct.pack('!H',20000+i));d.NL().create(vx,'vxlan',data)
def xlookup(i):indices[i]['vx']=d.NL().get(names(i)[4])
def xfrm(i,kind,direction):
 local,remote=('192.0.2.1','192.0.2.2') if direction=='out' else ('192.0.2.2','192.0.2.1');spi=str(101000+i)
 if kind=='state':args=['ip','xfrm','state','add','src',local,'dst',remote,'proto','esp','spi',spi,'aead','rfc4106(gcm(aes))',d.aead,'128','mode','transport']
 else:args=['ip','xfrm','policy','add','src',local,'dst',remote,'proto','udp','dport',str(20000+i),'dir',direction,'tmpl','src',local,'dst',remote,'proto','esp','spi',spi,'mode','transport']
 d.cmd(args)
apply('Namespace creation',lambda i:d.cmd(['ip','netns','add',names(i)[0]]))
apply('Stale veth lookup',lambda i:d.NL().get(names(i)[1]))
apply('Veth pair creation into namespace',veth)
apply('Namespace address',lambda i:nsconf(i,'address'))
apply('Namespace interface UP',lambda i:nsconf(i,'up'))
apply('Namespace default route',lambda i:nsconf(i,'route'))
apply('Stale bridge lookup',lambda i:d.NL().get(names(i)[3]))
apply('Bridge creation',createbridge)
apply('Bridge and veth lookup',lookup,2)
apply('Bridge address',lambda i:d.NL().addr(indices[i]['br'],addr(i)[0]))
apply('Bridge MTU and UP',lambda i:d.NL().up(indices[i]['br']))
apply('Veth bridge attach MTU UP',lambda i:d.NL().up(indices[i]['out'],indices[i]['br']))
def stale(i):
 for name in ['ms%04ds'%i,'ms%04dc'%i,'vs%04ds'%i,names(i)[4]]:d.NL().get(name)
apply('Four stale cross-host lookups',stale,4)
apply('VXLAN creation',vxlan)
apply('VXLAN lookup',xlookup)
apply('VXLAN bridge attach MTU UP',lambda i:d.NL().up(indices[i]['vx'],indices[i]['br']))
apply('IPsec key derivation subprocess',lambda i:d.cmd(['sha256sum'],d.KEY.encode()))
for kind,direction in [('state','out'),('policy','out'),('state','in'),('policy','in')]:apply('IPsec '+kind+' '+direction,lambda i,k=kind,di=direction:xfrm(i,k,di))
apply('Forwarding sysctl',lambda i:d.cmd(['sysctl','-w','net.ipv4.ip_forward=1']))
apply('Forwarding iptables policy',lambda i:d.cmd(['iptables','-P','FORWARD','ACCEPT']))
print('BARE_STEPS_COMPLETE',flush=True)
