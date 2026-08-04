# Aegis (Velox) — Architecture

Status: **Approved** · Date: 2026-08-02 · ADRs: see [`ADR/`](../ADR/)

This document is the authoritative description of the Aegis web server's
architecture: a production-grade, Nginx-class web server, reverse proxy, load
balancer, TLS terminator, HTTP/2 and HTTP/3 endpoint, and application gateway,
implemented from scratch in Rust.

Binary name: `aegis`. Repo/product name: **Velox**.

---

## 1. Design goals

- Event-driven, asynchronous, non-blocking; the event loop never blocks on
  request processing.
- Master/worker process architecture with graceful reload (zero dropped
  connections) and graceful shutdown.
- Modular: isolated subsystems behind narrow interfaces; dynamic modules with
  bounded blast radius.
- Linux = primary production platform (epoll first, io_uring-ready); macOS/BSD
  = development platform (kqueue).
- Measured performance over claims: benchmarks vs nginx/apache/caddy recorded
  before any performance claims.
- Security by construction: strict parsing, hard limits, defense in depth.

## 2. Process architecture

```
┌─────────────────────────────────────────────────────────────┐
│                        MASTER                               │
│  · parse + validate config   · spawn/supervise workers      │
│  · signal handling           · reload (new cfg + workers)   │
│  · graceful shutdown/drain                                   │
└───────────┬─────────────────────────────────────────────────┘
            │ fork; inherits pre-bound listeners (SO_REUSEPORT)
   ┌────────┴────────┐   ┌────────┴────────┐      ┌────────┴────────┐
   │ WORKER 0        │   │ WORKER 1        │  ... │ WORKER N        │
   │ own event loop  │   │ own event loop  │      │ own event loop  │
   │ conns, requests │   │ upstream pools  │      │ disk cache, etc │
   └─────────────────┘   └─────────────────┘      └─────────────────┘
   Shared via mmap:  rate-limit buckets · global connection counters
   Disk cache:  file-based, safe across processes
```

- Master never accepts connections (except during reload handoff window).
- Workers each run one event loop; `SO_REUSEPORT` balances accept across
  workers at kernel level.
- Single-process dev mode (`aegis start -g`) runs worker logic in-process for
  fast iteration and integration tests.

See ADR 0005.

## 3. Concurrency and async model

- **Per worker**: one reactor, one task scheduler, one timer wheel, one
  connection slab, one upstream pool.
- **Shared across workers (mmap)**: rate-limit buckets, connection counters,
  cache metadata.
- **Async model**: our own reactor + a minimal single-threaded-per-worker
  futures executor. No tokio. `async`/`await` is used internally for protocol
  state machines. See ADR 0002.

### 3.1 Event-loop architecture

```
Reactors per worker:
  EventDriver
     ├ linux/epoll.rs   epoll_create1 + epoll_ctl + epoll_wait
     ├ linux/uring.rs   io_uring (future; same PollDriver behind trait)
     └ macos/kqueue.rs  kqueue + EV_SET + kevent
  Slab<Token→IoObject> with generation counters (fd churn safety)
  TimerWheel: hierarchical hashed timing wheel (O(1) add/cancel)
  Executor: FIFO of ready tasks driven by waker registration
  Timers queue: TimerId → token → connection task wakeup
```

Loop body per worker:

```
loop {
  1. expire timer wheel → wake timed-out connection tasks
  2. drain task ready-queue (poll tasks; new I/O interest registered via wakers)
  3. driver.wait(timeout) → events → token→task → enqueue wakers
  4. handle signals / control messages (self-pipe / signalfd / EVFILT_SIGNAL)
}
```

Non-blocking property: no step performs blocking I/O; accept is batched;
`sendfile` uses zero-copy paths; everything else is readiness-driven.
io_uring fits behind the same `EventDriver` trait (submit/complete instead of
wait) with no architecture change.

## 4. Layer map

