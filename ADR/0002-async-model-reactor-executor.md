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

## Implementation notes (Phase 3)

- The executor and reactor are `Rc`/`RefCell`-based and futures need not be
  `Send`: one worker owns one reactor, and scale-out is by processes (per
  ADR 0005), so `Send` buys nothing while costing `Arc` overhead and bounds
  on every future. Wakers are built from a raw `RawWakerVTable` rather than
  the `Wake` trait, which would require `Send + Sync`.
- Task scheduling state lives in a `Cell` shared separately from the task, so
  a task can wake itself from inside its own `poll` (including from reactor
  readiness dispatch) without a double borrow.
- Readiness is level-triggered: `poll_readable`/`poll_writable` register the
  task's waker and return `Pending`; completion is driven by the loop, and the
  task performs non-blocking I/O, looping back on `WouldBlock`. The
  `AsyncRead`/`AsyncWrite` poll traits from the design were superseded in
  Phase 4 by an imperative connection manager (see below).

## Implementation notes (Phase 4)

- Per-connection buffering and flow control live in an imperative
  `ConnectionManager` over a generation-counted slab, not `AsyncRead`/
  `AsyncWrite` poll traits. Futures (the Phase 5 HTTP handler) call
  `read`/`write`/`flush` and park on the reactor's readiness wakers directly;
  this keeps the protocol codec simple while the manager owns backpressure.
- `buffer::IoBuf` is a cursor-based (start/end) buffer: consumed bytes are
  reclaimed by memmove without shrinking the allocation, and a zero-initialized
  spare window (`spare_mut`/`advance_written`) lets the manager read straight
  off the socket into unread space.
- Output is bounded by a high/low water-mark pair (hysteresis): the producer
  gets `WriteOutcome::Backpressured` above the high-water mark and only
  resumes after flushing drains below the low-water mark; input is bounded by
  the per-connection read cap (`ReadOutcome::Capacity`).
- Every connection carries a deadline timer whose `TimeoutKind` the HTTP layer
  re-arms per stage (Idle, HeadRead, BodyRead, …); `set_stage` swaps the timer
  under the same slot, so only one timer per connection is ever live.

## Consequences

- We own the performance-critical dispatch path and can add io_uring without
  an architecture change.
- Waker/reactor correctness is subtle and must be covered by dedicated tests
  (wake-before-poll, spurious wakeups, token reuse after fd close).
- Blocking work (e.g. `stat` storms on slow disks) is explicitly out-of-band;
  a small blocking thread-pool may be added per worker behind a documented
  boundary if profiling demands it.
