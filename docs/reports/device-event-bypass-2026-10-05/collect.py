"""Collect measurements and prove restoration against the initial inventory."""
import contextlib
import io
import json
from pathlib import Path
import runpy
import subprocess
import sys
import tarfile

sys.path.insert(0,'/tmp/nn-cpu-20261005/docs/reports/kernel-parallelism-2026-10-01')
import node

ROOT=Path('/tmp/nn-event-20261005')
before=json.loads((ROOT/'node.json.preservation.json').read_text())['before']
after=json.loads(json.dumps(node.original_inventory()))
checks={key:before[key]==after[key] for key in before}
with contextlib.redirect_stdout(io.StringIO()):
    runpy.run_path('/tmp/nn-event-discover.py')
inventory=json.loads(Path('/tmp/nn-event-sockets.json').read_text())
targets=[r for r in inventory['sockets'] if r['protocol']==0 or r['protocol']==15 and r['groups']&1]
assert all(r['original_filter_instructions']==0 for r in targets),targets
assert len(targets)>=7,targets
checks['subscribed_sockets_restored']=True
checks['trace_instance_removed']=not Path('/sys/kernel/tracing/instances/nn_est').exists()
checks['trace_probes_removed']='nn_est/' not in Path('/sys/kernel/tracing/kprobe_events').read_text()
(ROOT/'final-restoration.json').write_text(json.dumps({'checks':checks,'before':before,'after':after,
    'socket_inventory':inventory},indent=2)+'\n')
assert all(checks.values()),checks
with tarfile.open('/tmp/nn-event-evidence.tar.gz','w:gz') as archive:
    for directory in [ROOT,Path(str(ROOT)+'-sustained'),Path(str(ROOT)+'-final')]:
        archive.add(directory,arcname=directory.name)
    for path in Path('/tmp/nn-kernel-estimate').glob('events-*'):
        archive.add(path,arcname='raw/'+path.name)
    for name in ['socket_filter.py','benchmark.py','live_proof.py','traffic_node.py','verify_filter.py',
                 'nn-event-discover.py','nn-event-filter-tests.json']:
        archive.add('/tmp/'+name,arcname='scripts/'+name)
print(json.dumps({'restoration':checks,'evidence_bytes':Path('/tmp/nn-event-evidence.tar.gz').stat().st_size}))