```
┌──────────────────────────────────────────────────────────────┐
│  modules/  — lifecycle hooks (access, cache, rate, headers)  │
├──────────────────────────────────────────────────────────────┤
│  http/     — shared Request/Response core, headers, routing   │
│   ├ http1/  parser (ours) · response engine                   │
│   ├ http2/  framing · HPACK · stream FSM · flow control       │
│   └ http3/  h3 frames · QPACK · stream mapping (over quinn)   │
├──────────────────────────────────────────────────────────────┤
│  server/ proxy/ protocols/ cache/ compression/                │
│  rate_limit/ access_control/                                 │
├──────────────────────────────────────────────────────────────┤
│  connection/  — transport abstraction, buffering, backpressure│
│  tls/  quic/                                                  │
├──────────────────────────────────────────────────────────────┤
│  core/   reactor · event driver · timers · memory · executor  │
│  platform/  (linux: epoll [+io_uring later], macos: kqueue)   │
├──────────────────────────────────────────────────────────────┤
│  process/ signals/ config/ logging/ metrics/                  │
└──────────────────────────────────────────────────────────────┘
```

## 5. Repository structure

```
Cargo.toml / Cargo.lock / rust-toolchain.toml
AGENTS.md · CHANGELOG.md · TODO.md · LICENSE
ADR/                        architectural decision records
docs/                       architecture + subsystem docs
crates/
  aegis-core/               the server library (all src/ modules)
  aegis-cli/                the `aegis` binary
tests/                      integration & protocol tests
  unit/ integration/ protocol/ interop/ security/ fuzz/
benches/                    criterion benchmarks
fuzz/                       cargo-fuzz targets
examples/ configs/          example nginx-style configs
tools/                      interop runner + benchmark orchestration
ci/ .github/workflows/      CI (Linux + macOS)
```

Inside `aegis-core/src`: `core/ event_loop/ connection/ buffer/ memory/
timers/ worker/ process/ signals/ http/ tls/ quic/ server/ proxy/ protocols/
cache/ compression/ rate_limit/ access_control/ config/ logging/ metrics/
modules/ platform/ security/`.

## 6. Major interfaces / traits

```rust
trait EventDriver: Send {
    fn register(&mut self, fd: RawFd, interest: Interest, token: Token) -> io::Result<()>;
    fn modify(&mut self, fd: RawFd, interest: Interest, token: Token) -> io::Result<()>;
    fn deregister(&mut self, fd: RawFd) -> io::Result<()>;
    fn wait(&mut self, timeout: Option<Duration>) -> io::Result<Vec<Event>>;
}
struct Event { token: Token, ready: Ready }

trait Reactor {
    fn register_socket(&mut self, sock: &impl AsRawFd, interest: Interest) -> Token;
    fn poll_readable(&self, token: Token) -> Poll<io::Result<()>>;
    fn poll_writable(&self, token: Token) -> Poll<io::Result<()>>;
    fn schedule_timer(&self, token: Token, at: Instant, kind: TimeoutKind) -> TimerId;
    fn spawn(&self, fut: impl Future<Output=()> + 'static);
}

trait AsyncRead { fn poll_read(&mut self, cx, buf: &mut BufMut) -> Poll<io::Result<usize>>; }
trait AsyncWrite { fn poll_write(...); fn poll_flush(...); fn poll_shutdown(...); }

struct HttpRequest  { method, uri: Uri, version, headers: HeaderMap, body: BodyStream, ctx }
struct HttpResponse { status: StatusCode, headers: HeaderMap, body: BodyStream, trailers }
trait HttpBody { /* stream + size hint; chunked/gzip/cache replay */ }

trait Handler {
    fn handle(&self, req: &HttpRequest, resp: &mut HttpResponse) -> BoxFuture<Result<(), HandlerError>>;
}
enum Phase { Connection, RequestHeaders, Route, Access, Content, ResponseHeaders, Log }
trait Module: Send + Sync {
    fn name(&self) -> &'static str;
    fn phases(&self) -> &'static [Phase];
    fn on(&self, phase: Phase, ctx: &mut RequestContext) -> ModuleOutcome; // Continue|ShortCircuit|Error
}

trait LoadBalancer { fn pick(&self, up: &Upstream, ctx: &RequestContext) -> Option<PeerRef>; }
trait HealthCheck  { fn run(&self, peer: &Peer, tick: &mut HealthState); }
trait ProtocolAdapter { fn write_request(...); fn read_response(...); }  // fastcgi/scgi/uwsgi

trait CacheBackend { fn get(&self, k: &CacheKey) -> Option<CachedEntry>; fn put(...); fn invalidate(...); }
trait RateLimiter  { fn allow(&self, key: &LimitKey) -> Decision; }
trait AccessChecker{ fn check(&self, ctx: &RequestContext) -> AccessDecision; }
trait LogSink      { fn write(&self, rec: &LogRecord) -> io::Result<()>; }

trait Directive { fn name() -> &'static str; fn contexts() -> Contexts; fn parse(node) -> Result<Self>; }
```

