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

### Phase 7 — Routing + virtual hosts
- `routing::host`: `Host` parsing (`name`, `name:port`, `[IPv6]:port`, bare
  IPv6 — malformed values rejected) and nginx-style `server_name` patterns:
  exact names (optionally port-scoped), `*.suffix` / `prefix.*` wildcards,
  `~`/`~*` regexes, and the `_` catch-all. `match_names` selects the most
  specific name: exact > longest wildcard > regex (declaration order) > `_`.
- `routing::location`: `LocationMatcher` (exact, prefix, `^~` prefix, regex)
  and `Location<T>` carrying opaque handler config plus an optional named
  label for internal redirects. `match_location` applies the documented
  nginx-faithful precedence: exact wins; else longest prefix (a `^~` prefix
  halts the regex pass); else the first regex in declaration order; else the
  longest prefix. Prefix matching is a substring test on the raw path; regex
  locations expose capture groups (optional groups preserved) and can never
  match a non-UTF-8 path.
- `routing::router`: `VirtualHost<T>` (names + location table, named-location
  lookup) and `Router<T>` (host list + default server). `select_host` tries
  the `Host` header then the SNI value, matching exact > wildcard > regex >
  catch-all across all servers (nginx order); `route` combines host selection
  with location dispatch and reports unmatched paths so the caller can answer
  `404`.
- `regex` added as a production dependency per ADR 0003 (location regex
  matching); 275 unit + integration tests across the workspace.

### Phase 8 — TLS termination
- `tls::keypair`: `KeyPair` loads a certificate chain + private key from PEM
  (files or memory) with strict validation: non-empty chain, non-empty
  certificates, and a key supported by the `ring` signing backend (RSA,
  ECDSA, Ed25519). Produces rustls `CertifiedKey`s.
- `tls::resolver`: `SniResolver` implements `ResolvesServerCert`, selecting a
  certificate by SNI (case-insensitive) and falling back to the default —
  the nginx behaviour for a `default_server` certificate.
- `tls::config`: `TlsServerOptions` (TLS 1.3 + 1.2 by default, ALPN
  `http/1.1`, bounded TLS 1.2 session cache, resumption on) and `TlsConfig`,
  an immutable, rebuildable `ServerConfig`. `reload()` swaps the certificate
  resolver live while in-flight connections keep the old config. Resumption
  uses a ring-based `Ticketer` + `ServerSessionMemoryCache`; disabling it
  installs no-op ticket/storage impls.
- `tls::stream`: `TlsStream<S>` drives a rustls `ServerConnection` handshake
  over any `Read + Write` transport, exposes the negotiated version and ALPN
  protocol, and forwards plaintext `Read`/`Write`/`flush`.
- `ErrorKind::Tls` errors surface with the underlying `rustls::Error` as the
  source; 18 TLS unit tests (full handshakes over `UnixStream` pairs cover
  ALPN/version negotiation, per-name SNI selection, default fallback, session
  tickets + reconnect, and live cert reload); 308 tests across the workspace.

### Phase 9 — Reverse proxy
- `proxy::config`: nginx-style `proxy_pass` parsing (`http://`, `https://`,
  `unix:` sockets, explicit ports, IPv6 host brackets, `URI-prefix` semantics)
  into a resolved `ProxyTarget`; `ProxyOptions` with connect/read/send timeouts
  and retry count.
- `proxy::rewrite`: request-target rewriting (verbatim passthrough without a
  URI part; matched-location-prefix replacement preserving suffix + query),
  `Host` pinned to the upstream authority, hop-by-hop stripping honouring
  `Connection` tokens, and `X-Forwarded-For` / `X-Real-IP` /
  `X-Forwarded-Proto` injection.
- `proxy::exchange`: a streaming, fully blocking exchange. Request head + body
  relay (fixed and chunked, chunk decode → re-encode), client
  `Expect: 100-continue` handling, response-head parsing with interim `1xx`
  relay and limit checks, framing normalization (TE wins over CL; lone
  digits-only CL wins; close-delimited fallback chunk-encoded for HTTP/1.1
  clients and raw with `connection: close` for HTTP/1.0), trailer relay, and
  socket-level connect/read/send timeouts.
- Retries: only bodyless idempotent requests, capped at `retries` extra
  attempts, and only before any byte has reached the client (`Relayed` error
  guard); `https://` upstreams are parsed but rejected until outbound rustls
  lands.
- `Method::is_idempotent`; `net::connect_with_timeout` now restores blocking
  mode on its immediate-success path (previously left the socket non-blocking,
  surfacing as `WouldBlock` on Unix sockets).
- 51 proxy unit tests (in-memory peers + real Unix-socket upstreams) and 364
  tests across the workspace.

