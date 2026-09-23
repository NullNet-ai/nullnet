### 6. Set forwarding policy once, with reconciliation

**Difference.** Remove repeated `sysctl ip_forward=1` and `iptables -P FORWARD ACCEPT` from each endpoint setup after startup establishes them.

**Why it can preserve behavior.** These are host/namespace-global settings, not endpoint state. The previous benchmark proved repeated nftables transactions dominated setup. But existing startup code, Docker/firewall reloads and failure diagnostics must be reconciled; “once” cannot mean silently assuming the setting never changes. No broader firewall policy is proposed.



Status: experimental design; see ../analysis.md for measured scope and integration limits.