Interfaces exist where they isolate subsystems (platform, cache, LB, health,
log sinks, adapters, modules). Plain structs + functions everywhere else.

## 7. Data flow for an HTTP request

```
listener READABLE → accept() → Connection{slab token, transport} → reactor.register
READ → IoBuf → Http1Parser (incremental FSM, hard limits)
        · limits: request-line ≤8KB, header block ≤64KB, ≤100 headers,
          body ≤ client_max_body_size
        · smuggling checks: CL vs TE conflicts rejected; chunked validated strictly
routing: virtual_hosts (Host/port/SNI) → locations (prefix → regex → named)
access:  allow/deny (CIDR) · basic auth · method restriction · rate limit
CONTENT: handler produces HttpResponse
   static: stat → Last-Modified/ETag → conditional 304 / Range 206 → sendfile
   proxy:  LB.pick → pool.get_or_connect → write request (stream body) → stream response
   fcgi:   ProtocolAdapter over pooled upstream socket
   ├─ compression: Accept-Encoding → gzip/brotli/zstd streaming encoder
   ├─ cache: key → lookup (bypass rules) → hit: replay; miss: store+lock (stampede-safe)
response engine per protocol:
   http/1: status + headers (injection-safe) → Content-Length or chunked → body → trailers
   http/2: HEADERS/DATA on stream, flow control, WINDOW_UPDATE
   http/3: h3 HEADERS/DATA over quinn bidirectional streams, QPACK
WRITE with backpressure → keep-alive (reset FSM, idle timer) or graceful close
log + metrics: access record (remote, method, uri, status, size, ua, ref, dur, upstream)
```

Per-connection timers at every stage: accept, head-read, body-read,
upstream-connect, upstream-read, response-write, keep-alive idle — all on the
timer wheel; expiry → clean teardown + error status.

## 8. HTTP stack

### 8.1 HTTP/1.x
- Full method set (GET/HEAD/POST/PUT/PATCH/DELETE/OPTIONS/CONNECT/TRACE).
- Our own incremental parser with strict grammar and hard limits.
- Chunked transfer, trailers, Expect: 100-continue, keep-alive, pipelining.
- Query parsing, percent decoding (strict), header normalization, duplicate
  header handling.
- Request smuggling / desync defenses (see §11).
- Full 1xx/2xx/3xx/4xx/5xx status handling.

### 8.2 HTTP/2
- Binary framing, streams, multiplexing, stream state machines.
- SETTINGS/HEADERS/DATA/WINDOW_UPDATE/PING/GOAWAY/RST_STREAM, PRIORITY as
  appropriate, flow control.
- HPACK implemented in-house, validated against RFC 7541 test vectors.
- Connection vs stream error semantics. Shares request/response abstractions
  with HTTP/1.

