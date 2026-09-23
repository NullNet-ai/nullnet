"""Coordinator: real two-host packet tests, isolated macvlan underlays."""
import concurrent.futures as cf,json,pathlib,shlex,subprocess,time
ROOT=pathlib.Path(__file__).resolve().parent;results=[]
SSH=['ssh','-o','ControlMaster=auto','-o','ControlPath=/tmp/nn23-ssh-%h','-o','ControlPersist=600','-o','ConnectTimeout=8']
def remote(side,args,check=True,timeout=180):
 cmd='printf "debian\\n" | sudo -S -p "" '+shlex.join(args)
 p=subprocess.run(SSH+[f'debian@192.168.1.{103+side}',cmd],capture_output=True,text=True,timeout=timeout)
 if check and p.returncode:raise RuntimeError((side,args,p.stdout,p.stderr))
 return p

def both(action,*args):
 with cf.ThreadPoolExecutor(2) as ex:return list(ex.map(lambda s:remote(s,['python3','/tmp/traffic-node.py',action,str(s),*map(str,args)]).stdout,[0,1]))
def net(side,args,**kw):return remote(side,['ip','netns','exec','nn23-wire',*args],**kw)
def save(): (ROOT/'wire-results.json').write_text(json.dumps(results,indent=2))
def pingall(side,n=32):
 code="import concurrent.futures as c,subprocess,json,time; t=time.monotonic(); f=lambda i: subprocess.run(['ping','-n','-c','2','-i','0.02','-W','1','10.223.%d.%d'%(i,"+str(2-side)+")],capture_output=True,text=True).returncode; ex=c.ThreadPoolExecutor(32); r=list(ex.map(f,range("+str(n)+"))); print(json.dumps({'seconds':time.monotonic()-t,'success':r.count(0),'errors':r.count(1),'codes':r}))"
 return json.loads(net(side,['python3','-c',code]).stdout)
def capture_start(side,name):
 path=f'/tmp/nn23-{name}-{time.time_ns()}.pcap';cmd=f'nohup ip netns exec nn23-wire tcpdump -U -n -i underlay -w {path} "ip proto 50 or udp portrange 4789-4790 or udp portrange 21000-21031" >/tmp/nn23-capture.log 2>&1 & echo $!'
 return int(remote(side,['sh','-c',cmd]).stdout.strip()),path
def capture_stop(side,pid,path):
 remote(side,['kill','-INT',str(pid)]);time.sleep(.3)
 out=remote(side,['tcpdump','-n','-r',path],check=False).stdout
 (ROOT/(path.split('/')[-1]+'.txt')).write_text(out)
 return {'esp_lines':sum('ESP(' in l for l in out.splitlines()),'udp_vxlan_lines':sum('VXLAN' in l for l in out.splitlines()),'capture':path,'lines':len(out.splitlines())}
def main():
 try:
  # Leave the performance trials uncontended before starting packet work.
  while any(remote(s,['systemctl','is-active','--quiet','nn23-suite'],check=False).returncode==0 for s in [0,1]):time.sleep(15)
  print('LIFECYCLE_FINISHED',flush=True)
  # Admit only the isolated benchmark peer through the existing firewall.
  for side in [0,1]:
   maps=json.loads(remote(side,['bpftool','-j','map','show']).stdout);mid=next(m['id'] for m in maps if m.get('name')=='PEERS')
   octets=[f'{104-side:02x}','17','12','c6']
   remote(side,['bpftool','map','update','id',str(mid),'key','hex',*octets,'value','hex','01','noexist'])
  both('underlay');print(net(0,['ping','-n','-c','3','-W','1','198.18.23.104']).stdout,flush=True)
  for mode in ['current','shared','macsec','marked']:
   installs=both('install',mode,32);row={'mode':mode,'installs':installs};results.append(row);save();print('INSTALLED',mode,flush=True)
   pid,path=capture_start(1,mode+'-healthy');time.sleep(.3)
   row['connectivity']=[pingall(0),pingall(1)]
   row['mtu_ping']=net(0,['ping','-n','-c','3','-W','1','-M','do','-s','1052','10.223.0.2'],check=False).stdout
   row['healthy_capture']=capture_stop(1,pid,path);save();print(mode,row['connectivity'],row['healthy_capture'],flush=True)
   if all(r['success']==32 for r in row['connectivity']):
    remote(1,['sh','-c','nohup ip netns exec nn23-wire iperf3 -s -B 10.223.0.2 >/tmp/nn23-iperf.log 2>&1 & echo $!'])
    row['iperf']=[]
    for reverse in [False,True]:
     for streams in [1,8]:
      r=net(0,['iperf3','-c','10.223.0.2','-t','5','-P',str(streams),'-J']+(['-R'] if reverse else []),check=False)
      data=json.loads(r.stdout);row['iperf'].append({'reverse':reverse,'streams':streams,'result':data});save()
    row['udp_small']=json.loads(net(0,['iperf3','-c','10.223.0.2','-t','3','-u','-b','10M','-l','100','-J'],check=False).stdout);save()
    remote(1,['pkill','-f','^iperf3 -s -B 10.223.0.2$'],check=False)
   # Snapshot dummy test keys only, never product XFRM state.
   row['snapshots']=both('snapshot');save()
   pid,path=capture_start(1,mode+'-missing');time.sleep(.3)
   if mode=='macsec':
    net(0,['ip','macsec','set','m0','tx','sa','0','off'])
   else:
    for side in [0,1]:
     net(side,['ip','xfrm','policy','flush']);net(side,['ip','xfrm','state','flush'])
   row['missing_crypto_ping']=net(0,['ping','-n','-c','3','-W','1','10.223.0.2'],check=False).stdout
   row['missing_capture']=capture_stop(1,pid,path);save();print(mode,'MISSING',row['missing_crypto_ping'],row['missing_capture'],flush=True)
   both('clear')
 finally:
  for side in [0,1]:
   p=remote(side,['ip','netns','pids','nn23-wire'],check=False)
   for pid in p.stdout.split():remote(side,['kill',pid],check=False)
   remote(side,['ip','netns','del','nn23-wire'],check=False)
  for side in [0,1]:
   maps=json.loads(remote(side,['bpftool','-j','map','show']).stdout);mid=next(m['id'] for m in maps if m.get('name')=='PEERS')
   remote(side,['bpftool','map','delete','id',str(mid),'key','hex',f'{104-side:02x}','17','12','c6'],check=False)
  save()
 print('WIRE_COMPLETE',flush=True)

if __name__=='__main__':main()
