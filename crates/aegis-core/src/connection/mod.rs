//! The per-connection buffering and backpressure layer.
//!
//! A [`ConnectionManager`] owns the slab of live connections — each with a
//! read buffer ([`crate::buffer::IoBuf`]), a pending write buffer, a current
//! timeout stage, and a backpressure watermark — layered over the reactor.
//! It is transport-agnostic: any connection whose socket is non-blocking can
//! be registered, read, written, and timed out through this one API.
//!
//! # Model
//!
//! One protocol task per connection (spawned on the reactor executor, see
//! Phase 5's HTTP handler) drives a `ConnectionManager` handle captured via
//! [`Clone`]. The task is woken by the reactor on readability, writability,
//! or timer expiry, and on each poll:
//!
//! 1. calls [`ConnectionManager::check_timeout`] and tears down on expiry;
//! 2. drains whatever is readable into the read buffer with
//!    [`ConnectionManager::read`];
//! 3. consumes parsed bytes with [`ConnectionManager::consume`];
//! 4. enqueues output with [`ConnectionManager::write`], which flushes eagerly
//!    and parks above the high-water mark via backpressure;
//! 5. flushes the write buffer and re-registers interest.
//!
//! Readiness registration (`read_ready`/`write_ready`) follows the reactor's
//! level-triggered contract: they register the task's waker and return
//! `Pending`; completion is driven by the event loop.
//!
//! # Backpressure
//!
//! Output is bounded by a high/low water-mark pair (hysteresis). Once queued
//! bytes reach the high-water mark, [`ConnectionManager::write`] reports
//! [`WriteOutcome::Backpressured`] and the producer must pause; once flushing
//! drains the buffer to the low-water mark, the flag clears and writing
//! resumes. Input is bounded by the per-connection read cap, after which
//! [`ReadOutcome::Capacity`] signals the producer (the peer, or upstream) to
//! stop.

use crate::buffer::IoBuf;
use crate::event_loop::{Reactor, Slab};
use crate::net;
use crate::platform::{Interest, Token};
use crate::timers::{TimeoutKind, TimerId};
use std::cell::RefCell;
use std::fmt;
use std::io::{self, Read, Write};
use std::rc::Rc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

/// Per-connection buffer limits and timeout knobs.
#[derive(Debug, Clone, Copy)]
pub struct ConnectionLimits {
    /// Maximum bytes buffered for input per connection.
    pub read_cap: usize,
    /// Queued output at which backpressure engages.
    pub high_water: usize,
    /// Queued output below which backpressure clears.
    pub low_water: usize,
    /// Keep-alive idle timeout between requests.
    pub idle_timeout: Duration,
}

impl Default for ConnectionLimits {
    fn default() -> Self {
        Self {
            read_cap: 16 * 1024,
            high_water: 64 * 1024,
            low_water: 16 * 1024,
            idle_timeout: Duration::from_secs(75),
        }
    }
}

/// Outcome of a non-blocking read into the connection's read buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadOutcome {
    /// `n` bytes were appended to the read buffer.
    Read(usize),
    /// The peer closed; the connection should be torn down.
    Eof,
    /// No data is available right now.
    WouldBlock,
    /// The read buffer is at its cap; the producer must back off.
    Capacity,
}

/// Outcome of [`ConnectionManager::write`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteOutcome {
    /// The entire `data` slice reached the socket.
    Flushed(usize),
    /// Output queued below the high-water mark; `n` bytes pending.
    Buffered(usize),
    /// Queued output above the high-water mark; the producer must pause.
    Backpressured(usize),
}

/// The live state of one registered connection.
struct ConnectionState {
    conn: net::Connection,
    read: IoBuf,
    write: IoBuf,
    stage: TimeoutKind,
    deadline: Instant,
    timer: Option<TimerId>,
    pressured: bool,
    closed: bool,
}

/// Shared manager state.
struct Inner {
    reactor: Reactor,
    connections: RefCell<Slab<ConnectionState>>,
    limits: ConnectionLimits,
}