### 8.3 HTTP/3 (Phase 20–21)
- QUIC transport via `quinn` (audited: handshake, loss recovery, congestion
  control, flow control, connection IDs), driven from our reactor via
  `Endpoint::driver()`.
- HTTP/3 frames, control streams, stream mapping, and QPACK implemented by us
  on top of quinn streams.
- Not considered complete until the ngtcp2 interop harness is green.

## 9. TLS

- `rustls` (TLS 1.2 + 1.3), certificate/private-key loading, chains, SNI
  selection, session resumption, OCSP stapling where available, secure
  defaults, certificate reload (config rebuilt on `reload`).
- No cryptographic primitives implemented by us.

## 10. Static files, routing, proxy, load balancing

- **Static**: document roots, index files, MIME detection, directory listing,
  custom error pages, Last-Modified, ETag, conditional requests, Range/partial
  responses, sendfile/zero-copy where available, path normalization +
  traversal prevention.
- **Routing**: exact/prefix/regex locations, named locations, documented route
  precedence following nginx (exact > longest prefix, with `^~` halting the
  regex pass, > first regex in declaration order, > longest prefix), virtual
  hosts (host + port + SNI matching; exact > wildcard > regex > `_`).
- **Proxy**: streaming request/response forwarding, Host rewriting,
  X-Forwarded-For/X-Forwarded-Proto/X-Real-IP, connection reuse/keepalive,
  pooling, buffering, timeouts, body/response streaming, retries, upstream
  failure handling, max upstream connections. Streaming + event-driven, never
  fully buffered into memory.
- **Load balancing**: pluggable; round-robin, weighted RR, least-connections,
  passive failure detection, active health checks, backup servers, max
  connections, failure thresholds, recovery, timeouts.

## 11. Security model

- **Strict parsing, never speculative**: absolute limits on request-line,
  headers, URI, body; incremental parser consumes exactly what's available.
- **Request smuggling / HTTP desync**: reject CL+TE co-presence, reject
  `TE: chunked` variants on proxy requests, exact chunked grammar, digits-only
  single `Content-Length`, reject obs-fold, injection-safe response builder
  (no CR/LF → no response splitting).
- **Path safety**: canonicalization + `..` rejection, optional symlink
  disable, `Location` header only from validated URI.
- **Resource exhaustion**: per-connection memory/output caps, connection
  limits, slowloris defense (head-read timeout + min read-rate), timers on
  every stage.
- **TLS**: 1.2 minimum, secure ciphers, OCSP, HSTS/security headers opt-in.
- **Input hygiene**: strict percent decoding, control chars rejected, every
  field length/type-checked.
- **Privilege**: master binds, workers drop to unprivileged uid/gid.
- **DoS layers**: rate limiting (mmap-shared), access control, connection
  limits, `server_tokens off`.

## 12. Configuration architecture

```
aegis.conf → Lexer (line/col) → Parser (AST) → Validator (per-directive
contexts + semantic checks) → Runtime Config (typed, immutable, arc-shared)
```

- Nginx-inspired, human-readable format; **no nginx source code copied**.
- Inheritance `http → server → location` with documented override rules.
- Reload: new config fully parsed/validated before any worker is touched;
  failure → keep serving old config, exit nonzero.
- `aegis -t` = lexer + parser + validator only, exit 0/1 with `line:col`
  messages, e.g.
  `aegis: [emerg] "root" directive is not allowed here in aegis.conf:12:5`.

## 13. Module architecture

- Modules declare lifecycle `Phase`s; assembled into per-`server`/`location`
  pipelines at config-build time. `Continue` / `ShortCircuit` / `Error`.
- Modules receive `&mut RequestContext`, never core internals (low coupling).
- Static modules (access, rate limit, cache, compression, security headers)
  ship in-tree. Dynamic modules load as `cdylib` via a documented C ABI shim
  (`aegis_module_init`); ABI stable within a release train. Rust-first; see
  ADR 0004.

## 14. Configuration language example

