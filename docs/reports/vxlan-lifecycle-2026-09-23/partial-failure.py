"""Inject a mid-crypto error, clean only the failed edge, then reuse its slot."""
import os,json,pathlib,subprocess,threading
import lifecycle as b
from netlink import NL,attr,U,info
R=pathlib.Path('/tmp/nn23');results=[];created=[];host=os.open('/proc/self/ns/net',os.O_RDONLY)
def run(args,check=True):return subprocess.run(args,capture_output=True,text=True,check=check).stdout
os.environ.update(NN_POOLED='0' if b.BRIDGE_POOL else '1',NN_NATIVE_NS='1',NN_NATIVE_MACSEC='1',NN_UNIQUE_KEYS='1');b.SHARD_SIZE=128
b.native_xfrm.UNIQUE_REQID=os.environ.get('NN_UNIQUE_REQID')=='1';b.native_xfrm.REPLAY_WINDOW=int(os.environ.get('NN_REPLAY_WINDOW','128'))
SHARED=os.environ.get('NN_SHARED_INBOUND')=='1'
try:
 for i in range(2):
  name=f'nn23-fault-{i}';run(['docker','run','-d','--network','none','--name',name,'--label','nullnet-benchmark=20260923','alpine:latest','sleep','infinity']);created.append(name)
 pids=[c['State']['Pid'] for c in json.loads(run(['docker','inspect',*created]))];b.NS_FDS=[os.open(f'/proc/{p}/ns/net',os.O_RDONLY) for p in pids];b.PID_FDS=[os.pidfd_open(p) for p in pids]
 for mode in ['same_macsec','marked_ipsec']:
  run(['ip','netns','add','nn23-fault']);fd=os.open('/var/run/netns/nn23-fault',os.O_RDONLY);os.setns(fd,os.CLONE_NEWNET);os.close(fd)
  if b.BRIDGE_POOL:
   for i in range(16):b.bridge_pool.create(i)
  else:
   run(['ip','link','add','brpool0','address','02:23:ff:03:00:01','type','bridge','vlan_protocol',('802.1ad' if b.TRANSPARENT else '802.1Q'),'vlan_filtering','1','vlan_default_pvid','0','mcast_snooping','0']);run(['ip','link','set','brpool0','up'])
  nl=NL();pool=nl.get('brpool0');locks=[threading.Lock() for _ in range(8)]
  def clean(ids):
   if mode=='marked_ipsec':
    x=b.native_xfrm.Xfrm()
    for i in ids:
     for a,c,d in [('192.0.2.1','192.0.2.2',0),('192.0.2.2','192.0.2.1',1)]:
      for f in [lambda:x.policy(a,c,11000+i*2+d,4790,1000+i,1 if d==0 else 0,True),lambda:x.state(a,c,11000+i*2+d,'',1000+i,inbound=d==1,delete=True)]:
       try:f()
       except RuntimeError as e:
        if not isinstance(e.args[0],tuple) or e.args[0][-1] not in (-2,-3):raise
    x.s.close()
   for i in ids:
    for name in [f'o{i}',f'x{i}',f'm{i}',f'p{i//2}{i%2}']+([] if b.BRIDGE_POOL else [f'b{i}']):
     index=nl.get(name)
     if index:nl.request(19,info(index)+attr(27,U(0x4e660001)))
   nl.request(17,info()+attr(27,U(0x4e660001)),missing=True)
   if b.BRIDGE_POOL:b.bridge_pool.release({i:nl.get(f'b{i}') for i in ids if nl.get(f'b{i}') is not None},run)
   else:
    for i in ids:b.vlan_member(nl,pool,i+1,True,True)
  if mode=='marked_ipsec' and SHARED:run(['ip','xfrm','policy','add','src','192.0.2.2','dst','192.0.2.1','proto','udp','dport','4790','dir','in','tmpl','src','192.0.2.2','dst','192.0.2.1','proto','esp','mode','transport'])
  for i in range(8):b.setup(i,mode,True,pids,locks)
  baseline=b.inventory();target=b.native_crypto.Genl if mode=='same_macsec' else b.native_xfrm.Xfrm;method='call' if mode=='same_macsec' else 'policy';original=getattr(target,method)
  def failure(self,*args,**kwargs):
   if mode=='marked_ipsec' or args[1]==1:raise RuntimeError('NN23 injected mid-crypto failure')
   return original(self,*args,**kwargs)
  setattr(target,method,failure)
  try:
   try:b.setup(8,mode,True,pids,locks);raise AssertionError('Fault did not fire')
   except RuntimeError as e:assert 'injected' in str(e)
  finally:setattr(target,method,original)
  partial=b.inventory();clean([8,9]);assert b.inventory()==baseline
  b.KEY='77'*32
  for i in [8,9]:b.setup(i,mode,True,pids,locks)
  clean([8,9]);assert b.inventory()==baseline
  results.append({'mode':mode,'partial':partial,'survivors':baseline,'restored':b.inventory(),'retry_success':True});(R/('partial-failure-dedicated.json' if b.BRIDGE_POOL else 'partial-failure-transparent.json' if b.TRANSPARENT else 'partial-failure-selected.json' if SHARED else 'partial-failure.json')).write_text(json.dumps(results));print(mode,'PARTIAL_FAILURE_CLEAN_RETRY_OK',flush=True)
  clean(list(range(8)))
  nl.s.close();os.setns(host,os.CLONE_NEWNET);run(['ip','netns','del','nn23-fault'])
finally:
 os.setns(host,os.CLONE_NEWNET);run(['ip','netns','del','nn23-fault'],False)
 for fd in b.NS_FDS+b.PID_FDS:os.close(fd)
 for name in created:run(['docker','rm','-f',name],False)
