### 5. Replace namespace `nsenter ip` subprocesses with namespace-bound Netlink sockets

**Difference.** A dedicated OS thread enters the selected network namespace long enough to open a Netlink socket, then restores its original namespace. The socket remains bound to the target namespace and configures the endpoint address/MTU/UP directly.

**Why it can be equivalent.** It sends the same route-Netlink operations in the same order to the same namespace. No async-runtime thread may call `setns`. Preserve standalone default routes and Docker-owned routes. Namespace/socket FDs must be closed after ownership ends.

**Container identity condition.** Removing per-endpoint Docker inspection requires an authoritative discovery entry, container ID/start generation and a pinned namespace FD. A forever PID cache is unsafe under container restart, name reuse, PID reuse and watcher gaps. Refresh/invalidate on events, reconcile watcher reconnect, and validate generation before publishing a setup. The refreshed same-host proof restarts a benchmark container, rejects the old pinned pidfd generation before creating a veth, refreshes namespace/pid handles and restores connectivity. This is evidence for the identity mechanism, not an implementation of production Docker event/watch reconnect handling. The namespace-bound sockets are opened on ordinary dedicated worker threads, not async runtime threads.



Status: experimental design; see ../analysis.md for measured scope and integration limits.
