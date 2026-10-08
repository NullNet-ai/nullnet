"""Phase barriers attribute host-wide notification/deferred CPU to operations."""
import concurrent.futures
import contextlib
import io
import json
import os
from pathlib import Path
import resource
import socket
import subprocess
import sys
import time

ROOT = Path('/tmp/nn-cpu-20261005')
sys.path.insert(0, str(ROOT / 'docs/reports/kernel-parallelism-2026-10-01'))
import node
from node import attr, info, U, crypto, cross
import local_benchmark
import profile as accounting

accounting.install(node)
soft, hard = resource.getrlimit(resource.RLIMIT_NOFILE)
resource.setrlimit(resource.RLIMIT_NOFILE, (min(65536, hard), hard))


def host_cpu():
    return [int(x) for x in Path('/proc/stat').read_text().splitlines()[0].split()[1:]]


def benchmark(experiment, request):
    count = request['concurrency']
    slots = experiment.slots[:count]
    rows = []
    idle_before = host_cpu()
    idle_start = time.monotonic()
    time.sleep(3)
    (ROOT/'idle.json').write_text(json.dumps({'seconds':time.monotonic()-idle_start,
        'delta':[b-a for a,b in zip(idle_before,host_cpu())]}))
    for trial in range(3):
        sampling = trial == 2 and experiment.args.side == 0
        perf = None
        if sampling:
            perf_log = (ROOT/'perf.log').open('w')
            perf = subprocess.Popen(['perf','record','-a','-e','cpu-clock','-c','5000000',
                '--clockid','mono','-g','-o',str(ROOT/'perf.data')],stdout=perf_log,stderr=perf_log)
            time.sleep(.2)
        for slot in slots:
            experiment.keys[slot['sides'][0]['number']] = os.urandom(32).hex()
            slot['generation'] += 1

        def phase(name, fn, main=False):
            accounting.ROWS.clear()
            accounting.ENABLED = True
            cpu_before = host_cpu()
            proc_before = time.process_time_ns()
            started = time.monotonic()
            if main:
                accounting.LOCAL.phase = name.split('/')[0]
                fn()
            else:
                def work(slot):
                    experiment.io()
                    accounting.LOCAL.phase = name.split('/')[0]
                    return accounting.measured('phase_inclusive', fn, slot)
                list(experiment.pool.map(work, slots))
            operation_seconds = time.monotonic() - started
            # Include delayed device events; this is an attribution run, not throughput.
            node.command('udevadm', 'settle', '--timeout=60')
            time.sleep(.25)
            after = host_cpu()
            row = {'trial': trial, 'phase': name, 'endpoints': count,
                   'sampling':sampling, 'start_monotonic':started, 'end_monotonic':time.monotonic(),
                   'operation_seconds': operation_seconds, 'window_seconds': time.monotonic()-started,
                   'cpu_delta': [b-a for a,b in zip(cpu_before,after)],
                   'process_cpu_ms': (time.process_time_ns()-proc_before)/1e6,
                   'accounting': accounting.summarize()}
            accounting.ENABLED = False
            rows.append(row)
            (ROOT / 'phased.json').write_text(json.dumps(rows, indent=2)+'\n')

        def io_for(slot):
            n,x,targets = experiment.io()
            e = slot['sides'][0]
            return n,x,targets[0],e,e['indices'],e['names']

        def veth(slot):
            n,x,t,e,ix,names = io_for(slot)
            peer = info()+attr(3,names['inner'].encode()+b'\0')+attr(4,U(1080))+attr(28,U(experiment.targets[0]))
            ix['outer'] = node.create(n,names['outer'],'veth',attr(1|32768,peer),attr(4,U(1080)),e['group'])
            ix['inner'] = t.get(names['inner'])

        def vxlan(slot):
            n,x,t,e,ix,names = io_for(slot)
            data=attr(1,U(slot['id']))+attr(4,socket.inet_aton(experiment.local))+attr(2,socket.inet_aton(experiment.remote))
            import struct
            data+=attr(15,struct.pack('!H',cross.PORT))+attr(7,b'\0')
            ix['transport']=ix['macsec']=node.create(n,names['transport'],'vxlan',data,attr(4,U(1080)),e['group'])

        def state(slot, inbound, delete=False):
            n,x,t,e,ix,names=io_for(slot)
            side=experiment.args.side
            direction=1-side if inbound else side
            a,b=(experiment.remote,experiment.local) if inbound else (experiment.local,experiment.remote)
            key='' if delete else node.hashlib.sha256((experiment.keys[e['number']]+str(direction)).encode()).hexdigest()
            x.state(a,b,cross.SPI+e['number']*2+direction,key,e['mark'],inbound=inbound,delete=delete)

        def policy(slot, delete=False):
            n,x,t,e,ix,names=io_for(slot)
            x.policy(experiment.local,experiment.remote,cross.SPI+e['number']*2+experiment.args.side,cross.PORT,e['mark'],1,delete)

        def readiness(slot):
            n,x,t,e,ix,names=io_for(slot)
            for kind in ['transport','outer','bridge']:
                assert n.get(names[kind])==ix[kind]
            assert t.get(names['inner'])==ix['inner']
            slot['active']=True

        phase('setup/veth_create_and_peer_lookup',veth)
        phase('setup/vxlan_create',vxlan)
        phase('setup/tc_qdisc_and_three_filters',lambda s: crypto.tc_install(io_for(s)[0],io_for(s)[4]['transport'],io_for(s)[3]['mark']))
        phase('setup/ipsec_out_state',lambda s: state(s,False))
        phase('setup/ipsec_in_state',lambda s: state(s,True))
        phase('setup/ipsec_out_policy',policy)
        phase('setup/container_address',lambda s: io_for(s)[2].address(io_for(s)[4]['inner'],io_for(s)[3]['ip']))
        phase('setup/bridge_address',lambda s: io_for(s)[0].address(io_for(s)[4]['bridge'],io_for(s)[3]['gateway']))
        phase('setup/outer_attach_enable',lambda s: io_for(s)[0].configure(io_for(s)[4]['outer'],True,io_for(s)[4]['bridge']))
        phase('setup/vxlan_attach_enable',lambda s: io_for(s)[0].configure(io_for(s)[4]['transport'],True,io_for(s)[4]['bridge']))
        phase('setup/bridge_enable',lambda s: io_for(s)[0].configure(io_for(s)[4]['bridge'],True))
        phase('setup/container_peer_enable',lambda s: io_for(s)[2].configure(io_for(s)[4]['inner'],True))
        phase('setup/readiness',readiness)
        phase('teardown/vxlan_revoke',lambda s: io_for(s)[0].configure(io_for(s)[4]['transport']))
        phase('teardown/ipsec_policy_remove',lambda s: policy(s,True))
        phase('teardown/ipsec_out_state_remove',lambda s: state(s,False,True))
        phase('teardown/ipsec_in_state_remove',lambda s: state(s,True,True))
        def retiring_group(slot):
            n,x,t,e,ix,names=io_for(slot)
            for kind in ['outer','transport']:
                n.request(19,info(ix[kind])+attr(27,U(0x53000000|experiment.args.side)))
        phase('teardown/retiring_group_assignment',retiring_group)

        def delete():
            n=experiment.io()[0]
            for group in {0x53000000|experiment.args.side}:
                n.request(17,info()+attr(27,U(group)))
            for slot in slots:
                for kind in ['outer','inner','transport','macsec']:
                    slot['sides'][0]['indices'].pop(kind,None)
        phase('teardown/batched_vxlan_veth_delete',delete,True)
        phase('teardown/scoped_conntrack_flush',experiment.clear_flows)
        phase('teardown/bridge_disable',lambda s: io_for(s)[0].configure(io_for(s)[4]['bridge']))
        phase('teardown/bridge_rename',lambda s: io_for(s)[0].configure(io_for(s)[4]['bridge'],extra=attr(3,('np'+str(s['id'])).encode()+b'\0')))
        phase('teardown/bridge_name_restore',lambda s: io_for(s)[0].configure(io_for(s)[4]['bridge'],extra=attr(3,io_for(s)[5]['bridge'].encode()+b'\0')))
        phase('teardown/bridge_neighbor_dump',lambda s: io_for(s)[0].clear_neighbors(io_for(s)[4]['bridge']))
        phase('teardown/bridge_address_remove',lambda s: io_for(s)[0].address(io_for(s)[4]['bridge'],io_for(s)[3]['gateway'],True))
        phase('teardown/bridge_check',lambda s: io_for(s)[0].get(io_for(s)[5]['bridge']))
        for slot in slots:
            slot['active']=False
        experiment.inventory('phased-'+str(trial))
        if perf:
            perf.send_signal(2)
            perf.wait(timeout=30)
            perf_log.close()
    return {'phases':len(rows),'cycles':count*3}


local_benchmark.benchmark=benchmark
with (ROOT/'phased.log').open('w',buffering=1) as log, contextlib.redirect_stdout(log), contextlib.redirect_stderr(log):
    sys.stdin=io.StringIO(json.dumps({'op':'benchmark','concurrency':256})+'\n'+json.dumps({'op':'finish'})+'\n')
    sys.argv=['node.py','--side',sys.argv[1],'--slots','512','--workers','512','--variant','bridge','--output',str(ROOT/'phased-node.json')]
    node.main()
    (ROOT/'phased.done').write_text('complete\n')
