# ADR 0005: Process architecture — master + N worker processes

- Status: Accepted
- Date: 2026-08-02
- Deciders: Project maintainer

## Context

The server must support graceful reload without dropping connections, worker
supervision, and configurable worker counts, on Linux (primary) and macOS
(development).

## Decision

- **Master process**: parses/validates config, binds listeners, forks workers,
  supervises them (restart with backoff on unexpected death), owns signals
  (`TERM`/`QUIT` graceful shutdown, `HUP` reload, `USR1` log rotation, `USR2`
  binary upgrade).
- **Worker processes**: each runs exactly one event loop + executor. Workers
  inherit pre-bound listener sockets from the master and share accept load via
  `SO_REUSEPORT` (Linux and macOS). No accept lock; no intra-worker threading
  for request work.
- **Shared state**: a single master-created mmap region for rate-limit buckets
  and global counters. The disk cache is file-based (safe across processes).
- **Reload**: the new configuration is fully parsed and validated before any
  worker is touched. On success, a new worker generation is spawned; old
  workers stop accepting, drain in-flight requests up to
  `worker_shutdown_timeout`, then exit. Active connections are preserved.
- **Privilege model**: master may start as root to bind low ports, then
  workers drop to an unprivileged uid/gid before accepting.
- **Development mode**: `aegis start -g` runs a single in-process "worker"
  (no fork) for fast iteration and for integration tests.

## Consequences

- Fault isolation: a crashing worker cannot take down other workers or the
  master.
- Scale-out is process-based; the reactor and executor stay single-threaded
  and lock-free per worker, which keeps the hot path simple.
- Coordination (reload, shared limits) must go through the master + mmap;
  the cross-process protocol is small and versioned.
