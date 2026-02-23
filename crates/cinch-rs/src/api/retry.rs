//! Automatic retry with exponential backoff and jitter.
//!
//! Retries transient HTTP/API errors (429, 500, 502, 503, 504, network timeouts)
//! with configurable exponential backoff. Never retries 400 (bad request) or 401
//! (auth) errors.

use std::time::Duration;

/// Configuration for retry behavior.
#[derive(Debug, Clone)]
pub struct RetryConfig {
    /// Maximum number of retries (0 = no retries, just fail immediately).
    pub max_retries: u32,
    /// Initial delay before the first retry.
    pub initial_delay: Duration,
    /// Maximum delay between retries.
    pub max_delay: Duration,
    /// Backoff multiplier (typically 2.0 for exponential backoff).
    pub multiplier: f64,
    /// Whether to add jitter to prevent thundering herd.
    pub jitter: bool,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 0,
            initial_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(8),
            multiplier: 2.0,
            jitter: true,
        }
    }
}

impl RetryConfig {
    /// Create a config with the given number of retries. Uses sensible defaults.
    pub fn with_retries(retries: u32) -> Self {
        Self {
            max_retries: retries,
            ..Default::default()
        }
    }

    /// Calculate the delay for a given attempt number (0-indexed).
    pub fn delay_for_attempt(&self, attempt: u32) -> Duration {
        let base = self.initial_delay.as_secs_f64() * self.multiplier.powi(attempt as i32);
        let capped = base.min(self.max_delay.as_secs_f64());

        if self.jitter {
            // Full jitter: random value between 0 and the calculated delay.
            // Use a simple deterministic jitter based on attempt number since
            // we don't want to pull in rand just for this.
            let jitter_factor = match attempt % 4 {
                0 => 0.75,
                1 => 0.90,
                2 => 0.60,
                3 => 0.85,
                _ => 0.80,
            };
            Duration::from_secs_f64(capped * jitter_factor)
        } else {
            Duration::from_secs_f64(capped)
        }
    }
}

/// Whether an error string indicates a transient (retryable) failure.
pub fn is_transient_error(error: &str) -> bool {
    let transient_statuses = ["429", "500", "502", "503", "504"];
    if transient_statuses
        .iter()
        .any(|s| error.contains(&format!("HTTP {s}")))
    {
        return true;
    }

    let lower = error.to_lowercase();
    [
        "request failed:",
        "connection reset",
        "connection refused",
        "timed out",
        "timeout",
        "broken pipe",
        "network",
    ]
    .iter()
    .any(|p| lower.contains(p))
}

/// Whether an error is a permanent (non-retryable) failure.
pub fn is_permanent_error(error: &str) -> bool {
    [
        "HTTP 400",
        "HTTP 401",
        "HTTP 403",
        "HTTP 404",
        "HTTP 422",
        "invalid",
        "bad request",
        "unauthorized",
    ]
    .iter()
    .any(|p| error.contains(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_no_retries() {
        let config = RetryConfig::default();
        assert_eq!(config.max_retries, 0);
    }

    #[test]
    fn with_retries_sets_count() {
        let config = RetryConfig::with_retries(3);
        assert_eq!(config.max_retries, 3);
    }

    #[test]
    fn delay_increases_exponentially() {
        let config = RetryConfig {
            jitter: false,
            ..RetryConfig::with_retries(5)
        };
        let d0 = config.delay_for_attempt(0);
        let d1 = config.delay_for_attempt(1);
        let d2 = config.delay_for_attempt(2);

        assert!(d1 > d0, "d1={d1:?} should be > d0={d0:?}");
        assert!(d2 > d1, "d2={d2:?} should be > d1={d1:?}");
    }

    #[test]
    fn delay_capped_at_max() {
        let config = RetryConfig {
            jitter: false,
            max_delay: Duration::from_secs(2),
            ..RetryConfig::with_retries(10)
        };
        let d10 = config.delay_for_attempt(10);
        assert!(d10 <= Duration::from_secs(2));
    }

    #[test]
    fn jitter_reduces_delay() {
        let config = RetryConfig {
            jitter: true,
            ..RetryConfig::with_retries(3)
        };
        let no_jitter = RetryConfig {
            jitter: false,
            ..RetryConfig::with_retries(3)
        };

        let d_jitter = config.delay_for_attempt(2);
        let d_no_jitter = no_jitter.delay_for_attempt(2);
        assert!(d_jitter <= d_no_jitter);
    }

    #[test]
    fn transient_errors_detected() {
        assert!(is_transient_error("OpenRouter API HTTP 429: rate limited"));
        assert!(is_transient_error("OpenRouter API HTTP 502: bad gateway"));
        assert!(is_transient_error("request failed: connection reset"));
        assert!(is_transient_error("request failed: timed out"));
    }

    #[test]
    fn permanent_errors_detected() {
        assert!(is_permanent_error("OpenRouter API HTTP 400: bad request"));
        assert!(is_permanent_error("OpenRouter API HTTP 401: unauthorized"));
    }

    #[test]
    fn non_transient_not_retried() {
        assert!(!is_transient_error("OpenRouter API HTTP 400: bad request"));
        assert!(!is_transient_error("some random error"));
    }
}
