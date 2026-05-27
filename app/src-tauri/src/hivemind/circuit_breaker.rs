use std::fmt;
use std::time::{Duration, Instant};

use thiserror::Error;
use tokio::sync::Mutex;
use tracing::{debug, warn};

/// The three states of a circuit breaker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState {
    /// Normal operation -- requests flow through.
    Closed,
    /// Too many failures -- requests are rejected immediately.
    Open,
    /// Cooldown elapsed -- a single probe request is allowed through.
    HalfOpen,
}

impl fmt::Display for CircuitState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CircuitState::Closed => write!(f, "Closed"),
            CircuitState::Open => write!(f, "Open"),
            CircuitState::HalfOpen => write!(f, "HalfOpen"),
        }
    }
}

/// Errors returned when the circuit breaker blocks a request.
#[derive(Debug, Error)]
pub enum CircuitBreakerError {
    /// The circuit is open; the caller should wait before retrying.
    #[error("circuit breaker open, retry after {retry_after:?}")]
    Open { retry_after: Duration },

    /// The circuit is half-open and a probe request is already in flight.
    #[error("circuit breaker half-open, probe already in flight")]
    HalfOpenBusy,
}

/// All mutable state lives behind a single lock to prevent race conditions.
#[derive(Debug)]
struct CircuitBreakerInner {
    state: CircuitState,
    failure_count: u32,
    opened_at: Option<Instant>,
    /// Prevents multiple concurrent probes while in the HalfOpen state.
    probe_in_flight: bool,
}

impl CircuitBreakerInner {
    fn new() -> Self {
        Self {
            state: CircuitState::Closed,
            failure_count: 0,
            opened_at: None,
            probe_in_flight: false,
        }
    }
}

/// A three-state circuit breaker that protects downstream services from
/// cascading failures.
///
/// All mutable state is held inside a single `tokio::sync::Mutex` so that
/// state transitions are atomic and free of race conditions.
#[derive(Debug)]
pub struct CircuitBreaker {
    inner: Mutex<CircuitBreakerInner>,
    failure_threshold: u32,
    cooldown: Duration,
    name: String,
}

impl CircuitBreaker {
    /// Create a new circuit breaker.
    ///
    /// * `name` -- Human-readable label used in log messages.
    /// * `failure_threshold` -- Number of consecutive failures before the
    ///   circuit opens.
    /// * `cooldown` -- How long the circuit stays open before transitioning
    ///   to half-open.
    pub fn new(name: impl Into<String>, failure_threshold: u32, cooldown: Duration) -> Self {
        Self {
            inner: Mutex::new(CircuitBreakerInner::new()),
            failure_threshold,
            cooldown,
            name: name.into(),
        }
    }

    /// Check whether a request is allowed to proceed.
    ///
    /// Returns `Ok(())` if the request may proceed, or a
    /// [`CircuitBreakerError`] explaining why it was rejected.
    pub async fn before_request(&self) -> Result<(), CircuitBreakerError> {
        let mut inner = self.inner.lock().await;

        match inner.state {
            CircuitState::Closed => {
                // Normal operation -- always allow.
                Ok(())
            }
            CircuitState::Open => {
                // Invariant: `opened_at` should be set when state is Open. If a
                // bug or external mutation left it None, treat the cooldown as
                // already elapsed rather than panicking — the breaker will
                // self-heal by transitioning to HalfOpen on the next probe.
                let opened_at = match inner.opened_at {
                    Some(t) => t,
                    None => {
                        warn!(
                            circuit = %self.name,
                            "circuit Open with no opened_at timestamp; treating cooldown as elapsed and self-healing"
                        );
                        inner.state = CircuitState::HalfOpen;
                        inner.probe_in_flight = true;
                        return Ok(());
                    }
                };
                let elapsed = opened_at.elapsed();

                if elapsed >= self.cooldown {
                    // Cooldown elapsed -- transition to HalfOpen and allow
                    // exactly one probe.
                    debug!(
                        circuit = %self.name,
                        "cooldown elapsed ({:?}), transitioning Open -> HalfOpen",
                        elapsed,
                    );
                    inner.state = CircuitState::HalfOpen;
                    inner.probe_in_flight = true;
                    Ok(())
                } else {
                    let retry_after = self.cooldown - elapsed;
                    debug!(
                        circuit = %self.name,
                        "circuit open, retry after {:?}",
                        retry_after,
                    );
                    Err(CircuitBreakerError::Open { retry_after })
                }
            }
            CircuitState::HalfOpen => {
                if inner.probe_in_flight {
                    // A probe is already in progress -- reject.
                    debug!(
                        circuit = %self.name,
                        "half-open probe already in flight, rejecting request",
                    );
                    Err(CircuitBreakerError::HalfOpenBusy)
                } else {
                    // Allow one more probe.
                    inner.probe_in_flight = true;
                    Ok(())
                }
            }
        }
    }