```
worker_processes 4;

events {
    worker_connections 10000;
}

http {
    server {
        listen 80;
        location / {
            root /var/www/html;
        }
        location /api {
            proxy_pass http://backend;
        }
    }
}
```

## 15. Logging, metrics

- **Access logs**: remote address, timestamp, method, URI, status, size,
  user agent, referer, duration, upstream info. Configurable formats + JSON.
- **Error logs**: debug/info/notice/warn/error/crit/alert/emerg; buffered
  logging; rotation (SIGUSR1 reopen).
- **Metrics**: active/total connections, requests, requests/sec, response
  codes, latency, upstream latency/errors, cache hit/miss, worker health,
  memory, CPU, connection failures. Optional Prometheus-compatible endpoint.

## 16. CLI

```
aegis -v            version
aegis -V            version + build/configure details
aegis -t            validate configuration (exit 0/1)
aegis start         start the server (daemonize)
aegis stop          graceful shutdown
aegis reload        graceful reload (zero dropped connections)
aegis restart       restart
aegis status        worker/master status
```

## 17. Testing strategy

| Tier | Tooling | Scope |
|---|---|---|
| Unit | `cargo test` | parser FSMs, chunked, HPACK/QPACK (RFC vectors), timers, config, CIDR, token bucket, cache, LB, WS codec, FastCGI records |
| Integration | real sockets | full lifecycle: static, proxy, fcgi, WS, TLS, HTTP/2, keep-alive, reload-during-request |
| Protocol | raw-bytes + conformance | malformed/oversized/smuggled HTTP/1; h2load/curl/nginx interop; ngtcp2 interop for HTTP/3 |
| Property | `proptest` | invariants over fuzz input: no panic/loop, limits always enforced |
| Fuzz | `cargo-fuzz` | HTTP/1 parser, chunked, HPACK, QPACK, config parser |
| Security | dedicated suite | smuggling matrix, header injection, slowloris, traversal, oversize |
| Concurrency | stress | parallel conns, keep-alive churn, exhaustion, reload storms |
| Bench | `criterion` + `wrk`/`oha`/`h2load` | micro + macro, recorded before/after |

## 18. Benchmark strategy

- Identical workloads across Aegis vs nginx vs apache vs caddy on the same
  hardware, same TLS certs, documented config intent.
- Scenarios: static (small/large, cache/no-cache), reverse proxy (loopback
  origin), TLS (HTTP/1.1 + HTTP/2), WebSocket echo, FastCGI echo, keep-alive.
- Metrics: req/s, p50/p99/p999 latency, errors, CPU%, RSS. Recorded in
  `docs/performance.md` with methodology + hardware + versions + date.
- No superiority claims without a measurement table; regressions >5% block
  merge.

## 19. Implementation roadmap

Phases are executed strictly in order; each phase's exit criteria (compile +
tests + CI green) must be met before the next begins. See `TODO.md` for the
phase list and per-phase definitions of done.

- Phase 0: Architecture + skeleton ✅ (this doc, ADRs, workspace, CI)
- Phase 1: Foundations — errors, logging, config, CLI
- Phase 2: Sockets · Phase 3: Event loops · Phase 4: Connection manager
- Phase 5: HTTP/1 · Phase 6: Static files · Phase 7: Routing/hosts
- Phase 8: TLS · Phase 9: Reverse proxy · Phase 10: Upstream pooling
- Phase 11: Load balancing · Phase 12: WebSockets · Phase 13: FastCGI/SCGI
- Phase 14: Compression · Phase 15: Caching · Phase 16: Rate limit/ACL
- Phase 17: Master/worker · Phase 18: Reload/shutdown
- Phase 19: HTTP/2 · Phase 20: QUIC · Phase 21: HTTP/3
- Phase 22: Dynamic modules · Phase 23: Metrics · Phase 24: Hardening
- Phase 25: Fuzzing/compliance/load · Phase 26: Performance · Phase 27: Release
