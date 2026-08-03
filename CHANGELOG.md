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

### Phase 3 — Event loops
- `platform`: `EventDriver` trait (`register`/`modify`/`deregister`/`wait`)
  with native implementations — epoll on Linux, kqueue on macOS/iOS — behind
  a cfg-gated factory; `Token` (slab index | generation packed into a u64),
  `Interest`/`Ready` flag sets, and `Event`.
- `timers`: hierarchical hashed `TimerWheel` (10 ms tick, two 256-slot levels
  plus overflow list) with O(1) add/cancel, `TimeoutKind` covering the
  per-connection stages, and injected-clock polling; fixes for delivering
  due-timer events and for timers stranded by wheel advance.
- `event_loop::Slab`: generation-counted token slab with free-list reuse, so
  fd churn can never resurrect a stale token.
- `event_loop::Executor`: single-threaded FIFO executor whose futures need not
  be `Send`; wakers are built from a raw `RawWakerVTable` (the `Wake` trait
  would force `Send + Sync`), and scheduling state lives in a `Cell` shared
  separately from the task so a task can wake itself from inside its own
  `poll` without a double borrow.
- `event_loop::Reactor`: wires the driver, slab, timer wheel, and executor into
  the architecture §3.1 loop body — drain the executor, expire timers and wake
  connection tasks, block on the driver never past the next timer deadline,
  dispatch readiness to wakers, drain again. `poll_readable`/`poll_writable`
  follow a level-triggered contract; the reactor is a cheaply cloneable `Rc`
  handle that tasks capture to re-register interest from inside a poll.
- 120 unit + integration tests across the workspace.

### Planned phases
See [`TODO.md`](TODO.md) for the full phase-by-phase roadmap.