    /// Record a successful request. Resets the circuit to the Closed state.
    pub async fn record_success(&self) {
        let mut inner = self.inner.lock().await;
        let prev_state = inner.state;

        inner.failure_count = 0;
        inner.state = CircuitState::Closed;
        inner.probe_in_flight = false;
        inner.opened_at = None;

        if prev_state != CircuitState::Closed {
            debug!(
                circuit = %self.name,
                "success recorded, transitioning {} -> Closed",
                prev_state,
            );
        }
    }

    /// Record a failed request.
    ///
    /// * **Closed** — increments the failure counter; if the threshold is
    ///   reached the circuit transitions to Open with a fresh cooldown.
    /// * **HalfOpen** — the probe request failed; the circuit re-trips back
    ///   to Open with a new cooldown.
    /// * **Open** — the circuit is already open so the failure is logged but
    ///   `opened_at` is **not** extended (this guards against late failures
    ///   from in-flight requests resetting the cooldown timer).
    pub async fn record_failure(&self) {
        let mut inner = self.inner.lock().await;
        inner.probe_in_flight = false;

        match inner.state {
            CircuitState::Closed => {
                inner.failure_count += 1;
                if inner.failure_count >= self.failure_threshold {
                    inner.state = CircuitState::Open;
                    inner.opened_at = Some(Instant::now());
                    inner.failure_count = 0;
                    warn!(
                        circuit = %self.name,
                        threshold = self.failure_threshold,
                        "failure threshold reached, transitioning Closed -> Open",
                    );
                } else {
                    debug!(
                        circuit = %self.name,
                        failures = inner.failure_count,
                        threshold = self.failure_threshold,
                        "failure recorded ({}/{})",
                        inner.failure_count,
                        self.failure_threshold,
                    );
                }
            }
            CircuitState::HalfOpen => {
                // A single probe failure in HalfOpen re-trips the breaker
                // with a fresh cooldown timer.
                inner.state = CircuitState::Open;
                inner.opened_at = Some(Instant::now());
                inner.failure_count = 0;
                warn!(
                    circuit = %self.name,
                    "probe failed, transitioning HalfOpen -> Open",
                );
            }
            CircuitState::Open => {
                // Already open — do NOT reset opened_at, otherwise a late
                // failure from an in-flight request would permanently extend
                // the cooldown and prevent the circuit from ever reaching
                // HalfOpen again.
                debug!(
                    circuit = %self.name,
                    "failure recorded while already Open (ignored for transition)",
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn closed_allows_requests() {
        let cb = CircuitBreaker::new("test", 3, Duration::from_secs(10));
        assert!(cb.before_request().await.is_ok());
    }

    #[tokio::test]
    async fn opens_after_threshold_failures() {
        let cb = CircuitBreaker::new("test", 3, Duration::from_secs(10));
        cb.record_failure().await;
        cb.record_failure().await;
        cb.record_failure().await;
        assert!(matches!(
            cb.before_request().await,
            Err(CircuitBreakerError::Open { .. })
        ));
    }
}
