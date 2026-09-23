### 14. Admit encrypted traffic through the existing peer firewall, without a shared plaintext UDP allowance

**Difference.** The marked-IPsec path needs the existing reference-counted known-peer allowance for ESP. It does not need an entry mapping shared UDP 4790 to one peer in VXLAN_PORTS. Keep the plaintext-drop policy guards before exposure. Unencrypted VXLAN keeps its existing admission path.

**Why this is required.** VXLAN_PORTS currently maps one port to one peer, and its removal is per edge, so naively registering every encrypted shared-port edge would overwrite peer bindings and remove a shared entry prematurely. The existing eBPF program already permits ESP for known peers; PEERS already has per-edge reference counting. No widening of the static UDP allowlist or loss of per-edge cryptographic authentication is needed.

**Evidence and implementation boundary.** Every real-host marked-IPsec packet proof runs with only the two temporary PEERS entries; it does not add a UDP4790 VXLAN_PORTS entry. Wrong SA/VNI, missing crypto and unrelated-edge survival are tested. The current product call site still registers nondefault destination ports, so the future marked-encryption branch must explicitly skip that plaintext registration and preserve the existing PEERS lifetime handling. This is part of the implementation recipe, not a claim that product code was changed.


Status: experimental design; see ../analysis.md for measured scope and integration limits.
