### 12. Enable a sufficiently wide IPsec replay window

**Difference.** Current per-port IPsec leaves replay protection at its default disabled value. The selected cross-host candidate uses 4,096 packets, represented through XFRMA_REPLAY_ESN_VAL without enabling extended sequence numbers.

**Why this change is needed.** A finite window must accommodate normal packet reordering while rejecting old captured ciphertext. The 4,096-packet setting passed the final load and replay tests. It does not alter keys, SPI/mark binding or confidentiality.

**Evidence and limits.** The combined candidate repeats the full two-host packet proof, including an old ESP frame replayed after more than 4,096 packets, wrong key, wrong authenticated SA/VNI, total crypto removal and fresh-key recreation. The final 1,000-edge run passed all 2,000 directional pings, with zero sequence rejections and no XFRM error-counter increases after both hosts finished installation. Phase snapshots record 974 missing-SA packets on host 104 during concurrent installation, when the two sides are not yet jointly ready; that count stays unchanged through every ping and TCP sample. Product readiness must wait for both endpoint setup acknowledgements. No claim of zero cumulative installation drops is made. TCP throughput still varies by edge position and TCP retransmissions remain; those are reported separately. The window is a measured deployment choice, not a guarantee for arbitrary network reordering. Sequence exhaustion and coordinated fresh-key rekeying remain necessary. Do not silently reset sequence numbers under a reused key.




Status: experimental design; see ../analysis.md for measured scope and integration limits.
