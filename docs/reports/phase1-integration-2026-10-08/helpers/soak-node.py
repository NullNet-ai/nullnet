"""Incremental lifecycle/resource observer, isolated to a bounded lab run."""
import argparse,json,os,pathlib,re,shutil,subprocess,time
p=argparse.ArgumentParser();p.add_argument('--label',required=True);p.add_argument('--duration',type=float,required=True);p.add_argument('--stop-file',required=True);a=p.parse_args()
out=pathlib.Path('/root/nullnet-layer1-20261008/evidence')/a.label;out.mkdir(exist_ok=False,parents=True)
def command(*args):return subprocess.check_output(args,text=True)
units=['nullnet-client','nullnet-server','nullnet-proxy'];pids={u:int(command('systemctl','show',u,'--property=MainPID','--value')) for u in units}
cursors={u:json.loads(command('journalctl','-u',u,'-n','1','-o','json','--no-pager'))['__CURSOR'] for u in units if pids[u]}
total={'setups':0,'retirements':0,'server_releases':0,'setup_failures':0};failures=[];start=time.monotonic();window=[]

def resources():
 result={}
 for unit,pid in pids.items():
  if not pid:continue
  proc=pathlib.Path('/proc')/str(pid)
  if not proc.exists():failures.append({'unit':unit,'error':'process exited or restarted'});continue
  status=proc.joinpath('status').read_text();cg=pathlib.Path('/sys/fs/cgroup/system.slice')/(unit+'.service')
  cpu={k:int(v) for k,v in (line.split() for line in cg.joinpath('cpu.stat').read_text().splitlines())}
  result[unit]={'pid':pid,'rss_kb':int(re.search(r'VmRSS:\s+(\d+)',status)[1]),'fds':len(list(proc.joinpath('fd').iterdir())),'cpu':cpu,'cgroup_memory_bytes':int(cg.joinpath('memory.current').read_text())}
 return result

with out.joinpath('samples.jsonl').open('x') as stream:
 while time.monotonic()-start<a.duration:
  setup_ms=[];retire_ms=[]
  for unit,cursor in list(cursors.items()):
   for line in command('journalctl','-u',unit,'--after-cursor='+cursor,'-o','json','--no-pager').splitlines():
    entry=json.loads(line);cursors[unit]=entry['__CURSOR'];m=entry.get('MESSAGE','');when=int(entry['__REALTIME_TIMESTAMP'])/1e6
    if unit=='nullnet-client':
     match=re.search(r'VXLAN \d+ setup completed in (\d+) ms',m)
     if match:total['setups']+=1;setup_ms.append(int(match[1]))
     match=re.search(r'VXLAN \d+ teardown completed in (\d+) ms',m)
     if match:total['retirements']+=1;retire_ms.append(int(match[1]))
     if re.search(r'VXLAN \d+ setup FAILED',m):total['setup_failures']+=1
     if re.search(r'VXLAN \d+ (setup|teardown) FAILED',m):failures.append({'unix':when,'message':m})
    if unit=='nullnet-server' and re.search(r'Network \d+ teardown complete; ID released',m):total['server_releases']+=1
    if re.search(r'\[egress\].*(refusing steer|partial install|failed)|egress_trigger timeout',m):failures.append({'unix':when,'message':m})
    if re.search(r'(Suppressed \d+ messages|queue.*overflow|history.*dropped)',m,re.I):failures.append({'unix':when,'message':m})
  owned=[name for name in os.listdir('/sys/class/net') if re.match(r'^(br_\d+_[sc]|nnv_\d+_[sc]|vxlan-ns_\d+_[sc]|ns_\d+_[sc]-(out|o)|veth-\d+-[sc]|macsec-\d+-?[sc])$',name)]
  usage=resources();available=int(re.search(r'MemAvailable:\s+(\d+)',pathlib.Path('/proc/meminfo').read_text())[1]);free=shutil.disk_usage(out).free
  guard=None
  if free<1024**3:guard='disk free below 1 GiB'
  if available<1024**2:guard='available host RAM below 1 GiB'
  if len(owned)>40000:guard='owned root interface population exceeds 40000'
  if any(r['fds']>131072 for r in usage.values()):guard='service descriptors exceed 131072'
  if failures:guard='lifecycle or process failure'
  row={'cpu_count':os.cpu_count(),'unix':time.time(),'elapsed':time.monotonic()-start,'counts':dict(total),'completed_not_retired':total['setups']+total['setup_failures']-total['retirements'],'owned_root_links':len(owned),'resources':usage,'available_ram_kb':available,'disk_free_bytes':free,'setup_ms':{'count':len(setup_ms),'sum':sum(setup_ms),'max':max(setup_ms,default=0)},'teardown_ms':{'count':len(retire_ms),'sum':sum(retire_ms),'max':max(retire_ms,default=0)},'failures':failures[-10:],'stop_reason':guard}
  stream.write(json.dumps(row)+'\n');stream.flush();tmp=out/'status.tmp';tmp.write_text(json.dumps(row));tmp.replace(out/'status.json')
  if guard:
   pathlib.Path(a.stop_file).touch();out.joinpath('guard.json').write_text(json.dumps(row,indent=2)+'\n')
  time.sleep(10)
