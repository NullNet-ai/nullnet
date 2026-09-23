import concurrent.futures as cf,importlib.util,json,pathlib,subprocess,time,os
MODE=os.environ.get("NN_PROOF_MODE","pooled")
ROOT=pathlib.Path(__file__).resolve().parent
spec=importlib.util.spec_from_file_location('wire',ROOT/'wire-analysis.py');w=importlib.util.module_from_spec(spec);spec.loader.exec_module(w)
results={'configuration':{'port':4790 if MODE.startswith('pooled_marked') or MODE=='dedicated_marked' else '21000–21031' if MODE=='current_docker' else 4789,'replay_window':4096 if MODE in ('pooled_marked_final','pooled_marked_conditional','dedicated_marked') else 128}};created={0:[],1:[]};pids={};admitted=[]
def record(): (ROOT/('dedicated-marked-proof.json' if MODE=='dedicated_marked' else 'pooled-marked-conditional-proof.json' if MODE=='pooled_marked_conditional' else 'pooled-marked-final-proof.json' if MODE in ('pooled_marked_final','pooled_marked_conditional','dedicated_marked') else 'pooled-marked-shared-in-proof.json' if MODE=='pooled_marked_shared_in' else 'current-docker-proof.json' if MODE=='current_docker' else 'pooled-marked-proof.json' if MODE=='pooled_marked' else 'pooled-proof.json')).write_text(json.dumps(results,indent=2))
def ep(side,i,args,check=True):return w.remote(side,['nsenter','-t',str(pids[side][i%12]),'-n',*args],check=check)
def pings():
 checks=[]
 for side in [0,1]:
  with cf.ThreadPoolExecutor(16) as ex:
   r=list(ex.map(lambda i:ep(side,i,['ping','-n','-c','2','-i','.02','-W','1',f'10.223.{i}.{2-side}'],False).returncode,range(32)))
  checks.append({'side':side,'success':r.count(0),'codes':r})
 return checks

def start_probe(side,i,mode,iface,file,endpoint=False):
 args=['nsenter','-t',str(pids[side][i%12]),'-n'] if endpoint else ['ip','netns','exec','nn23-wire']
 import shlex
 line='nohup '+shlex.join(args+['python3','/tmp/packet-probe.py',mode,iface,file])+' >/tmp/nn23-probe.log 2>&1 & echo $!'
 return int(w.remote(side,['sh','-c',line]).stdout)
