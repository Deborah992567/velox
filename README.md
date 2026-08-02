# Velox — a production-grade Nginx-class web server written from scratch in Rust

**Velox** (binary `aegis`) is a high-performance, event-driven, asynchronous,
multi-worker web server, reverse proxy, load balancer, TLS terminator, and
application gateway. The networking core, event loop, HTTP processing
pipeline, configuration system, connection management, proxying architecture,
and worker architecture are implemented from scratch — no existing web server
framework is used as the core.

This is a serious systems project, built phase by phase with a test suite and
benchmark evidence at every milestone. See
[`docs/architecture.md`](docs/architecture.md) for the full design.

## Status

**Phase 0 — architecture and repository skeleton (current).**

The roadmap is defined in [`TODO.md`](TODO.md). Progress is tracked in
[`CHANGELOG.md`](CHANGELOG.md); architectural decisions in
[`ADR/`](ADR/).

## Target capabilities (final form)

- HTTP/1.0, HTTP/1.1, HTTP/2, HTTP/3 (over QUIC)
- HTTPS/TLS 1.2 + 1.3 with SNI, session resumption, OCSP stapling
- Static file serving (sendfile/zero-copy, Range, ETag, conditional requests)
- Reverse proxying with upstream pools, keepalive, retries, streaming
- Load balancing: round-robin, weighted RR, least-connections, health checks
- WebSocket proxying, FastCGI/SCGI/uWSGI gateways
- Compression (gzip, Brotli, zstd), caching (memory + disk), rate limiting,
  access control (CIDR, basic auth)
- Master/worker processes, graceful reload and shutdown, dynamic modules
- Structured logging, Prometheus-compatible metrics, extensive tests + fuzzing

## Development

Requirements: Rust stable (see `rust-toolchain.toml`).

```sh
cargo build --workspace        # build all crates
cargo test  --workspace        # run all tests
cargo clippy --workspace --all-targets --all-features   # lint
cargo fmt --all                # format
cargo run -p aegis-cli -- -v   # run the CLI
```

`AGENTS.md` defines `b`/`t`/`l`/`f`/`check` shorthands used during development.

## Platform support

- **Linux** — primary production target: epoll event driver (io_uring planned
  behind the same trait), `sendfile`, `SO_REUSEPORT`, `TCP_DEFER_ACCEPT`.
- **macOS / BSD** — development target: kqueue event driver, `sendfile`.

## Layout

```
crates/aegis-core   the server library (all src/ modules)
crates/aegis-cli    the `aegis` command-line binary
docs/               architecture + subsystem docs
ADR/                architectural decision records
tests/ benches/ fuzz/ examples/ configs/   (populated as phases land)
```

## License

MIT — see `LICENSE`.
