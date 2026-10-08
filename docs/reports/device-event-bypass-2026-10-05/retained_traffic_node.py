"""Run the existing traffic proof against retained bridge/veth/VXLAN devices."""
from pathlib import Path
import runpy
import sys

REPORTS = Path('/tmp/nullnet-retained-profile/docs/reports')
sys.path.insert(0, str(REPORTS / 'kernel-parallelism-2026-10-01'))
sys.path.insert(0, str(REPORTS / 'device-event-bypass-2026-10-05'))
import node

node.Minimal = node.cross.Cross
runpy.run_path(str(REPORTS / 'device-event-bypass-2026-10-05/traffic_node.py'), run_name='__main__')
