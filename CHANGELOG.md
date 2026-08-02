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

### Phase 1 — Foundations: errors, logging, configuration, CLI
- `core::error`: `AegisError` with kind, source chaining, and
  `file:line:column` positions (`ErrorKind`, `SourcePos`, `Context`).
- `logging`: severity `Level` (syslog-style), structured `LogRecord`, text and
  JSON `LogFormat`, `LogSink` (stdout/stderr/file/null) with buffering and
  reopen, a process-global `Logger` with level filtering and fan-out, and the
  `log!`/`log_<level>!` macros.
- `config`: a hand-written lexer with `line:column` tracking, an AST
  (`ConfigRoot`/`ConfigNode`) with depth-first traversal, a recursive-descent
  parser, and a validator with a Phase 1 directive registry (context, argument
  count, and type checks: `worker_processes auto|N`, sizes with `k/m/g`
  suffixes, `on|off`).
- `aegis` CLI: `-v`/`--version`, `-V` build information (git revision, rustc,
  target — captured by `build.rs`), and `-t [FILE]` configuration testing in
  nginx style with default search paths.
- `configs/aegis.conf.example` reference configuration.
- Integration tests that spawn the binary; 62 unit + 8 integration tests.

### Phase 2 — Cross-platform sockets
- `net::InetAddr`: IPv4, IPv6, and Unix domain endpoints; `listen`-style
  parsing (bare port expands to dual-stack wildcards, bracketed IPv6, `unix:`
  paths) and raw `sockaddr` conversion.
- `net` socket layer on `libc`: `socket`/`bind`/`listen`/`accept`,
  `setsockopt` (reuseaddr, SO_REUSEPORT, keepalive, TCP_NODELAY, IPv6-only),
  `O_NONBLOCK` control, and `getsockname`/`getpeername` — the crate's single
  scoped `unsafe` zone with SAFETY comments on every block.
- `net::SocketOptions`: builder-style listener knobs (reuseport, ipv6_only,
  keepalive, nodelay, backlog, nonblocking).
- `net::Listener`: bind + listen + non-blocking accept with per-connection
  option application; `net::Connection`: owned fd wrapper with addresses,
  options, and `Read`/`Write`.
- Tests cover parsing, sockaddr round-trips, IPv4 + Unix accept/echo,
  SO_REUSEPORT sharing, and bind conflicts.

### Planned phases
See [`TODO.md`](TODO.md) for the full phase-by-phase roadmap.
