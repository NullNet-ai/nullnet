# Observation mode verification — 2026-09-15

## Automated checks

Linux lab builds, formatting and Clippy passed for the server, proxy, gRPC,
client and eBPF components. Server tests: 254 passed; proxy: 38 passed;
client: 85 passed, 2 existing ignored. UI TypeScript and production build passed.
UI lint reports 16 existing errors: comparison with unchanged HEAD produced
exactly the same file/rule counts. No new lint findings were introduced.

New server tests cover activation conflicts, concurrent activation, config-write
locks, permissions, retention-independent counts, open-session seeding, frozen
completed runs, backend-only application and rejection of stale results after
service configuration changes.

## Two-host traffic verification

Eight nginx services ran across Linux hosts 192.168.1.103 and 192.168.1.104,
four on each host, with distinct ports 18100–18107. The saved configuration
initially allowed only obs0 → obs7 and retained a separate proxy dependency.

- Before activation, obs0 → obs1 failed (curl exit 7, 3845 ms).
- Activation installed 56 backend triggers while preserving the saved config.
- Traffic exercised 24 directed pairs, including same-host and cross-host
  connections. All 480 load requests succeeded, plus all 24 warmup requests.
  Host 104 completed 240 requests in 2.50 s; host 103 in 2.35 s. The measured
  p95 command durations were 161.51 ms and 166.27 ms respectively.
- Stop/apply reported 24 added edges and one unused edge removed. The resulting
  config contained 24 backends; proxy dependencies were unchanged.
- After application, obs0 → obs1 succeeded in 314.99 ms.

Snapshots of `/api/graph/observation_e2e` contained eight registered service
nodes. Before activation there was one egress edge and no backend edge; after
warmup there were 24 backend edges. The load snapshot contained 25 total edges,
including egress. After application and a fresh request, the live graph showed
obs0 → obs1 with `setup_ms: 229`.

A separate restart run preserved its nonzero observation counts, restored all
56 runtime triggers and registered all eight replicas. A newly observed
obs0 → obs4 connection succeeded in 438.3 ms; after stopping observation the
same connection timed out, confirming restoration of the saved rules.
Containers were restarted after clients initialized, as required by lab setup.

## Browser verification

Playwright against the lab server verified:

- Topology automatically fetched three history pages containing 1201 sessions.
- The session side panel retained pagination.
- Observation fetched no session details and displayed no proxy nodes or panel.
- Both matrices outlined saved links even when they had zero sessions.
- Config fields were disabled during observation.
- Stop/apply worked through the UI, and a count of 600 survived deletion of
  individual session history.
- No browser page errors occurred.

The initial browser run checked a 20-unit proxy gap. The subsequent visual
revision uses a 36-unit empty gap, individual fixed-size grid cells and brighter
2.5-unit saved-link borders. Its TypeScript/production build passed, and the
updated assets are served by the fake-data preview at localhost:5173.

## Lab artifacts

Detailed logs and JSON snapshots are in `/tmp/nullnet-observation-*` on host
104 (host 103 also has its load log). Local screenshots and restart results are
under `/private/tmp/nullnet-observation-evidence` and
`/private/tmp/nullnet-observation-restart-result.json`. Testing used an isolated
source directory and database under `/root/nullnet-observation-preview`; the
existing dirty source checkout and its saved configuration were preserved.