/// A connection manager handle for one worker.
#[derive(Clone)]
pub struct ConnectionManager {
    inner: Rc<Inner>,
}

impl fmt::Debug for ConnectionManager {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConnectionManager")
            .field("active", &self.active())
            .field("limits", &self.inner.limits)
            .finish()
    }
}

impl ConnectionManager {
    /// A manager with default limits, driving the given reactor.
    pub fn new(reactor: Reactor) -> Self {
        Self::with_limits(reactor, ConnectionLimits::default())
    }

    /// A manager with explicit buffer limits and idle timeout.
    pub fn with_limits(reactor: Reactor, limits: ConnectionLimits) -> Self {
        Self {
            inner: Rc::new(Inner {
                reactor,
                connections: RefCell::new(Slab::new()),
                limits,
            }),
        }
    }

    /// The reactor this manager drives.
    pub fn reactor(&self) -> &Reactor {
        &self.inner.reactor
    }

    /// Number of live connections.
    pub fn active(&self) -> usize {
        self.inner.connections.borrow().len()
    }

    /// Register a connection: force non-blocking mode, register it with the
    /// reactor, and arm the keep-alive idle timer.
    ///
    /// # Panics
    ///
    /// Panics only if a cell is already borrowed, which is a programming error
    /// in the manager itself.
    pub fn register(&self, conn: net::Connection) -> io::Result<Token> {
        conn.set_nonblocking(true)?;
        let token = self
            .inner
            .reactor
            .register_socket(&conn, Interest::READABLE)?;
        let limits = self.inner.limits;
        let deadline = Instant::now() + limits.idle_timeout;
        let timer = self
            .inner
            .reactor
            .schedule_timer(token, deadline, TimeoutKind::Idle);
        let state = ConnectionState {
            conn,
            read: IoBuf::with_capacity(limits.read_cap),
            write: IoBuf::new(),
            stage: TimeoutKind::Idle,
            deadline,
            timer: Some(timer),
            pressured: false,
            closed: false,
        };
        self.inner.connections.borrow_mut().insert(state);
        Ok(token)
    }

    /// Close a connection: cancel its timer, deregister from the reactor, and
    /// drop the state. Safe to call on an already-closed token.
    ///
    /// # Panics
    ///
    /// Panics only if a cell is already borrowed, which is a programming error
    /// in the manager itself.
    pub fn close(&self, token: Token) -> io::Result<()> {
        let mut connections = self.inner.connections.borrow_mut();
        let Some(state) = connections.get_mut(token) else {
            return Ok(());
        };
        if state.closed {
            return Ok(());
        }
        state.closed = true;
        if let Some(id) = state.timer.take() {
            self.inner.reactor.cancel_timer(id);
        }
        drop(connections);
        let result = self.inner.reactor.deregister(token);
        self.inner.connections.borrow_mut().remove(token);
        result
    }

    /// Register `cx`'s waker for readability on `token` (level-triggered; see
    /// the reactor contract).
    pub fn read_ready(&self, token: Token, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.inner.reactor.poll_readable(token, cx)
    }

    /// Register `cx`'s waker for writability on `token` (level-triggered; see
    /// the reactor contract).
    pub fn write_ready(&self, token: Token, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.inner.reactor.poll_writable(token, cx)
    }

