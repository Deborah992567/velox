//! Platform event drivers: the OS-specific half of the reactor.
//!
//! An [`EventDriver`] registers file descriptors for readiness notification
//! and waits for events. The concrete drivers are `kqueue` on macOS/BSD and
//! `epoll` on Linux; both expose the identical trait so the reactor (and the
//! future `io_uring` driver) are platform-independent.
//!
//! # Safety policy
//!
//! Like [`crate::net`], this module is a scoped syscall zone: the workspace
//! default `unsafe_code = "warn"` is lifted here, every `unsafe` block carries
//! a `// SAFETY:` comment, and the trait surface presented to the rest of the
//! crate is fully safe.
#![allow(unsafe_code)]

#[cfg(target_os = "linux")]
mod epoll;
#[cfg(any(target_os = "macos", target_os = "ios"))]
mod kqueue;

use std::io;
use std::os::fd::RawFd;
use std::time::Duration;

/// Opaque handle identifying a registered source. `index` selects the slab
/// slot; `generation` guards against reuse after deregistration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Token(u64);

impl Token {
    // Packing loses no bits: index and generation are both 32-bit halves of a
    // 64-bit token, so the casts are lossless by construction.
    #[allow(clippy::cast_lossless)]
    pub const fn from_parts(index: u32, generation: u32) -> Self {
        Self(((generation as u64) << 32) | index as u64)
    }

    const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    /// The slab slot index carried by this token.
    ///
    /// # Panics
    ///
    /// Cannot panic: the low 32 bits always fit.
    pub fn index(self) -> u32 {
        u32::try_from(self.0 & 0xFFFF_FFFF).expect("masked to 32 bits")
    }

    /// The generation guard carried by this token, for fd-churn safety.
    ///
    /// # Panics
    ///
    /// Cannot panic: the high 32 bits always fit.
    pub fn generation(self) -> u32 {
        u32::try_from(self.0 >> 32).expect("high half is 32 bits")
    }

    const fn raw(self) -> u64 {
        self.0
    }
}

/// The event classes a source is interested in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Interest(u8);

const INTEREST_READABLE: u8 = 1 << 0;
const INTEREST_WRITABLE: u8 = 1 << 1;

impl Interest {
    /// Readiness for reading (incoming data, EOF, hangup).
    pub const READABLE: Self = Self(INTEREST_READABLE);
    /// Readiness for writing (send buffer has room).
    pub const WRITABLE: Self = Self(INTEREST_WRITABLE);

    /// Combine two interests.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Whether the readable interest is set.
    pub const fn is_readable(self) -> bool {
        self.0 & INTEREST_READABLE != 0
    }

    /// Whether the writable interest is set.
    pub const fn is_writable(self) -> bool {
        self.0 & INTEREST_WRITABLE != 0
    }
}

/// What the driver reported as ready for a source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Ready(u8);

const READY_READABLE: u8 = 1 << 0;
const READY_WRITABLE: u8 = 1 << 1;
const READY_ERROR: u8 = 1 << 2;
const READY_HUP: u8 = 1 << 3;

impl Ready {
    /// Readable (data available, EOF, or hangup).
    pub const READABLE: Self = Self(READY_READABLE);
    /// Writable (send buffer has room).
    pub const WRITABLE: Self = Self(READY_WRITABLE);
    /// Error occurred (also implies readable).
    pub const ERROR: Self = Self(READY_ERROR);
    /// Peer hung up or closed (also implies readable).
    pub const HUP: Self = Self(READY_HUP);

    const fn from_bits(bits: u8) -> Self {
        Self(bits)
    }

    /// Whether no readiness bit is set (a spurious wakeup).
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Whether the readable bit is set.
    pub const fn is_readable(self) -> bool {
        self.0 & READY_READABLE != 0
    }

    /// Whether the writable bit is set.
    pub const fn is_writable(self) -> bool {
        self.0 & READY_WRITABLE != 0
    }

    /// Whether an error was reported (sticky; also implies readability so a
    /// read attempt surfaces the error).
    pub const fn is_error(self) -> bool {
        self.0 & READY_ERROR != 0
    }

    /// Whether the peer closed or hung up.
    pub const fn is_hup(self) -> bool {
        self.0 & READY_HUP != 0
    }

    /// Combine two readiness reports.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// A single readiness event produced by [`EventDriver::wait`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Event {
    /// The registered source this event belongs to.
    pub token: Token,
    /// What became ready.
    pub ready: Ready,
}

