#!/bin/bash
set -euo pipefail
cleanup() {
 if [[ -d /sys/kernel/tracing/instances/nn-setup-attribution ]]; then python3 /tmp/nullnet-attribution-trace.py stop; fi
 python3 - <<'PY'
import subprocess,json,pathlib
names=subprocess.check_output(['ip','netns','list'],text=True).splitlines()
if any(l.split()[0]=='nn-direct-base' for l in names):
 r=subprocess.run(['ip','-n','nn-direct-base','link','del','group',str(0x4e600001)],capture_output=True)
 remaining=json.loads(subprocess.check_output(['ip','-n','nn-direct-base','-j','link','show'],text=True))
 assert not [x for x in remaining if x.get('group')==0x4e600001],r.stderr
for line in subprocess.check_output(['ip','netns','list'],text=True).splitlines():
 name=line.split()[0]
 if name.startswith('nn-dir-') or name=='nn-direct-base':subprocess.run(['ip','netns','del',name],check=True)
left=[x for x in subprocess.check_output(['ip','netns','list'],text=True).splitlines() if x.split()[0].startswith('nn-dir-') or x.split()[0]=='nn-direct-base']
assert not left,left
pathlib.Path('/tmp/nn-direct-cleanup.json').write_text(json.dumps({'residual_test_namespaces':len(left),'group_devices_removed':True}))
PY
}
trap cleanup EXIT
ip netns add nn-direct-base
ip -n nn-direct-base link add underlay type dummy
ip -n nn-direct-base addr add 192.0.2.1/24 dev underlay
ip -n nn-direct-base link set underlay up
if [[ ${4:-0} == 1 ]]; then python3 /tmp/nullnet-attribution-trace.py start; fi
python3 "/tmp/${3:-nullnet-direct.py}" "$1" "$2"
if [[ ${4:-0} == 1 ]]; then python3 /tmp/nullnet-attribution-trace.py stop; fi
