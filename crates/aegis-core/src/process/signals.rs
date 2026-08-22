//! Signal state tracking for the master process.
//!
//! Phase 17: Non-blocking signal dispatch for SIGHUP (reload),
//! SIGTERM/SIGINT (shutdown), SIGUSR1 (reopen logs), SIGCHLD (reap).
#![allow(unsafe_code)]

use std::sync::atomic::{AtomicBool, Ordering};

/// Pending signals, checked non-blockingly by the master loop.
#[derive(Debug)]
pub struct SignalState {
    reload: AtomicBool,
    shutdown: AtomicBool,
    reopen: AtomicBool,
    child_exit: AtomicBool,
}

/// Kind of signal received.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    Reload,
    Shutdown,
    ReopenLogs,
    ChildExit,
}

impl SignalState {
    pub const fn new() -> Self {
        Self {
            reload: AtomicBool::new(false),
            shutdown: AtomicBool::new(false),
            reopen: AtomicBool::new(false),
            child_exit: AtomicBool::new(false),
        }
    }

    pub fn mark_reload(&self) {
        self.reload.store(true, Ordering::Release);
    }

    pub fn mark_shutdown(&self) {
        self.shutdown.store(true, Ordering::Release);
    }

    pub fn mark_reopen(&self) {
        self.reopen.store(true, Ordering::Release);
    }

    pub fn mark_child_exit(&self) {
        self.child_exit.store(true, Ordering::Release);
    }

    /// Drain and return all pending signals.
    pub fn drain(&self) -> Vec<Signal> {
        let mut signals = Vec::new();
        if self.child_exit.swap(false, Ordering::AcqRel) {
            signals.push(Signal::ChildExit);
        }
        if self.shutdown.swap(false, Ordering::AcqRel) {
            signals.push(Signal::Shutdown);
        }
        if self.reload.swap(false, Ordering::AcqRel) {
            signals.push(Signal::Reload);
        }
        if self.reopen.swap(false, Ordering::AcqRel) {
            signals.push(Signal::ReopenLogs);
        }
        signals
    }

    pub fn has_pending(&self) -> bool {
        self.reload.load(Ordering::Acquire)
            || self.shutdown.load(Ordering::Acquire)
            || self.reopen.load(Ordering::Acquire)
            || self.child_exit.load(Ordering::Acquire)
    }
}

impl Default for SignalState {
    fn default() -> Self {
        Self::new()
    }
}

/// Write a signal byte to a self-pipe fd so the master event loop wakes up.
///
/// # Safety
///
/// `fd` must be a valid, writable file descriptor.
pub unsafe fn notify_self_pipe(fd: i32) {
    let byte: u8 = 1;
    // SAFETY: caller guarantees fd is valid and writable.
    unsafe {
        let _ = libc::write(fd, (&raw const byte).cast(), 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drain_signals() {
        let state = SignalState::new();
        state.mark_reload();
        state.mark_reopen();

        let signals = state.drain();
        assert_eq!(signals.len(), 2);
        assert!(signals.contains(&Signal::Reload));
        assert!(signals.contains(&Signal::ReopenLogs));

        assert!(state.drain().is_empty());
    }

    #[test]
    fn shutdown_takes_priority() {
        let state = SignalState::new();
        state.mark_reload();
        state.mark_shutdown();

        let signals = state.drain();
        assert!(signals.contains(&Signal::Shutdown));
        assert!(signals.contains(&Signal::Reload));
    }

    #[test]
    fn has_pending() {
        let state = SignalState::new();
        assert!(!state.has_pending());
        state.mark_child_exit();
        assert!(state.has_pending());
        state.drain();
        assert!(!state.has_pending());
    }
}
