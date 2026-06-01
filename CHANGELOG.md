# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.1](https://github.com/Popsink/kora/compare/v0.3.0...v0.3.1) - 2026-06-01

### Other

- *(deps)* bump rust in the docker-all group ([#38](https://github.com/Popsink/kora/pull/38))
- *(deps)* bump the rust-minor-patch group across 1 directory with 9 updates ([#39](https://github.com/Popsink/kora/pull/39))
- *(deps)* bump azure/setup-helm from 4 to 5 in the actions-all group ([#33](https://github.com/Popsink/kora/pull/33))

## [0.3.0](https://github.com/Romderful/kora/compare/v0.2.4...v0.3.0) - 2026-04-27

### Added

- *(chart)* support existingSecret for password and URL via secretKeys ([#32](https://github.com/Romderful/kora/pull/32))

### Other

- *(deps)* bump sha2 from 0.10.9 to 0.11.0 ([#26](https://github.com/Romderful/kora/pull/26))
- *(deps)* bump metrics-exporter-prometheus from 0.16.2 to 0.18.1 ([#27](https://github.com/Romderful/kora/pull/27))
- *(deps)* bump jsonschema from 0.45.1 to 0.46.2 ([#28](https://github.com/Romderful/kora/pull/28))
- *(deps)* bump the rust-minor-patch group with 3 updates ([#25](https://github.com/Romderful/kora/pull/25))

## [0.2.4](https://github.com/Romderful/kora/compare/v0.2.3...v0.2.4) - 2026-04-25

### Added

- add Bitnami-style Helm chart for Kubernetes deployment ([#23](https://github.com/Romderful/kora/pull/23))

## [0.2.3](https://github.com/Romderful/kora/compare/v0.2.2...v0.2.3) - 2026-04-19

### Other

- *(deps)* bump the docker-all group with 2 updates ([#19](https://github.com/Romderful/kora/pull/19))
- configure Dependabot for automated dependency updates ([#14](https://github.com/Romderful/kora/pull/14))

## [0.2.2](https://github.com/Romderful/kora/compare/v0.2.1...v0.2.2) - 2026-04-19

### Added

- structured request logging with OTel-aligned field names ([#12](https://github.com/Romderful/kora/pull/12))

### Fixed

- accept case-insensitive boolean query params (Confluent Python compat) ([#9](https://github.com/Romderful/kora/pull/9))

## [0.2.1](https://github.com/Romderful/kora/compare/v0.2.0...v0.2.1) - 2026-04-18

### Fixed

- content-type negotiation — default to application/json (Confluent compat)

## [0.2.0](https://github.com/Romderful/kora/compare/v0.1.2...v0.2.0) - 2026-04-18

### Added

- k6 load test suite + performance and correctness fixes

## [0.1.2](https://github.com/Romderful/Kora/compare/v0.1.1...v0.1.2) - 2026-04-15

### Other

- align API endpoint descriptions + update dev recipes in README

## [0.1.1](https://github.com/Romderful/Kora/compare/v0.1.0...v0.1.1) - 2026-04-15

### Added

- CI/CD pipeline — GitHub Actions, release-plz, Docker multi-arch
