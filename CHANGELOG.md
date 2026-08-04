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

### Phase 4 — Connection manager + buffers
- `buffer::IoBuf`: cursor-based (start/end) per-connection buffer with
  `put`/`read`/`peek`, `consume` and prefix-only `try_consume`, `reserve`/
  `reclaim` (moves the live tail to the front without shrinking the
  allocation), and a zero-initialized `spare_mut`/`advance_written` window for
  direct reads off the socket.
- `connection`: `ConnectionManager` — a cloneable `Rc` handle over a
  generation-counted `Slab` of per-connection state. `register` forces
  non-blocking, registers READABLE interest, and arms an idle timer;
  `close` is idempotent (cancel timer, deregister, drop the slot).
- `ReadOutcome` (Read/Eof/WouldBlock/Capacity) and `WriteOutcome`
  (Flushed/Buffered/Backpressured) drive a bounded pipeline: input is capped
  by the per-connection read cap, and output uses a high/low water-mark pair
  (hysteresis) so producers pause above the high-water mark and resume only
  after flushing drains below the low-water mark.
- `set_stage`/`check_timeout` re-arm per-connection deadline timers by stage
  (Idle, HeadRead, BodyRead, …) ahead of the HTTP layer; an `EchoHandler`
  future exercises read → echo → re-arm → clean close on EOF/timeout.
- `net::set_int_option` exposed for raw `setsockopt` access (test socket
  tuning); 134 unit + integration tests across the workspace.

### Phase 5 — HTTP/1.1 parser + response engine
- Strict header field line parsing: first-colon split, tchar-validated names
  (obs-fold continuation lines rejected), OWS-stripped values, and control-byte
  rejection other than HTAB; `trim_ows`, `validate_field_value`, `hex_digit`.
- `RequestParser`: incremental head FSM that consumes exactly what it is fed
  and applies `RequestLimits` *during* scanning so a head can never grow
  unbounded. Request lines split into exactly `method SP target SP version`
  with tchar-only methods; targets validated against the RFC 9112 forms
  (origin, asterisk-`OPTIONS`-only, authority-`CONNECT`-only, absolute).
  Structural smuggling defenses: CL+TE co-presence rejected, single
  digits-only `Content-Length`, single exactly-`chunked` `Transfer-Encoding`,
  and a mandatory single `Host` on HTTP/1.1. Errors map to 400/414/431/505;
  completed heads auto-reset with trailing bytes (body/pipelined request)
  buffered for `drain_pending`.
- `ChunkedDecoder`: incremental chunked transfer decoder (RFC 9112 §7.1)
  writing decoded bytes into a caller buffer, with exact-hex sizes, strict
  CRLF framing, structural extension checks, and capped trailer parsing.
- `engine`: injection-safe response encoder — every outgoing field is
  re-validated (a CR/LF in a value is an error, not a split), framing headers
  are derived and suppressed for bodyless statuses, and chunked frames stream
  via `encode_chunk`/`encode_last_chunk`. Adds the shared `Response` head type.
- 191 unit + integration tests across the workspace.

### Phase 6 — Static file server
- `static_files::mime`: extension-driven `Content-Type` table (case-insensitive,
  `application/octet-stream` fallback) shared by files, listings, and error
  pages.
- `static_files::date`: HTTP-date formatting and parsing — IMF-fixdate plus the
  two obsolete formats a recipient must accept (RFC 850, asctime), with strict
  weekday/range validation and RFC 6265 two-digit-year expansion.
- `static_files::validators`: strong ETags (`"<hex(mtime)>-<hex(size)>"`) and
  second-granularity `Last-Modified` from `Metadata`, plus conditional-request
  evaluation (`If-Match` strong, `If-None-Match` weak, `If-Modified-Since`,
  `If-Unmodified-Since`) with quote-aware list splitting.
- `static_files::range`: RFC 9110 §14 byte-range parser (`int-range`,
  `suffix-range`, multiple ranges, `416`/ignore semantics) resolving ranges
  against a resource length.
- `static_files::resolver`: percent-decoding → dot-segment normalization →
  traversal rejection (`403`), plus NUL/backslash rejection and a final
  under-root containment check (architecture §11).
- `static_files::listing`: HTML directory listings — dirs first, dotfiles
  hidden, HTML-escaped display names, percent-encoded hrefs, human-readable
  sizes.
- `platform::sendfile`: zero-copy `send_file` helper — Linux `sendfile(2)` and
  the macOS two-fd variant, returning partial-transfer counts for looping.
- `static_files::handler`: the orchestrator — `GET`/`HEAD` only (else `405`
  with `Allow`), `400`/`403` for resolver failures, trailing-slash `301`
  redirects for directories, index-file fallback, optional listings, `304`
  from `If-None-Match`/`If-Modified-Since`, and `200`/`206` file responses with
  `Date`, `ETag`, `Last-Modified`, `Accept-Ranges`, `Content-Type`, and
  `Content-Range` (`416` with `bytes */len`); bodies are returned as
  [`StaticBody`] ready for `sendfile`.
- `HeaderName` gains `LastModified` and `Allow`; 246 unit + integration tests
  across the workspace.
See [`TODO.md`](TODO.md) for the full phase-by-phase roadmap.
