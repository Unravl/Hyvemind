use std::time::Duration;

use tracing::debug;

/// Exponential backoff calculator with jitter.
///
/// Formula: `min(max_delay, base_delay * 2^attempt + rand(0, jitter_range))`
///
/// Defaults: `min(60s, 5s * 2^attempt + rand(0, 2s))`
#[derive(Debug, Clone)]
pub struct BackoffCalculator {
    base_delay: Duration,
    max_delay: Duration,
    jitter_range: Duration,
}

impl BackoffCalculator {
    /// Create a new backoff calculator with explicit parameters.
    pub fn new(base_delay: Duration, max_delay: Duration, jitter_range: Duration) -> Self {
        Self {
            base_delay,
            max_delay,
            jitter_range,
        }
    }

    /// Calculate the delay for the given attempt number (zero-indexed).
    ///
    /// The result is clamped to `max_delay`. Robust against attempt counts
    /// large enough to overflow `f64` exponentials or exceed
    /// `Duration::from_secs_f64`'s representable range — in those cases the
    /// result is simply `max_delay`.
    pub fn calculate(&self, attempt: u32) -> Duration {
        let base_secs = self.base_delay.as_secs_f64();
        let exponential = base_secs * 2.0_f64.powi(attempt as i32);
        let jitter = rand::random::<f64>() * self.jitter_range.as_secs_f64();
        let total = exponential + jitter;

        // Guard against NaN / infinity / values too large for Duration. Any
        // such value is, by definition, well above the cap, so collapse to
        // max_delay rather than panic in `Duration::from_secs_f64`.
        let max_secs = self.max_delay.as_secs_f64();
        let delay = if !total.is_finite() || total >= max_secs {
            self.max_delay
        } else {
            Duration::from_secs_f64(total).min(self.max_delay)
        };

        debug!(
            attempt,
            delay_ms = delay.as_millis() as u64,
            "backoff calculated",
        );

        delay
    }
}

impl Default for BackoffCalculator {
    /// Sensible defaults: 5 s base, 60 s ceiling, 2 s jitter.
    fn default() -> Self {
        Self::new(
            Duration::from_secs(5),
            Duration::from_secs(60),
            Duration::from_secs(2),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_values() {
        let bc = BackoffCalculator::default();
        assert_eq!(bc.base_delay, Duration::from_secs(5));
        assert_eq!(bc.max_delay, Duration::from_secs(60));
        assert_eq!(bc.jitter_range, Duration::from_secs(2));
    }

    #[test]
    fn respects_max_delay() {
        let bc = BackoffCalculator::new(
            Duration::from_secs(10),
            Duration::from_secs(30),
            Duration::from_secs(0), // no jitter for deterministic test
        );
        // 10 * 2^5 = 320s, should be clamped to 30s
        let delay = bc.calculate(5);
        assert_eq!(delay, Duration::from_secs(30));
    }

    #[test]
    fn zero_attempt_is_base_delay_plus_jitter() {
        let bc = BackoffCalculator::new(
            Duration::from_secs(5),
            Duration::from_secs(60),
            Duration::from_secs(0),
        );
        // 5 * 2^0 = 5s (no jitter)
        let delay = bc.calculate(0);
        assert_eq!(delay, Duration::from_secs(5));
    }

    #[test]
    fn exponential_growth() {
        let bc = BackoffCalculator::new(
            Duration::from_secs(1),
            Duration::from_secs(120),
            Duration::from_secs(0),
        );
        // 1*1, 1*2, 1*4, 1*8
        assert_eq!(bc.calculate(0), Duration::from_secs(1));
        assert_eq!(bc.calculate(1), Duration::from_secs(2));
        assert_eq!(bc.calculate(2), Duration::from_secs(4));
        assert_eq!(bc.calculate(3), Duration::from_secs(8));
    }
}

// ----------------------------------------------------------------------------
// Property-based tests (proptest)
// ----------------------------------------------------------------------------
//
// Invariants exercised:
//   * Monotonicity with zero jitter: backoff(n+1) >= backoff(n).
//   * Cap invariant: backoff(n) <= max_delay for all n (including overflow).
//   * Overflow at attempt = u32::MAX must not panic and must still respect
//     the cap.
#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    /// Helper: a deterministic backoff calculator (no jitter).
    fn no_jitter(base_secs: u64, max_secs: u64) -> BackoffCalculator {
        BackoffCalculator::new(
            Duration::from_secs(base_secs),
            Duration::from_secs(max_secs),
            Duration::from_secs(0),
        )
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        /// With zero jitter, `calculate(n+1) >= calculate(n)` for any
        /// reasonable attempt and base/max combination.
        #[test]
        fn monotone_no_jitter(
            base_secs in 1u64..=10,
            max_secs in 1u64..=600,
            attempt in 0u32..=20,
        ) {
            let bc = no_jitter(base_secs, max_secs);
            let a = bc.calculate(attempt);
            let b = bc.calculate(attempt + 1);
            prop_assert!(
                b >= a,
                "expected monotone backoff, got a={:?} b={:?} (attempt={})",
                a, b, attempt
            );
        }

        /// The result is never larger than `max_delay`, for any attempt in
        /// 0..=1000 and any jitter range (jitter only adds, but the cap
        /// must still hold).
        #[test]
        fn cap_invariant(
            base_secs in 1u64..=10,
            max_secs in 1u64..=120,
            jitter_secs in 0u64..=10,
            attempt in 0u32..=1000,
        ) {
            let bc = BackoffCalculator::new(
                Duration::from_secs(base_secs),
                Duration::from_secs(max_secs),
                Duration::from_secs(jitter_secs),
            );
            let d = bc.calculate(attempt);
            prop_assert!(
                d <= Duration::from_secs(max_secs),
                "delay {:?} exceeded max {:?} (attempt={})",
                d, Duration::from_secs(max_secs), attempt
            );
        }

        /// Extreme attempt counts (including `u32::MAX`) must not panic
        /// and must still respect the cap.
        #[test]
        fn no_panic_on_overflow(
            base_secs in 1u64..=10,
            max_secs in 1u64..=120,
            attempt_in in prop_oneof![
                Just(u32::MAX),
                Just(u32::MAX - 1),
                (1_000_000u32..u32::MAX),
            ],
        ) {
            let bc = no_jitter(base_secs, max_secs);
            // Just calling this must not panic.
            let d = bc.calculate(attempt_in);
            prop_assert!(
                d <= Duration::from_secs(max_secs),
                "overflowed delay {:?} exceeded max {:?} for attempt={}",
                d, Duration::from_secs(max_secs), attempt_in
            );
        }
    }
}
