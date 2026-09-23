"""Real 1,000-edge cross-host connectivity and first/last-policy traffic."""
import concurrent.futures as cf,importlib.util,json,pathlib,shlex,subprocess,time,socket,struct,os
R=pathlib.Path(__file__).resolve().parent
spec=importlib.util.spec_from_file_location('wire',R/'wire-analysis.py');w=importlib.util.module_from_spec(spec);spec.loader.exec_module(w)
results=[]
while any(w.remote(s,['systemctl','is-active','--quiet',unit],False).returncode==0 for s in [0,1] for unit in ['nn23-final32','nn23-selected']):time.sleep(3)
def addr(i,side):return socket.inet_ntoa(struct.pack('!I',0x0ade0000+i*8+side+1))
for mode in os.environ.get('NN_SCALE_MODES','current_docker,pooled_marked').split(','):
 created={0:[],1:[]};pids={};admitted=[];row={'mode':mode,'n':1000};results.append(row)
 def save():(R/(os.environ.get('NN_SCALE_OUTPUT','scale-policy-experiment.json' if os.environ.get('NN_SCALE_POLICY')=='1' else 'scale-proof.json'))).write_text(json.dumps(results,indent=2))
 try:
  for side in [0,1]:
   mid=next(m['id'] for m in json.loads(w.remote(side,['bpftool','-j','map','show']).stdout) if m.get('name')=='PEERS');w.remote(side,['bpftool','map','update','id',str(mid),'key','hex',f'{104-side:02x}','17','12','c6','value','hex','01','noexist']);admitted.append((side,mid))
   for i in range(12):
    name=f'nn23-scale-{i}';w.remote(side,['docker','run','-d','--network','none','--name',name,'--label','nullnet-benchmark=20260923','alpine:latest','sleep','infinity']);created[side].append(name)
   pids[side]=[c['State']['Pid'] for c in json.loads(w.remote(side,['docker','inspect',*created[side]]).stdout)];w.remote(side,['python3','-c',"import pathlib;pathlib.Path('/tmp/nn23-wire-pids.json').write_text("+repr(json.dumps(pids[side]))+")"])
  w.both('underlay');row['xfrm_phases']={'before_install':[w.net(s,['cat','/proc/net/xfrm_stat']).stdout for s in [0,1]]};row['install']=w.both('install',mode,1000);row['xfrm_phases']['after_install']=[w.net(s,['cat','/proc/net/xfrm_stat']).stdout for s in [0,1]];save();print('SCALE_INSTALLED',mode,flush=True)
  def ping(side):
   code="import concurrent.futures as c,subprocess,json,socket,struct,time; pids="+repr(pids[side])+"; t=time.monotonic(); f=lambda i:subprocess.run(['nsenter','-t',str(pids[i%12]),'-n','ping','-n','-c','1','-W','2',socket.inet_ntoa(struct.pack('!I',0x0ade0000+i*8+"+str(2-side)+"))],capture_output=True).returncode; ex=c.ThreadPoolExecutor(32); r=list(ex.map(f,range(1000)));print(json.dumps({'success':r.count(0),'codes':r,'seconds':time.monotonic()-t}))"
   return json.loads(w.remote(side,['python3','-c',code]).stdout)
  with cf.ThreadPoolExecutor(2) as ex:row['connectivity']=list(ex.map(ping,[0,1]))
  assert all(r['success']==1000 for r in row['connectivity']);row['xfrm_phases']['after_pings']=[w.net(s,['cat','/proc/net/xfrm_stat']).stdout for s in [0,1]];row['traffic']=[];save();print('SCALE_2000_PINGS_OK',mode,flush=True)
  for i in [0,999]:
   target=addr(i,1);server=w.remote(1,['sh','-c','nohup '+shlex.join(['nsenter','-t',str(pids[1][i%12]),'-n','iperf3','-s','-B',target])+' >/tmp/nn23-scale-iperf.log 2>&1 & echo $!']).stdout.strip()
   try:
    for reverse in [False,True]:
     for streams in [1,8]:
      args=['nsenter','-t',str(pids[0][i%12]),'-n','iperf3','-c',target,'-t','3','-P',str(streams),'-J']+(['-R'] if reverse else []);data=json.loads(w.remote(0,args).stdout);assert 'error' not in data;row['traffic'].append({'edge':i,'reverse':reverse,'streams':streams,'result':data,'xfrm_stats':[w.net(s,['cat','/proc/net/xfrm_stat']).stdout for s in [0,1]]});save()
   finally:w.remote(1,['kill',server],False)
  if os.environ.get('NN_SCALE_POLICY')=='1':
   row['policy_experiment']=[]
   for variant in ['per_edge_inbound','shared_inbound']:
    if variant=='shared_inbound':
     for side in [0,1]:
      local=f'198.18.23.{103+side}';remote=f'198.18.23.{104-side}'
      w.net(side,['ip','xfrm','policy','deleteall','dir','in'])
      w.net(side,['ip','xfrm','policy','add','src',remote,'dst',local,'proto','udp','dport','4790','dir','in','tmpl','src',remote,'dst',local,'proto','esp','mode','transport'])
    with cf.ThreadPoolExecutor(2) as ex:checks=list(ex.map(ping,[0,1]))
    assert all(x['success']==1000 for x in checks)
    sample={'variant':variant,'connectivity':checks,'traffic':[]};row['policy_experiment'].append(sample);save()
    perf=[]
    for side in [0]:
     path=f'/tmp/nn23-{variant}-{side}.perf';pid=w.remote(side,['sh','-c','nohup '+shlex.join(['perf','record','-a','-e','cpu-clock:k','-F','99','-o',path,'--','sleep','12'])+' >/tmp/nn23-perf.log 2>&1 & echo $!']).stdout.strip();perf.append((side,path,pid))
    for i in [0,999]:
     target=addr(i,1);server=w.remote(1,['sh','-c','nohup '+shlex.join(['nsenter','-t',str(pids[1][i%12]),'-n','iperf3','-s','-B',target])+' >/tmp/nn23-scale-iperf.log 2>&1 & echo $!']).stdout.strip()
     try:
      data=json.loads(w.remote(0,['nsenter','-t',str(pids[0][i%12]),'-n','iperf3','-c',target,'-t','5','-P','8','-J']).stdout);sample['traffic'].append({'edge':i,'result':data});save()
     finally:w.remote(1,['kill',server],False)
    time.sleep(2)
    sample['perf']=[]
    for side,path,pid in perf:sample['perf'].append(w.remote(side,['perf','report','-i',path,'--stdio','--no-children','--percent-limit','0.1'],False).stdout)
    save();print('POLICY_VARIANT',variant,flush=True)
  row['xfrm_stats']=[w.net(s,['cat','/proc/net/xfrm_stat']).stdout for s in [0,1]];save();print('SCALE_COMPLETE',mode,flush=True)
 finally:
  for side in [0,1]:
   for pid in w.remote(side,['ip','netns','pids','nn23-wire'],False).stdout.split():w.remote(side,['kill',pid],False)
   w.remote(side,['ip','netns','del','nn23-wire'],False)
   for name in created[side]:w.remote(side,['docker','rm','-f',name],False)
  for side,mid in admitted:w.remote(side,['bpftool','map','delete','id',str(mid),'key','hex',f'{104-side:02x}','17','12','c6'],False)
  save()
