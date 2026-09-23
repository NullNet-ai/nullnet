"""Isolated endpoint lifecycle attribution; no product code or root-netns policies."""
import concurrent.futures as cf
import hashlib,json,os,pathlib,socket,struct,subprocess,sys,threading,time
from netlink import NL as RawNL,U,attr,info
import native_crypto
import native_xfrm
import native_namespace
import native_vlan
import bridge_pool
BRIDGE_POOL=os.environ.get("NN_BRIDGE_POOL")=="1"
TRANSPARENT=bool(os.environ.get("NN_TRANSPARENT"))
CONDITIONAL=os.environ.get("NN_TRANSPARENT")=="conditional"
ECHO=os.environ.get("NN_LINK_ECHO")=="1"
import select
NS_FDS=[]
PID_FDS=[]
EDGE_LOCKS=[threading.Lock() for _ in range(4096)]
SHARD_SIZE=int(os.environ.get('NN_SHARD_SIZE','256'))
BASE='nn23-base'; SLOTS=threading.Semaphore(8); TLS=threading.local(); KEY='11'*32
AEAD='0x'+KEY+hashlib.sha256(KEY.encode()).hexdigest()[:8]

class NL(RawNL):
 def request(self,kind,payload,flags=5,missing=False):
  start=time.monotonic()
  result=super().request(kind,payload,flags,missing)
  if hasattr(TLS,'operations'):
   family=payload[0];fields=dict(native_crypto.attrs(payload[16:])) if kind not in (20,24,36,44) else {}
   name=fields.get(3,b'').rstrip(b'\0').decode(errors='replace')
   index=struct.unpack_from('i',payload,4)[0]
   names=getattr(self,'names',{})
   if name and result[0]:names[result[0]]=name;self.names=names
   name=name or names.get(index,'')
   cls=('endpoint veth' if name.startswith(('o','i')) else 'gateway' if name.startswith('b') else 'VXLAN' if name.startswith('x') else 'MACsec' if name.startswith('m') else 'transport veth' if name.startswith('p') else 'link')
   op={16:'create',18:'lookup',19:'configure',20:'address',17:'delete'}.get(kind,str(kind))
   if family==7:cls='VLAN membership'
   if kind==24:cls='namespace default route';op='add'
   if kind==36:cls='TC qdisc';op='create'
   if kind==44:
    parent,pref=struct.unpack_from('II',payload,12);cls='TC outbound mark' if parent==0xfffffff3 else 'TC inbound accept' if pref>>16==1 else 'TC inbound drop';op='create'
   TLS.operations.append({'step':cls+' '+op,'seconds':time.monotonic()-start})
  return result
native_crypto.NL=NL

def crypto_metric(cmd,seconds):
 if hasattr(TLS,'operations'):TLS.operations.append({'step':cmd if isinstance(cmd,str) else {1:'MACsec RX channel',4:'MACsec TX SA',7:'MACsec RX SA'}.get(cmd,'MACsec family lookup'),'seconds':seconds})
native_crypto.METRIC=crypto_metric
native_xfrm.METRIC=crypto_metric

def command(args,data=None):
 t=time.monotonic()
 with SLOTS:
  entered=time.monotonic(); p=subprocess.run(args,input=data,text=True,capture_output=True,timeout=180)
  done=time.monotonic()
 if hasattr(TLS,'commands'):TLS.commands.append({'command':' '.join(args[:4]),'argv':args,'queue':entered-t,'execute':done-entered})
 if p.returncode:raise RuntimeError((args,p.stderr))
 return p.stdout

def ipbatch(lines):return command(['ip','-batch','-'],'\n'.join(lines)+'\n')
def mac(i,side):return '02:23:%02x:%02x:00:%02x'%((i>>8)&255,i&255,side+1)
def maclines(name,parent,peer,key=KEY):
 return [f'link add link {parent} {name} type macsec port 1 cipher gcm-aes-256 encrypt on',f'macsec add {name} tx sa 0 pn 1 on key {"00"*16} {key}',f'macsec add {name} rx port 1 address {peer} on',f'macsec add {name} rx port 1 address {peer} sa 0 pn 1 on key {"00"*16} {key}']
