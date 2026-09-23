import json,os,pathlib,subprocess,time
from netlink import NL,U,attr,info
root=pathlib.Path('/tmp/nn23');ns='nn23-trace'
def run(args):return subprocess.run(args,check=True,capture_output=True,text=True).stdout
run(['ip','netns','add',ns]);fd=os.open('/proc/self/ns/net',os.O_RDONLY);os.setns(os.open('/var/run/netns/'+ns,os.O_RDONLY),os.CLONE_NEWNET)
t=pathlib.Path('/sys/kernel/tracing/instances/nn23-components');t.mkdir()
def put(n,s):(t/n).write_text(str(s))
try:
 put('tracing_on',0);put('buffer_size_kb',16384);put('current_tracer','function_graph');put('set_ftrace_filter','rtnl_dellink\nbr_dev_delete\nbr_multicast_dev_del\nbr_fdb_hash_fini\nnetdev_rx_handler_unregister\nnetdev_run_todo\nudp_tunnel_sock_release\nsynchronize_net\nsynchronize_rcu\nsynchronize_rcu_expedited\nrcu_barrier\n');put('set_ftrace_pid',os.getpid());put('options/sleep-time',1)
 results=[]
 for mode in ['bridge','veth','vxlan_shared','macsec']:
  nl=NL()
  for i in range(16):
   if mode=='bridge':nl.create(f'b{i}','bridge',b'');nl.up(nl.get(f'b{i}'))
   elif mode=='vxlan_shared':
    nl.create(f'x{i}','vxlan',attr(1,U(400000+i))+attr(4,b'\xc0\x00\x02\x01')+attr(2,b'\xc0\x00\x02\x02')+attr(15,b'\x12\xb5'));nl.up(nl.get(f'x{i}'))
   else:
    nl.create(f'p{i}','veth',attr(1|32768,info()+attr(3,f'q{i}'.encode()+b'\0')));nl.up(nl.get(f'p{i}'));nl.up(nl.get(f'q{i}'))
    if mode=='macsec':run(['ip','link','add','link',f'p{i}',f'm{i}','type','macsec','port','1','encrypt','on'])
  put('trace','');put('tracing_on',1);start=time.monotonic();nl.request(17,info()+attr(27,U(0x4e600001)));elapsed=time.monotonic()-start;put('tracing_on',0)
  (root/f'trace-{mode}.txt').write_text((t/'trace').read_text());results.append({'mode':mode,'n':16,'delete_seconds':elapsed});print(results[-1],flush=True)
  assert len(json.loads(run(['ip','-j','link'])))==1
 (root/'trace-results.json').write_text(json.dumps(results))
finally:
 put('tracing_on',0);put('current_tracer','nop');t.rmdir();os.setns(fd,os.CLONE_NEWNET);run(['ip','netns','del',ns])
