# TODO

Roadmap for Aegis/Velox. Each phase is a milestone that **must compile, pass
its tests, and be green in CI before the next phase begins.** Only genuine
future work is listed here; completed items move to `CHANGELOG.md`.

## Legend

- ✅ done
- 🔄 in progress
- ⬜ pending

## Phase 0 — Architecture and repository skeleton ✅
- [x] Approved architecture (see `docs/architecture.md`)
- [x] ADR 0001–0005
- [x] Workspace + CI + housekeeping

## Phase 1 — Foundations ✅
- [x] Build system polish, crate wiring, error types (`AegisError` with context)
- [x] Logging: levels, formats, JSON structured logs, buffered sinks, rotation hooks
- [x] Config foundations: lexer, parser (AST with line/column), `aegis -t`
- [x] CLI skeleton: `aegis -v`, `-V`, `-t`

## Phase 2 — Cross-platform sockets ✅
- [x] TCP/IPv4, TCP/IPv6, Unix domain sockets; non-blocking + options

## Phase 3 — Event loops ✅
- [x] Linux: epoll driver; macOS: kqueue driver; timers; reactor + executor

## Phase 4 — Connection manager + buffers ✅
- [x] Connection slab, IoBuf with reclamation, backpressure, keep-alive, timeouts
- [x] ConnStats: server-wide accept/close/error tracking with peak counters

## Phase 5 — HTTP/1.1 parser + response engine ✅
- [x] Full method set, chunked, trailers, Expect: 100-continue, pipelining,
      smuggling defenses, limits
- [x] Body size limiter with per-request tracking
- [x] Error page templates with customizable branding
- [x] Status page endpoint for health checks

## Phase 6 — Static file server ✅
- [x] Document roots, MIME, index, listing, ETag/Last-Modified, Range, sendfile

## Phase 7 — Routing + virtual hosts ✅
- [x] Exact/prefix/regex locations, precedence, host/port/SNI matching
- [x] Route parameter extraction (named params, wildcards)

## Phase 8 — TLS ✅
- [x] rustls integration, SNI cert selection, session resumption, cert reload
- [x] Session ticket cache with TTL-based eviction

## Phase 9 — Reverse proxy ✅
- [x] `proxy_pass` parsing (http/https/unix, URI-prefix semantics, IPv6), request
      target/header rewriting (Host, hop-by-hop, forwarded-*), streaming exchange
      with chunked decode/re-encode, 100-continue, retries (bodyless idempotent),
      connect/read/send timeouts, close-delimited fallback
- [x] Retry policy with exponential backoff and method safety checks

## Phase 10 — Upstream connection pooling ✅
- [x] Keepalive pool, max connections, lifecycle management
- [x] PoolStats/PoolMetrics: atomic counters for pool observability

## Phase 11 — Load balancing + health checks ✅
- [x] RR / weighted RR / least-connections, passive + active health, backups
- [x] HealthTracker with failure/success thresholds and cluster health summary
- [x] LbHealthConfig: failover/weighted/strict policies for LB integration

## Phase 12 — WebSockets ✅
- [x] Handshake (upgrade classification, accept key, client request builder)
- [x] Frame codec (encode/decode, masking, extended lengths, limits)
- [x] ping/pong/close control frames, fragmentation + UTF-8 validation
- [x] Proxying (bidirectional streaming upgrade between client and upstream)
- [x] WebSocketConfig: frame/message sizes, timeouts, compression, buffers

## Phase 13 — FastCGI / SCGI / uWSGI ✅
- [x] ProtocolAdapter trait; FastCGI/SCGI/uWSGI gateway adapters

## Phase 14 — Compression ✅
- [x] gzip + deflate + zlib codecs, Accept-Encoding negotiation (RFC 9110)
- [x] CompressionConfig: algorithm presets, levels, min-size thresholds
- [x] ClientPreference parser with quality-value ordering

## Phase 15 — Caching ✅
- [x] In-memory cache with TTL expiry and LRU eviction
- [x] WarmingHintTracker: access pattern analysis for proactive prefetching

## Phase 16 — Rate limiting + access control ✅
- [x] Token bucket, per-IP/per-route, connection limits; CIDR allow/deny
- [x] Rate limit response headers (RFC 6585): ratelimit-limit/remaining/reset/policy

## Phase 17 — Master/worker architecture ⬜
- [ ] fork model, SO_REUSEPORT, supervision, privilege drop, `start`/`stop`/`status`

## Phase 18 — Graceful reload / shutdown ✅
- [x] Config hot-reload watcher with debounce policies (immediate/debounced/manual)
- [x] ShutdownCoordinator: drain with connection counting and timeout

## Phase 19 — HTTP/2 ✅
- [x] Frame parsing, HPACK (RFC 7541), stream FSM, flow control

## Phase 20 — QUIC foundation ⬜
- [ ] quinn integration behind our reactor; handshake + streams over UDP

## Phase 21 — HTTP/3 ⬜
- [ ] h3 frames + QPACK + control streams; ngtcp2 interop harness green

## Phase 22 — Dynamic modules ⬜
- [ ] Lifecycle hooks + cdylib loading; example module; ABI docs

## Phase 23 — Metrics ✅
- [x] Counters, gauges, histograms with bucket-based snapshots
- [x] Prometheus text format endpoint
- [x] Registry with lazy metric creation and thread-safe access

## Phase 24 — Security hardening ✅
- [x] Smuggling/desync detection (Content-Length vs Transfer-Encoding conflicts)
- [x] Slowloris defense: connection timer with request/header/connect timeouts
- [x] Security headers: CSP, HSTS, X-Content-Type-Options, X-Frame-Options, etc.
- [x] CSP nonce generator and policy builder

## Phase 25 — Fuzzing + compliance + load testing ⬜
- [ ] cargo-fuzz targets (parser, HPACK, QPACK, chunked), compliance suites

## Phase 26 — Performance optimization ⬜
- [ ] Profile-driven optimization (sendfile, buffer reuse, syscall batching); benchmarks

## Phase 27 — Packaging + release ⬜
- [ ] systemd/launchd units, deb/rpm/brew, versioned releases, docs complete

## Benchmarking
- [ ] Benchmark suites vs nginx/apache/caddy (static, proxy, TLS, h2, h3,
      concurrency, latency, CPU, memory) — no claims without measurements.
