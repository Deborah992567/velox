# ADR 0002: Async model — own reactor + minimal single-threaded executor

- Status: Accepted
- Date: 2026-08-02
- Deciders: Project maintainer

## Context

The server core — event loop, reactor, connection handling, proxy pipeline —
must be implemented by us (project requirement). We considered:

1. **Tokio / async-std**: mature, but the event loop, driver, and executor
   would be a third-party runtime's, not ours. Violates the core requirement.
2. **Callback / explicit state machines only** (Nginx style): maximum control,
   but HTTP/2 stream FSMs and HTTP/3/QUIC are notoriously painful to write by
   hand; productivity and correctness suffer.
3. **Own reactor + minimal single-threaded-per-worker futures executor**
   (chosen): we own the epoll/kqueue/io_uring driver, the timer wheel, the
   connection slab, and a small waker-based executor. `async`/`await` is used
   internally for protocol state machines.

## Decision

Build our own reactor on top of a platform `EventDriver` trait, plus a small
executor that runs one task queue per worker process.

## Key design points

- `EventDriver` (epoll on Linux, kqueue on macOS/BSD, io_uring behind the same
  trait later) returns readiness events; the reactor maps tokens to tasks via a
  slab with generation counters.
- A hierarchical timer wheel provides O(1) add/cancel for the hundreds of
  per-connection timeouts.
- The executor is a FIFO of ready tasks; Wakers enqueue a task for polling,
  driven by I/O readiness and timers. No preemption, no work stealing across
  processes; scale-out is via processes (master/worker), not threads.
- Connections, upstream sockets, and protocol codecs all implement our own
  `AsyncRead`/`AsyncWrite` poll traits backed by the reactor.

## Alternatives rejected

- **Tokio**: its reactor/executor would become the core we are required to
  write ourselves. Would also impose its timer wheel, slab, and scheduling.
- **Pure callbacks**: ruled out for HTTP/2/3 complexity; async/await keeps
  those FSMs readable and auditable.

## Consequences

- We own the performance-critical dispatch path and can add io_uring without
  an architecture change.
- Waker/reactor correctness is subtle and must be covered by dedicated tests
  (wake-before-poll, spurious wakeups, token reuse after fd close).
- Blocking work (e.g. `stat` storms on slow disks) is explicitly out-of-band;
  a small blocking thread-pool may be added per worker behind a documented
  boundary if profiling demands it.
