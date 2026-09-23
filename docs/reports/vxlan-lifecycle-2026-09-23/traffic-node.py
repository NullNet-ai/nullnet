import hashlib,json,os,pathlib,subprocess,sys,time,struct
from netlink import NL,U,attr,info
import native_crypto
import native_xfrm
import native_vlan
import bridge_pool
from lifecycle import vlan_member
GEN=int(sys.argv[5]) if len(sys.argv)>5 else 0
NS='nn23-wire';SIDE=int(sys.argv[2]);OTHER=1-SIDE;LOCAL=f'198.18.23.{103+SIDE}';REMOTE=f'198.18.23.{103+OTHER}'
def run(args,check=True,data=None):
 p=subprocess.run(args,text=True,input=data,capture_output=True,timeout=60)
 if check and p.returncode:raise RuntimeError((args,p.stderr))
 return p.stdout

def ip(line):return run(['ip']+line.split())
def batch(lines):return run(['ip','-batch','-'],data='\n'.join(lines)+'\n')
def mac(i,side):return '02:23:01:%02x:%02x:%02x'%((i>>8)&255,i&255,side+1)
def key(i,direction):return hashlib.sha256(f'nn23-test-only-{i}-{direction}-generation-{GEN}'.encode()).hexdigest()
def state(i,direction,mode):
 a=f'198.18.23.{103+direction}';b=f'198.18.23.{104-direction}';k=key(i,direction if mode!='current' else 0);salt=hashlib.sha256(k.encode()).hexdigest()[:8]
 return f'xfrm state add src {a} dst {b} proto esp spi {11000+i*2+direction} aead rfc4106(gcm(aes)) 0x{k}{salt} 128 mode transport'+(' replay-window 128' if mode!='current' else '')
def policies(i,port,mode):
 lines=[]
 for direction in [SIDE,OTHER]:
  a=f'198.18.23.{103+direction}';b=f'198.18.23.{104-direction}';d='out' if direction==SIDE else 'in'
  lines += [state(i,direction,mode),f'xfrm policy add src {a} dst {b} proto udp dport {port} dir {d} tmpl src {a} dst {b} proto esp spi {11000+i*2+direction} mode transport']
 return lines

action=sys.argv[1]
if action=='underlay':
 run(['ip','netns','add',NS]);ip(f'link add nn23-under link ens18 type macvlan mode bridge');ip(f'link set nn23-under netns {NS}')
 run(['ip','-n',NS,'link','set','nn23-under','name','underlay']);run(['ip','-n',NS,'addr','add',LOCAL+'/24','dev','underlay']);run(['ip','-n',NS,'link','set','underlay','up']);run(['ip','-n',NS,'link','set','lo','up']);print('UNDERLAY_READY');sys.exit()
