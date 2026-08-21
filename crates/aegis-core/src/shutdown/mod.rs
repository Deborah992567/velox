//! Graceful shutdown and connection draining.
//!
//! Phase 25: Coordinate server shutdown with connection drain periods.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Phase of the shutdown sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownPhase {
    Running,
    Draining,
    ShuttingDown,
    Terminated,
}

/// Coordinator for graceful shutdown.
#[derive(Debug)]
pub struct ShutdownCoordinator {
    /// Whether shutdown has been requested.
    shutdown_requested: Arc<AtomicBool>,
    /// Current phase.
    phase: AtomicU8,
    /// When shutdown was initiated.
    shutdown_started: Option<Instant>,
    /// Maximum time to drain connections.
    drain_timeout: Duration,
    /// Count of active connections.
    active_connections: Arc<AtomicU64>,
}

impl ShutdownCoordinator {
    pub fn new(drain_timeout: Duration) -> Self {
        Self {
            shutdown_requested: Arc::new(AtomicBool::new(false)),
            phase: AtomicU8::new(0),
            shutdown_started: None,
            drain_timeout,
            active_connections: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Request shutdown.
    pub fn request_shutdown(&mut self) {
        self.shutdown_requested.store(true, Ordering::Release);
        self.phase
            .store(ShutdownPhase::Draining as u8, Ordering::Release);
        self.shutdown_started = Some(Instant::now());
    }

    pub fn is_shutdown_requested(&self) -> bool {
        self.shutdown_requested.load(Ordering::Acquire)
    }

    pub fn phase(&self) -> ShutdownPhase {
        match self.phase.load(Ordering::Acquire) {
            1 => ShutdownPhase::Draining,
            2 => ShutdownPhase::ShuttingDown,
            3 => ShutdownPhase::Terminated,
            _ => ShutdownPhase::Running,
        }
    }

    pub fn begin_shutdown(&self) {
        self.phase
            .store(ShutdownPhase::ShuttingDown as u8, Ordering::Release);
    }

    pub fn terminate(&self) {
        self.phase
            .store(ShutdownPhase::Terminated as u8, Ordering::Release);
    }

    pub fn increment_connections(&self) {
        self.active_connections.fetch_add(1, Ordering::Relaxed);
    }

    pub fn decrement_connections(&self) {
        self.active_connections.fetch_sub(1, Ordering::Relaxed);
    }

    pub fn active_connections(&self) -> u64 {
        self.active_connections.load(Ordering::Relaxed)
    }

    /// Whether drain timeout has elapsed or all connections are gone.
    pub fn is_drain_complete(&self) -> bool {
        if self.active_connections() == 0 {
            return true;
        }
        self.shutdown_started
            .is_some_and(|started| started.elapsed() >= self.drain_timeout)
    }

    pub fn remaining_drain_time(&self) -> Option<Duration> {
        self.shutdown_started.map(|started| {
            let elapsed = started.elapsed();
            self.drain_timeout
                .checked_sub(elapsed)
                .unwrap_or(Duration::ZERO)
        })
    }
}

impl Default for ShutdownCoordinator {
    fn default() -> Self {
        Self::new(Duration::from_secs(30))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_running() {
        let coord = ShutdownCoordinator::new(Duration::from_secs(5));
        assert_eq!(coord.phase(), ShutdownPhase::Running);
        assert!(!coord.is_shutdown_requested());
    }

    #[test]
    fn request_shutdown() {
        let mut coord = ShutdownCoordinator::new(Duration::from_secs(5));
        coord.request_shutdown();
        assert!(coord.is_shutdown_requested());
        assert_eq!(coord.phase(), ShutdownPhase::Draining);
    }

    #[test]
    fn connection_counting() {
        let coord = ShutdownCoordinator::new(Duration::from_secs(5));
        assert_eq!(coord.active_connections(), 0);
        coord.increment_connections();
        coord.increment_connections();
        assert_eq!(coord.active_connections(), 2);
        coord.decrement_connections();
        assert_eq!(coord.active_connections(), 1);
    }

    #[test]
    fn drain_complete_when_no_connections() {
        let mut coord = ShutdownCoordinator::new(Duration::from_secs(5));
        coord.request_shutdown();
        assert!(coord.is_drain_complete());
    }

    #[test]
    fn drain_not_complete_with_active_connections() {
        let mut coord = ShutdownCoordinator::new(Duration::from_mins(1));
        coord.request_shutdown();
        coord.increment_connections();
        assert!(!coord.is_drain_complete());
    }

    #[test]
    fn drain_complete_on_timeout() {
        let mut coord = ShutdownCoordinator::new(Duration::ZERO);
        coord.request_shutdown();
        coord.increment_connections();
        assert!(coord.is_drain_complete());
    }

    #[test]
    fn phase_transitions() {
        let mut coord = ShutdownCoordinator::new(Duration::from_secs(5));
        assert_eq!(coord.phase(), ShutdownPhase::Running);
        coord.request_shutdown();
        assert_eq!(coord.phase(), ShutdownPhase::Draining);
        coord.begin_shutdown();
        assert_eq!(coord.phase(), ShutdownPhase::ShuttingDown);
        coord.terminate();
        assert_eq!(coord.phase(), ShutdownPhase::Terminated);
    }

    #[test]
    fn remaining_drain_time() {
        let coord = ShutdownCoordinator::new(Duration::from_secs(30));
        assert!(coord.remaining_drain_time().is_none());

        let mut coord = coord;
        coord.request_shutdown();
        let remaining = coord.remaining_drain_time().unwrap();
        assert!(remaining <= Duration::from_secs(30));
    }
}
