"""Reduce raw coordinator results without counting endpoint halves as cycles."""
import argparse
import hashlib
import json
from pathlib import Path
import statistics

p=argparse.ArgumentParser();p.add_argument('evidence');p.add_argument('output');args=p.parse_args()
root=Path(args.evidence)
result={'scope':'isolated cross-host kernel prototype','target_cycles_per_second':500,'workloads':{}}
for name in ['overlap','burst','stress']:
    path=root/name/'results.json';data=json.loads(path.read_text())
    assert data['completed'] and all(c['cleaned'] and all(c['preservation'].values()) for c in data['cleanup'])
    trials=[]
    for trial in data['trials']:
        assert trial['creations']==trial['retirements']
        phases={phase.removesuffix('_s')+'_ms':1000*statistics.mean(row[phase] for row in trial['phases']) for phase in ['activation_s','traffic_s','retirement_s','rejection_s']}
        trials.append({k:v for k,v in trial.items() if k!='phases'} | {'mean_batch_phase_ms':phases})
    rates=[t['cycles_per_second'] for t in trials]
    result['workloads'][name]={'sha256':hashlib.sha256(path.read_bytes()).hexdigest(),
        'median_cycles_per_second':statistics.median(rates),'min_cycles_per_second':min(rates),'max_cycles_per_second':max(rates),
        'total_cycles':sum(t['creations'] for t in trials),'trials':trials,'ready':data['ready'],
        'warm':data['warm'],'proofs':data['proofs'],'cleanup':data['cleanup'],
        'fixed_interface_inventory':data['initial_inventory']==data['final_inventory']}
result['total_measured_cycles']=sum(w['total_cycles'] for w in result['workloads'].values())
Path(args.output).write_text(json.dumps(result,indent=2)+'\n')
