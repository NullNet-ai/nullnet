"""Read-only post-experiment continuity and resource audit."""
import json,pathlib,subprocess
R=pathlib.Path('/tmp/nn23')
def run(args):return subprocess.run(args,capture_output=True,text=True,check=True).stdout
names=run(['docker','ps','-q']).split();containers=json.loads(run(['docker','inspect',*names])) if names else []
current={c['Name']:{'pid':c['State']['Pid'],'start':c['State']['StartedAt'],'networks':c['NetworkSettings']['Networks']} for c in containers}
before=json.loads((R/'before.json').read_text())
links=json.loads(run(['ip','-j','addr']));routes=json.loads(run(['ip','-j','route']))
original_links={(l['ifindex'],l['ifname'],l.get('mtu'),l.get('address')) for l in before['links']};current_links={(l['ifindex'],l['ifname'],l.get('mtu'),l.get('address')) for l in links}
benchmark_containers=run(['docker','ps','-aq','--filter','label=nullnet-benchmark=20260923']).split();benchmark_namespaces=[l for l in run(['ip','netns','list']).splitlines() if l.startswith('nn23')]
peers=next(m['id'] for m in json.loads(run(['bpftool','-j','map','show'])) if m.get('name')=='PEERS')
maprows=json.loads(run(['bpftool','-j','map','dump','id',str(peers)]));test_keys=[[f'0x{last:02x}','0x17','0x12','0xc6'] for last in [103,104]]
remaining_test_peers=[]
for last in [103,104]:
 probe=subprocess.run(['bpftool','map','lookup','id',str(peers),'key','hex',f'{last:02x}','17','12','c6'],capture_output=True,text=True)
 if probe.returncode==0:remaining_test_peers.append(last)
 else:assert probe.returncode==254 and 'Not found' in probe.stdout,(probe.returncode,probe.stdout,probe.stderr)
processes=[line for line in run(['ps','-eo','pid,args']).splitlines() if '/tmp/packet-probe.py' in line or 'iperf3 -s -B 10.223.' in line or 'iperf3 -s -B 10.222.' in line]
experiment_units=run(['systemctl','list-units','--state=running','--no-legend','nn23*'])
result={'remaining_benchmark_processes':processes,'running_experiment_units':experiment_units,'original_containers_unchanged':current==before['containers'],'original_links_unchanged':original_links==current_links,'routes_unchanged':routes==before['routes'],'remaining_benchmark_containers':benchmark_containers,'remaining_benchmark_namespaces':benchmark_namespaces,'remaining_test_peers':remaining_test_peers,'running_nullnet_units':run(['systemctl','list-units','--state=running','--no-legend','nullnet*']),'kernel':run(['uname','-r']).strip(),'cpu':json.loads(run(['lscpu','-J'])),'memory':run(['free','-b']),'containers':current,'links':links,'routes':routes}
(R/'final-audit.json').write_text(json.dumps(result,indent=2));print(json.dumps({k:v for k,v in result.items() if k not in ['cpu','memory','containers','links','routes']}))
assert result['original_containers_unchanged'] and result['original_links_unchanged'] and result['routes_unchanged'] and not benchmark_containers and not benchmark_namespaces and not remaining_test_peers and not processes and not experiment_units
