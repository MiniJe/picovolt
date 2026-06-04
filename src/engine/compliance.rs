//! Optional, application-driven usage-policy hook.
//!
//! PicoVolt itself is Apache-2.0 and imposes **no** usage limits. This module is
//! a small, opt-in utility an application can call to enforce its *own* business
//! rules — for example, a self-imposed free-tier cap on a hosted product. It is
//! purely local (no network calls) and inert unless the application calls
//! [`ComplianceMonitor::assert_compliance`]; nothing in the engine invokes it.
//!
//! The default thresholds are illustrative; configure them (or ignore the module
//! entirely) to suit your product.

use crate::core::errors::ComplianceError;

/// Usage metrics supplied by the host application at audit time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RuntimeMetrics {
    /// Current Monthly Active Users of the host application.
    pub current_mau: u64,
    /// Gross monthly revenue of the operating business, in USD.
    pub monthly_revenue: f64,
    /// Whether an authorizing key (e.g. the application's own paid-plan token)
    /// is present, exempting it from the thresholds.
    pub has_authorizing_key: bool,
}

impl RuntimeMetrics {
    /// Metrics for a deployment well within the configured free tier.
    pub fn free_tier() -> Self {
        Self {
            current_mau: 0,
            monthly_revenue: 0.0,
            has_authorizing_key: false,
        }
    }
}

/// The configurable thresholds and the audit entry point.
#[derive(Debug, Clone, Copy)]
pub struct ComplianceMonitor {
    /// MAU above which a commercial license is required.
    pub mau_threshold: u64,
    /// Monthly USD revenue above which the policy trips.
    pub revenue_threshold_usd: f64,
    /// Reserved flag for stricter enforcement policies (application-defined).
    pub enforcement_strict: bool,
}

impl ComplianceMonitor {
    /// Construct with illustrative default thresholds (50,000 MAU / $10,000 USD).
    pub fn new() -> Self {
        Self {
            mau_threshold: 50_000,
            revenue_threshold_usd: 10_000.0,
            enforcement_strict: false,
        }
    }

    /// Assert that `metrics` satisfy the configured policy.
    ///
    /// Crossing *either* the MAU or revenue threshold without an authorizing key
    /// yields [`ComplianceError::ThresholdExceeded`].
    pub fn assert_compliance(&self, metrics: &RuntimeMetrics) -> Result<(), ComplianceError> {
        let crossed_trigger = metrics.current_mau > self.mau_threshold
            || metrics.monthly_revenue > self.revenue_threshold_usd;
        if crossed_trigger && !metrics.has_authorizing_key {
            return Err(ComplianceError::ThresholdExceeded);
        }
        Ok(())
    }
}

impl Default for ComplianceMonitor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_tier_is_compliant() {
        let monitor = ComplianceMonitor::new();
        assert!(monitor
            .assert_compliance(&RuntimeMetrics::free_tier())
            .is_ok());
    }

    #[test]
    fn crossing_mau_without_key_fails() {
        let monitor = ComplianceMonitor::new();
        let metrics = RuntimeMetrics {
            current_mau: 50_001,
            monthly_revenue: 0.0,
            has_authorizing_key: false,
        };
        assert_eq!(
            monitor.assert_compliance(&metrics),
            Err(ComplianceError::ThresholdExceeded)
        );
    }

    #[test]
    fn crossing_revenue_with_key_is_allowed() {
        let monitor = ComplianceMonitor::new();
        let metrics = RuntimeMetrics {
            current_mau: 1_000_000,
            monthly_revenue: 250_000.0,
            has_authorizing_key: true,
        };
        assert!(monitor.assert_compliance(&metrics).is_ok());
    }

    #[test]
    fn exactly_at_threshold_is_compliant() {
        // Thresholds are strict "greater than".
        let monitor = ComplianceMonitor::new();
        let metrics = RuntimeMetrics {
            current_mau: 50_000,
            monthly_revenue: 10_000.0,
            has_authorizing_key: false,
        };
        assert!(monitor.assert_compliance(&metrics).is_ok());
    }
}
