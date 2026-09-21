# CRM outage investigation — September 17, 2026

Handoff written September 18 for resuming on Monday, September 21.
Status: investigation only; no fix implemented and no lab reproduction yet.

## Report and scope

The team reported a CRM white page / failure to load lasting approximately one
hour, with no warning or error in the Events tab. The user placed it roughly
ten hours before the end of the supplied twelve-hour logs. Working window:
September 17, 14:40–15:40, with 14:00–16:00 as the wider search window.
These are the logs' timestamps; their timezone has not been established.
The exact outage boundaries are not known.

Prioritize Nullnet routing delays. CRM application logs are deferred because
the measured waits occur before the HTTP upstream is contacted.

## Evidence and provenance

Local input files at the repository root (currently untracked):

| File | Coverage | Lines |
| --- | --- | ---: |
| `18-09.log` | Sep 17 12:41:54 – Sep 18 00:41:56 | 260,290 |
| `plog.log` | Sep 17 12:48:51 – Sep 18 00:49:02 | 601,163 |

The first file contains only `nullnet-server` output, PID 2777557. The second
contains `nullnet-proxy` output, PID 2777568. Both identify host
`dna-insta-docker-130-30-1`. Source was inspected at local HEAD `28a4c2e`;
the deployed revision was not verified. Confirm it before treating the source
analysis below as an exact account of production behavior.

### Server observations

- 14:40–15:40: 2,654 CRM routing requests, 2,631 session-reuse messages.
- Example: `18-09.log:73073–73074`, 14:40:00, CRM request followed by
  `112.198.193.25` being reported as already set up.
- Both nodes continued service reports. The longest per-node report interval
  across the whole file was 21 seconds. This does not prove application or
  data-plane health, but does not show an hour-long control-channel absence.
- No explicit failure messages were found in this server journal.
- `Proxy client ... timed out` means idle-session expiry, not an HTTP timeout.
- Events are persisted separately; absence from this journal does not establish
  absence from the Events database. No database export was inspected.

### Proxy observations

- `plog.log:225569–225570`, 15:12:12: upstream `10.0.6.137:8937` immediately
  precedes `TOTAL VLANS SETUP TIME: 79826 ms`.
- `plog.log:225424–225425`, 15:12:05: the same CRM upstream immediately
  precedes a 72,675 ms timing.
- `plog.log:225421–225422`, 15:12:05: upstream `10.0.5.17:8934` immediately
  precedes a 136,027 ms timing. The slowdown extends beyond CRM.
- In 14:40–15:40, 1,327 routing timings were at least ten seconds.
- Pairing timings with an immediately preceding upstream on CRM port 8937
  yields 295 samples of at least ten seconds, including 25 of at least a
  minute, in that window. This is an adjacency-based classification, not a
  guaranteed request-ID join: concurrent log messages can interleave.
- There are 1,467 identical HTTP/2 error-response write failures across the
  proxy log, including 391 in 14:40–15:40:

  ```text
  Failed to send proxy error response:  WriteError context: while writing h2 response to downstream cause: user error: unexpected frame type
  ```

  These messages omit the original failure, hostname and request identity.
  They describe failure to send an error response, not the original reason
  routing or HTTP processing failed. They also occur outside the suspected
  outage. Do not label them the CRM root cause.

### Arrival versus server processing correlation

For CRM and external IP `112.198.193.25`, in the half-open window
15:05:00–15:16:00, there are 597 proxy `ProxyRequest` entries and 597 server
`is already set up` entries. Pairing those sequences in order gives delays
up to 79 seconds before the server reuse message, consistent with proxy
timings. Example: proxy line 223753 at 15:10:54 and server line 97711 at
15:12:13. This pairing assumes requests preserve order; no request IDs exist
to prove individual matches.

| Minute | Proxy arrivals | Server reuse messages |
| --- | ---: | ---: |
| 15:05 | 74 | 74 |
| 15:06 | 36 | 36 |
| 15:07 | 26 | 5 |
| 15:08 | 39 | 45 |
| 15:09 | 53 | 26 |
| 15:10 | 98 | 111 |
| 15:11 | 11 | 18 |
| 15:12 | 115 | 130 |
| 15:13 | 65 | 72 |
| 15:14 | 8 | 8 |
| 15:15 | 72 | 72 |

Cumulative arrivals minus reuse messages, starting at 15:05, peaks at 54
at 15:12:21 and returns to zero by the end. This is a backlog proxy, not a
direct measurement of a specific mutex's waiters. Existing-session reuse
dominates this episode; tunnel creation alone cannot explain it.