/// The OS-specific half of the reactor.
///
/// Implementations are single-threaded: they must not be shared across
/// threads.
pub trait EventDriver: Send {
    /// Start reporting `interest` for `fd`, mapping future events to `token`.
    fn register(&mut self, fd: RawFd, interest: Interest, token: Token) -> io::Result<()>;

    /// Change the interest set for an already-registered `fd`.
    fn modify(&mut self, fd: RawFd, interest: Interest, token: Token) -> io::Result<()>;

    /// Stop reporting events for `fd`. The source is no longer polled.
    fn deregister(&mut self, fd: RawFd) -> io::Result<()>;

    /// Block for readiness events up to `timeout` (`None` blocks indefinitely,
    /// `Some(ZERO)` polls once). Returns all events currently available.
    fn wait(&mut self, timeout: Option<Duration>) -> io::Result<Vec<Event>>;
}

/// Build the driver for the current platform.
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub fn driver() -> io::Result<Box<dyn EventDriver>> {
    Ok(Box::new(kqueue::Kqueue::new()?))
}

/// Build the driver for the current platform.
#[cfg(target_os = "linux")]
pub fn driver() -> io::Result<Box<dyn EventDriver>> {
    Ok(Box::new(epoll::Epoll::new()?))
}

/// Build the driver for the current platform.
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "ios")))]
pub fn driver() -> io::Result<Box<dyn EventDriver>> {
    Err(io::Error::other("no event driver for this platform"))
}

#[cfg(test)]
mod tests {
    use super::{Interest, Ready, Token};
    use std::io::Write;
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    fn test_pair() -> (UnixStream, UnixStream) {
        UnixStream::pair().unwrap()
    }

    fn token(n: u32) -> Token {
        Token::from_parts(n, 1)
    }

    #[test]
    fn driver_readable_event_fires() {
        let mut driver = super::driver().unwrap();
        let (mut a, b) = test_pair();
        let t = token(7);
        driver
            .register(b.as_raw_fd(), Interest::READABLE, t)
            .unwrap();

        a.write_all(b"x").unwrap();
        let events = driver.wait(Some(Duration::from_millis(500))).unwrap();
        assert!(!events.is_empty(), "expected a readiness event");
        let event = events[0];
        assert_eq!(event.token, t);
        assert!(event.ready.is_readable());
    }

    #[test]
    fn driver_writable_event_fires() {
        let mut driver = super::driver().unwrap();
        let (_, b) = test_pair();
        let t = token(8);
        driver
            .register(b.as_raw_fd(), Interest::WRITABLE, t)
            .unwrap();
        let events = driver.wait(Some(Duration::from_millis(500))).unwrap();
        assert!(!events.is_empty());
        assert!(events[0].ready.is_writable());
    }

    #[test]
    fn driver_modify_changes_interest() {
        let mut driver = super::driver().unwrap();
        let (mut a, b) = test_pair();
        let t = token(9);
        driver
            .register(b.as_raw_fd(), Interest::READABLE, t)
            .unwrap();
        // Demote to writable only: a write must no longer produce a readable
        // event, but the socket is writable.
        driver.modify(b.as_raw_fd(), Interest::WRITABLE, t).unwrap();
        a.write_all(b"y").unwrap();
        let events = driver.wait(Some(Duration::from_millis(200))).unwrap();
        assert!(
            events
                .iter()
                .all(|e| e.ready.is_writable() && !e.ready.is_readable()),
            "readable interest should have been removed: {events:?}"
        );
    }

    #[test]
    fn driver_deregister_stops_events() {
        let mut driver = super::driver().unwrap();
        let (mut a, b) = test_pair();
        let t = token(10);
        driver
            .register(b.as_raw_fd(), Interest::READABLE, t)
            .unwrap();
        driver.deregister(b.as_raw_fd()).unwrap();
        a.write_all(b"z").unwrap();
        let events = driver.wait(Some(Duration::from_millis(100))).unwrap();
        assert!(events.is_empty(), "deregistered fd must not report events");
    }

    #[test]
    fn interest_and_ready_flags() {
        let interest = Interest::READABLE.union(Interest::WRITABLE);
        assert!(interest.is_readable());
        assert!(interest.is_writable());
        let ready = Ready::READABLE.union(Ready::HUP);
        assert!(ready.is_readable());
        assert!(ready.is_hup());
        assert!(!ready.is_writable());
        assert!(!ready.is_empty());
    }

    #[test]
    fn token_packs_and_unpacks() {
        let t = token(42);
        assert_eq!(t.index(), 42);
        assert_eq!(t.generation(), 1);
    }
}
