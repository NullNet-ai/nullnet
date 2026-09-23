"""Concurrent endpoint churn in isolated namespaces, including slot reuse."""
import concurrent.futures as cf,json,os,pathlib,subprocess,threading,time,statistics
import lifecycle as b
from netlink import NL,U,attr,info
ROOT=pathlib.Path('/tmp/nn23');created=[];results=[]
def run(args):return subprocess.run(args,capture_output=True,text=True,check=True).stdout
hostfd=os.open('/proc/self/ns/net',os.O_RDONLY)
os.environ.update(NN_POOLED='0' if b.BRIDGE_POOL else '1',NN_NATIVE_NS='1',NN_NATIVE_MACSEC='1',NN_UNIQUE_KEYS='1')
b.SHARD_SIZE=128
WORKERS=int(os.environ.get('NN_WORKERS','16'));SHARED=os.environ.get('NN_SHARED_INBOUND')=='1'
b.native_xfrm.UNIQUE_REQID=os.environ.get('NN_UNIQUE_REQID')=='1';b.native_xfrm.REPLAY_WINDOW=int(os.environ.get('NN_REPLAY_WINDOW','128'))
try:
 for i in range(12):
  name=f'nn23-churn-{i}';run(['docker','run','-d','--network','none','--name',name,'--label','nullnet-benchmark=20260923','alpine:latest','sleep','infinity']);created.append(name)
 pids=[c['State']['Pid'] for c in json.loads(run(['docker','inspect']+created))]
 b.NS_FDS=[os.open(f'/proc/{p}/ns/net',os.O_RDONLY) for p in pids];b.PID_FDS=[os.pidfd_open(p) for p in pids]
 for mode in ['same_macsec','marked_ipsec']:
  for batch in map(int,os.environ.get('NN_CHURN_BATCHES','16,64,128,256').split(',')):
   run(['ip','netns','add','nn23-churn']);fd=os.open('/var/run/netns/nn23-churn',os.O_RDONLY);os.setns(fd,os.CLONE_NEWNET);os.close(fd)
   run(['ip','link','add','dummy0','type','dummy']);run(['ip','addr','add','192.0.2.1/24','dev','dummy0']);run(['ip','link','set','dummy0','up'])
   nl=NL();pools=[]
   if b.BRIDGE_POOL:
    with cf.ThreadPoolExecutor(32) as ex:list(ex.map(b.bridge_pool.create,range(1024)))
   for j in range(0 if b.BRIDGE_POOL else 8):
    run(['ip','link','add',f'brpool{j}','address',f'02:23:ff:00:00:{j:02x}','type','bridge','vlan_protocol',('802.1ad' if b.TRANSPARENT else '802.1Q'),'vlan_filtering','1','vlan_default_pvid','0','mcast_snooping','0']);run(['ip','link','set',f'brpool{j}','mtu','1080','up']);pools.append(nl.get(f'brpool{j}'))
   if mode=='marked_ipsec':
    for chain,direction in [('OUTPUT','out'),('INPUT','in')]:run(['iptables','-A',chain,'-p','udp','--dport','4790','-m','policy','--dir',direction,'--pol','none','-j','DROP'])
   if mode=='marked_ipsec' and SHARED:run(['ip','xfrm','policy','add','src','192.0.2.2','dst','192.0.2.1','proto','udp','dport','4790','dir','in','tmpl','src','192.0.2.2','dst','192.0.2.1','proto','esp','mode','transport'])
   locks=[threading.Lock() for _ in range(512)];live=list(range(512));rounds=[];base=b.inventory()
   def one(i):
    try:return b.setup(i,mode,True,pids,locks)
    except Exception as e:
     print('CREATE_FAILED',i,repr(e),run(['ip','-j','-d','link','show','brpool'+str(i//128)]),flush=True);raise
   def create(ids):
    admitted=time.monotonic()
    def timed(i):
     row=one(i);row['completion']=time.monotonic()-admitted;return row
    with cf.ThreadPoolExecutor(WORKERS) as ex:return list(ex.map(timed,ids))
   def delete(ids):
    start=time.monotonic()
    if mode=='marked_ipsec':
     with cf.ThreadPoolExecutor(WORKERS) as ex:list(ex.map(lambda i:b.native_xfrm.remove(i,'192.0.2.1','192.0.2.2',4790,shared_inbound=SHARED),ids))
    n=NL();links={x['ifname']:x['ifindex'] for x in json.loads(run(['ip','-j','link']))};batches=[]
    for offset in range(0,len(ids),batch):
     t=time.monotonic()
     for i in ids[offset:offset+batch]:
      for name in [f'o{i}',f'x{i}',f'm{i}']+([] if b.BRIDGE_POOL else [f'b{i}'])+([f'p{i//2}{i%2}'] if mode=='same_macsec' else []):
       ix=links.get(name)
       if ix:n.request(19,info(ix)+attr(27,U(0x4e640001)),missing=True)
     regroup=time.monotonic()-t;deleted=time.monotonic();n.request(17,info()+attr(27,U(0x4e640001)),missing=True);batches.append({'regroup':regroup,'delete':time.monotonic()-deleted,'wall':time.monotonic()-t})
    if b.BRIDGE_POOL:b.bridge_pool.release({i:links[f'b{i}'] for i in ids},run)
    else:
     for i in ids:b.vlan_member(n,pools[i//128],i%128+1,True,True)
    n.s.close();return {'wall':time.monotonic()-start,'batch_wall':batches}
   create(live);nextid=512
   for k in range(6):
    print('ROUND',mode,batch,k,flush=True)
    old=live[:256];new=[i%1024 for i in range(nextid,nextid+256)];nextid+=256;b.KEY=('%02x'%(50+k))*32;start=time.monotonic()
    with cf.ThreadPoolExecutor(2) as ex:
     deleting=ex.submit(delete,old);creating=ex.submit(create,new);newrows=creating.result();create_elapsed=time.monotonic()-start;deleted=deleting.result()
    total=time.monotonic()-start;live=live[256:]+new;inv=b.inventory()
    assert inv['bridge']==(1024 if b.BRIDGE_POOL else 8) and inv.get('vlan',0)==(0 if b.BRIDGE_POOL else 512) and inv['veth']==(1024 if mode=='same_macsec' else 512),inv
    assert inv.get('macsec',0)==(512 if mode=='same_macsec' else 0),inv
    assert inv['states']==(1024 if mode=='marked_ipsec' else 0),inv
    assert inv['policies']==((513 if SHARED else 1024) if mode=='marked_ipsec' else 0),inv
    rounds.append({'round':k,'wall':total,'create_wall':create_elapsed,'delete':deleted,'endpoints':newrows,'live':inv})
   delete(live);inv=b.inventory();assert inv==base,(inv,base)
   peers=sum(sum(x['ifname'].startswith('i') and x['ifname'][1:].isdigit() for x in json.loads(run(['nsenter','-t',str(p),'-n','ip','-j','link']))) for p in pids);assert peers==0
   assert all(not x.get('vlans') for x in json.loads(run(['bridge','-j','vlan','show'])) if x['ifname'].startswith('brpool'))
   row={'mode':mode,'workers':WORKERS,'shared_inbound':SHARED,'batch':batch,'live_endpoints':512,'rounds':rounds,'remaining':inv,'container_peers':peers};results.append(row);(ROOT/('churn-dedicated.json' if b.BRIDGE_POOL else 'churn-transparent.json' if b.TRANSPARENT else 'churn-selected.json' if SHARED else 'churn-final-admission.json')).write_text(json.dumps(results));print(mode,batch,'COMPLETE',flush=True)
   if b.BRIDGE_POOL:b.bridge_pool.verify(run);nl.request(17,info()+attr(27,U(b.bridge_pool.GROUP)))
   nl.s.close();os.setns(hostfd,os.CLONE_NEWNET);run(['ip','netns','del','nn23-churn'])
finally:
 os.setns(hostfd,os.CLONE_NEWNET)
 if 'nn23-churn' in run(['ip','netns','list']):run(['ip','netns','del','nn23-churn'])
 for fd in b.NS_FDS+b.PID_FDS:os.close(fd)
 for name in created:run(['docker','rm','-f',name])
print('CHURN_COMPLETE',flush=True)
