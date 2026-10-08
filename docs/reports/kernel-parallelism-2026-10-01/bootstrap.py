import runpy,shutil,sys
from pathlib import Path
p=Path('/tmp/nn-kernel-estimate/docs/reports/vxlan-minimal-dedicated-2026-09-29')
for name in ['node.py','local_benchmark.py','trace_lock.py']:
 shutil.copyfile('/tmp/'+name,p/name)
sys.path.insert(0,str(p))
runpy.run_path(str(p/'node.py'),run_name='__main__')
