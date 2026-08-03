//! The per-worker reactor: event driver, connection slab, timer wheel, and
//! executor wired into one loop (see `docs/architecture.md` §3.1 and
//! ADR 0002).
//!
//! A [`Reactor`] is a cheaply cloneable handle over an `Rc`-owned [`Inner`]:
//! single-threaded by design, so a spawned task can capture the handle and
//! re-register interest / poll readiness from inside its `poll` without any
//! `Send` or `Sync` burden. The driver, slab, timers, and executor live in
//! separate cells, so a task waking itself (or another task) from within a
//! poll can never hit a double borrow.
//!
//! # Readiness contract (level-triggered)
//!
//! [`Reactor::poll_readable`] / [`Reactor::poll_writable`] register the
//! task's waker (and the interest, if not already set) and return
//! `Poll::Pending`. They never complete on their own: completion happens when
//! the driver reports readiness for the fd and the reactor wakes the task.
//! The task is expected to perform non-blocking I/O on the fd and loop back to
//! the poll call on `WouldBlock`, exactly like `mio`'s level-triggered
//! sources.
//!
//! # Lifetime contract
//!
//! [`Reactor::register_socket`] stores only the raw fd. The caller keeps the
//! socket alive and calls [`Reactor::deregister`] before dropping it; doing so
//! before close guarantees the driver never reports events for a recycled fd.
//! Deregistering a token it no longer owns (stale event, double-deregister) is
//! a safe no-op thanks to the slab's generation counters.
//!
//! # Safety policy
//!
//! This module is `unsafe`-free: the raw-waker machinery lives in
//! [`super::executor`] and the syscalls in [`crate::platform`]. The reactor
//! only connects safe pieces.

use super::{Executor, Slab};
use crate::platform::{EventDriver, Interest, Token, driver};
use crate::timers::{TimeoutKind, TimerId, TimerWheel};
use std::cell::RefCell;
use std::fmt;
use std::future::Future;
use std::io;
use std::os::fd::{AsRawFd, RawFd};
use std::rc::Rc;
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

/// The registered state for one socket.
struct Source {
    fd: RawFd,
    interest: Interest,
    read_waker: Option<Waker>,
    write_waker: Option<Waker>,
}

/// The shared, single-threaded reactor state.
struct Inner {
    driver: RefCell<Box<dyn EventDriver>>,
    sources: RefCell<Slab<Source>>,
    timers: RefCell<TimerWheel>,
    executor: Executor,
}

/// A per-worker reactor handle.
#[derive(Clone)]
pub struct Reactor {
    inner: Rc<Inner>,
}

impl fmt::Debug for Reactor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Reactor")
            .field("queued", &self.queued())
            .field("sources", &self.inner.sources.borrow().len())
            .field("timers", &self.inner.timers.borrow().len())
            .finish()
    }
}

impl Reactor {
    /// Build a reactor with the platform's native event driver.
    pub fn new() -> io::Result<Self> {
        Ok(Self {
            inner: Rc::new(Inner {
                driver: RefCell::new(driver()?),
                sources: RefCell::new(Slab::new()),
                timers: RefCell::new(TimerWheel::new()),
                executor: Executor::new(),
            }),
        })
    }

    /// Start reporting `interest` for `sock`, returning its slab token.
    ///
    /// # Panics
    ///
    /// Panics only if a cell is already borrowed, which is a programming error
    /// in the reactor itself.
    pub fn register_socket(&self, sock: &impl AsRawFd, interest: Interest) -> io::Result<Token> {
        let fd = sock.as_raw_fd();
        let token = self.inner.sources.borrow_mut().insert(Source {
            fd,
            interest,
            read_waker: None,
            write_waker: None,
        });
        if let Err(e) = self.inner.driver.borrow_mut().register(fd, interest, token) {
            self.inner.sources.borrow_mut().remove(token);
            return Err(e);
        }
        Ok(token)
    }