os.setns(os.open('/var/run/netns/'+NS,os.O_RDONLY),os.CLONE_NEWNET)
if action in ('install','reinstall'):
 mode=sys.argv[3];conditional=mode=='pooled_marked_conditional';dedicated=mode=='dedicated_marked';mode='pooled_marked_final' if conditional or dedicated else mode;n=int(sys.argv[4]);scale=n>128;start=time.monotonic()
 shared_inbound=mode in ('pooled_marked_shared_in','pooled_marked_final')
 if mode=='pooled_marked_final':native_xfrm.REPLAY_WINDOW=4096
 if mode in ('pooled_marked_reqid','pooled_marked_shared_in','pooled_marked_final'):native_xfrm.UNIQUE_REQID=True;mode='pooled_marked'
 if mode in ('pooled','pooled_marked','current_docker'):
  pids=json.loads(pathlib.Path('/tmp/nn23-wire-pids.json').read_text());nl=NL()
  if action=='install' and dedicated:
   for slot in range(n):bridge_pool.create(slot)
  if action=='install' and mode!='current_docker' and not dedicated:
   for shard in range((n+127)//128):
    poolname=f'pool{shard}' if scale else 'pool';ip(f'link add {poolname} address 02:23:ff:01:{shard:02x}:0{SIDE+1} type bridge vlan_protocol {"802.1ad" if conditional else "802.1Q"} vlan_filtering 1 vlan_default_pvid 0 mcast_snooping 0');ip(f'link set {poolname} mtu 1080 up')
  pool=nl.get('pool0' if scale else 'pool') if mode!='current_docker' and not dedicated else None
 if action=='install' and mode in ('shared','marked','native_marked','pooled_marked'):
  # Fail closed even if every XFRM policy disappears.
  for chain,d in [('OUTPUT','out'),('INPUT','in')]:run(['iptables','-A',chain,'-p','udp','--dport',('4790' if mode in ('native_marked','pooled_marked') else '4789'),'-m','policy','--dir',d,'--pol','none','-j','DROP'])
  if mode=='shared':batch(policies(0,4789,mode))
  if shared_inbound:ip(f'xfrm policy add src {REMOTE} dst {LOCAL} proto udp dport 4790 dir in tmpl src {REMOTE} dst {LOCAL} proto esp mode transport')
 for i in range(n):
  vid=i%128+1 if scale else i+1
  if scale and mode!='current_docker' and not dedicated:pool=nl.get(f'pool{i//128}')
  port=21000+i if mode in ('current','current_docker') else 4790 if mode in ('native_marked','pooled_marked') else 4789
  ip(f'link add x{i} address {mac(i,SIDE)} type vxlan id {500000+i} local {LOCAL} remote {REMOTE} dstport {port} nolearning')
  if mode in ('native_marked','pooled_marked'):native_xfrm.install(NL(),NL().get(f'x{i}'),i,LOCAL,REMOTE,key(i,0),port,side=SIDE,shared_inbound=shared_inbound)
  if mode in ('current','current_docker'):batch(policies(i,port,'current'))
  if mode=='marked':
   mark=str(1000+i);lines=[]
   for line in policies(i,4789,mode):
    if 'state add' in line:
     line += (' mark '+mark if 'src '+LOCAL+' ' in line else ' output-mark '+mark)
    else:line=line.replace(' tmpl ', ' mark '+mark+' tmpl ')
    lines.append(line)
   batch(lines)
   run(['tc','qdisc','add','dev',f'x{i}','clsact'])
   run(['tc','filter','add','dev',f'x{i}','egress','pref','1','matchall','action','skbedit','mark',mark])
   run(['tc','filter','add','dev',f'x{i}','ingress','pref','1','handle',mark,'fw','action','pass'])
   run(['tc','filter','add','dev',f'x{i}','ingress','pref','2','matchall','action','drop'])
  if mode in ('macsec','pooled'):
   ip(f'link set x{i} mtu 1120 up');name=f'm{i}';peer=mac(i,OTHER)
   if mode in ('pooled','pooled_marked'):native_crypto.install(nl,name,f'x{i}',peer,key(i,0),replay=True)
   else:batch([f'link add link x{i} {name} type macsec port 1 cipher gcm-aes-256 encrypt on replay on window 128 validate strict',f'macsec add {name} tx sa 0 pn 1 on key {"00"*16} {key(i,0)}',f'macsec add {name} rx port 1 address {peer} on',f'macsec add {name} rx port 1 address {peer} sa 0 pn 1 on key {"00"*16} {key(i,0)}'])
  else:name=f'x{i}'
  if mode in ('pooled','pooled_marked','current_docker'):
   pid=pids[i%len(pids)];peerdata=info()+attr(3,f'in{i}'.encode()+b'\0')+attr(4,U(1080))+attr(19,U(pid))
   nl.create(f'out{i}','veth',attr(1|32768,peerdata),attr(4,U(1080)))
   # Use /24 here solely to match the packet harness address plan.
   fd=os.open('/proc/thread-self/ns/net',os.O_RDONLY);target=os.open(f'/proc/{pid}/ns/net',os.O_RDONLY)
   try:os.setns(target,os.CLONE_NEWNET);ep=NL()
   finally:os.setns(fd,os.CLONE_NEWNET);os.close(fd);os.close(target)
   ix=ep.get(f'in{i}');address=struct.pack('!I',0x0ade0000+i*8+SIDE+1) if scale else __import__('socket').inet_aton(f'10.223.{i}.{SIDE+1}')
   ep.request(20,struct.pack('BBBBI',2,29 if scale else 24,0,0,ix)+attr(1,address)+attr(2,address),5|512|1024);ep.up(ix);ep.s.close()
   if dedicated:pool=bridge_pool.lease(nl,i,f'gw{i}')
   elif mode=='current_docker':nl.create(f'gw{i}','bridge',b'');pool=nl.get(f'gw{i}')
   else:vlan_member(nl,pool,vid,True);nl.create(f'gw{i}','vlan',attr(1,struct.pack('H',vid))+(attr(5,struct.pack('!H',0x88a8)) if conditional else b''),attr(5,U(pool)))
   gateway=__import__('socket').inet_ntoa(struct.pack('!I',0x0ade0000+i*8+SIDE+3)) if scale else f'10.223.{i}.{SIDE+3}'
   batch([f'addr add {gateway}/{29 if scale else 24} dev gw{i}',f'link set gw{i} mtu 1080 up'])
   for dev in [name,f'out{i}']:
    index=nl.get(dev)
    if conditional:native_vlan.install_conditional(nl,index,vid,1000+i if dev.startswith('x') else None)
    nl.up(index,pool)
    if mode!='current_docker' and not dedicated:vlan_member(nl,index,vid)
  else:batch([f'addr add 10.223.{i}.{SIDE+1}/24 dev {name}',f'link set {name} mtu 1080 up'])
 pathlib.Path('/tmp/nn23-wire-mode').write_text(mode);print(json.dumps({'mode':mode,'n':n,'install_seconds':time.monotonic()-start}))
elif action=='clear':
 links=json.loads(ip('-j link show'))
 for l in links:
  if l['ifname'].startswith('x') and l['ifname'][1:].isdigit():ip('link del '+l['ifname'])
 ip('xfrm policy flush');ip('xfrm state flush');run(['iptables','-F']);print('CLEARED')
elif action=='snapshot':
 print(json.dumps({'links':json.loads(ip('-j -s -d link show')),'xfrm':ip('-s xfrm state'),'policies':ip('xfrm policy'),'macsec':ip('-s macsec show')}))
