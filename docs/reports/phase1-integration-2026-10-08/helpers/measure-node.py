"""Read CPU, scoped lifecycle log records and remaining owned links."""
import json,pathlib,re,subprocess,sys,time
root=pathlib.Path('/root/nullnet-layer1-20261008');mode,label=sys.argv[1:3];directory=root/'evidence'/label;directory.mkdir(exist_ok=True,parents=True)
def command(*args):return subprocess.check_output(args,text=True)
def snapshot():
 values=list(map(int,pathlib.Path('/proc/stat').read_text().splitlines()[0].split()[1:]));hz=__import__('os').sysconf('SC_CLK_TCK')
 return {'unix':time.time(),'host_cpu_seconds':sum(values[i] for i in [0,1,2,5,6])/hz,'client_cpu':command('cat','/sys/fs/cgroup/system.slice/nullnet-client.service/cpu.stat'),'all_client_pid':command('systemctl','show','nullnet-client','--property=MainPID','--value').strip(),'conntrack_count':int(pathlib.Path('/proc/sys/net/netfilter/nf_conntrack_count').read_text())}
if mode=='before':
 before=snapshot();last=json.loads(command('journalctl','-u','nullnet-client','-n','1','-o','json','--no-pager'));before['cursor']=last['__CURSOR'];
 if pathlib.Path('/run/systemd/system/nullnet-server.service.d/99-nn-layer1.conf').exists():before['server_cursor']=json.loads(command('journalctl','-u','nullnet-server','-n','1','-o','json','--no-pager'))['__CURSOR']
 (directory/'before.json').write_text(json.dumps(before,indent=2)+'\n');print(json.dumps(before))
else:
 before=json.loads((directory/'before.json').read_text());data=command('journalctl','-u','nullnet-client','--after-cursor='+before['cursor'],'-o','json','--no-pager');setups=[];retirements=[];failed=[]
 for line in data.splitlines():
  entry=json.loads(line);message=entry.get('MESSAGE','');timestamp=int(entry['__REALTIME_TIMESTAMP'])/1e6
  match=re.search(r'^VXLAN (\d+) setup completed in (\d+) ms \(docker: (.*)\)',message)
  if match:setups.append({'id':int(match[1]),'ms':int(match[2]),'docker':match[3],'unix':timestamp})
  match=re.search(r'^VXLAN(?: (\d+))? teardown completed in (\d+) ms',message)
  if match:retirements.append({'id':int(match[1]) if match[1] else None,'ms':int(match[2]),'unix':timestamp})
  if message.startswith('VXLAN ') and 'FAILED' in message:failed.append({'unix':timestamp,'message':message})
 links=json.loads(command('ip','-d','-j','link'));owned=[{'name':l['ifname'],'kind':l.get('linkinfo',{}).get('info_kind'),'index':l['ifindex']} for l in links if re.match(r'^(br_\d+_[sc]|nnv_\d+_[sc]|vxlan-ns_\d+_[sc]|ns_\d+_[sc]-(out|o)|veth-\d+-[sc]|macsec-\d+-?[sc])$',l['ifname'])]
 server_retirements=[]
 if 'server_cursor' in before:
  for line in command('journalctl','-u','nullnet-server','--after-cursor='+before['server_cursor'],'-o','json','--no-pager').splitlines():
   entry=json.loads(line);match=re.fullmatch(r'Network (\d+) teardown complete; ID released',entry.get('MESSAGE',''))
   if match:server_retirements.append({'id':int(match[1]),'unix':int(entry['__REALTIME_TIMESTAMP'])/1e6})
 after=snapshot();result={'label':label,'before':before,'after':after,'host_cpu_seconds':after['host_cpu_seconds']-before['host_cpu_seconds'],'setups':len(setups),'retirements':len(retirements),'lifecycle_errors':failed,'remaining_owned_links':owned,'server_retirements':len(server_retirements),'last_server_retirement_unix':max((r['unix'] for r in server_retirements),default=None),'last_retirement_unix':max((r['unix'] for r in retirements),default=None),'records':{'setups':setups,'retirements':retirements,'server_retirements':server_retirements}}
 if mode=='after':(directory/'result.json').write_text(json.dumps(result,indent=2)+'\n')
 print(json.dumps({k:v for k,v in result.items() if k!='records'}))
