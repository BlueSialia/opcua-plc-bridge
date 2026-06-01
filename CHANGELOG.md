# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [v0.2.2] - 2026-06-01

### Changed

- Use awk to extract release notes instead of git-cliff

- Measure compile time with cargo-bloat to compile only once


### Fixed

- Add certificate rejection store and PLC connection tuning options

## [v0.2.1] - 2026-05-31

### Added

- Rename gateway to bridge in e2e-tests


### Changed

- Release 0.2.1

- Move `apply_byte_order` function to driver-common

- Extract common byte-order parsing into helper function

- Extract driver setup and OPC UA config into dedicated helpers

- Remove unused cargo.toml and native feature

- Add Docker container build and push to release workflow

- Improve binary check and release workflows

- Set workflow permissions explicitly


### Documentation

- Add ´#![warn(missing_docs)]` and docs to all crates


### Fixed

- Use bounded channel and log dropped events

- Skip decode when tag definition is missing in registry

- Gracefully shut down write bridge thread

- Log health channel send errors instead of ignoring them

- Add trace log for clock skew detection in tag staleness check

- Prevent overflow when advancing modbus chunk_start

- Use relaxed ordering for sid counter increment

## [v0.2.0] - 2026-05-31

### Added

- Initial release


### Changed

- Release 0.2.0