    /// Non-blocking read into the connection's read buffer, up to the cap.
    ///
    /// Returns [`ReadOutcome::Capacity`] once the buffer is full so the
    /// producer can back off.
    ///
    /// # Panics
    ///
    /// Panics only if a cell is already borrowed, which is a programming error
    /// in the manager itself.
    pub fn read(&self, token: Token) -> io::Result<ReadOutcome> {
        let mut connections = self.inner.connections.borrow_mut();
        let Some(state) = connections.get_mut(token) else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "connection not registered",
            ));
        };
        if state.closed {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "connection closed",
            ));
        }
        if state.read.len() >= self.inner.limits.read_cap {
            return Ok(ReadOutcome::Capacity);
        }
        let room = self.inner.limits.read_cap - state.read.len();
        let window = state.read.spare_mut(room);
        match state.conn.read(window) {
            Ok(0) => Ok(ReadOutcome::Eof),
            Ok(n) => {
                state.read.advance_written(n);
                Ok(ReadOutcome::Read(n))
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(ReadOutcome::WouldBlock),
            Err(e) => Err(e),
        }
    }

    /// Number of unread bytes currently buffered for `token`.
    pub fn read_len(&self, token: Token) -> Option<usize> {
        self.inner
            .connections
            .borrow()
            .get(token)
            .map(|s| s.read.len())
    }

    /// Zero-copy view of the unread region, scoped to `f` so no borrow leaks.
    pub fn with_read<T>(&self, token: Token, f: impl FnOnce(&[u8]) -> T) -> Option<T> {
        let connections = self.inner.connections.borrow();
        connections.get(token).map(|s| f(s.read.peek()))
    }

    /// Mark `n` unread bytes as consumed by the protocol handler.
    ///
    /// # Panics
    ///
    /// Panics if `n` exceeds the currently buffered bytes.
    pub fn consume(&self, token: Token, n: usize) -> io::Result<()> {
        let mut connections = self.inner.connections.borrow_mut();
        let Some(state) = connections.get_mut(token) else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "connection not registered",
            ));
        };
        state.read.consume(n);
        Ok(())
    }

    /// Enqueue `data` for sending, flushing eagerly.
    ///
    /// Returns [`WriteOutcome::Backpressured`] once queued output crosses the
    /// high-water mark; the producer must pause until the flag clears (see
    /// [`ConnectionManager::is_backpressured`]).
    ///
    /// # Panics
    ///
    /// Panics only if a cell is already borrowed, which is a programming error
    /// in the manager itself.
    pub fn write(&self, token: Token, data: &[u8]) -> io::Result<WriteOutcome> {
        let mut connections = self.inner.connections.borrow_mut();
        let Some(state) = connections.get_mut(token) else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "connection not registered",
            ));
        };
        if state.closed {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "connection closed",
            ));
        }
        if !state.write.is_empty() {
            Self::flush_state(state, self.inner.limits)?;
        }
        if state.write.is_empty() {
            match state.conn.write(data) {
                Ok(n) if n == data.len() => return Ok(WriteOutcome::Flushed(n)),
                Ok(n) => state.write.put(&data[n..]),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => state.write.put(data),
                Err(e) => return Err(e),
            }
        } else {
            state.write.put(data);
        }
        Self::flush_state(state, self.inner.limits)?;
        let queued = state.write.len();
        if queued == 0 {
            return Ok(WriteOutcome::Flushed(data.len()));
        }
        if !state.pressured && queued >= self.inner.limits.high_water {
            state.pressured = true;
        }
        if state.pressured {
            Ok(WriteOutcome::Backpressured(queued))
        } else {
            Ok(WriteOutcome::Buffered(queued))
        }
    }

    /// Push as much queued output as the socket accepts.
    ///
    /// Returns the number of bytes flushed; callers retry when the connection
    /// is reported writable.
    ///
    /// # Panics
    ///
    /// Panics only if a cell is already borrowed, which is a programming error
    /// in the manager itself.
    pub fn flush(&self, token: Token) -> io::Result<usize> {
        let mut connections = self.inner.connections.borrow_mut();
        let Some(state) = connections.get_mut(token) else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "connection not registered",
            ));
        };
        Self::flush_state(state, self.inner.limits)
    }

    /// Bytes currently queued for output, if the token is live.
    pub fn pending(&self, token: Token) -> Option<usize> {
        self.inner
            .connections
            .borrow()
            .get(token)
            .map(|s| s.write.len())
    }

    /// Whether the write buffer is currently above the high-water mark.
    pub fn is_backpressured(&self, token: Token) -> Option<bool> {
        self.inner
            .connections
            .borrow()
            .get(token)
            .map(|s| s.pressured)
    }

    /// Move the connection to a new timeout stage, re-arming its timer.
    ///
    /// # Panics
    ///
    /// Panics only if a cell is already borrowed, which is a programming error
    /// in the manager itself.
    pub fn set_stage(&self, token: Token, kind: TimeoutKind, timeout: Duration) -> io::Result<()> {
        let mut connections = self.inner.connections.borrow_mut();
        let Some(state) = connections.get_mut(token) else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "connection not registered",
            ));
        };
        if state.closed {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "connection closed",
            ));
        }
        if let Some(id) = state.timer.take() {
            self.inner.reactor.cancel_timer(id);
        }
        let deadline = Instant::now() + timeout;
        let timer = self.inner.reactor.schedule_timer(token, deadline, kind);
        state.stage = kind;
        state.deadline = deadline;
        state.timer = Some(timer);
        Ok(())
    }

    /// Whether the current stage deadline has passed by `now`. Returns the
    /// stage to act on, or `None` while the stage is still live.
    pub fn check_timeout(&self, token: Token, now: Instant) -> Option<TimeoutKind> {
        let connections = self.inner.connections.borrow();
        let state = connections.get(token)?;
        if state.closed || now < state.deadline {
            None
        } else {
            Some(state.stage)
        }
    }

    /// One pass of the reactor loop body (see [`Reactor::run_once`]).
    pub fn run_once(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.inner.reactor.run_once(timeout)
    }

    /// Write as much of the queue as the socket accepts, clearing the
    /// backpressure flag once it drains to the low-water mark.
    fn flush_state(state: &mut ConnectionState, limits: ConnectionLimits) -> io::Result<usize> {
        let mut flushed = 0;
        while !state.write.is_empty() {
            match state.conn.write(state.write.peek()) {
                Ok(0) => break,
                Ok(n) => {
                    state.write.consume(n);
                    flushed += n;
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(e),
            }
        }
        if state.pressured && state.write.len() <= limits.low_water {
            state.pressured = false;
        }
        Ok(flushed)
    }
}