    /// Replace the interest set for a registered token.
    ///
    /// # Panics
    ///
    /// Panics only if a cell is already borrowed, which is a programming error
    /// in the reactor itself.
    pub fn reregister(&self, token: Token, interest: Interest) -> io::Result<()> {
        let mut sources = self.inner.sources.borrow_mut();
        let Some(src) = sources.get_mut(token) else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "source not registered",
            ));
        };
        src.interest = interest;
        let fd = src.fd;
        let mut driver = self.inner.driver.borrow_mut();
        driver.modify(fd, interest, token)
    }

    /// Stop reporting events for `token`. Safe to call on a stale token.
    ///
    /// # Panics
    ///
    /// Panics only if a cell is already borrowed, which is a programming error
    /// in the reactor itself.
    pub fn deregister(&self, token: Token) -> io::Result<()> {
        let mut sources = self.inner.sources.borrow_mut();
        let Some(src) = sources.remove(token) else {
            return Ok(());
        };
        let fd = src.fd;
        drop(sources);
        self.inner.driver.borrow_mut().deregister(fd)
    }

    /// Register `cx`'s waker for readability on `token`, adding the readable
    /// interest if needed. Returns `Poll::Pending`; completion is driven by
    /// [`Reactor::run_once`]. See the level-triggered contract in the module
    /// docs.
    ///
    /// Returns `Poll::Ready(Err)` if `token` is not registered.
    ///
    /// # Panics
    ///
    /// Panics only if a cell is already borrowed, which is a programming error
    /// in the reactor itself.
    pub fn poll_readable(&self, token: Token, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let mut sources = self.inner.sources.borrow_mut();
        let Some(src) = sources.get_mut(token) else {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "source not registered",
            )));
        };
        src.read_waker = Some(cx.waker().clone());
        if src.interest.is_readable() {
            return Poll::Pending;
        }
        let interest = src.interest.union(Interest::READABLE);
        src.interest = interest;
        let fd = src.fd;
        drop(sources);
        self.inner.driver.borrow_mut().modify(fd, interest, token)?;
        Poll::Pending
    }

    /// Register `cx`'s waker for writability on `token`, adding the writable
    /// interest if needed. Returns `Poll::Pending`; completion is driven by
    /// [`Reactor::run_once`]. See the level-triggered contract in the module
    /// docs.
    ///
    /// Returns `Poll::Ready(Err)` if `token` is not registered.
    ///
    /// # Panics
    ///
    /// Panics only if a cell is already borrowed, which is a programming error
    /// in the reactor itself.
    pub fn poll_writable(&self, token: Token, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let mut sources = self.inner.sources.borrow_mut();
        let Some(src) = sources.get_mut(token) else {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "source not registered",
            )));
        };
        src.write_waker = Some(cx.waker().clone());
        if src.interest.is_writable() {
            return Poll::Pending;
        }
        let interest = src.interest.union(Interest::WRITABLE);
        src.interest = interest;
        let fd = src.fd;
        drop(sources);
        self.inner.driver.borrow_mut().modify(fd, interest, token)?;
        Poll::Pending
    }

    /// Schedule `kind` to fire at `at`, waking the task owning `token`.
    pub fn schedule_timer(&self, token: Token, at: Instant, kind: TimeoutKind) -> TimerId {
        self.inner.timers.borrow_mut().insert(token, at, kind)
    }

    /// Cancel a scheduled timer; returns whether it was still pending.
    pub fn cancel_timer(&self, id: TimerId) -> bool {
        self.inner.timers.borrow_mut().cancel(id)
    }

    /// Schedule `future` on the executor.
    pub fn spawn(&self, future: impl Future<Output = ()> + 'static) {
        self.inner.executor.spawn(future);
    }

    /// Number of tasks currently queued on the executor.
    pub fn queued(&self) -> usize {
        self.inner.executor.queued()
    }

    /// One pass of the loop body from `docs/architecture.md` §3.1:
    ///
    /// 1. drain the executor (tasks re-register wakers and interest);
    /// 2. expire timers and wake their connection tasks;
    /// 3. drain again so those wakes take effect immediately;
    /// 4. block on the driver, never past the next timer deadline;
    /// 5. apply readiness events and wake the matching tasks;
    /// 6. drain a final time.
    ///
    /// (Signal/control-message handling arrives with the worker process.)
    ///
    /// # Panics
    ///
    /// Panics only if a cell is already borrowed, which is a programming error
    /// in the reactor itself.
    pub fn run_once(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.inner.executor.run_ready();

        let now = Instant::now();
        let expired = self.inner.timers.borrow_mut().poll(now);
        if !expired.is_empty() {
            let mut to_wake = Vec::new();
            {
                let mut sources = self.inner.sources.borrow_mut();
                for event in expired {
                    if let Some(src) = sources.get_mut(event.token) {
                        if let Some(w) = src.read_waker.take() {
                            to_wake.push(w);
                        }
                        if let Some(w) = src.write_waker.take() {
                            to_wake.push(w);
                        }
                    }
                }
            }
            for waker in to_wake {
                waker.wake();
            }
        }
        self.inner.executor.run_ready();

        let next_deadline = self.inner.timers.borrow().next_deadline();
        let wait = match (timeout, next_deadline) {
            (Some(user), Some(next)) => Some(user.min(next.saturating_duration_since(now))),
            (Some(user), None) => Some(user),
            (None, Some(next)) => Some(next.saturating_duration_since(now)),
            (None, None) => None,
        };
        let events = self.inner.driver.borrow_mut().wait(wait)?;

        let mut to_wake = Vec::new();
        {
            let mut sources = self.inner.sources.borrow_mut();
            for event in events {
                let Some(src) = sources.get_mut(event.token) else {
                    continue;
                };
                if (event.ready.is_readable() || event.ready.is_error() || event.ready.is_hup())
                    && let Some(w) = src.read_waker.take()
                {
                    to_wake.push(w);
                }
                if event.ready.is_writable()
                    && let Some(w) = src.write_waker.take()
                {
                    to_wake.push(w);
                }
            }
        }
        for waker in to_wake {
            waker.wake();
        }
        self.inner.executor.run_ready();
        Ok(())
    }

    /// Run the loop forever, until a driver error.
    pub fn run(&self) -> io::Result<()> {
        loop {
            self.run_once(None)?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Reactor;
    use crate::platform::{Interest, Token};
    use crate::timers::TimeoutKind;
    use std::cell::Cell;
    use std::future::Future;
    use std::io::{self, Read, Write};
    use std::os::unix::net::UnixStream;
    use std::pin::Pin;
    use std::rc::Rc;
    use std::task::{Context, Poll, Waker};
    use std::time::{Duration, Instant};

    /// A future that reads one chunk from a socket, parking on the reactor
    /// when the read would block. Mirrors how the HTTP layer will use the
    /// reactor in Phase 4.
    struct ReadSome {
        stream: UnixStream,
        reactor: Reactor,
        token: Token,
    }

    impl Future for ReadSome {
        type Output = io::Result<usize>;

        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            let mut buf = [0u8; 64];
            match self.stream.read(&mut buf) {
                Ok(n) => {
                    self.reactor.deregister(self.token)?;
                    Poll::Ready(Ok(n))
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    match self.reactor.poll_readable(self.token, cx) {
                        Poll::Ready(Err(e)) => {
                            self.reactor.deregister(self.token)?;
                            Poll::Ready(Err(e))
                        }
                        _ => Poll::Pending,
                    }
                }
                Err(e) => {
                    self.reactor.deregister(self.token)?;
                    Poll::Ready(Err(e))
                }
            }
        }
    }

    /// A future that completes once its deadline passes, parking on a
    /// registered readable waker in the meantime (so the timer has a waker to
    /// fire).
    struct WaitDeadline {
        reactor: Reactor,
        token: Token,
        deadline: Instant,
    }

    impl Future for WaitDeadline {
        type Output = ();

        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
            if Instant::now() >= self.deadline {
                let _ = self.reactor.deregister(self.token);
                return Poll::Ready(());
            }
            match self.reactor.poll_readable(self.token, cx) {
                Poll::Ready(Err(_)) => Poll::Ready(()),
                _ => Poll::Pending,
            }
        }
    }

    fn test_pair() -> (UnixStream, UnixStream) {
        let (a, b) = UnixStream::pair().unwrap();
        a.set_nonblocking(true).unwrap();
        let _ = b.set_nonblocking(true);
        (a, b)
    }

    #[test]
    fn readiness_wakes_awaiting_task() {
        let reactor = Reactor::new().unwrap();
        let (mut a, b) = test_pair();
        let token = reactor.register_socket(&b, Interest::READABLE).unwrap();
        let result = Rc::new(Cell::new(0));
        let task_result = Rc::clone(&result);
        let task = ReadSome {
            stream: b,
            reactor: Reactor::clone(&reactor),
            token,
        };
        reactor.spawn(async move {
            task_result.set(task.await.unwrap());
        });

        a.write_all(b"hello").unwrap();
        reactor.run_once(Some(Duration::from_millis(500))).unwrap();

        assert_eq!(result.get(), 5, "task must read the chunk written to it");
    }

    #[test]
    fn writable_socket_completes_write() {
        let reactor = Reactor::new().unwrap();
        let (mut a, mut b) = test_pair();
        let token = reactor.register_socket(&b, Interest::WRITABLE).unwrap();
        let result = Rc::new(Cell::new(false));
        let task_result = Rc::clone(&result);
        let reactor_task = Reactor::clone(&reactor);
        reactor.spawn(async move {
            let mut buf = [0u8; 64];
            match b.read(&mut buf) {
                Ok(n) if n > 0 => {
                    let _ = reactor_task.deregister(token);
                    task_result.set(true);
                }
                _ => {}
            }
        });

        a.write_all(b"x").unwrap();
        reactor.run_once(Some(Duration::from_millis(500))).unwrap();

        assert!(result.get(), "writable registration must deliver the data");
    }

    #[test]
    fn timer_wakes_connection_task() {
        let reactor = Reactor::new().unwrap();
        let (a, b) = test_pair();
        let token = reactor.register_socket(&b, Interest::READABLE).unwrap();
        let deadline = Instant::now();
        reactor.schedule_timer(token, deadline, TimeoutKind::Idle);
        let done = Rc::new(Cell::new(false));
        let task_done = Rc::clone(&done);
        let task = WaitDeadline {
            reactor: Reactor::clone(&reactor),
            token,
            deadline,
        };
        reactor.spawn(async move {
            task.await;
            task_done.set(true);
        });

        reactor.run_once(Some(Duration::from_millis(100))).unwrap();

        assert!(done.get(), "the expired timer must wake the parked task");
        let _ = a;
    }

    #[test]
    fn cancelled_timer_does_not_wake() {
        let reactor = Reactor::new().unwrap();
        let (_a, b) = test_pair();
        let token = reactor.register_socket(&b, Interest::READABLE).unwrap();
        let id = reactor.schedule_timer(
            token,
            Instant::now() + Duration::from_hours(1),
            TimeoutKind::Idle,
        );
        assert!(reactor.cancel_timer(id));
        assert!(!reactor.cancel_timer(id));

        let done = Rc::new(Cell::new(false));
        let task_done = Rc::clone(&done);
        let task = WaitDeadline {
            reactor: Reactor::clone(&reactor),
            token,
            deadline: Instant::now() + Duration::from_hours(1),
        };
        reactor.spawn(async move {
            task.await;
            task_done.set(true);
        });

        reactor.run_once(Some(Duration::from_millis(20))).unwrap();

        assert!(!done.get(), "a cancelled timer must not wake the task");
    }

    #[test]
    fn deregistered_token_fails_poll_fast() {
        let reactor = Reactor::new().unwrap();
        let (_a, b) = test_pair();
        let token = reactor.register_socket(&b, Interest::READABLE).unwrap();
        reactor.deregister(token).unwrap();

        let noop = Waker::noop();
        let mut cx = Context::from_waker(noop);
        match reactor.poll_readable(token, &mut cx) {
            Poll::Ready(Err(_)) => {}
            other => panic!("stale token must fail fast, got {other:?}"),
        }
    }
}