## Source findings

1. `nullnet-proxy/src/main.rs::upstream_peer` starts the timing before resolving
   an upstream via gRPC and prints it before returning the `HttpPeer`.
   It is not CRM application response time. The label "VLANS SETUP TIME" also
   includes queueing and lookup overhead for already-established sessions.
2. `nullnet-server/src/nullnet_grpc_impl.rs::handle_proxy_request` serializes
   on `(service_name, external_client_ip, proxy_ip)`. The mutex remains held
   through `proxy_request_locked` and `mark_connection_open`.
   Users behind a shared public IP share this serialization key.
3. The server's `Received proxy request` message is inside
   `proxy_request_locked`, after acquiring that mutex. It does not log RPC
   arrival before the queue.
4. Even sticky reuse awaits `Event::sticky_session_reused` persistence, then
   acquires the global services write lock to update the timestamp. The
   connection-open accounting acquires that lock again.
5. `events.rs::EventStore::emit` awaits database insertion. `db/events.rs`
   uses the database's shared `Arc<Mutex<AsyncSqlite>>`; repositories share
   this connection. This puts database contention on routing's critical path.
6. `timeout.rs::check_timeouts` holds the global services write lock while
   awaiting backend reaping, timeout application and pause reconciliation.
   Teardown paths await session persistence and event emission under that
   lock. Teardown ACK waiting itself is detached in
   `orchestrator.rs::spawn_deferred_net_id_free`; do not incorrectly blame a
   synchronous 30-second ACK wait here. Outbound channel sends still await.
7. `nullnet-grpc-lib/src/lib.rs::proxy` has no explicit per-RPC deadline.
   The configured connection timeout and HTTP/2 keepalive are not request
   latency budgets. Long lookups can eventually succeed without an error.
8. Successful routing / sticky reuse events are informational. HTTP
   `fail_to_proxy` does not report the original failure as an event;
   `logging` ignores its error argument and handles connection accounting.
   Upstream lookup failures have an event, but a long successful lookup does
   not become a warning. The raw TCP relay's connect-failure event does not
   cover HTTP proxy failures.

All source paths above are under `members/` unless otherwise stated.

## Working hypothesis and limits

The evidence supports a Nullnet routing backlog, including requests whose
sessions are already established. Per-client serialization can amplify slow
database persistence or services-lock acquisition into minute-long waits.
This is a source-supported hypothesis, not a reproduced root cause.

The logs do not identify the initiating bottleneck. Database/disk latency,
global-lock contention, gRPC transport queueing and runtime scheduling have
not been separated. There are no lock timings, database timings, resource
metrics, request IDs, or confirmed deployed revision. Do not claim SQLite,
cleanup, CRM, or the HTTP/2 write error caused the reported hour-long outage.
The logs also do not prove a full hour of continuous unavailability.

## Monday plan

1. Confirm production revision and timestamp timezone. Preserve both input
   logs. Establish the precise outage window if available.
2. Reproduce on the two Linux lab hosts using the deployed behavior: multiple
   services/replicas, normal expiry/teardown activity, and concurrent HTTP
   requests sharing an external client IP. Include a different-IP comparison.
   Test established sessions as well as fresh setup. Follow the lab setup
   instructions and inspect remote dirty state before copying anything.
3. Add diagnostic timing sufficient to distinguish RPC arrival/transport,
   per-session mutex wait, services-lock wait/hold, event/database wait/write,
   setup ACK wait and total routing latency. Include a correlation ID so
   interleaved messages can be joined reliably. Do not overload Events with
   routine per-request timing telemetry.
4. Compare response latency and queue growth with database load and idle
   cleanup activity. Use controlled delays only as mechanism tests; do not
   confuse them with proof of the production initiating cause.
5. Choose a fix only after reproduction identifies the bottleneck. Preserve
   session serialization's prevention of duplicate chain/refcount creation,
   sticky dependency validation, open/close accounting (including retries),
   teardown ordering, and event/session persistence semantics. Simply removing
   the mutex, adding an arbitrary timeout, or disabling expiry is not a
   justified fix. Cancellation after a deadline also needs accounting review.
6. Follow all four project gates: reproduced failure; static regression review
   and full CI on Linux; multi-host E2E under complex concurrent load with
   before/after timings and graph state; then operator diagnostics and docs.
   Decide separately how to report sustained routing degradation and final
   HTTP upstream failures without flooding Events. Complete proto/server/UI
   wiring if adding an event.

No implementation, build, deployment or E2E verification was performed during
this investigation. Existing `CHANGELOG.md` modifications were left untouched.