### Phase 10 — Upstream connection pooling
- `net::socket::peek`: non-blocking `MSG_PEEK` classification of a connected
  socket (`Empty` quiescent / `Data` leftover pipelined bytes / `Eof` peer
  closed) via a zero-timeout `poll` + `recv(MSG_PEEK|MSG_DONTWAIT)`.
- `proxy::pool::UpstreamPool`: a keepalive pool of upstream connections with
  `PoolOptions` (`max_connections` total per target, `max_idle` kept, `idle_timeout`
  expiry, `acquire_timeout`). `borrow` draws a healthy idle connection (dropping
  stale/expired ones detected by `peek`) or connects within the cap, blocking
  on a condition variable while saturated; `try_borrow` is non-blocking.
  `close_idle` drains idle connections at shutdown/reload; `in_use`/`idle_len`/
  `total` report state. Returned connections are reused only when the exchange
  ended at a message boundary (`PooledConnection::mark_reusable`), else closed.
- `proxy::exchange`: the retry loop is shared by the direct and pooled paths.
  `proxy_exchange_pooled` runs the same exchange borrowing from `UpstreamPool`
  and returns fully-consumed connections to it; close-delimited and failed
  connections are closed instead. `PoolOptions` `Default` is 32 max / 8 idle /
  60 s idle / 5 s acquire.
- 9 pool tests (fd reuse, stale-leftover discard, close-on-non-reusable, capacity
  blocking, saturated timeout + release wait, `max_idle` trimming, `close_idle`,
  idle expiry) and 3 pooled-exchange end-to-end tests (single keepalive
  connection across requests, close-delimited not pooled, retry over the pool);
  362 tests across the workspace.

### Phase 11 — Load balancing + health checks
- `proxy::upstream::UpstreamGroup`: a named group of `UpstreamServer` peers
  (each with `weight`, `backup`, `max_fails`, `fail_timeout`) sharing one
  `BalancePolicy`, each with its own keepalive `UpstreamPool`. Down servers
  whose `fail_timeout` has elapsed are revived on the next selection; backups
  are only used when every primary is down.
- `BalancePolicy`: `RoundRobin` (cyclic cursor), nginx-style smooth
  `WeightedRoundRobin` (signed accumulator, `current_weight - total` after each
  pick, no clumping), and `LeastConnections` (fewest in-flight requests).
- Passive health: `SelectedPeer::finish_success` resets the failure streak,
  `SelectedPeer::fail` marks a peer down after `max_fails` consecutive
  failures; connect/relay/prepare failures on bodyless idempotent requests
  retry across other peers capped at `ProxyOptions::retries`.
- Active health: `HealthChecker` spawns a background probe thread over
  `HealthCheckConfig` (interval, timeout, `ProbeKind::Tcp` or `ProbeKind::Http`
  requiring a 2xx/3xx status). `probe_all` feeds results into the same
  passive-health state, bypassing the pools so probes never borrow connections.
- `proxy::exchange`: the response engine exposes `prepare_response`/`relay_body`
  and `BodyRelay`/`PreparedResponse` to the balancer; `NoHealthyUpstream` when
  every server (and backup) is down.
- `proxy_exchange_lb`: runs the Phase 9 exchange against balanced peers and
  reports outcomes back to group health; 9 group/balancer/health tests
  including an end-to-end round-robin exchange over two live Unix-socket
  upstreams; 386 tests across the workspace.

### Phase 12 — WebSockets
- `websocket::handshake`: RFC 6455 upgrade classification (`is_websocket_upgrade`),
  `accept_key` computation, `build_accept_header`, `client_request` builder;
  10 handshake tests covering RFC vectors, edge cases, and header validation.
- `websocket::frame`: full `Frame` encode/decode with masking, extended 16/64-bit
  lengths, 125-byte control-payload cap, `FrameDecoder` incremental parser, and
  `MessageDecoder` for reassembling fragmented text/binary messages with UTF-8
  validation; 19 frame tests including RFC interop vectors.
- `proxy::websocket`: bidirectional `ws_relay` copying raw bytes between client
  and upstream until one side EOFs or errors; 3 relay tests (both directions,
  EOF exit, upstream close).
- `proxy::exchange`: `prepare_response` detects 101 Switching Protocols (and no
  longer misclassifies it as an interim 1xx); `is_ws_upgrade_response` guard
  routes to `BodyRelay::WsRelay`, `clear_ws_timeouts` disables `SO_RCVTIMEO`/`SO_SNDTIMEO`
  on the upstream before entering the relay. `WsRelay` variant added to `BodyRelay`
  and handled in both direct and load-balanced exchange paths.
- `proxy::rewrite`: `rewrite_ws_request_headers` preserves `Upgrade`, `Connection`,
  and `Sec-WebSocket-*` headers while applying standard proxy header rewriting;
  `is_ws_passthrough` classifies WebSocket-specific header names.
- 406 tests across the workspace.
See [`TODO.md`](TODO.md) for the full phase-by-phase roadmap.