def xlines(i,delete=False):
 port=20000+i;spi=101000+i; lines=[]
 for a,b,d in [('192.0.2.1','192.0.2.2','out'),('192.0.2.2','192.0.2.1','in')]:
  if delete:
   lines += [f'xfrm policy delete src {a} dst {b} proto udp dport {port} dir {d}',f'xfrm state delete src {a} dst {b} proto esp spi {spi}']
  else:
   lines += [f'xfrm state add src {a} dst {b} proto esp spi {spi} aead rfc4106(gcm(aes)) {AEAD} 128 mode transport',f'xfrm policy add src {a} dst {b} proto udp dport {port} dir {d} tmpl src {a} dst {b} proto esp spi {spi} mode transport']
 return lines

def vlan_member(nl,index,vid,self_port=False,delete=False):
 payload=struct.pack('BBHiII',7,0,0,index,0,0)+attr(26|32768,(attr(0,struct.pack('H',2)) if self_port else b'')+attr(2,struct.pack('HH',0 if self_port or (TRANSPARENT and not CONDITIONAL) else 6,vid)))
 return nl.request(17 if delete else 19,payload)

def setup(i,mode,batched,pids,pairlocks):
 if mode=='same_macsec':
  with EDGE_LOCKS[i//2]:return setup_locked(i,mode,batched,pids,pairlocks)
 return setup_locked(i,mode,batched,pids,pairlocks)

def setup_locked(i,mode,batched,pids,pairlocks):
 start=time.monotonic(); last=start; stages={};TLS.commands=[];TLS.operations=[];nl=NL()
 def stage(name):
  nonlocal last
  now=time.monotonic();stages[name]=now-last;last=now
 try:
  side=i%2;edge=i//2;pid=pids[i%len(pids)];enc_key=hashlib.sha256((KEY+str(edge if mode=='same_macsec' else i)).encode()).hexdigest() if os.environ.get('NN_UNIQUE_KEYS')=='1' else KEY
  current=os.environ.get('NN_CURRENT')=='1';standalone=os.environ.get('NN_STANDALONE')=='1';ns_path=None
  if standalone:
   ns_path=f'/var/run/netns/nn23-end-{i}'
   if os.environ.get('NN_NATIVE_NAMED')=='1':native_namespace.create(ns_path)
   else:command(['ip','netns','add',f'nn23-end-{i}'])
   stage('namespace_create')
  elif current:
   pid=int(command(['docker','inspect','-f','{{.State.Pid}}',f'nn23-bench-{i%len(pids)}']));stage('docker_pid_lookup')
  if PID_FDS:
   poll=select.poll();poll.register(PID_FDS[i%len(pids)],select.POLLIN)
   if poll.poll(0):raise RuntimeError('Container generation exited; refresh discovery')
  out=f'o{i}';inn=f'i{i}';br=f'b{i}';vx=f'x{i}'
  fd=os.open(ns_path,os.O_RDONLY) if ns_path else NS_FDS[i%len(pids)] if NS_FDS else None
  peer=info()+attr(3,inn.encode()+b'\0')+attr(4,U(1080))+(attr(28,U(fd)) if fd is not None else attr(19,U(pid)))
  if not ECHO:nl.get(out)
  created_out=nl.create(out,'veth',attr(1|32768,peer),attr(4,U(1080)))[0]
  if ns_path:os.close(fd)
  stage('endpoint_veth')
  address=socket.inet_ntoa(struct.pack('!I',0x0afe0000+i*4+2));prefix=['nsenter','--net='+ns_path,'--'] if ns_path else ['nsenter','-t',str(pid),'-n']
  if os.environ.get('NN_NATIVE_NS')=='1':native_crypto.configure_namespace(pid,inn,address,ns_path or (f'/proc/self/fd/{fd}' if fd is not None else None),default_route=standalone)
  elif current:
   command(prefix+['ip','addr','add',address+'/30','dev',inn]);stage('namespace_address')
   command(prefix+['ip','link','set',inn,'up'])
  else:command(prefix+['ip','-batch','-'],f'addr add {address}/30 dev {inn}\nlink set {inn} up\n')
  stage('endpoint_namespace')
  if standalone and os.environ.get('NN_NATIVE_NS')!='1':
   gateway=socket.inet_ntoa(struct.pack('!I',0x0afe0000+i*4+1));command(prefix+['ip','route','add','default','via',gateway]);stage('namespace_default_route')
  if not ECHO:nl.get(br)
  pooled=os.environ.get('NN_POOLED')=='1'
  if pooled:
   attach_index=nl.get(f'brpool{i//SHARD_SIZE}');vlan_member(nl,attach_index,i%SHARD_SIZE+1,True)
   created_bridge=nl.create(br,'vlan',attr(1,struct.pack('H',i%SHARD_SIZE+1))+(attr(5,struct.pack('!H',0x88a8)) if TRANSPARENT else b''),attr(5,U(attach_index)))[0]
  elif BRIDGE_POOL:created_bridge=bridge_pool.lease(nl,i,br)
  else:
   created_bridge=nl.create(br,'bridge',b'')[0]
  bi=created_bridge if ECHO or BRIDGE_POOL else nl.get(br)
  if not pooled:attach_index=bi
  nl.addr(bi,socket.inet_ntoa(struct.pack('!I',0x0afe0000+i*4+1)));nl.up(bi);oi=created_out if ECHO else nl.get(out);nl.up(oi,attach_index)
  if pooled:
   if TRANSPARENT:(native_vlan.install_conditional if CONDITIONAL else native_vlan.install)(nl,oi,i%SHARD_SIZE+1)
   vlan_member(nl,oi,i%SHARD_SIZE+1)
  stage('bridge_attach')
  if mode=='same_macsec':
   if current:
    nl.get(f'x{i}');command(['ip','xfrm','state','deleteall','proto','esp','spi',str(101000+edge)]);stage('stale_cross_host_cleanup')
   with pairlocks[edge]:
    parent=f'p{edge}{side}';peername=f'p{edge}{1-side}'
    if nl.get(parent) is None:
     pdata=info()+attr(3,peername.encode()+b'\0')+attr(1,bytes.fromhex(mac(edge,1-side).replace(':','')))
     nl.create(parent,'veth',attr(1|32768,pdata),attr(1,bytes.fromhex(mac(edge,side).replace(':',''))))
   nl.request(19,info(nl.get(parent),1,1)+attr(4,U(1120)));stage('transport_veth')
   if current:nl.get(f'm{i}')
   lines=maclines(f'm{i}',parent,mac(edge,1-side),enc_key)
  else:
   if current:
    for stale in [f'm{i}s',f'm{i}c',f'p{i}s']:nl.get(stale)
    stage('stale_same_host_cleanup')
   port=20000+i if mode=='unique_ipsec' else 4790 if mode=='marked_ipsec' else 4789
   data=attr(1,U(100000+i))+attr(4,socket.inet_aton('192.0.2.1'))+attr(2,socket.inet_aton('192.0.2.2'))+attr(15,struct.pack('!H',port))
   if not ECHO:nl.get(vx)
   created_vx=nl.create(vx,'vxlan',data,attr(1,bytes.fromhex(mac(i,side).replace(':',''))))[0];vi=nl.get(vx)
   if mode=='vxlan_macsec':nl.request(19,info(vi,1,1)+attr(4,U(1120)))
   elif mode!='marked_ipsec':
    nl.up(vi,attach_index)
    if pooled:vlan_member(nl,vi,i%SHARD_SIZE+1)
   stage('vxlan')
   lines=maclines(f'm{i}',vx,mac(i,1-side),enc_key) if mode=='vxlan_macsec' else xlines(i) if mode=='unique_ipsec' else []
  if mode=='marked_ipsec':
   native_xfrm.install(nl,vi,i,'192.0.2.1','192.0.2.2',enc_key,port=4790,shared_inbound=os.environ.get('NN_SHARED_INBOUND')=='1')
  if current and mode=='unique_ipsec':command(['sha256sum'],KEY);stage('key_salt_hash')
  if os.environ.get('NN_NATIVE_MACSEC')=='1' and mode in ('same_macsec','vxlan_macsec'):
   native_crypto.install(nl,f'm{i}',parent if mode=='same_macsec' else vx,mac(edge,1-side) if mode=='same_macsec' else mac(i,1-side),enc_key,replay=True)
  elif batched and lines:ipbatch(lines)
  else:
   for line in lines:command(['ip']+line.split())
  stage('crypto_install')
  if mode in ('same_macsec','vxlan_macsec'):
   mi=nl.get(f'm{i}');nl.up(mi,attach_index)
   if pooled:
    if TRANSPARENT:(native_vlan.install_conditional if CONDITIONAL else native_vlan.install)(nl,mi,i%SHARD_SIZE+1)
    vlan_member(nl,mi,i%SHARD_SIZE+1)
  if mode=='marked_ipsec':
   if TRANSPARENT:(native_vlan.install_conditional if CONDITIONAL else native_vlan.install)(nl,vi,i%SHARD_SIZE+1,1000+i)
   nl.up(vi,attach_index)
   if pooled:vlan_member(nl,vi,i%SHARD_SIZE+1)
  stage('crypto_attach')
  if current:
   command(['sysctl','-w','net.ipv4.ip_forward=1']);stage('ip_forward')
   command(['iptables','-P','FORWARD','ACCEPT']);stage('forward_policy')
  if PID_FDS and poll.poll(0):raise RuntimeError('Container generation exited during setup; cleanup required')
  return {'i':i,'seconds':time.monotonic()-start,'stages':stages,'commands':TLS.commands,'operations':TLS.operations}
 finally:nl.s.close()

def inventory():
 links=json.loads(command(['ip','-j','-d','link','show']));counts={}
 for l in links:
  k=l.get('linkinfo',{}).get('info_kind','other');counts[k]=counts.get(k,0)+1
 counts['states']=sum(l.startswith('src ') for l in command(['ip','xfrm','state']).splitlines())
 counts['policies']=sum(l.startswith('src ') for l in command(['ip','xfrm','policy']).splitlines())
 return counts

def main():
 native_xfrm.UNIQUE_REQID=os.environ.get('NN_UNIQUE_REQID')=='1'
 native_xfrm.REPLAY_WINDOW=int(os.environ.get('NN_REPLAY_WINDOW','128'))
 mode=sys.argv[1];n=int(sys.argv[2]);batched=sys.argv[3]=='batch';delete=sys.argv[4];outfile=sys.argv[5]
 pids=json.loads(pathlib.Path('/tmp/nn23-pids.json').read_text());os.setns(os.open('/var/run/netns/'+BASE,os.O_RDONLY),os.CLONE_NEWNET)
 if os.environ.get('NN_NATIVE_MACSEC')=='1':
  gen=native_crypto.Genl();native_crypto.FAMILY=gen.family();gen.s.close()
 global NS_FDS,PID_FDS
 identity_init=0
 if os.environ.get('NN_CURRENT')!='1' and os.environ.get('NN_STANDALONE')!='1':
  t=time.monotonic();NS_FDS=[os.open(f'/proc/{pid}/ns/net',os.O_RDONLY) for pid in pids];PID_FDS=[os.pidfd_open(pid) for pid in pids];identity_init=time.monotonic()-t
 pairlocks=[threading.Lock() for _ in range((n+1)//2)]
 pool_init=0
 if BRIDGE_POOL:
  t=time.monotonic()
  with cf.ThreadPoolExecutor(32) as ex:list(ex.map(bridge_pool.create,range(n)))
  pool_init=time.monotonic()-t
 if os.environ.get('NN_POOLED')=='1':
  t=time.monotonic()
  for shard in range((n+SHARD_SIZE-1)//SHARD_SIZE):
   name=f'brpool{shard}';command(['ip','link','add',name,'address',f'02:23:ff:00:{shard//256:02x}:{shard%256:02x}','type','bridge','vlan_protocol',('802.1ad' if TRANSPARENT else '802.1Q'),'vlan_filtering','1','vlan_default_pvid','0','mcast_snooping','0']);command(['ip','link','set',name,'mtu','1080','up'])
  pool_init=time.monotonic()-t
 forwarding_init=0
 if os.environ.get('NN_CURRENT')!='1':
  t=time.monotonic();command(['sysctl','-w','net.ipv4.ip_forward=1']);command(['iptables','-P','FORWARD','ACCEPT']);forwarding_init=time.monotonic()-t
 security_init=0
 if mode=='marked_ipsec':
  t=time.monotonic()
  for chain,direction in [('OUTPUT','out'),('INPUT','in')]:command(['iptables','-A',chain,'-p','udp','--dport','4790','-m','policy','--dir',direction,'--pol','none','-j','DROP'])
  security_init=time.monotonic()-t
 before=inventory();shared_init=0
 if mode=='marked_ipsec' and os.environ.get('NN_SHARED_INBOUND')=='1':
  t=time.monotonic();command(['ip','xfrm','policy','add','src','192.0.2.2','dst','192.0.2.1','proto','udp','dport','4790','dir','in','tmpl','src','192.0.2.2','dst','192.0.2.1','proto','esp','mode','transport']);shared_init=time.monotonic()-t
 if mode=='shared_ipsec':
  t=time.monotonic();ipbatch([l.replace('20000','4789') for l in xlines(0)]);shared_init=time.monotonic()-t
 start=time.monotonic();rows=[];errors=[]
 with cf.ThreadPoolExecutor(max_workers=int(os.environ.get('NN_WORKERS','32'))) as ex:
  futures={ex.submit(setup,i,mode,batched,pids,pairlocks):i for i in range(n)}
  for f in cf.as_completed(futures):
   try:r=f.result();r['completion']=time.monotonic()-start;rows.append(r)
   except Exception as e:errors.append({'i':futures[f],'error':str(e)})
 setup_seconds=time.monotonic()-start;objects=inventory();teardown={}
 if errors:print('SETUP_ERRORS',json.dumps(errors),flush=True)
 tdrows=[]
 if os.environ.get('NN_STANDALONE')=='1':
  for i in [0,n-1]:
   gateway=socket.inet_ntoa(struct.pack('!I',0x0afe0000+i*4+1));command(['ip','netns','exec',f'nn23-end-{i}','ping','-n','-c','1','-W','1',gateway])
   assert 'default via '+gateway in command(['ip','-n',f'nn23-end-{i}','route','show','default'])
 def crypto_delete(i):
  TLS.commands=[];TLS.operations=[];t=time.monotonic()
  if mode=='marked_ipsec':native_xfrm.remove(i,'192.0.2.1','192.0.2.2',port=4790,shared_inbound=os.environ.get('NN_SHARED_INBOUND')=='1')
  elif mode=='unique_ipsec':
   if delete=='exact':ipbatch(xlines(i,True))
   else:
    for direction in ['out','in']:command(['ip','xfrm','policy','deleteall','proto','udp','dport',str(20000+i),'dir',direction])
    command(['ip','xfrm','state','deleteall','proto','esp','spi',str(101000+i)])
  elif mode=='same_macsec' and delete=='current':command(['ip','xfrm','state','deleteall','proto','esp','spi',str(101000+i)])
  return {'i':i,'seconds':time.monotonic()-t,'commands':TLS.commands,'operations':TLS.operations}
 tdstart=time.monotonic()
 with cf.ThreadPoolExecutor(max_workers=int(os.environ.get('NN_WORKERS','32'))) as ex:
  tdrows=list(ex.map(crypto_delete,range(n)))
 teardown['crypto_remove']=time.monotonic()-tdstart
 trace=None
 if os.environ.get('NN_TRACE'):
  trace=pathlib.Path('/sys/kernel/tracing/instances/nn23-lifecycle');trace.mkdir()
  for name,value in [('tracing_on','0'),('buffer_size_kb','8192'),('current_tracer','function_graph'),('set_ftrace_filter','rtnl_dellink\nbr_multicast_dev_del\nrcu_barrier\nsynchronize_rcu\nsynchronize_rcu_expedited'),('set_ftrace_pid',str(os.getpid())),('options/sleep-time','1'),('trace',''),('tracing_on','1')]: (trace/name).write_text(value)
 nl=NL();t=time.monotonic();links=json.loads(command(['ip','-j','link','show']));teardown['link_dump']=time.monotonic()-t
 # Match the product's retiring-group work, using batches of 128 endpoints.
 regroup=0;deletion=0;batchrows=[]
 for first in range(0,n,int(os.environ.get('NN_DELETE_BATCH','128'))):
  ids=set(range(first,min(first+int(os.environ.get('NN_DELETE_BATCH','128')),n)));selected=[]
  for l in links:
   name=l['ifname'];idx=None
   if name[:1] in ('b','o','x','m') and name[1:].isdigit():idx=int(name[1:])
   elif name.startswith('p') and name[1:].isdigit():idx=int(name[1:-1])*2+int(name[-1])
   if idx in ids and not (BRIDGE_POOL and name.startswith('b')):selected.append(l)
  t=time.monotonic()
  for l in selected:
   nl.request(19,info(l['ifindex'])+attr(27,U(0x4e630001)),missing=True)
  regroup+=time.monotonic()-t;t=time.monotonic()
  nl.request(17,info()+attr(27,U(0x4e630001)),missing=True);elapsed=time.monotonic()-t;deletion+=elapsed;batchrows.append(elapsed)
 teardown.update(regroup=regroup,link_delete=deletion)
 if trace:
  (trace/'tracing_on').write_text('0');pathlib.Path(outfile+'.trace').write_text((trace/'trace').read_text());(trace/'current_tracer').write_text('nop');trace.rmdir()
 if mode=='shared_ipsec':
  t=time.monotonic();ipbatch([l.replace('20000','4789') for l in xlines(0,True)]);teardown['last_peer_crypto']=time.monotonic()-t
 if mode=='marked_ipsec' and os.environ.get('NN_SHARED_INBOUND')=='1':
  t=time.monotonic();command(['ip','xfrm','policy','delete','src','192.0.2.2','dst','192.0.2.1','proto','udp','dport','4790','dir','in']);teardown['last_peer_crypto']=time.monotonic()-t
 if os.environ.get('NN_POOLED')=='1':
  t=time.monotonic();pools=[nl.get(f'brpool{j}') for j in range((n+SHARD_SIZE-1)//SHARD_SIZE)]
  for i in range(n):vlan_member(nl,pools[i//SHARD_SIZE],i%SHARD_SIZE+1,True,True)
  teardown['vlan_remove']=time.monotonic()-t
  assert all(not r.get('vlans') for r in json.loads(command(['bridge','-j','vlan','show'])) if r['ifname'].startswith('brpool')), 'Remaining bridge VLANs'
 if BRIDGE_POOL:
  t=time.monotonic();indices={int(l['ifname'][1:]):l['ifindex'] for l in links if l['ifname'].startswith('b') and l['ifname'][1:].isdigit()};bridge_pool.release(indices,command);teardown['bridge_reset']=time.monotonic()-t
 if os.environ.get('NN_STANDALONE')=='1':
  t=time.monotonic()
  def remove_namespace(i):
   if os.environ.get('NN_NATIVE_NAMED')=='1':native_namespace.remove(f'/var/run/netns/nn23-end-{i}')
   else:command(['ip','netns','del',f'nn23-end-{i}'])
  with cf.ThreadPoolExecutor(max_workers=32) as ex:list(ex.map(remove_namespace,range(n)))
  teardown['namespace_remove']=time.monotonic()-t
  assert not any(pathlib.Path(f'/var/run/netns/nn23-end-{i}').exists() for i in range(n))
 teardown_seconds=time.monotonic()-tdstart;pool_clean=bridge_pool.verify(command) if BRIDGE_POOL else None;remaining=inventory();peers=0
 for pid in pids:
  ls=json.loads(command(['nsenter','-t',str(pid),'-n','ip','-j','link','show']));peers+=sum(l['ifname'].startswith('i') and l['ifname'][1:].isdigit() for l in ls)
 pool_finalize=0
 if BRIDGE_POOL:
  t=time.monotonic();nl.request(17,info()+attr(27,U(bridge_pool.GROUP)));pool_finalize=time.monotonic()-t
 if os.environ.get('NN_POOLED')=='1':
  t=time.monotonic()
  for shard in range((n+SHARD_SIZE-1)//SHARD_SIZE):command(['ip','link','del',f'brpool{shard}'])
  pool_finalize=time.monotonic()-t
 for fd in NS_FDS+PID_FDS:os.close(fd)
 result=dict(bridge_pool=BRIDGE_POOL,pool_clean=pool_clean,link_echo=ECHO,transparent=TRANSPARENT,conditional_tags=CONDITIONAL,replay_window=native_xfrm.REPLAY_WINDOW,unique_reqid=native_xfrm.UNIQUE_REQID,shared_inbound=os.environ.get('NN_SHARED_INBOUND')=='1',native_named=os.environ.get('NN_NATIVE_NAMED')=='1',identity_init=identity_init,forwarding_init=forwarding_init,endpoint_type='standalone' if os.environ.get('NN_STANDALONE')=='1' else 'docker',security_init=security_init,shard_size=SHARD_SIZE,native_ns=os.environ.get('NN_NATIVE_NS')=='1',native_macsec=os.environ.get('NN_NATIVE_MACSEC')=='1',delete_batch=int(os.environ.get('NN_DELETE_BATCH','128')),pool_finalize=pool_finalize,mode=mode,pooled=os.environ.get('NN_POOLED')=='1',pool_init=pool_init,current_steps=os.environ.get('NN_CURRENT')=='1',n=n,workers=int(os.environ.get('NN_WORKERS','32')),slots=8,batched=batched,delete=delete,setup_seconds=setup_seconds,shared_init=shared_init,teardown_seconds=teardown_seconds,teardown=teardown,delete_batches=batchrows,inventory=objects,before=before,remaining=remaining,remaining_container_peers=peers,errors=errors,endpoints=rows,teardowns=tdrows)
 pathlib.Path(outfile).write_text(json.dumps(result));print(json.dumps({k:v for k,v in result.items() if k not in ('endpoints','teardowns','delete_batches')}),flush=True)
 assert not errors and len(rows)==n and peers==0 and remaining==before,(errors,remaining,before,peers)
if __name__=='__main__':main()
