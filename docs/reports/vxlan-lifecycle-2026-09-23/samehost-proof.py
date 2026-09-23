import concurrent.futures as cf,json,os,pathlib,socket,struct,subprocess,threading,time
import lifecycle as b
from netlink import NL,U,attr,info
R=pathlib.Path('/tmp/nn23');result={};created=[];hostfd=os.open('/proc/self/ns/net',os.O_RDONLY)
def run(args,check=True):
 p=subprocess.run(args,capture_output=True,text=True)
 if check and p.returncode:raise RuntimeError((args,p.stdout,p.stderr))
 return p
def save(): (R/('samehost-dedicated-proof.json' if b.BRIDGE_POOL else 'samehost-transparent-proof.json' if b.TRANSPARENT else 'samehost-proof.json')).write_text(json.dumps(result,indent=2))
def address(i):return socket.inet_ntoa(struct.pack('!I',0x0ae00000+(i//2)*8+2+(i%2)))
def ep(i,args,check=True):return run(['nsenter','-t',str(pids[i%12]),'-n',*args],check)
def configure(i):
 # Use the production-style shared /29 for each same-host edge.
 ep(i,['ip','-4','addr','flush','dev',f'i{i}']);ep(i,['ip','addr','add',address(i)+'/29','dev',f'i{i}'])
 run(['ip','-4','addr','flush','dev',f'b{i}']);root=socket.inet_ntoa(struct.pack('!I',0x0ae00000+(i//2)*8+(1 if i%2==0 else 4)));run(['ip','addr','add',root+'/29','dev',f'b{i}'])
def ping(i):return ep(i,['ping','-n','-c','2','-i','.02','-W','1',address(i^1)],False).returncode
def remove(ids):
 n=NL()
 for i in ids:
  for name in [f'o{i}',f'm{i}',f'p{i//2}{i%2}']+([] if b.BRIDGE_POOL else [f'b{i}']):
   ix=n.get(name)
   if ix:n.request(19,info(ix)+attr(27,U(0x4e650001)))
 n.request(17,info()+attr(27,U(0x4e650001)),missing=True)
 if b.BRIDGE_POOL:b.bridge_pool.release({i:n.get(f'b{i}') for i in ids},lambda a:run(a).stdout)
 else:
  for i in ids:b.vlan_member(n,pool,i+1,True,True)
try:
 for i in range(12):
  name=f'nn23-local-{i}';run(['docker','run','-d','--network','none','--name',name,'--label','nullnet-benchmark=20260923','alpine:latest','sleep','infinity']);created.append(name)
 pids=[c['State']['Pid'] for c in json.loads(run(['docker','inspect',*created]).stdout)]
 b.NS_FDS=[os.open(f'/proc/{p}/ns/net',os.O_RDONLY) for p in pids];b.PID_FDS=[os.pidfd_open(p) for p in pids]
 run(['ip','netns','add','nn23-local-proof']);os.setns(os.open('/var/run/netns/nn23-local-proof',os.O_RDONLY),os.CLONE_NEWNET)
 if b.BRIDGE_POOL:
  for i in range(64):b.bridge_pool.create(i)
  nl=NL();pool=None
 else:
  run(['ip','link','add','brpool0','address','02:23:ff:00:00:00','type','bridge','vlan_protocol',('802.1ad' if b.TRANSPARENT else '802.1Q'),'vlan_filtering','1','vlan_default_pvid','0','mcast_snooping','0']);run(['ip','link','set','brpool0','mtu','1080','up']);nl=NL();pool=nl.get('brpool0')
 os.environ.update(NN_POOLED='0' if b.BRIDGE_POOL else '1',NN_NATIVE_NS='1',NN_NATIVE_MACSEC='1',NN_UNIQUE_KEYS='1');locks=[threading.Lock() for _ in range(32)]
 with cf.ThreadPoolExecutor(8) as ex:list(ex.map(lambda i:b.setup(i,'same_macsec',True,pids,locks),range(64)))
 for i in range(64):configure(i)
 with cf.ThreadPoolExecutor(16) as ex:result['pings']=list(ex.map(ping,range(64)))
 assert not any(result['pings']);result['mtu']=ep(0,['ping','-n','-c','3','-W','1','-M','do','-s','1052',address(1)]).stdout;save();print('SAMEHOST_CONNECTIVITY_OK',flush=True)
 run(['ip','macsec','set','m0','tx','sa','0','off']);result['missing_key']=ping(0);result['survivor']=ping(2);assert result['missing_key'] and result['survivor']==0
 run(['ip','macsec','set','m0','tx','sa','0','on']);assert ping(0)==0
 # Delete/reuse the same endpoint/VLAN/SCI identifiers with a fresh key.
 result['reuse']=[]
 for generation in range(1,6):
  remove([0,1]);assert nl.get('o0') is None and nl.get('m0') is None and nl.get('p00') is None
  b.KEY=('%02x'%(generation+32))*32
  for i in [0,1]:b.setup(i,'same_macsec',True,pids,locks);configure(i)
  codes=[ping(0),ping(1),ping(2)];assert not any(codes);result['reuse'].append({'generation':generation,'codes':codes});save()
 # Restart only a benchmark container, resolve its new namespace, recreate its edge.
 oldpid=pids[0];remove([0,1]);os.setns(hostfd,os.CLONE_NEWNET);run(['docker','restart',created[0]])
 pids[0]=int(run(['docker','inspect','-f','{{.State.Pid}}',created[0]]).stdout);assert pids[0]!=oldpid
 os.setns(os.open('/var/run/netns/nn23-local-proof',os.O_RDONLY),os.CLONE_NEWNET);b.KEY='55'*32
 try:b.setup(0,'same_macsec',True,pids,locks);raise AssertionError('Stale generation accepted')
 except RuntimeError as error:
  result['stale_generation_rejected']=str(error);assert 'generation exited' in str(error)
 assert nl.get('o0') is None
 os.close(b.NS_FDS[0]);os.close(b.PID_FDS[0]);b.NS_FDS[0]=os.open(f'/proc/{pids[0]}/ns/net',os.O_RDONLY);b.PID_FDS[0]=os.pidfd_open(pids[0])
 for i in [0,1]:b.setup(i,'same_macsec',True,pids,locks);configure(i)
 result['restart']={'oldpid':oldpid,'newpid':pids[0],'codes':[ping(0),ping(1),ping(2)]};assert not any(result['restart']['codes']);save();print('SAMEHOST_REUSE_RESTART_OK',flush=True)
finally:
 os.setns(hostfd,os.CLONE_NEWNET);run(['ip','netns','del','nn23-local-proof'],False)
 for fd in b.NS_FDS+b.PID_FDS:os.close(fd)
 for name in created:run(['docker','rm','-f',name],False)
 save()