#[cfg(test)]
mod tests {
    use super::{ConnectionLimits, ConnectionManager, ReadOutcome, WriteOutcome};
    use crate::event_loop::Reactor;
    use crate::net;
    use crate::platform::Token;
    use crate::timers::TimeoutKind;
    use std::cell::Cell;
    use std::future::Future;
    use std::io::{Read, Write};
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixStream;
    use std::pin::Pin;
    use std::rc::Rc;
    use std::task::{Context, Poll};
    use std::time::{Duration, Instant};

    /// A future that runs the echo protocol for one connection, for tests and
    /// as a template for Phase 5's HTTP handler: read what arrives, echo it
    /// back, re-arm the idle timer, and close cleanly on EOF or timeout.
    struct EchoHandler {
        mgr: ConnectionManager,
        token: Token,
        idle: Duration,
        done: Rc<Cell<bool>>,
    }

    impl Future for EchoHandler {
        type Output = ();

        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
            loop {
                if let Some(_stage) = self.mgr.check_timeout(self.token, Instant::now()) {
                    let _ = self.mgr.close(self.token);
                    self.done.set(true);
                    return Poll::Ready(());
                }
                match self.mgr.read(self.token) {
                    Ok(ReadOutcome::Read(_n)) => {
                        let payload = self.mgr.with_read(self.token, <[u8]>::to_vec);
                        if let Some(bytes) = payload
                            && !bytes.is_empty()
                        {
                            let _ = self.mgr.write(self.token, &bytes);
                            let _ = self.mgr.consume(self.token, bytes.len());
                            let _ = self.mgr.set_stage(self.token, TimeoutKind::Idle, self.idle);
                        }
                        continue;
                    }
                    Ok(ReadOutcome::Eof) => {
                        let _ = self.mgr.close(self.token);
                        self.done.set(true);
                        return Poll::Ready(());
                    }
                    Ok(ReadOutcome::Capacity | ReadOutcome::WouldBlock) => {}
                    Err(_e) => {
                        let _ = self.mgr.close(self.token);
                        self.done.set(true);
                        return Poll::Ready(());
                    }
                }
                let flushed = self.mgr.flush(self.token).unwrap_or(0);
                if flushed > 0 {
                    continue;
                }
                let pending = self.mgr.pending(self.token).unwrap_or(0);
                if pending > 0
                    && let Poll::Ready(Err(_)) = self.mgr.write_ready(self.token, cx)
                {
                    let _ = self.mgr.close(self.token);
                    self.done.set(true);
                    return Poll::Ready(());
                }
                match self.mgr.read_ready(self.token, cx) {
                    Poll::Ready(Err(_)) => {
                        let _ = self.mgr.close(self.token);
                        self.done.set(true);
                        return Poll::Ready(());
                    }
                    Poll::Ready(Ok(())) => {}
                    Poll::Pending => return Poll::Pending,
                }
            }
        }
    }

    fn drive_until<F: Fn() -> bool>(reactor: &Reactor, done: F) -> std::io::Result<()> {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done() && Instant::now() < deadline {
            reactor.run_once(Some(Duration::from_millis(10)))?;
        }
        Ok(())
    }

    fn register_stream(mgr: &ConnectionManager, stream: UnixStream) -> Token {
        mgr.register(net::Connection::from_owned(stream.into()))
            .unwrap()
    }

    #[test]
    fn register_arms_idle_stage() {
        let reactor = Reactor::new().unwrap();
        let mgr = ConnectionManager::new(reactor);
        let (_client, server) = UnixStream::pair().unwrap();
        let token = register_stream(&mgr, server);
        assert_eq!(mgr.active(), 1);
        assert!(
            mgr.check_timeout(token, Instant::now()).is_none(),
            "fresh connection must not be timed out"
        );
        assert!(
            mgr.check_timeout(token, Instant::now() + Duration::from_mins(2))
                .is_some(),
            "idle stage must expire after the idle timeout"
        );
        mgr.close(token).unwrap();
        assert_eq!(mgr.active(), 0);
    }

    #[test]
    fn stage_timers_expire_in_order() {
        let reactor = Reactor::new().unwrap();
        let mgr = ConnectionManager::new(reactor);
        let (_client, server) = UnixStream::pair().unwrap();
        let token = register_stream(&mgr, server);

        mgr.set_stage(token, TimeoutKind::HeadRead, Duration::from_millis(5))
            .unwrap();
        let t0 = Instant::now();
        assert_eq!(
            mgr.check_timeout(token, t0 + Duration::from_millis(2)),
            None
        );
        assert_eq!(
            mgr.check_timeout(token, t0 + Duration::from_millis(20)),
            Some(TimeoutKind::HeadRead)
        );
    }

    #[test]
    fn read_buffers_and_consumes() {
        let reactor = Reactor::new().unwrap();
        let mgr = ConnectionManager::new(reactor);
        let (mut client, server) = UnixStream::pair().unwrap();
        let token = register_stream(&mgr, server);
        client.set_nonblocking(true).unwrap();
        client.write_all(b"hello").unwrap();

        let mut n = 0;
        for _ in 0..10 {
            if let ReadOutcome::Read(got) = mgr.read(token).unwrap() {
                n = got;
                break;
            }
        }
        assert_eq!(n, 5);
        assert_eq!(mgr.read_len(token), Some(5));
        assert_eq!(
            mgr.with_read(token, <[u8]>::to_vec),
            Some(b"hello".to_vec())
        );
        mgr.consume(token, 2).unwrap();
        assert_eq!(mgr.read_len(token), Some(3));
        mgr.close(token).unwrap();
    }

    #[test]
    fn write_flushes_and_reports_outcomes() {
        let reactor = Reactor::new().unwrap();
        let mgr = ConnectionManager::new(reactor);
        let (mut client, server) = UnixStream::pair().unwrap();
        let token = register_stream(&mgr, server);

        match mgr.write(token, b"small").unwrap() {
            WriteOutcome::Flushed(n) => assert_eq!(n, 5),
            other => panic!("small write must flush directly, got {other:?}"),
        }
        assert_eq!(mgr.pending(token), Some(0));

        let mut buf = [0u8; 16];
        assert_eq!(client.read(&mut buf).unwrap(), 5);
        assert_eq!(&buf[..5], b"small");
        mgr.close(token).unwrap();
    }

    #[test]
    fn backpressure_blocks_producer_until_drained() {
        let reactor = Reactor::new().unwrap();
        let limits = ConnectionLimits {
            read_cap: 16 * 1024,
            high_water: 8 * 1024,
            low_water: 2 * 1024,
            idle_timeout: Duration::from_mins(1),
        };
        let mgr = ConnectionManager::with_limits(reactor, limits);
        let (client, server) = UnixStream::pair().unwrap();
        // Shrink the peer's receive window so the socket fills quickly.
        net::set_int_option(client.as_raw_fd(), libc::SOL_SOCKET, libc::SO_RCVBUF, 4096).unwrap();
        let token = register_stream(&mgr, server);

        let chunk = vec![b'A'; 16 * 1024];
        let mut backed_off = false;
        for _ in 0..1024 {
            if let WriteOutcome::Backpressured(_) = mgr.write(token, &chunk).unwrap() {
                backed_off = true;
                break;
            }
        }
        assert!(backed_off, "filling the socket must engage backpressure");
        assert_eq!(mgr.is_backpressured(token), Some(true));

        // Drain the peer side until the socket empties, then flush.
        let mut client = client;
        client.set_nonblocking(true).unwrap();
        let mut sink = [0u8; 8192];
        let mut drained = 0;
        for _ in 0..10_000 {
            match client.read(&mut sink) {
                Ok(0) | Err(_) => break,
                Ok(n) => drained += n,
            }
        }
        assert!(drained > 0, "client must have received buffered output");

        let flushed = mgr.flush(token).unwrap();
        assert!(flushed > 0 || mgr.pending(token).unwrap() == 0);
        assert_eq!(mgr.is_backpressured(token), Some(false));
        assert!(
            mgr.pending(token).unwrap_or(0) <= limits.low_water,
            "flush must drain below the low-water mark"
        );
        mgr.close(token).unwrap();
    }

    #[test]
    fn echo_keepalive_then_idle_timeout() {
        let reactor = Reactor::new().unwrap();
        let limits = ConnectionLimits {
            read_cap: 16 * 1024,
            high_water: 64 * 1024,
            low_water: 16 * 1024,
            idle_timeout: Duration::from_millis(50),
        };
        let mgr = ConnectionManager::with_limits(reactor.clone(), limits);
        let (mut client, server) = UnixStream::pair().unwrap();
        client.set_nonblocking(true).unwrap();
        let token = register_stream(&mgr, server);

        let done = Rc::new(Cell::new(false));
        let handler = EchoHandler {
            mgr: mgr.clone(),
            token,
            idle: Duration::from_millis(50),
            done: Rc::clone(&done),
        };
        reactor.spawn(handler);

        client.write_all(b"ping\n").unwrap();
        drive_until(&reactor, || done.get()).unwrap();

        let mut buf = [0u8; 16];
        let mut echoed = Vec::new();
        loop {
            match client.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    echoed.extend_from_slice(&buf[..n]);
                    if echoed.len() >= 5 {
                        break;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(_) => break,
            }
        }
        assert_eq!(echoed, b"ping\n", "handler must echo the request bytes");
        assert!(done.get(), "handler must close after the idle timeout");
        assert_eq!(mgr.active(), 0);

        match client.read(&mut buf) {
            Ok(0) => {}
            Ok(n) => panic!("expected EOF after close, read {n} more bytes"),
            Err(e) => panic!("expected EOF after close, got {e}"),
        }
    }
}
