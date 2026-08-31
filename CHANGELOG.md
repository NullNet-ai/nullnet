# Changelog

All Nullnet releases with the relative changes are documented in this file.

## [UNRELEASED]
### Added
- Persist ingress and egress sessions to SQLite and show the full history on the Sessions page, filterable by direction and service, with its own retention window ([#170](https://github.com/NullNet-ai/nullnet/pull/170))
### Changed
- Create/tear down VXLAN tunnels via `rtnetlink` in-process instead of spawning `vxlan-setup.sh`/`vxlan-teardown.sh`, following the same strategy as VLAN access ports; MACsec SA/key installation and XFRM state/policy stay CLI-based, since neither is exposed by rtnetlink ([#168](https://github.com/NullNet-ai/nullnet/pull/168) — fixes [#141](https://github.com/NullNet-ai/nullnet/issues/141))
- Tear edges down on proven connection liveness instead of routing-event timers ([#164](https://github.com/NullNet-ai/nullnet/pull/164) — fixes [#126](https://github.com/NullNet-ai/nullnet/issues/126))
- Move stack service configuration from TOML files to normalized SQLite tables, and rework the Config page to edit services through per-field widgets instead of a raw TOML textarea; the whole configuration can still be imported from or exported to a TOML file via dedicated buttons ([#163](https://github.com/NullNet-ai/nullnet/pull/163) — fixes [#140](https://github.com/NullNet-ai/nullnet/issues/140))
- Persist events to SQLite with time-based retention ([#157](https://github.com/NullNet-ai/nullnet/pull/157) — fixes [#151](https://github.com/NullNet-ai/nullnet/issues/151))
- Install BPF linker as a prebuilt binary rather than compiling it from source ([#158](https://github.com/NullNet-ai/nullnet/pull/158))
### Removed
### Fixed
- Stop a teardown from corrupting a chain that is still being set up ([#167](https://github.com/NullNet-ai/nullnet/pull/167) — fixes [#166](https://github.com/NullNet-ai/nullnet/issues/166))
- Reject a service name claimed by more than one stack, instead of resolving it to an arbitrary stack ([#165](https://github.com/NullNet-ai/nullnet/pull/165) — fixes [#129](https://github.com/NullNet-ai/nullnet/issues/129))
- Show the date alongside the time for timestamps from before today, instead of `hh:mm:ss` only, in the topology panels, Sessions, and Events pages ([#159](https://github.com/NullNet-ai/nullnet/pull/159) — fixes [#135](https://github.com/NullNet-ai/nullnet/issues/135))

## [v0.1.0] - 2026-08-17
Nullnet control plane first release — routing in the dark
