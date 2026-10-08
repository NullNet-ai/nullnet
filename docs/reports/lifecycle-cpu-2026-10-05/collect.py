"""Preserve measurements, hardware metadata and restoration checks."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile

root=Path('/tmp/nn-cpu-20261005')
for stem in ['node','phased-node','concurrent-node']:
    result=json.loads((root/(stem+'.json')).read_text())
    assert 'error' not in result,(stem,result.get('error'))
    preservation=json.loads((root/(stem+'.json.preservation.json')).read_text())
    assert all(preservation['checks'].values()),(stem,preservation['checks'])
metadata={name:subprocess.check_output(command,text=True) for name,command in {
    'hardware':['lscpu'],'kernel':['uname','-a'],'services':['systemctl','show','nullnet-client','nullnet-server','nullnet-proxy','-p','MainPID','-p','NRestarts'],
    'processes':['ps','-e','-o','pid,comm'],'lab_dirty_files':['git','-C','/root/nullnet','status','--short'],
    'profiling_units':['systemctl','show','nn-cpu-20261005','nn-cpu-phased-20261005','nn-cpu-phased-v2-20261005','nn-cpu-workload-20261005','-p','ActiveState']}.items()}
metadata['trace_instances']=os.listdir('/sys/kernel/tracing/instances')
metadata['remaining_probe_definitions']=[line for line in Path('/sys/kernel/tracing/kprobe_events').read_text().splitlines() if 'nn_est/' in line]
assert not metadata['remaining_probe_definitions']
assert 'nn_est' not in metadata['trace_instances']
assert 'ActiveState=active' not in metadata['profiling_units']
(root/'metadata.json').write_text(json.dumps(metadata,indent=2)+'\n')
destination=Path('/tmp/nn-cpu-evidence-'+sys.argv[1]+'.tar.gz')
with tarfile.open(destination,'w:gz') as archive:
    archive.add(root,arcname='host-'+sys.argv[1]+'/measurements')
    archive.add('/tmp/nn-kernel-estimate',arcname='host-'+sys.argv[1]+'/raw')
destination.chmod(0o644)
print(json.dumps({'archive':str(destination),'bytes':destination.stat().st_size,'restoration':'passed'}))
