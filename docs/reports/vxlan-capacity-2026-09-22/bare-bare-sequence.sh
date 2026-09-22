#!/bin/bash
set -euo pipefail
exec > /tmp/nullnet-bare-sequence.log 2>&1
python3 /tmp/nullnet-docker-step.py
bash /tmp/nullnet-direct-run.sh 1000 32 nullnet-bare-steps.py
cp /tmp/nn-bare-steps.json /tmp/nn-final-steps.json
bash /tmp/nullnet-direct-run.sh 1000 32 nullnet-direct.py "${1:-0}"
cp /tmp/nn-direct-result.json /tmp/nn-final-bare-full.json
if [[ ${1:-0} == 1 ]]; then
 cp /tmp/nn-setup-kernel.trace /tmp/nn-final-bare-kernel.trace
 cp /tmp/nn-setup-kernel-stats.json /tmp/nn-final-bare-kernel-stats.json
fi
NN_FORWARD_ONCE=1 bash /tmp/nullnet-direct-run.sh 1000 32 nullnet-direct.py
cp /tmp/nn-direct-result.json /tmp/nn-final-bare-once.json
printf 'BARE_SEQUENCE_COMPLETE\n'
