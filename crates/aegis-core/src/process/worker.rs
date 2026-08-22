//! Worker process management.
//!
//! Phase 17: Fork-based worker spawning with listener sharing,
//! worker lifecycle tracking, and automatic restart on crash.
#![allow(unsafe_code)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Configuration for a worker process.
#[derive(Debug, Clone)]
pub struct WorkerConfig {
    pub id: u32,
    pub config_path: String,
    pub worker_connections: usize,
}

/// Result of forking a worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForkResult {
    Master(u32),
    Worker(u32),
}

/// Fork a worker process.
#[allow(clippy::cast_possible_wrap)]
pub fn fork_worker(config: &WorkerConfig) -> std::io::Result<ForkResult> {
    // SAFETY: libc::fork() is safe when called single-threaded (the master).
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err(std::io::Error::last_os_error());
    }
    if pid == 0 {
        Ok(ForkResult::Worker(config.id))
    } else {
        Ok(ForkResult::Master(pid.cast_unsigned()))
    }
}

/// Reap a single finished child, returning its PID and exit status.
#[allow(clippy::cast_possible_wrap)]
pub fn reap_child() -> Option<(u32, i32)> {
    let mut status: libc::c_int = 0;
    // SAFETY: waitpid with WNOHANG is non-blocking.
    // std::ptr::addr_of_mut! satisfies clippy's `implicit borrow as raw pointer`.
    let pid = unsafe { libc::waitpid(-1, std::ptr::addr_of_mut!(status), libc::WNOHANG) };
    if pid > 0 {
        let exit_code = if libc::WIFEXITED(status) {
            libc::WEXITSTATUS(status)
        } else if libc::WIFSIGNALED(status) {
            128 + libc::WTERMSIG(status)
        } else {
            -1
        };
        Some((pid.cast_unsigned(), exit_code))
    } else {
        None
    }
}

/// Reap all finished children.
pub fn reap_all() -> Vec<(u32, i32)> {
    let mut reaped = Vec::new();
    while let Some((child_pid, exit_status)) = reap_child() {
        reaped.push((child_pid, exit_status));
    }
    reaped
}

/// Atomic flag for signaling a worker to shut down.
#[derive(Debug)]
pub struct WorkerShutdown {
    flag: Arc<AtomicBool>,
}

impl WorkerShutdown {
    pub fn new() -> Self {
        Self {
            flag: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn share(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.flag)
    }

    pub fn trigger(&self) {
        self.flag.store(true, Ordering::Release);
    }

    pub fn is_triggered(&self) -> bool {
        self.flag.load(Ordering::Acquire)
    }
}

impl Default for WorkerShutdown {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_shutdown_flag() {
        let ws = WorkerShutdown::new();
        assert!(!ws.is_triggered());
        ws.trigger();
        assert!(ws.is_triggered());
    }

    #[test]
    fn worker_shutdown_shared() {
        let ws = WorkerShutdown::new();
        let shared = ws.share();
        assert!(!shared.load(Ordering::Acquire));
        ws.trigger();
        assert!(shared.load(Ordering::Acquire));
    }

    #[test]
    fn worker_config_fields() {
        let config = WorkerConfig {
            id: 1,
            config_path: "/tmp/test.conf".to_owned(),
            worker_connections: 1024,
        };
        assert_eq!(config.id, 1);
        assert_eq!(config.worker_connections, 1024);
    }

    #[test]
    fn reap_returns_none_when_no_children() {
        assert!(reap_child().is_none());
    }
}
