# Changelog

All Nullnet releases with the relative changes are documented in this file.

## [UNRELEASED]
### Added
### Changed
- Tear edges down on proven connection liveness instead of routing-event timers ([#164](https://github.com/NullNet-ai/nullnet/pull/164) — fixes [#126](https://github.com/NullNet-ai/nullnet/issues/126))
- Move stack service configuration from TOML files to normalized SQLite tables and rework the Config page to edit services through per-field widgets instead of a raw TOML textarea ([#163](https://github.com/NullNet-ai/nullnet/pull/163) — part of [#140](https://github.com/NullNet-ai/nullnet/issues/140))
- Persist events to SQLite with time-based retention ([#157](https://github.com/NullNet-ai/nullnet/pull/157) — fixes [#151](https://github.com/NullNet-ai/nullnet/issues/151))
- Install BPF linker as a prebuilt binary rather than compiling it from source ([#158](https://github.com/NullNet-ai/nullnet/pull/158))
### Removed
### Fixed
- Show the date alongside the time for timestamps from before today, instead of `hh:mm:ss` only, in the topology panels, Sessions, and Events pages ([#159](https://github.com/NullNet-ai/nullnet/pull/159) — fixes [#135](https://github.com/NullNet-ai/nullnet/issues/135))

## [v0.1.0] - 2026-08-17
Nullnet control plane first release — routing in the dark