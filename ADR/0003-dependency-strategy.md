# ADR 0003: Dependency strategy

- Status: Accepted
- Date: 2026-08-02
- Deciders: Project maintainer

## Context

The project must not be "a wrapper around an existing server", but must also
not reimplement mature cryptography/protocol codecs. We need a clear line
between what we build and what we buy.

## Decision

**Own:** the networking core, event loop, reactor/executor, HTTP processing
pipeline, configuration system, connection management, proxying architecture,
worker architecture, and all major server components.

**Buy (mature, audited, standards/crypto/protocol-critical):**

| Area | Crate | Justification |
|---|---|---|
| TLS 1.2/1.3 | `rustls` + `rustls-pemfile` | Audited, memory-safe, SNI, session resumption, OCSP stapling; we rebuild config for cert reload. |
| QUIC transport | `quinn` | Audited: handshake, loss recovery, congestion control, flow control, connection IDs. Driven from our reactor via `Endpoint::driver()`. HTTP/3 framing + QPACK are ours. |
| Compression | `flate2` · `brotli` · `zstd` | RFC-compliant codecs. |
| Crypto/aux | `sha2` `hmac` `bcrypt` `base64` `subtle` `getrandom` | Basic auth, session tokens, constant-time compares. |
| Regex | `regex` | Location regex matching, log-format parsing. |
| Serialization | `serde` `serde_json` | JSON structured logs, config dump, metrics exposition (we generate the Prometheus text format ourselves). |
| Syscall bindings | `libc` | epoll/kqueue/accept4/sendfile/setsockopt/mmap/signalfd. |
| CLI parsing | `clap` | Robust `aegis -v/-V/-t/start/stop/reload/restart/status`. |
| Dev/test | `proptest` `criterion` `tempfile` `rcgen` `libloading` | Property tests, benchmarks, temp dirs, test TLS certs, dynamic module loading. |**Own implementations (must not be delegated):** HTTP/1.x parser + response
engine; HTTP/2 framing/streams/flow-control and HPACK (validated against RFC
7541 vectors); HTTP/3 framing + QPACK over quinn streams; FastCGI/SCGI/uWSGI
record codecs; WebSocket frame codec; chunked codec; URL/percent decoding;
RFC date parsing/formatting; CIDR matching; cache keying; token-bucket
limiter; config lexer/parser/validator; logging pipeline; metrics registry.

## Rules

- Every production dependency must have a justification above or in an ADR
  amendment; new dependencies require a decision note.
- Explicitly excluded: `hyper`, `tokio`, `actix`, `axum`, and any existing web
  server/framework. `quinn` is a transport library, not a server: all server
  logic, HTTP layers, and the event loop remain ours.
- Keep the dependency graph small; prefer `no_std`-friendly or dependency-free
  crates where quality is comparable.

## Consequences

- Audit surface is minimal and concentrated in a few well-known crates.
- We remain the authors of every correctness-sensitive protocol path that the
  product is judged on.

## Amendment 2026-08-02 — Phase 1 additions

- `serde` + `serde_json`: JSON structured log records and future config AST
  serialization.
- `time`: ISO-8601 / RFC 3339 timestamps for logs (and later RFC 7231 dates
  for HTTP); avoids hand-rolled civil-time arithmetic.
- `clap` is **deferred** to Phase 17: the Phase 1 CLI (`-v`, `-V`, `-t`) has
  three flags and is hand-rolled and fully tested; `clap` is introduced when
  the daemon lifecycle subcommands (`start`/`stop`/`reload`/...) arrive.

## Amendment 2026-08-02 — Phase 2 addition

- `libc`: direct syscall bindings for the owned networking core
  (`socket`/`bind`/`listen`/`accept`/`setsockopt`/`fcntl`/`getsockname`/
  `getpeername`), as the `net` module is the one place in the crate that
  touches the C socket API. `unsafe_code` is scoped off for `net` only (see
  `net/mod.rs`); every `unsafe` block carries a `// SAFETY:` comment.
