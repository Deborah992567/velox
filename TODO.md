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

## Phase 5 — HTTP/1.1 parser + response engine ✅
- [x] Full method set, chunked, trailers, Expect: 100-continue, pipelining,
      smuggling defenses, limits

## Phase 6 — Static file server ✅
- [x] Document roots, MIME, index, listing, ETag/Last-Modified, Range, sendfile

## Phase 7 — Routing + virtual hosts ✅
- [x] Exact/prefix/regex locations, precedence, host/port/SNI matching

## Phase 8 — TLS ⬜
- [ ] rustls integration, SNI cert selection, session resumption, cert reload

## Phase 9 — Reverse proxy ⬜
- [ ] Streaming request/response forwarding, header rewriting, retries, timeouts

## Phase 10 — Upstream connection pooling ⬜
- [ ] Keepalive pool, max connections, lifecycle management

## Phase 11 — Load balancing + health checks ⬜
- [ ] RR / weighted RR / least-connections, passive + active health, backups

## Phase 12 — WebSockets ⬜
- [ ] Handshake, frame codec, ping/pong/close/fragmentation, proxying

## Phase 13 — FastCGI / SCGI / uWSGI ⬜
- [ ] ProtocolAdapter trait; FastCGI gateway with pooling/timeouts

## Phase 14 — Compression ⬜
- [ ] gzip + Brotli + zstd, negotiation, MIME filters, streaming, Vary

## Phase 15 — Caching ⬜
- [ ] Memory + disk backends, TTL, invalidation, LRU, stampede-safe locking

## Phase 16 — Rate limiting + access control ⬜
- [ ] Token bucket, per-IP/per-route, connection limits; CIDR allow/deny, basic auth

## Phase 17 — Master/worker architecture ⬜
- [ ] fork model, SO_REUSEPORT, supervision, privilege drop, `start`/`stop`/`status`

## Phase 18 — Graceful reload / shutdown ⬜
- [ ] `reload` with zero dropped connections, drain, log rotation

## Phase 19 — HTTP/2 ⬜
- [ ] Framing, HPACK (RFC 7541 vectors), stream FSM, flow control, GOAWAY

## Phase 20 — QUIC foundation ⬜
- [ ] quinn integration behind our reactor; handshake + streams over UDP

## Phase 21 — HTTP/3 ⬜
- [ ] h3 frames + QPACK + control streams; ngtcp2 interop harness green

## Phase 22 — Dynamic modules ⬜
- [ ] Lifecycle hooks + cdylib loading; example module; ABI docs

## Phase 23 — Metrics ⬜
- [ ] Counters/histograms, latency, cache hit/miss, upstream errors, Prometheus endpoint

## Phase 24 — Security hardening ⬜
- [ ] Smuggling/desync fuzz results, header injection, slowloris simulation, limits

## Phase 25 — Fuzzing + compliance + load testing ⬜
- [ ] cargo-fuzz targets (parser, HPACK, QPACK, chunked), compliance suites

## Phase 26 — Performance optimization ⬜
- [ ] Profile-driven optimization (sendfile, buffer reuse, syscall batching); benchmarks

## Phase 27 — Packaging + release ⬜
- [ ] systemd/launchd units, deb/rpm/brew, versioned releases, docs complete

## Benchmarking
- [ ] Benchmark suites vs nginx/apache/caddy (static, proxy, TLS, h2, h3,
      concurrency, latency, CPU, memory) — no claims without measurements.
