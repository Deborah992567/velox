# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Phase 0 — Architecture and repository skeleton
- Workspace layout (`crates/aegis-core`, `crates/aegis-cli`) with a clean build.
- ADR 0001–0005: language (Rust), async model (own reactor + executor),
  dependency strategy, module system, process architecture.
- CI: fmt + clippy + test on Linux and macOS.
- Developer workflow (`AGENTS.md`), repository housekeeping.

### Planned phases
See [`TODO.md`](TODO.md) for the full phase-by-phase roadmap.
