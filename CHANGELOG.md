# Changelog

All Nullnet releases with the relative changes are documented in this file.

## [UNRELEASED]
### Added
- Optional service host pinning and automatic proxy TCP/UDP listen-port firewall allowances ([#184](https://github.com/NullNet-ai/nullnet/pull/184) — fixes [#177](https://github.com/NullNet-ai/nullnet/issues/177))
- Per-service egress/ingress traffic filters: arbitrary AND/OR/group combinations of Country, Organization, Src IP (ingress), and Dst IP (egress) conditions, evaluated via `rpn-predicate-interpreter` — replaces the country-only egress/ingress policy ([#171](https://github.com/NullNet-ai/nullnet/pull/171) — fixes [#143](https://github.com/NullNet-ai/nullnet/issues/143))
- Persist ingress and egress sessions to SQLite and show the full history on the Sessions page, filterable by status, service, direction, and policy verdict, with its own retention window ([#170](https://github.com/NullNet-ai/nullnet/pull/170) — fixes [#156](https://github.com/NullNet-ai/nullnet/issues/156))
### Changed
- Create/tear down VXLAN tunnels via `rtnetlink` in-process instead of spawning `vxlan-setup.sh`/`vxlan-teardown.sh`, following the same strategy as VLAN access ports; MACsec SA/key installation and XFRM state/policy stay CLI-based, since neither is exposed by rtnetlink ([#168](https://github.com/NullNet-ai/nullnet/pull/168) — fixes [#141](https://github.com/NullNet-ai/nullnet/issues/141))
- Tear edges down on proven connection liveness instead of routing-event timers ([#164](https://github.com/NullNet-ai/nullnet/pull/164) — fixes [#126](https://github.com/NullNet-ai/nullnet/issues/126))
- Move stack service configuration from TOML files to normalized SQLite tables, and rework the Config page to edit services through per-field widgets instead of a raw TOML textarea; the whole configuration can still be imported from or exported to a TOML file via dedicated buttons ([#163](https://github.com/NullNet-ai/nullnet/pull/163) — fixes [#140](https://github.com/NullNet-ai/nullnet/issues/140))
- Persist events to SQLite with time-based retention ([#157](https://github.com/NullNet-ai/nullnet/pull/157) — fixes [#151](https://github.com/NullNet-ai/nullnet/issues/151))
- Install BPF linker as a prebuilt binary rather than compiling it from source ([#158](https://github.com/NullNet-ai/nullnet/pull/158))
### Removed
### Fixed
- Set both same-host veth MAC addresses at creation to prevent udev races from breaking encrypted connections ([#185](https://github.com/NullNet-ai/nullnet/pull/185))
- Stop a teardown from corrupting a chain that is still being set up ([#167](https://github.com/NullNet-ai/nullnet/pull/167) — fixes [#166](https://github.com/NullNet-ai/nullnet/issues/166))
- Reject a service name claimed by more than one stack, instead of resolving it to an arbitrary stack ([#165](https://github.com/NullNet-ai/nullnet/pull/165) — fixes [#129](https://github.com/NullNet-ai/nullnet/issues/129))
- Show the date alongside the time for timestamps from before today, instead of `hh:mm:ss` only, in the topology panels, Sessions, and Events pages ([#159](https://github.com/NullNet-ai/nullnet/pull/159) — fixes [#135](https://github.com/NullNet-ai/nullnet/issues/135))
- Stop a trigger whose RPC timed out from permanently killing a container's egress (fixes [#178](https://github.com/NullNet-ai/nullnet/issues/178))
- End each egress session when its own connections close, instead of holding every destination on an edge open until the last one finishes ([#181](https://github.com/NullNet-ai/nullnet/pull/181) — fixes [#179](https://github.com/NullNet-ai/nullnet/issues/179))

## [v0.1.0] - 2026-08-17
Nullnet control plane first release — routing in the dark
