"""Recompute report tables from the archived lab measurements."""
import json
from pathlib import Path
import statistics
import sys

root=Path(sys.argv[1])
out=Path(sys.argv[2])
ACTIVE=(0,1,2,5,6)
summary={'hosts':{}}
for host in (103,104):
    directory=root/f'host-{host}'/'measurements'
    for stem in ['node','phased-node','concurrent-node']:
        assert 'error' not in json.loads((directory/f'{stem}.json').read_text())
        assert all(json.loads((directory/f'{stem}.json.preservation.json').read_text())['checks'].values())
    diagnostics=[json.loads(p.read_text()) for p in sorted(directory.glob('c256*-cpu1.cpu.json'))]
    cycles=sum(d['cycles'] for d in diagnostics)
    costs={key:sum(d['accounting'][key]['cpu_ms'] for d in diagnostics)/cycles for key in diagnostics[0]['accounting']}
    expected={'setup/create_veth':1,'setup/create_vxlan':1,'setup/getlink_readiness':6,
              'setup/address_add':2,'setup/tc_filter_add':3,'teardown/conntrack_delete':4,
              'teardown/delete_link_batch':1/256}
    for key,count in expected.items():
        assert sum(d['accounting'][key]['calls'] for d in diagnostics)==cycles*count,(host,key)
    workloads=[]
    for p in sorted(directory.glob('*.concurrent.json')):
        d=json.loads(p.read_text())
        workloads.append({'label':d['label'],'cycles':d['cycles'],'cycles_per_second':d['cycles_per_second'],
            'host_cpu_ms_per_cycle':sum(d['cpu_delta'][i] for i in ACTIVE)*10/d['cycles'],
            'process_cpu_ms_per_cycle':d['process_delta']['clock']/1e6/d['cycles'],
            'process_user_ms_per_cycle':d['process_delta']['user']*1000/d['cycles'],
            'process_system_ms_per_cycle':d['process_delta']['system']*1000/d['cycles']})
    phases=json.loads((directory/'phased.json').read_text())
    phase_costs={}
    for name in dict.fromkeys(r['phase'] for r in phases):
        rows=[r for r in phases if r['phase']==name and not r['sampling']]
        category=name.split('/')[0]+'/phase_inclusive'
        values=[(r['accounting'][category] if category in r['accounting'] else r['accounting']['teardown/delete_link_batch'])['cpu_ms']/r['endpoints'] for r in rows]
        phase_costs[name]={'caller_cpu_ms':statistics.mean(values),'min_ms':min(values),'max_ms':max(values),
                         'repeats':len(values),'endpoints_per_repeat':256}
    baselines=[json.loads(p.read_text()) for p in directory.glob('c256*-cpu0.cpu.json')]
    rates=[d['cycles_per_second'] for d in baselines]
    cpus=[sum(d['cpu_delta'][i] for i in ACTIVE)*10/d['cycles'] for d in baselines]
    summary['hosts'][str(host)]={'diagnostic_cycles':cycles,'concurrent_request_cpu_ms':costs,
        'workloads':workloads,'phased_caller_costs':phase_costs,
        'untraced_separate_repeats':{'rates':rates,'host_cpu_ms':cpus}}
samples=json.loads((root/'host-103/measurements/concurrent-samples.json').read_text())
assert not samples['unparsed_examples']
sampled=next(w for w in summary['hosts']['103']['workloads'] if w['label']=='sampled-mixed-c256')
active=sum(v for k,v in samples['families_ns'].items() if k!='idle')
summary['sampled_cpu']={k:{'ms_per_cycle':v/1e6/sampled['cycles'],'active_percent':100*v/active}
                       for k,v in samples['families_ns'].items() if k!='idle'}
locks=json.loads((root/'host-103/raw/locks-mixed-c256.analysis.json').read_text())
assert not any(locks['overruns'].values())
assert not locks['boundary_errors']
assert not locks['incomplete_holds']
traced=next(w for w in summary['hosts']['103']['workloads'] if w['label']=='locks-mixed-c256')
summary['locks']={'cycles':traced['cycles'],'seconds':locks['seconds'],'occupancy':locks['lock_occupancy'],
    'fixture_hold_ms_per_cycle':sum(v['sum_ms'] for k,v in locks['holds'].items() if k.startswith('fixture/'))/traced['cycles'],
    'rcu_within_fixture_hold_ms_per_cycle':locks['rcu_union_inside_hold_ms']['fixture']/traced['cycles'],
    'holds_by_operation_ms_per_cycle':{k:v['sum_ms']/traced['cycles'] for k,v in locks['holds'].items() if k.startswith('fixture/')},
    'fixture_lock_wait_ms_per_cycle':locks['waits']['fixture']['sum_ms']/traced['cycles']}
out.write_text(json.dumps(summary,indent=2)+'\n')
print(json.dumps({'restoration':'passed','operation_counts':'passed','trace_overruns':0,'sample_parse_errors':0,
                  'hosts':list(summary['hosts'])}))
