# Changelog

All Nullnet releases with the relative changes are documented in this file.

## [UNRELEASED]
### Added
- Add per-stack observation mode with all-to-all backend triggers and configuration suggestions ([#194](https://github.com/NullNet-ai/nullnet/pull/194))
- Show backend trigger sessions in session history with a destination service peer, gray N/A policy, and a Kind column ([#189](https://github.com/NullNet-ai/nullnet/pull/189))
- Serve the UI with a managed certificate selected by `UI_TLS_DOMAIN`, with first-run self-signed setup and automatic certificate reload ([#188](https://github.com/NullNet-ai/nullnet/pull/188))
- Enable one-year HSTS on proxy HTTPS responses by default, with `HSTS_ENABLED=false` for development ([#188](https://github.com/NullNet-ai/nullnet/pull/188))
- Add an opt-in “Pausable when idle” checkbox for Docker services, persisted in configuration with a false default ([#187](https://github.com/NullNet-ai/nullnet/pull/187) — fixes [#180](https://github.com/NullNet-ai/nullnet/issues/180))
- Optional service host pinning and automatic proxy TCP/UDP listen-port firewall allowances ([#184](https://github.com/NullNet-ai/nullnet/pull/184) — fixes [#177](https://github.com/NullNet-ai/nullnet/issues/177))
- Per-service egress/ingress traffic filters: arbitrary AND/OR/group combinations of Country, Organization, Src IP (ingress), and Dst IP (egress) conditions, evaluated via `rpn-predicate-interpreter` — replaces the country-only egress/ingress policy ([#171](https://github.com/NullNet-ai/nullnet/pull/171) — fixes [#143](https://github.com/NullNet-ai/nullnet/issues/143))
- Persist ingress and egress sessions to SQLite and show the full history on the Sessions page, filterable by status, service, direction, and policy verdict, with its own retention window ([#170](https://github.com/NullNet-ai/nullnet/pull/170) — fixes [#156](https://github.com/NullNet-ai/nullnet/issues/156))
### Changed
- Avoid redundant sudo in the root client’s VXLAN lifecycle and release the topology lock before persisting idle backend session closures ([performance report](docs/nullnet-performance-2026-09-21.md); PR pending).
- Configure persistent conntrack capacity during client setup, preserving higher host limits ([performance report](docs/nullnet-performance-2026-09-21.md); PR pending).
- Simplify the dashboard to linked session, service, and node count cards ([#194](https://github.com/NullNet-ai/nullnet/pull/194))
- Unify live and historical session views ([#191](https://github.com/NullNet-ai/nullnet/pull/191))
- Simplify backend configuration to a `backends` array of service names and derive trigger ports from peer declarations ([#190](https://github.com/NullNet-ai/nullnet/pull/190))
- Replace backend trigger chains with a single `peer` service in configuration and the UI ([#189](https://github.com/NullNet-ai/nullnet/pull/189))
- Create/tear down VXLAN tunnels via `rtnetlink` in-process instead of spawning `vxlan-setup.sh`/`vxlan-teardown.sh`, following the same strategy as VLAN access ports; MACsec SA/key installation and XFRM state/policy stay CLI-based, since neither is exposed by rtnetlink ([#168](https://github.com/NullNet-ai/nullnet/pull/168) — fixes [#141](https://github.com/NullNet-ai/nullnet/issues/141))
- Tear edges down on proven connection liveness instead of routing-event timers ([#164](https://github.com/NullNet-ai/nullnet/pull/164) — fixes [#126](https://github.com/NullNet-ai/nullnet/issues/126))
- Move stack service configuration from TOML files to normalized SQLite tables, and rework the Config page to edit services through per-field widgets instead of a raw TOML textarea; the whole configuration can still be imported from or exported to a TOML file via dedicated buttons ([#163](https://github.com/NullNet-ai/nullnet/pull/163) — fixes [#140](https://github.com/NullNet-ai/nullnet/issues/140))
- Persist events to SQLite with time-based retention ([#157](https://github.com/NullNet-ai/nullnet/pull/157) — fixes [#151](https://github.com/NullNet-ai/nullnet/issues/151))
- Install BPF linker as a prebuilt binary rather than compiling it from source ([#158](https://github.com/NullNet-ai/nullnet/pull/158))
### Removed
### Fixed
- Release backend session locks during history writes and save configuration changes atomically in one transaction ([performance report](docs/nullnet-performance-2026-09-21.md); PR pending).
- Keep NetworkManager from adopting Nullnet-owned interfaces during tunnel creation ([performance report](docs/nullnet-performance-2026-09-21.md); PR pending).
- Batch event persistence, bound RPC bursts, accelerate VXLAN lifecycle operations, and wait for both endpoints before releasing backend/egress traffic ([performance report](docs/nullnet-performance-2026-09-21.md); PR pending).
- Route same-host egress directly with container-scoped forwarding and NAT ([#197](https://github.com/NullNet-ai/nullnet/pull/197))
- Track container overlay addresses when deciding whether backend connections are idle ([#196](https://github.com/NullNet-ai/nullnet/pull/196))
- Make backend and egress trigger claims atomic, preserve liveness during setup, and report setup failures instead of transient packet-wait timeouts ([#186](https://github.com/NullNet-ai/nullnet/pull/186))
- Set both same-host veth MAC addresses at creation to prevent udev races from breaking encrypted connections ([#185](https://github.com/NullNet-ai/nullnet/pull/185))
- Stop a teardown from corrupting a chain that is still being set up ([#167](https://github.com/NullNet-ai/nullnet/pull/167) — fixes [#166](https://github.com/NullNet-ai/nullnet/issues/166))
- Reject a service name claimed by more than one stack, instead of resolving it to an arbitrary stack ([#165](https://github.com/NullNet-ai/nullnet/pull/165) — fixes [#129](https://github.com/NullNet-ai/nullnet/issues/129))
- Show the date alongside the time for timestamps from before today, instead of `hh:mm:ss` only, in the topology panels, Sessions, and Events pages ([#159](https://github.com/NullNet-ai/nullnet/pull/159) — fixes [#135](https://github.com/NullNet-ai/nullnet/issues/135))
- Stop a trigger whose RPC timed out from permanently killing a container's egress (fixes [#178](https://github.com/NullNet-ai/nullnet/issues/178))
- End each egress session when its own connections close, instead of holding every destination on an edge open until the last one finishes ([#181](https://github.com/NullNet-ai/nullnet/pull/181) — fixes [#179](https://github.com/NullNet-ai/nullnet/issues/179))

## [v0.1.0] - 2026-08-17
Nullnet control plane first release — routing in the dark
