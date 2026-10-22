// SPDX-License-Identifier: MIT OR Apache-2.0
//! Pool configuration.

use core::time::Duration;

use super::state::Level;

/// Static configuration of the spare pool.
#[derive(Clone, Debug)]
pub struct PoolConfig {
    /// Master switch: disabled means an empty pool and `take` always
    /// `Unavailable` with no actions.
    pub enabled: bool,
    /// How far spares are pre-loaded.
    pub level: Level,
    /// Maintain the Windowed (hidden window + GL) slot.
    pub windowed: bool,
    /// Maintain the Windowless (hidden WebView) slot.
    pub windowless: bool,
    /// Backoff base for the first refill attempt after a failure.
    pub backoff_base: Duration,
    /// Upper bound of the exponential backoff.
    pub backoff_max: Duration,
    /// Consecutive failures that trip the circuit breaker.
    pub max_consecutive_failures: u32,
    /// Long pause of the circuit breaker.
    pub failure_pause: Duration,
    /// Pause applied after pressure (kill or GL disable) before refilling.
    pub pressure_backoff: Duration,
}

impl Default for PoolConfig {
    fn default() -> Self {
        PoolConfig {
            enabled: true,
            level: Level::Skeleton,
            windowed: true,
            windowless: true,
            backoff_base: Duration::from_millis(250),
            backoff_max: Duration::from_secs(8),
            max_consecutive_failures: 4,
            failure_pause: Duration::from_secs(30),
            pressure_backoff: Duration::from_secs(2),
        }
    }
}
