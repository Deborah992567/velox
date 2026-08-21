//! Upstream health check strategies.
//!
//! Active health checks that probe backend servers to determine availability.

use std::time::{Duration, Instant};

/// Health check status for an upstream server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum HealthStatus {
    /// Server is healthy and accepting requests.
    Healthy,
    /// Server is degraded but may still serve some requests.
    Degraded,
    /// Server is unreachable or failing health checks.
    Unhealthy,
    /// Server health has not been determined yet.
    #[default]
    Unknown,
}

/// Configuration for health checking an upstream server.
#[derive(Debug, Clone)]
pub struct HealthCheckConfig {
    /// Interval between health checks.
    pub interval: Duration,
    /// Timeout for each health check probe.
    pub timeout: Duration,
    /// Number of consecutive failures before marking unhealthy.
    pub failure_threshold: u32,
    /// Number of consecutive successes before marking healthy again.
    pub success_threshold: u32,
    /// HTTP path for HTTP-based health checks (None = TCP connect only).
    pub http_path: Option<String>,
    /// Expected HTTP status code for HTTP checks.
    pub expected_status: u16,
}

impl Default for HealthCheckConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(10),
            timeout: Duration::from_secs(5),
            failure_threshold: 3,
            success_threshold: 2,
            http_path: Some("/health".into()),
            expected_status: 200,
        }
    }
}

/// Tracks health state for a single upstream server.
#[derive(Debug, Clone)]
pub struct HealthTracker {
    status: HealthStatus,
    consecutive_failures: u32,
    consecutive_successes: u32,
    last_check: Option<Instant>,
    config: HealthCheckConfig,
}

impl HealthTracker {
    pub const fn new(config: HealthCheckConfig) -> Self {
        Self {
            status: HealthStatus::Unknown,
            consecutive_failures: 0,
            consecutive_successes: 0,
            last_check: None,
            config,
        }
    }

    pub fn record_success(&mut self) {
        self.consecutive_successes += 1;
        self.consecutive_failures = 0;
        self.last_check = Some(Instant::now());

        if self.consecutive_successes >= self.config.success_threshold {
            self.status = HealthStatus::Healthy;
        } else if self.status == HealthStatus::Unknown {
            self.status = HealthStatus::Degraded;
        }
    }

    pub fn record_failure(&mut self) {
        self.consecutive_failures += 1;
        self.consecutive_successes = 0;
        self.last_check = Some(Instant::now());

        if self.consecutive_failures >= self.config.failure_threshold {
            self.status = HealthStatus::Unhealthy;
        } else {
            self.status = HealthStatus::Degraded;
        }
    }

    #[must_use]
    pub const fn status(&self) -> HealthStatus {
        self.status
    }

    pub fn is_healthy(&self) -> bool {
        self.status == HealthStatus::Healthy
    }

    pub fn is_usable(&self) -> bool {
        self.status == HealthStatus::Healthy || self.status == HealthStatus::Degraded
    }

    pub fn should_check(&self) -> bool {
        self.last_check
            .is_none_or(|last| last.elapsed() >= self.config.interval)
    }

    #[must_use]
    pub const fn last_check(&self) -> Option<Instant> {
        self.last_check
    }

    #[must_use]
    pub const fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures
    }

    pub const fn reset(&mut self) {
        self.status = HealthStatus::Unknown;
        self.consecutive_failures = 0;
        self.consecutive_successes = 0;
        self.last_check = None;
    }
}

/// Cluster-level health summary.
#[derive(Debug, Clone, Copy, Default)]
pub struct ClusterHealth {
    pub total: u32,
    pub healthy: u32,
    pub degraded: u32,
    pub unhealthy: u32,
}

impl ClusterHealth {
    pub fn healthy_ratio(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            f64::from(self.healthy) / f64::from(self.total)
        }
    }

    #[must_use]
    pub const fn has_available_backends(&self) -> bool {
        self.healthy + self.degraded > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_unknown() {
        let tracker = HealthTracker::new(HealthCheckConfig::default());
        assert_eq!(tracker.status(), HealthStatus::Unknown);
        assert!(!tracker.is_healthy());
    }

    #[test]
    fn becomes_healthy_after_successes() {
        let config = HealthCheckConfig {
            success_threshold: 2,
            ..Default::default()
        };
        let mut tracker = HealthTracker::new(config);
        tracker.record_success();
        assert!(!tracker.is_healthy());
        tracker.record_success();
        assert!(tracker.is_healthy());
    }

    #[test]
    fn becomes_unhealthy_after_failures() {
        let config = HealthCheckConfig {
            failure_threshold: 2,
            ..Default::default()
        };
        let mut tracker = HealthTracker::new(config);
        tracker.record_failure();
        assert_eq!(tracker.status(), HealthStatus::Degraded);
        tracker.record_failure();
        assert_eq!(tracker.status(), HealthStatus::Unhealthy);
    }

    #[test]
    fn failure_resets_success_count() {
        let config = HealthCheckConfig {
            success_threshold: 2,
            failure_threshold: 1,
            ..Default::default()
        };
        let mut tracker = HealthTracker::new(config);
        tracker.record_success();
        tracker.record_failure();
        tracker.record_success();
        assert!(!tracker.is_healthy());
    }

    #[test]
    fn should_check_initial() {
        let tracker = HealthTracker::new(HealthCheckConfig::default());
        assert!(tracker.should_check());
    }

    #[test]
    fn reset_clears_state() {
        let mut tracker = HealthTracker::new(HealthCheckConfig::default());
        tracker.record_failure();
        tracker.record_failure();
        tracker.reset();
        assert_eq!(tracker.status(), HealthStatus::Unknown);
        assert_eq!(tracker.consecutive_failures(), 0);
    }

    #[test]
    fn is_usable_when_degraded() {
        let config = HealthCheckConfig {
            failure_threshold: 3,
            success_threshold: 1,
            ..Default::default()
        };
        let mut tracker = HealthTracker::new(config);
        tracker.record_failure();
        assert!(tracker.is_usable());
    }

    #[test]
    fn cluster_health() {
        let h = ClusterHealth {
            total: 10,
            healthy: 7,
            degraded: 2,
            unhealthy: 1,
        };
        assert!((h.healthy_ratio() - 0.7).abs() < f64::EPSILON);
        assert!(h.has_available_backends());
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn cluster_empty() {
        let h = ClusterHealth::default();
        assert_eq!(h.healthy_ratio(), 0.0);
        assert!(!h.has_available_backends());
    }
}
