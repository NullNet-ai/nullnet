import pathlib,sys,json,subprocess,time
base=pathlib.Path('/sys/kernel/tracing/instances/nn-setup-attribution')
def put(k,v):(base/k).write_text(str(v))
if sys.argv[1]=='start':
 base.mkdir();put('tracing_on',0);put('buffer_size_kb',16384);put('current_tracer','function_graph')
 funcs=['netlink_sendmsg','rtnetlink_rcv_msg','rtnl_lock','rtnl_lock_killable','rtnl_newlink','rtnl_getlink','rtnl_setlink','inet_rtm_newaddr','inet_rtm_newroute','xfrm_user_rcv_msg','synchronize_net','synchronize_rcu','synchronize_rcu_expedited','__x64_sys_unshare','__x64_sys_mount','setup_net']
 funcs+=['nf_tables_commit','nf_tables_trans_destroy_work','nf_tables_valid_genid','nfnetlink_rcv_batch','netdev_run_todo','__mutex_lock.constprop.0','__mutex_lock','rtnl_unlock']
 available={l.split()[0] for l in pathlib.Path('/sys/kernel/tracing/available_filter_functions').read_text().splitlines()}
 put('set_ftrace_filter','\n'.join(f for f in funcs if f in available))
 for k in ['sleep-time','funcgraph-abstime','funcgraph-proc','funcgraph-tail']:put('options/'+k,1)
 put('trace_clock','global');put('trace','');put('tracing_on',1)
 print('TRACE_STARTED',time.time())
else:
 put('tracing_on',0)
 pathlib.Path('/tmp/nn-setup-kernel.trace').write_text((base/'trace').read_text())
 stats={p.name:(p/'stats').read_text() for p in (base/'per_cpu').iterdir()};pathlib.Path('/tmp/nn-setup-kernel-stats.json').write_text(json.dumps(stats))
 put('current_tracer','nop');base.rmdir();print('TRACE_SAVED',time.time())
