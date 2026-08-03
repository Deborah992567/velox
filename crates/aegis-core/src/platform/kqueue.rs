//! macOS/BSD kqueue driver.
//!
//! `kqueue(2)` reports readiness via filters: `EVFILT_READ` and `EVFILT_WRITE`.
//! Because a filter's interest set cannot be changed in place, [`Kqueue::modify`]
//! deletes both filters and re-adds the requested ones; [`Kqueue::deregister`]
//! deletes both. A delete of a filter that was never added fails with `ENOENT`,
//! which is treated as success.
//!
//! Tokens travel through the opaque `udata` field, so `Token` must fit the
//! pointer width (guaranteed on the 64-bit targets this server supports).
#![allow(unsafe_code)]
// Syscall glue: casts at the kqueue boundary are width conversions only.
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

/// The maximum number of events retrieved by a single `kevent(2)` call.
const BATCH: usize = 1024;

/// The kernel event queue for one worker's event loop.
#[derive(Debug)]
pub struct Kqueue {
    kq: OwnedFd,
}

impl Kqueue {
    /// Create a new kernel event queue.
    pub fn new() -> io::Result<Self> {
        // SAFETY: kqueue(2) allocates a fresh kernel event queue and returns a
        // descriptor that this caller now owns.
        let fd = unsafe { libc::kqueue() };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `fd` is a newly-created, owned descriptor.
        Ok(Self {
            kq: unsafe { OwnedFd::from_raw_fd(fd) },
        })
    }

    fn build_change(
        fd: RawFd,
        filter: libc::c_short,
        flags: libc::c_ushort,
        token: Option<Token>,
    ) -> libc::kevent {
        let ident = usize::try_from(fd).expect("file descriptor is non-negative");
        let udata = token.map_or(std::ptr::null_mut(), |t| {
            (t.raw() as usize) as *mut libc::c_void
        });
        libc::kevent {
            ident,
            filter,
            flags,
            fflags: 0,
            data: 0,
            udata,
        }
    }

    /// Apply a single change; `ENOENT` (filter not present) is success.
    fn apply(&self, change: libc::kevent, ignore_enoint: bool) -> io::Result<()> {
        // SAFETY: `change` is a fully-initialized kevent; no result events are
        // requested, so the event list pointer is null with zero count.
        let rc = unsafe {
            libc::kevent(
                self.kq.as_raw_fd(),
                std::ptr::from_ref(&change),
                1,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        };
        if rc < 0 {
            let err = io::Error::last_os_error();
            if ignore_enoint && err.raw_os_error() == Some(libc::ENOENT) {
                return Ok(());
            }
            Err(err)
        } else {
            Ok(())
        }
    }

    fn add_filter(&self, fd: RawFd, filter: libc::c_short, token: Token) -> io::Result<()> {
        // EV_ADD is idempotent: re-adding an existing filter updates it.
        self.apply(
            Self::build_change(fd, filter, libc::EV_ADD | libc::EV_ENABLE, Some(token)),
            false,
        )
    }

    fn remove_filter(&self, fd: RawFd, filter: libc::c_short) -> io::Result<()> {
        self.apply(Self::build_change(fd, filter, libc::EV_DELETE, None), true)
    }

    fn translate(event: libc::kevent) -> Event {
        let mut bits = 0;
        match event.filter {
            libc::EVFILT_READ => bits |= super::READY_READABLE,
            libc::EVFILT_WRITE => bits |= super::READY_WRITABLE,
            _ => {}
        }
        if event.flags & libc::EV_EOF != 0 {
            bits |= super::READY_HUP;
        }
        if event.flags & libc::EV_ERROR != 0 {
            bits |= super::READY_ERROR;
        }
        let raw = event.udata as usize as u64;
        Event {
            token: Token::from_raw(raw),
            ready: Ready::from_bits(bits),
        }
    }
}

impl EventDriver for Kqueue {
    fn register(&mut self, fd: RawFd, interest: Interest, token: Token) -> io::Result<()> {
        if interest.is_readable() {
            self.add_filter(fd, libc::EVFILT_READ, token)?;
        }
        if interest.is_writable() {
            self.add_filter(fd, libc::EVFILT_WRITE, token)?;
        }
        Ok(())
    }

    fn modify(&mut self, fd: RawFd, interest: Interest, token: Token) -> io::Result<()> {
        self.remove_filter(fd, libc::EVFILT_READ)?;
        self.remove_filter(fd, libc::EVFILT_WRITE)?;
        if interest.is_readable() {
            self.add_filter(fd, libc::EVFILT_READ, token)?;
        }
        if interest.is_writable() {
            self.add_filter(fd, libc::EVFILT_WRITE, token)?;
        }
        Ok(())
    }

    fn deregister(&mut self, fd: RawFd) -> io::Result<()> {
        self.remove_filter(fd, libc::EVFILT_READ)?;
        self.remove_filter(fd, libc::EVFILT_WRITE)?;
        Ok(())
    }

    fn wait(&mut self, timeout: Option<Duration>) -> io::Result<Vec<Event>> {
        let mut raw = vec![
            libc::kevent {
                ident: 0,
                filter: 0,
                flags: 0,
                fflags: 0,
                data: 0,
                udata: std::ptr::null_mut(),
            };
            BATCH
        ];
        let timeout = timeout.map(|d| libc::timespec {
            tv_sec: libc::time_t::try_from(d.as_secs()).expect("duration fits timespec seconds"),
            tv_nsec: d.subsec_nanos() as libc::c_long,
        });
        let timeout_ptr = timeout
            .as_ref()
            .map_or(std::ptr::null(), std::ptr::from_ref);
        // SAFETY: `raw` is a valid buffer of `BATCH` kevent structs that the
        // kernel writes into; `timeout_ptr` is null or a valid timespec.
        let n = unsafe {
            libc::kevent(
                self.kq.as_raw_fd(),
                std::ptr::null(),
                0,
                raw.as_mut_ptr(),
                i32::try_from(BATCH).expect("batch size fits c_int"),
                timeout_ptr,
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
    use super::Kqueue;
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
    fn kqueue_readable_event_fires() {
        let mut kq = Kqueue::new().unwrap();
        let (mut a, b) = pair();
        kq.register(b.as_raw_fd(), Interest::READABLE, token(1))
            .unwrap();
        a.write_all(b"hi").unwrap();
        let events = kq.wait(Some(Duration::from_millis(500))).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].token, token(1));
        assert!(events[0].ready.is_readable());
    }

    #[test]
    fn kqueue_modify_removes_readable_interest() {
        let mut kq = Kqueue::new().unwrap();
        let (mut a, b) = pair();
        kq.register(b.as_raw_fd(), Interest::READABLE, token(2))
            .unwrap();
        kq.modify(b.as_raw_fd(), Interest::WRITABLE, token(2))
            .unwrap();
        a.write_all(b"x").unwrap();
        let events = kq.wait(Some(Duration::from_millis(200))).unwrap();
        assert!(
            events
                .iter()
                .all(|e| e.ready.is_writable() && !e.ready.is_readable()),
            "readable interest should have been removed: {events:?}"
        );
    }

    #[test]
    fn kqueue_deregister_stops_events() {
        let mut kq = Kqueue::new().unwrap();
        let (mut a, b) = pair();
        kq.register(b.as_raw_fd(), Interest::READABLE, token(3))
            .unwrap();
        kq.deregister(b.as_raw_fd()).unwrap();
        a.write_all(b"z").unwrap();
        let events = kq.wait(Some(Duration::from_millis(100))).unwrap();
        assert!(events.is_empty());
    }
}
