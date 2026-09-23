# VXLAN encrypted setup and teardown

[Start here: next steps and key documents from September 22–23](../VXLAN-NEXT-STEPS.md).

This package contains the current baseline and the final tested endpoint candidate. Superseded designs and intermediate measurements are excluded.

- [Full before/after report](nullnet-setup-teardown-before-after.pdf) — all four cases, individual steps, complete sequences, initialization and final infrastructure destruction; [Markdown](analysis.md).
- [Change rationale](change-rationale.pdf) — what changes, why it can preserve behavior and the evidence; [Markdown](change-rationale.md) and [14 individual documents](changes/README.md).
- [Sequence totals](sequence-summary.csv), [step breakdown](step-breakdown.csv), [operation breakdown](operation-breakdown.csv), [all 48 lifecycle trials](all-trials.csv) and [repeated dataplane results](dataplane-trials.csv).
- [Evidence validation summary](validation-summary.json).

The final Docker endpoint prototype measures **647–801 setups/s and 876–996 teardowns/s**, using medians of three 1,000-endpoint trials per host and topology. A bounded pool reuses fully reset dedicated bridges. Same-host keeps per-edge MACsec; cross-host uses shared-port VXLAN with independent directional IPsec SAs/keys and authenticated SA-to-VNI checks. No VLAN-based replacement for VXLAN is proposed.

**Limits:** retiring an endpoint leaves only an anonymous clean bridge available for reuse. Destroying the complete 1,000-bridge pool still takes approximately 19–20 seconds. Fresh standalone namespace sequences remain below 600/s. These are kernel endpoint measurements with real packet proofs, not full Nullnet RPC/control-plane or application-ready network throughput. Product code was not changed.

## Evidence layout

`raw-103.tar.gz` and `raw-104.tar.gz` contain the selected records, attribution traces and continuity audits. Extract into `raw-103/` and `raw-104/` to inspect or regenerate the reports. The selected prefixes are:

| Prefix | Measurement |
| --- | --- |
| `current-` | Current Docker endpoint sequence |
| `standalone-before-current-` | Current standalone sequence |
| `dedicated-verified-native-` | Final Docker candidate |
| `dedicated-standalone-verified-native-` | Final standalone candidate |

Each combination has three repeats for same-host and cross-host, on both hosts. Trial records include full endpoint timings, operation timings, configuration, errors and cleanup inventories. `churn-dedicated.json`, `partial-failure-dedicated.json` and `samehost-dedicated-proof.json` retain the additional host-local checks.

`dedicated-marked-proof.json` records two-host connectivity, gateway/MTU, wrong key, wrong authenticated SA/VNI, replay rejection, fresh-key reuse, seeded bridge reset and fail-closed capture. `frame-parity.json` compares exact Ethernet delivery and routed SNAT/FORWARD behavior with current setup. `traffic-dedicated-repeat-*.json` contains three alternating current/candidate dataplane repetitions. `scale-dedicated-proof.json` records 1,000 real cross-host edges, 2,000 directional pings, first/last-edge TCP and XFRM counters.

The baseline source is local commit `6b62eff4c705777f9c99f51983fd50d8053f1404`. Existing application containers and the lab repositories' pre-existing changes were left untouched.

## Reproduction

The retained Python harnesses are the experimental source used for the final runs, not production Rust implementations. They require root on Linux, iproute2, Docker, iptables, bpftool, iperf3, tcpdump and Python 3.13. They contain lab-specific addresses, fixture names and SSH access. Packet coordinators temporarily admit only the two isolated test peer addresses to the existing firewall and remove those exact entries afterward. Review their scope before running them elsewhere.

Copy the required Python modules to `/tmp/` and `/tmp/nn23/` on both hosts. Run the lifecycle suite with 32 workers and eight CLI slots. The final environment is:

```
NN_BRIDGE_POOL=1 NN_LINK_ECHO=1 NN_WORKERS=32 NN_DELETE_BATCH=256
NN_UNIQUE_KEYS=1 NN_UNIQUE_REQID=1 NN_SHARED_INBOUND=1 NN_REPLAY_WINDOW=4096
NN_LABEL=dedicated-verified
```

Export those variables, then run `python3 /tmp/nn23/run-suite.py native same` and `python3 /tmp/nn23/run-suite.py native marked`. Standalone adds `NN_STANDALONE=1 NN_NATIVE_NAMED=1` and changes the label to `dedicated-standalone-verified`. Run `samehost-proof.py`, `partial-failure.py` and `churn.py` with the Docker environment; churn also uses `NN_CHURN_BATCHES=256`. Every suite owns its fixtures and checks original container continuity.

Two-host coordinators use `NN_PROOF_MODE=dedicated_marked` for `pooled-proof.py` (its historical filename), `NN_DEDICATED_TRAFFIC=1` for `repeat-traffic.py`, and `NN_SCALE_MODES=dedicated_marked NN_SCALE_OUTPUT=scale-dedicated-proof.json` for `scale-proof.py`. The retained harness supports more modes than the selected report; only the configuration above is the final candidate. Finish with `audit-lab.py` on each host.

After extracting the archives, `python3 validate-evidence.py` checks the retained outcomes offline. `build-report.py` regenerates the PDFs, Markdown, individual change documents and CSVs using ReportLab. The reports distinguish experimental validation from the remaining product release gates.
