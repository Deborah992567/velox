//! Linux epoll driver.
//!
//! `epoll(7)` tracks a set of descriptors and their interest mask. Registering
//! maps an `Interest` to `EPOLLIN`/`EPOLLOUT`; modifying updates the mask via
//! `EPOLL_CTL_MOD`. `EPOLLERR`/`EPOLLHUP` are always reported by epoll and
//! imply readability, matching the semantics the reactor expects.
#![allow(unsafe_code)]
// Syscall glue: casts at the epoll boundary are width conversions only.
#![allow(
    clippy::cast_lossless,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::time::Duration;

use super::{Event, EventDriver, Interest, Ready, Token};

/// The maximum number of events retrieved by a single `epoll_wait(2)` call.
const BATCH: usize = 1024;

/// The kernel event poller for one worker's event loop.
#[derive(Debug)]
pub struct Epoll {
    epfd: OwnedFd,
}

impl Epoll {
    /// Create a new epoll instance.
    pub fn new() -> io::Result<Self> {
        // SAFETY: epoll_create1(2) allocates a fresh poller and returns a
        // descriptor that this caller now owns.
        let fd = unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `fd` is a newly-created, owned descriptor.
        Ok(Self {
            epfd: unsafe { OwnedFd::from_raw_fd(fd) },
        })
    }

    fn interest_flags(interest: Interest) -> u32 {
        let mut flags = 0;
        if interest.is_readable() {
            flags |= libc::EPOLLIN as u32;
        }
        if interest.is_writable() {
            flags |= libc::EPOLLOUT as u32;
        }
        flags
    }

    fn translate(event: libc::epoll_event) -> Event {
        let mut bits = 0;
        if event.events & libc::EPOLLIN as u32 != 0 {
            bits |= super::READY_READABLE;
        }
        if event.events & libc::EPOLLOUT as u32 != 0 {
            bits |= super::READY_WRITABLE;
        }
        if event.events & libc::EPOLLERR as u32 != 0 {
            bits |= super::READY_ERROR | super::READY_READABLE;
        }
        if event.events & libc::EPOLLHUP as u32 != 0 {
            bits |= super::READY_HUP | super::READY_READABLE;
        }
        Event {
            token: Token::from_raw(event.u64),
            ready: Ready::from_bits(bits),
        }
    }
}

impl EventDriver for Epoll {
    fn register(&mut self, fd: RawFd, interest: Interest, token: Token) -> io::Result<()> {
        let mut event = libc::epoll_event {
            events: Self::interest_flags(interest),
            u64: token.raw(),
        };
        // SAFETY: `event` is a fully-initialized epoll_event; `fd` is a valid
        // descriptor being added to this epoll instance.
        let rc = unsafe {
            libc::epoll_ctl(
                self.epfd.as_raw_fd(),
                libc::EPOLL_CTL_ADD,
                fd,
                std::ptr::from_mut(&mut event),
            )
        };
        if rc != 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn modify(&mut self, fd: RawFd, interest: Interest, token: Token) -> io::Result<()> {
        let mut event = libc::epoll_event {
            events: Self::interest_flags(interest),
            u64: token.raw(),
        };
        // SAFETY: `event` is a fully-initialized epoll_event; `fd` was added
        // to this epoll instance by `register`.
        let rc = unsafe {
            libc::epoll_ctl(
                self.epfd.as_raw_fd(),
                libc::EPOLL_CTL_MOD,
                fd,
                std::ptr::from_mut(&mut event),
            )
        };
        if rc != 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn deregister(&mut self, fd: RawFd) -> io::Result<()> {
        // SAFETY: EPOLL_CTL_DEL ignores the event pointer entirely; a null
        // pointer is accepted.
        let rc = unsafe {
            libc::epoll_ctl(
                self.epfd.as_raw_fd(),
                libc::EPOLL_CTL_DEL,
                fd,
                std::ptr::null_mut(),
            )
        };
        if rc != 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn wait(&mut self, timeout: Option<Duration>) -> io::Result<Vec<Event>> {
        let mut raw = vec![libc::epoll_event { events: 0, u64: 0 }; BATCH];
        let timeout_ms = match timeout {
            None => -1,
            Some(d) => {
                let capped = d.as_millis().min(u128::from(i32::MAX));
                i32::try_from(capped).expect("clamped to i32::MAX")
            }
        };
        // SAFETY: `raw` is a valid buffer of `BATCH` epoll_event structs the
        // kernel writes into.
        let n = unsafe {
            libc::epoll_wait(
                self.epfd.as_raw_fd(),
                raw.as_mut_ptr(),
                i32::try_from(BATCH).expect("batch size fits c_int"),
                timeout_ms,
            )
        };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        raw.truncate(usize::try_from(n).expect("event count is non-negative"));
        Ok(raw.into_iter().map(Self::translate).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::Epoll;
    use crate::platform::{EventDriver, Interest, Token};
    use std::io::Write;
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    fn pair() -> (UnixStream, UnixStream) {
        UnixStream::pair().unwrap()
    }

    fn token(n: u32) -> Token {
        Token::from_parts(n, 3)
    }

    #[test]
    fn epoll_readable_event_fires() {
        let mut ep = Epoll::new().unwrap();
        let (mut a, b) = pair();
        ep.register(b.as_raw_fd(), Interest::READABLE, token(1))
            .unwrap();
        a.write_all(b"hi").unwrap();
        let events = ep.wait(Some(Duration::from_millis(500))).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].token, token(1));
        assert!(events[0].ready.is_readable());
    }

    #[test]
    fn epoll_modify_removes_readable_interest() {
        let mut ep = Epoll::new().unwrap();
        let (mut a, b) = pair();
        ep.register(b.as_raw_fd(), Interest::READABLE, token(2))
            .unwrap();
        ep.modify(b.as_raw_fd(), Interest::WRITABLE, token(2))
            .unwrap();
        a.write_all(b"x").unwrap();
        let events = ep.wait(Some(Duration::from_millis(200))).unwrap();
        assert!(
            events
                .iter()
                .all(|e| e.ready.is_writable() && !e.ready.is_readable()),
            "readable interest should have been removed: {events:?}"
        );
    }

    #[test]
    fn epoll_deregister_stops_events() {
        let mut ep = Epoll::new().unwrap();
        let (mut a, b) = pair();
        ep.register(b.as_raw_fd(), Interest::READABLE, token(3))
            .unwrap();
        ep.deregister(b.as_raw_fd()).unwrap();
        a.write_all(b"z").unwrap();
        let events = ep.wait(Some(Duration::from_millis(100))).unwrap();
        assert!(events.is_empty());
    }
}
