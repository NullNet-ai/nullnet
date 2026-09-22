import pathlib,os,json,subprocess,importlib.util,time
spec=importlib.util.spec_from_file_location('bench','/tmp/nullnet-port-comparison.py');b=importlib.util.module_from_spec(spec);spec.loader.exec_module(b)
os.setns(os.open('/var/run/netns/nn-kcause',os.O_RDONLY),os.CLONE_NEWNET)
t=pathlib.Path('/sys/kernel/tracing/instances/nn-vxlan-cause');t.mkdir()
def put(n,s):(t/n).write_text(str(s))
try:
 put('tracing_on',0);put('buffer_size_kb',8192);put('current_tracer','function_graph')
 put('set_ftrace_filter','vxlan_sock_release\nudp_tunnel_sock_release\nsynchronize_net\nsynchronize_rcu\nsynchronize_rcu_expedited\n')
 put('set_ftrace_pid',os.getpid());put('options/sleep-time',1)
 results=[]
 for shared in [False,True]:
  b.SHARED=shared;nl=b.NL();rows=[]
  for start in range(0,1000,32):rows+=nl.create(range(start,min(start+32,1000)))
  assert all(e==0 for e,tm in rows)
  put('trace','');put('tracing_on',1);start=time.perf_counter();deleted=nl.delete();elapsed=time.perf_counter()-start;put('tracing_on',0)
  label='shared' if shared else 'unique'
  pathlib.Path('/tmp/nn-kernel-'+label+'.trace').write_text((t/'trace').read_text())
  residual=json.loads(subprocess.check_output(['ip','-j','link','show','type','vxlan'],text=True))
  r={'shared':shared,'delete_seconds':elapsed,'delete_errors':sum(e!=0 for e,tm in deleted),'residual':len(residual)};results.append(r);print(json.dumps(r),flush=True)
  assert not residual and r['delete_errors']==0
 pathlib.Path('/tmp/nn-kernel-results.json').write_text(json.dumps(results))
finally:
 put('tracing_on',0);put('current_tracer','nop');t.rmdir()
