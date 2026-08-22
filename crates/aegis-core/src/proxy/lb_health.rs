//! Upstream server selection and load balancer health integration.
//!
//! Connects the health check module with the load balancer for
//! automatic removal of unhealthy backends.

use std::time::Duration;

/// How the load balancer should react to health check failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HealthPolicy {
    /// Remove unhealthy servers from rotation entirely.
    #[default]
    Failover,
    /// Reduce weight of unhealthy servers but keep them in rotation.
    Weighted,
    /// Return error immediately if no healthy servers.
    Strict,
}

/// Configuration for LB health integration.
#[derive(Debug, Clone)]
pub struct LbHealthConfig {
    pub policy: HealthPolicy,
    pub min_healthy_servers: usize,
    pub recovery_delay: Duration,
    pub degraded_weight_ratio: f64,
}

impl Default for LbHealthConfig {
    fn default() -> Self {
        Self {
            policy: HealthPolicy::Failover,
            min_healthy_servers: 1,
            recovery_delay: Duration::from_secs(30),
            degraded_weight_ratio: 0.5,
        }
    }
}

impl LbHealthConfig {
    pub const fn failover() -> Self {
        Self {
            policy: HealthPolicy::Failover,
            min_healthy_servers: 1,
            recovery_delay: Duration::from_secs(30),
            degraded_weight_ratio: 0.5,
        }
    }

    pub const fn weighted() -> Self {
        Self {
            policy: HealthPolicy::Weighted,
            min_healthy_servers: 1,
            recovery_delay: Duration::from_secs(30),
            degraded_weight_ratio: 0.5,
        }
    }

    pub const fn strict() -> Self {
        Self {
            policy: HealthPolicy::Strict,
            min_healthy_servers: 1,
            recovery_delay: Duration::ZERO,
            degraded_weight_ratio: 0.0,
        }
    }
}

/// Result of backend selection after health filtering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectionResult {
    Ok(usize),
    NoHealthyBackends,
    InsufficientHealthy(usize),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_failover() {
        assert_eq!(HealthPolicy::default(), HealthPolicy::Failover);
    }

    #[test]
    fn failover_config() {
        let c = LbHealthConfig::failover();
        assert_eq!(c.policy, HealthPolicy::Failover);
    }

    #[test]
    fn weighted_config() {
        let c = LbHealthConfig::weighted();
        assert_eq!(c.policy, HealthPolicy::Weighted);
        assert!((c.degraded_weight_ratio - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn strict_config() {
        let c = LbHealthConfig::strict();
        assert_eq!(c.policy, HealthPolicy::Strict);
        assert_eq!(c.recovery_delay, Duration::ZERO);
    }

    #[test]
    fn selection_result_variants() {
        assert_eq!(SelectionResult::Ok(0), SelectionResult::Ok(0));
        assert_ne!(SelectionResult::Ok(0), SelectionResult::NoHealthyBackends);
        assert_ne!(
            SelectionResult::InsufficientHealthy(2),
            SelectionResult::NoHealthyBackends
        );
    }
}
