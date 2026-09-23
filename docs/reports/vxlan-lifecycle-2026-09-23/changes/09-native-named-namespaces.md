### 9. Native creation and removal of standalone network namespaces

**Difference.** A dedicated OS thread saves its network namespace FD, creates a new network namespace with unshare(CLONE_NEWNET), bind-mounts /proc/thread-self/ns/net at the owned named path, and restores the original namespace in a finally block. Removal uses umount2(MNT_DETACH) and unlink after endpoint links have been deleted. This replaces one ip netns process per creation/removal and preserves a fresh namespace for each incarnation.

**Why it can preserve behavior.** These are the same core operations used by iproute2. The /run/netns mount directory must first be initialized and made shared exactly once under the startup lock; the experiment's outer ip netns fixture provides that initialization. Use the calling thread's namespace path, never /proc/self/ns/net on a multithreaded client. Treat a namespace-restore failure as fatal to that worker. Do not recycle dirty namespaces or move arbitrary async-runtime threads.

**Observed evidence and limit.** Both hosts passed three 1,000-endpoint same-host and cross-host trials, native default-route installation, endpoint-to-gateway ping, exact owned-object cleanup and absence of the namespace mount paths afterward. This reduces process overhead, especially removal, but standalone full-sequence throughput remains below the 600+/s goal. This is a measured extra candidate, not grounds to claim the Docker rate for standalone endpoints.

[Actual iproute2 namespace implementation](https://github.com/iproute2/iproute2/blob/v6.12.0/ip/ipnetns.c).



Status: experimental design; see ../analysis.md for measured scope and integration limits.
