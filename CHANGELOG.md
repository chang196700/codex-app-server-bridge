# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.4](https://github.com/chang196700/codex-app-server-bridge/compare/v0.1.3...v0.1.4) - 2026-10-14

### Fixed

- use a single configuration file for service lifecycle

## [0.1.3](https://github.com/chang196700/codex-app-server-bridge/compare/v0.1.2...v0.1.3) - 2026-10-14

### Added

- optionally start and recover the Codex app-server daemon

## [0.1.2](https://github.com/chang196700/codex-app-server-bridge/compare/v0.1.1...v0.1.2) - 2026-10-14

### Added

- update installed services and executables from GitHub releases

## [0.1.1](https://github.com/chang196700/codex-app-server-bridge/compare/v0.1.0...v0.1.1) - 2026-10-14

### Fixed

- refresh pending changelogs from complete Git history

## [0.1.0](https://github.com/chang196700/codex-app-server-bridge/releases/tag/v0.1.0) - 2026-10-14

### Other

- include native static musl release archives
- use native GNU release-plz on Ubuntu runners
- automate native multi-architecture releases with release-plz
- Fix systemd unit scalar values and stopped-unit cleanup
- Add config migration and CLI with stable service executables
- Add English README and retain Chinese documentation
- Rename Codex WebSocket environment publication setting
- Make automatic environment publication opt-in
- Fix macOS environment Clippy warning
- Implement cross-platform Codex socket WebSocket bridge