def macstats(side):return w.net(side,['ip','-s','macsec','show','m0']).stdout
try:
 while any(w.remote(s,['systemctl','is-active','--quiet',unit],check=False).returncode==0 for s in [0,1] for unit in ['nn23-before','nn23-shards','nn23-churn-fixed','nn23-validated']):time.sleep(10)
 for side in [0,1]:
  mid=next(m['id'] for m in json.loads(w.remote(side,['bpftool','-j','map','show']).stdout) if m.get('name')=='PEERS')
  w.remote(side,['bpftool','map','update','id',str(mid),'key','hex',f'{104-side:02x}','17','12','c6','value','hex','01','noexist']);admitted.append((side,mid))
  for i in range(12):
   name=f'nn23-wire-{i}';w.remote(side,['docker','run','-d','--network','none','--name',name,'--label','nullnet-benchmark=20260923','alpine:latest','sleep','infinity']);created[side].append(name)
  containers=json.loads(w.remote(side,['docker','inspect',*created[side]]).stdout);pids[side]=[c['State']['Pid'] for c in containers]
  w.remote(side,['python3','-c',"import pathlib;pathlib.Path('/tmp/nn23-wire-pids.json').write_text("+repr(json.dumps(pids[side]))+")"])
 w.both('underlay');results['install']=w.both('install',MODE,32);record();print('INSTALLED',flush=True)
 results['connectivity']=pings();assert all(r['success']==32 for r in results['connectivity']);record();print('CONNECTIVITY_OK',flush=True)
 results['root_to_container']=w.net(0,['ping','-n','-c','3','-W','1','10.223.0.2']).stdout
 results['mtu']=ep(0,0,['ping','-n','-c','3','-W','1','-M','do','-s','1052','10.223.0.2']).stdout;record()
 results['throughput']=[]
 import shlex
 server=w.remote(1,['sh','-c','nohup '+shlex.join(['nsenter','-t',str(pids[1][0]),'-n','iperf3','-s','-B','10.223.0.2'])+' >/tmp/nn23-pool-iperf.log 2>&1 & echo $!']).stdout.strip()
 try:
  for reverse in [False,True]:
   for streams in [1,8]:
    data=json.loads(ep(0,0,['iperf3','-c','10.223.0.2','-t','5','-P',str(streams),'-J']+(['-R'] if reverse else [])).stdout)
    results['throughput'].append({'reverse':reverse,'streams':streams,'result':data});record()
  results['udp_small']=json.loads(ep(0,0,['iperf3','-c','10.223.0.2','-t','3','-u','-b','10M','-l','100','-J']).stdout);record()
 finally:w.remote(1,['kill',server],False)
 if MODE=='current_docker' or os.environ.get('NN_TRAFFIC_ONLY')=='1':
  record();print('CURRENT_DOCKER_COMPLETE',flush=True);raise SystemExit(0)
 # Positive control, other VLAN, and double-tagged other VLAN.
 results['vlan_isolation']=[]
 for tags,target,expected in [([],0,3),([1],0,3),([2],1,0),([1,2],1,0)]:
  path=f'/tmp/nn23-probe-{time.time_ns()}.json';start_probe(1,target,'listen',f'in{target}',path,True);time.sleep(.25)
  ep(0,0,['python3','/tmp/packet-probe.py','inject','in0',json.dumps(tags),f'10.223.{target}.2']);time.sleep(3.1)
  r=json.loads(w.remote(1,['cat',path]).stdout);r.update(tags=tags,target=target,expected=expected);results['vlan_isolation'].append(r);record();assert r['received']==expected,r
 print('VLAN_ISOLATION_OK',flush=True)
 if MODE=='pooled':
  # Capture an authenticated frame, advance beyond the MACsec replay window.
  frame='/tmp/nn23-oldframe.json';start_probe(0,0,'capture','x0',frame);time.sleep(.2);ep(0,0,['ping','-n','-c','3','-W','1','10.223.0.2']);time.sleep(.3)
  ep(0,0,['ping','-n','-c','300','-i','.002','-W','1','10.223.0.2']);results['replay_before']=macstats(1)
  w.net(0,['python3','/tmp/packet-probe.py','replay','x0',frame]);time.sleep(.3);results['replay_after']=macstats(1)
  # A valid edge-0 ciphertext deliberately sent through edge-1's VNI.
  results['wrong_vni_before']=w.net(1,['ip','-s','macsec','show','m1']).stdout
  w.net(0,['python3','/tmp/packet-probe.py','replay','x1',frame]);time.sleep(.3);results['wrong_vni_after']=w.net(1,['ip','-s','macsec','show','m1']).stdout;record()
  # Plaintext frames on the transport parent must not enter the endpoint.
  path=f'/tmp/nn23-plaintext-{time.time_ns()}.json';start_probe(1,0,'listen','in0',path,True);time.sleep(.2)
  w.net(0,['python3','/tmp/packet-probe.py','inject','x0','[]','10.223.0.2']);time.sleep(3.1);results['plaintext']=json.loads(w.remote(1,['cat',path]).stdout);assert results['plaintext']['received']==0;record()
  # Wrong RX key: edge 0 fails; independent edge 1 continues.
  peer='02:23:01:00:00:01';w.net(1,['ip','macsec','set','m0','rx','port','1','address',peer,'sa','0','off']);w.net(1,['ip','macsec','del','m0','rx','port','1','address',peer,'sa','0'])
  w.net(1,['ip','macsec','add','m0','rx','port','1','address',peer,'sa','0','pn','1','on','key','00'*16,'99'*32])
  results['wrong_key']=ep(0,0,['ping','-n','-c','2','-W','1','10.223.0.2'],False).returncode;results['unrelated_edge']=ep(0,1,['ping','-n','-c','3','-W','1','10.223.1.2']).returncode
  assert results['wrong_key']!=0 and results['unrelated_edge']==0;results['wrong_key_stats']=macstats(1);record();print('CRYPTO_FAILURE_ISOLATION_OK',flush=True)
  # Parent deletion removes MACsec and stops only its own edge.
  w.net(0,['ip','link','del','x0']);results['deleted_edge']=ep(0,0,['ping','-n','-c','2','-W','1','10.223.0.2'],False).returncode;results['survivor']=ep(0,1,['ping','-n','-c','3','-W','1','10.223.1.2']).returncode
  assert results['deleted_edge']!=0 and results['survivor']==0;record();print('PROOF_COMPLETE',flush=True)
 else:
  results['edge_reuse']=[]
  for generation in range(1,4):
   for side in [0,1]:
    if MODE=='dedicated_marked':
     w.net(side,['ip','neigh','replace','10.223.0.99','lladdr','02:23:aa:bb:cc:dd','nud','permanent','dev','gw0'])
     w.net(side,['bridge','mdb','add','dev','gw0','port','out0','grp','239.23.0.1','permanent'])
    code=f"import sys;sys.path.insert(0,'/tmp');import native_xfrm;native_xfrm.remove(0,'198.18.23.{103+side}','198.18.23.{104-side}',4790,side={side},shared_inbound={MODE in ('pooled_marked_shared_in','pooled_marked_final','pooled_marked_conditional','dedicated_marked')})"
    w.net(side,['python3','-c',code])
    for dev in (['x0','out0'] if MODE=='dedicated_marked' else ['x0','out0','gw0']):w.net(side,['ip','link','del',dev])
    if MODE=='dedicated_marked':
     code="import sys,subprocess;sys.path.insert(0,'/tmp');import bridge_pool;from netlink import NL;bridge_pool.release({0:NL().get('gw0')},lambda args:subprocess.check_output(args,text=True));print(bridge_pool.verify(lambda args:subprocess.check_output(args,text=True)))"
     results.setdefault('clean_pool_resets',[]).append({'generation':generation,'side':side,'proof':w.net(side,['python3','-c',code]).stdout});record()
    else:w.net(side,['bridge','vlan','del','dev','pool','vid','1','self'])
   assert ep(0,0,['ping','-n','-c','1','-W','1','10.223.0.2'],False).returncode!=0
   assert ep(0,1,['ping','-n','-c','1','-W','1','10.223.1.2']).returncode==0
   w.both('reinstall',MODE,1,generation)
   codes=[ep(side,0,['ping','-n','-c','2','-W','1',f'10.223.0.{2-side}']).returncode for side in [0,1]]
   results['edge_reuse'].append({'generation':generation,'codes':codes});record()
  frame='/tmp/nn23-oldesp.json';start_probe(0,0,'capture_esp','underlay',frame);time.sleep(.2);ep(0,0,['ping','-n','-c','3','-W','1','10.223.0.2']);time.sleep(.3)
  ep(0,0,['ping','-n','-c',('4200' if MODE in ('pooled_marked_final','pooled_marked_conditional','dedicated_marked') else '300'),'-i','.002','-W','1','10.223.0.2']);results['replay_before']=w.net(1,['cat','/proc/net/xfrm_stat']).stdout
  w.net(0,['python3','/tmp/packet-probe.py','replay','underlay',frame]);time.sleep(.3);results['replay_after']=w.net(1,['cat','/proc/net/xfrm_stat']).stdout
  def counter(text):return int(dict(line.split() for line in text.splitlines())['XfrmInStateSeqError'])
  assert counter(results['replay_after'])>counter(results['replay_before']);record()
  results['wrong_binding_before']=w.net(1,['tc','-s','filter','show','dev','x1','ingress']).stdout
  w.net(0,['tc','filter','del','dev','x1','egress','pref','1'])
  w.net(0,['tc','filter','add','dev','x1','egress','pref','1','matchall','action','skbedit','mark','1000'])
  results['wrong_binding_ping']=ep(0,1,['ping','-n','-c','3','-W','1','10.223.1.2'],False).returncode
  results['wrong_binding_after']=w.net(1,['tc','-s','filter','show','dev','x1','ingress']).stdout
  results['binding_survivor']=ep(0,0,['ping','-n','-c','3','-W','1','10.223.0.2']).returncode
  assert results['wrong_binding_ping']!=0 and results['binding_survivor']==0
  w.net(0,['tc','filter','del','dev','x1','egress','pref','1'])
  w.net(0,['tc','filter','add','dev','x1','egress','pref','1','matchall','action','skbedit','mark','1001'])
  assert ep(0,1,['ping','-n','-c','3','-W','1','10.223.1.2']).returncode==0
  record();print('SA_VNI_BINDING_OK',flush=True)
  w.net(1,['ip','xfrm','state','delete','src','198.18.23.103','dst','198.18.23.104','proto','esp','spi','11000'])
  code="import sys;sys.path.insert(0,'/tmp');import native_xfrm;x=native_xfrm.Xfrm();x.state('198.18.23.103','198.18.23.104',11000,'99'*32,1000,inbound=True)"
  w.net(1,['python3','-c',code])
  results['wrong_key']=ep(0,0,['ping','-n','-c','2','-W','1','10.223.0.2'],False).returncode
  results['wrong_key_survivor']=ep(0,1,['ping','-n','-c','3','-W','1','10.223.1.2']).returncode
  assert results['wrong_key']!=0 and results['wrong_key_survivor']==0
  results['xfrm_wrong_key']=w.net(1,['ip','-s','xfrm','state']).stdout;record()
  capture,path=w.capture_start(1,'pooled-marked-missing');time.sleep(.3)
  for side in [0,1]:w.net(side,['ip','xfrm','state','flush']);w.net(side,['ip','xfrm','policy','flush'])
  results['missing_crypto']=ep(0,0,['ping','-n','-c','3','-W','1','10.223.0.2'],False).returncode
  results['missing_capture']=w.capture_stop(1,capture,path);assert results['missing_crypto']!=0 and results['missing_capture']['udp_vxlan_lines']==0
  record();print('MARKED_PROOF_COMPLETE',flush=True)

finally:
 for side in [0,1]:
  for pid in w.remote(side,['ip','netns','pids','nn23-wire'],False).stdout.split():w.remote(side,['kill',pid],False)
  w.remote(side,['ip','netns','del','nn23-wire'],False)
  for name in created[side]:w.remote(side,['docker','rm','-f',name],False)
 for side,mid in admitted:w.remote(side,['bpftool','map','delete','id',str(mid),'key','hex',f'{104-side:02x}','17','12','c6'],False)
 record()
