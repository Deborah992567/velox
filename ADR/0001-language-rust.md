# ADR 0001: Implementation language — Rust

- Status: Accepted
- Date: 2026-08-02
- Deciders: Project maintainer (on behalf of the Aegis/Velox project)

## Context

Aegis is a from-scratch, production-grade web server / reverse proxy with the
attack surface of Nginx: untrusted client input, hand-written parsers
(HTTP/1.x, HTTP/2 HPACK, HTTP/3 QPACK, FastCGI, WebSockets), complex protocol
state machines, multi-process architecture, and TLS termination.

We evaluated Rust and C++ against the criteria below.

## Decision

Use **Rust** (stable channel, edition 2024) for the entire implementation.

## Rationale

| Criterion | Rust | C++ |
|---|---|---|
| Memory safety without GC | Enforced by the borrow checker. Eliminates the dominant CVE class in this domain (buffer overruns, use-after-free in parsers). | Manual. The risk is concentrated exactly where this project is hardest (hand-written parsers, stream FSMs). |
| Untrusted input handling | Safety is enforced structurally; malformed input cannot corrupt memory. | Every parse path is a potential memory-safety bug. |
| Concurrency correctness | `Send`/`Sync` are compile-time enforced across the worker/shared-state boundaries. | Data races are silent UB. |
| Performance | Zero-cost abstractions, no GC/runtime; within noise of C++ for this workload. | Slightly higher ceiling with heroic effort. |
| Protocol state machines | Type-safe FSM modeling (enums + total transition functions returning `Result`). | Manually managed, error-prone. |
| Ecosystem (crypto/protocols) | Audited pure-Rust: `rustls`, `quinn`, `flate2`/`brotli`/`zstd`, RustCrypto. | Mostly OpenSSL-driven; more risk owned. |
| Tooling | `cargo` unifies build, test, fuzz (`cargo-fuzz`), property tests (`proptest`), and benchmarks (`criterion`). | Fragmented build/test/fuzz toolchains. |

C++'s advantages (maximal memory-layout control, 30-year ecosystem) do not
outweigh the memory-safety argument for a network daemon. Prior art agrees:
Cloudflare Pingora, Quiche, and AWS s2n-quic chose Rust for this exact profile.

## Consequences

- Memory safety is guaranteed at the language level; we can spend review effort
  on protocol correctness instead of memory management.
- We implement the core networking stack (event loop, reactor, parser,
  proxy pipeline) ourselves in safe Rust; `unsafe` is reserved for OS-syscall
  interop (epoll/kqueue/io_uring wrappers, `sendfile`, mmap) and must be
  contained in `platform/` with safety contracts and tests.
- Release builds use `panic = "abort"`; worker processes are supervised by the
  master, so a fatal error is contained and the worker is restarted.
