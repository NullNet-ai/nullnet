"""Current versus pooled bridges: exact frame preservation and isolation."""
import concurrent.futures as cf,importlib.util,json,pathlib,time,os
ROOT=pathlib.Path(__file__).resolve().parent
def module(name,file):
    spec=importlib.util.spec_from_file_location(name,ROOT/file);m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m);return m
w=module('wire','wire-analysis.py');probe=module('probe','frame-parity-node.py')
results=[]
for mode in os.environ.get('NN_FRAME_MODES','current_docker,pooled_marked_final,transparent').split(','):
    created={0:[],1:[]};admitted=[]
    try:
        for side in [0,1]:
            mid=next(m['id'] for m in json.loads(w.remote(side,['bpftool','-j','map','show']).stdout) if m.get('name')=='PEERS')
            w.remote(side,['bpftool','map','update','id',str(mid),'key','hex',f'{104-side:02x}','17','12','c6','value','hex','01','noexist']);admitted.append((side,mid))
            for i in range(2):
                name=f'nn23-wire-{i}';w.remote(side,['docker','run','-d','--network','none','--name',name,'--label','nullnet-benchmark=20260923','alpine:latest','sleep','infinity']);created[side].append(name)
            pids=[c['State']['Pid'] for c in json.loads(w.remote(side,['docker','inspect',*created[side]]).stdout)]
            w.remote(side,['python3','-c',"import pathlib;pathlib.Path('/tmp/nn23-wire-pids.json').write_text("+repr(json.dumps(pids))+")"])
        w.both('underlay');w.both('install','pooled_marked_final' if mode in ('transparent','conditional','transparent_nf') else mode,8)
        if mode in ('transparent','conditional','transparent_nf'):
            for side in [0,1]:
                w.net(side,['ip','link','set','pool','type','bridge','vlan_protocol','802.1ad'])
                for i in range(8):
                    w.net(side,['ip','link','del',f'gw{i}'])
                    w.net(side,['ip','link','add','link','pool','name',f'gw{i}','type','vlan','protocol','802.1ad','id',str(i+1)])
                    w.net(side,['ip','addr','add',f'10.223.{i}.{side+3}/24','dev',f'gw{i}'])
                    w.net(side,['ip','link','set',f'gw{i}','mtu','1080','up'])
                    for dev in [f'out{i}',f'x{i}']:
                        w.net(side,['bridge','vlan','del','dev',dev,'vid',str(i+1)])
                        w.net(side,['bridge','vlan','add','dev',dev,'vid',str(i+1)]+(['pvid','untagged'] if mode=='conditional' else []))
                        if dev.startswith('out'):w.net(side,['tc','qdisc','add','dev',dev,'clsact'])
                        code="import sys;sys.path.insert(0,'/tmp/nn23');from netlink import NL;import native_vlan;n=NL();native_vlan.{INSTALL}(n,n.get("+repr(dev)+"),"+str(i+1)+","+(str(1000+i) if dev.startswith('x') else 'None')+")"
                        code=code.replace('{INSTALL}','install_conditional' if mode=='conditional' else 'install')
                        if dev.startswith('out'):w.net(side,['tc','qdisc','del','dev',dev,'clsact'])
                        w.net(side,['python3','-c',code])
        if mode=='transparent_nf':
            for side in [0,1]:w.net(side,['sysctl','-w','net.bridge.bridge-nf-filter-vlan-tagged=1','net.bridge.bridge-nf-pass-vlan-input-dev=1'])
        row={'mode':mode,'directions':[]};results.append(row)
        for source in [0,1]:
            with cf.ThreadPoolExecutor(1) as ex:
                listening=ex.submit(w.remote,1-source,['python3','/tmp/frame-parity-node.py','listen'])
                time.sleep(.8);w.remote(source,['python3','/tmp/frame-parity-node.py','send']);received=json.loads(listening.result().stdout.splitlines()[-1])
            expected=[probe.frame(i,t).hex() for i,t in enumerate(probe.CASES)]
            counts=[sum(r['endpoint']==0 and r['frame']==f for r in received) for f in expected]
            foreign=[r for r in received if r['endpoint']!=0]
            row['directions'].append({'source':source,'exact_counts':counts,'foreign_endpoint_frames':foreign,'received':received})
            print(mode,source,counts,'foreign',len(foreign),flush=True)
        pids0=json.loads(w.remote(0,['cat','/tmp/nn23-wire-pids.json']).stdout)
        def ping(target):return w.remote(0,['nsenter','-t',str(pids0[0]),'-n','ping','-n','-c','2','-W','1',target],False).returncode
        row['gateway_ping']=ping('10.223.0.3');row['remote_ping']=ping('10.223.0.2')
        w.net(0,['iptables','-I','FORWARD','1','-s','10.223.0.1','-d','10.223.0.2','-j','DROP'])
        row['forward_drop_ping']=ping('10.223.0.2')
        row['forward_counters']=w.net(0,['iptables','-nvL','FORWARD']).stdout
        row['bridge_sysctls']=w.net(0,['sysctl','net.bridge.bridge-nf-call-iptables','net.bridge.bridge-nf-filter-vlan-tagged'],check=False).stdout
        print(mode,'ROUTING',row['gateway_ping'],row['remote_ping'],'FORWARD_DROP',row['forward_drop_ping'],flush=True)
        w.net(0,['iptables','-D','FORWARD','-s','10.223.0.1','-d','10.223.0.2','-j','DROP'])
        row['routed']=module('routed','routed-parity.py').prove(w);print(mode,'ROUTED_SNAT',row['routed']['ping'],flush=True)
        (ROOT/'frame-parity.json').write_text(json.dumps(results,indent=2))
    finally:
        for side in [0,1]:
            for pid in w.remote(side,['ip','netns','pids','nn23-wire'],False).stdout.split():w.remote(side,['kill',pid],False)
            w.remote(side,['ip','netns','del','nn23-wire'],False)
            for name in created[side]:w.remote(side,['docker','rm','-f',name],False)
        for side,mid in admitted:w.remote(side,['bpftool','map','delete','id',str(mid),'key','hex',f'{104-side:02x}','17','12','c6'],False)
