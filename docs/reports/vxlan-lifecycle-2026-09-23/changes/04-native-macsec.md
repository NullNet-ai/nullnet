### 4. Replace MACsec subprocesses with native route + generic Netlink

**Difference.** Send MACsec device creation through NETLINK_ROUTE and TX SA/RX channel/RX SA updates through the `macsec` generic-Netlink family. Retain the same order and check every ACK. This replaces four `ip` launches, or one `ip -batch` launch, per endpoint.

**Why it can be equivalent.** The experiment follows the actual Linux UAPI and iproute2 source, including 64-bit cipher ID, 16-byte key ID, 32-byte AES key, association number, PN and SCI. In particular `IFLA_MACSEC_PORT` is encoded in network byte order: the existing rtnetlink `.port(1)` endianness bug must NOT be reintroduced. A normal Netlink ACK does not contain a newly allocated interface index.

**Validation.** Compare normalized kernel configuration and actual encrypted delivery against iproute2, not just successful ACKs. Batching/native Netlink is not atomic: partial configuration still requires cleanup.

**Observed evidence.** The native path carries actual encrypted traffic in the production-style same-host /29 layout. On both hosts it passes bidirectional traffic, MTU, TX-SA disable/restore, five fresh-key reuse cycles, unrelated-edge survival and benchmark-container restart recovery. The selected after includes all three generic-Netlink operations and device creation. Replay is explicitly enabled at window 128, whereas current setup defaults differ; this is a security strengthening with a finite reordering window, not byte-for-byte configuration equivalence.



Status: experimental design; see ../analysis.md for measured scope and integration limits.
