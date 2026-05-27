//! Per-(session, trigger-kind) sliding-window storm guard.
//!
//! Lifted from the legacy `core::nurse_service::ERROR_STORM_*` window. A
//! single `(session_id, trigger_kind)` cannot fire more than
//! `MAX_PER_WINDOW` times in a `WINDOW` second sliding window; after the
//! cap is hit, further raises are dropped until `skip_until` elapses.
//!
//! CRITICAL severity signals bypass the storm guard unconditionally — see
//! [`NurseEngine`](crate::nurse::engine::NurseEngine)'s decision pipeline.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const MAX_PER_WINDOW: usize = 3;
const WINDOW: Duration = Duration::from_secs(60);
const COOLDOWN: Duration = Duration::from_secs(60);

#[derive(Debug, Default)]
struct PerKeyState {
    /// Timestamps of recent admissions inside `WINDOW`.
    recent: VecDeque<Instant>,
    /// `Some(t)` while gated. Re-admission allowed only after `t`.
    skip_until: Option<Instant>,
}

#[derive(Debug, Default)]
pub struct StormGuardState {
    by_key: HashMap<String, PerKeyState>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StormGuardOutcome {
    Passed,
    Gated {
        trigger_kind: String,
        recent_count: usize,
        skip_until_unix_ms: u64,
    },
}

/// Process-wide storm guard. Per-session state keyed by composite
/// `"{session_id}|{trigger_kind}"`.
#[derive(Debug, Default)]
pub struct StormGuard {
    inner: Mutex<StormGuardState>,
}

impl StormGuard {
    pub fn new() -> Self {
        Self::default()
    }

    /// Try to admit an intervention. Returns `Passed` when admitted (and
    /// records the admission); `Gated` when the per-key cap is exceeded
    /// or a cooldown is in effect.
    pub fn try_admit(&self, session_id: &str, trigger_kind: &str) -> StormGuardOutcome {
        let key = format!("{}|{}", session_id, trigger_kind);
        let mut guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let state = guard.by_key.entry(key).or_default();
        let now = Instant::now();

        // Trim window
        while let Some(&t) = state.recent.front() {
            if now.duration_since(t) >= WINDOW {
                state.recent.pop_front();
            } else {
                break;
            }
        }

        if let Some(until) = state.skip_until {
            if now < until {
                let ms_until = duration_to_unix_ms(now, until);
                return StormGuardOutcome::Gated {
                    trigger_kind: trigger_kind.to_string(),
                    recent_count: state.recent.len(),
                    skip_until_unix_ms: ms_until,
                };
            }
            state.skip_until = None;
        }

        if state.recent.len() >= MAX_PER_WINDOW {
            let until = now + COOLDOWN;
            state.skip_until = Some(until);
            let ms_until = duration_to_unix_ms(now, until);
            return StormGuardOutcome::Gated {
                trigger_kind: trigger_kind.to_string(),
                recent_count: state.recent.len(),
                skip_until_unix_ms: ms_until,
            };
        }

        state.recent.push_back(now);
        StormGuardOutcome::Passed
    }

    pub fn reset_for_session(&self, session_id: &str) {
        let mut guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let prefix = format!("{}|", session_id);
        guard.by_key.retain(|k, _| !k.starts_with(&prefix));
    }
}

/// Convert a future `Instant` to approximate epoch-ms via `SystemTime::now()`
/// as a stable epoch anchor.
fn duration_to_unix_ms(now: Instant, target: Instant) -> u64 {
    let extra = target.saturating_duration_since(now);
    let sys_now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    sys_now.saturating_add(extra.as_millis() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admits_up_to_cap_then_gates() {
        let g = StormGuard::new();
        assert!(matches!(g.try_admit("s", "k"), StormGuardOutcome::Passed));
        assert!(matches!(g.try_admit("s", "k"), StormGuardOutcome::Passed));
        assert!(matches!(g.try_admit("s", "k"), StormGuardOutcome::Passed));
        match g.try_admit("s", "k") {
            StormGuardOutcome::Gated { recent_count, .. } => assert_eq!(recent_count, 3),
            other => panic!("expected Gated, got {:?}", other),
        }
    }

    #[test]
    fn per_session_per_kind_isolated() {
        let g = StormGuard::new();
        // Saturate (s1, k).
        for _ in 0..MAX_PER_WINDOW {
            assert!(matches!(g.try_admit("s1", "k"), StormGuardOutcome::Passed));
        }
        // s1 with same k now gated.
        assert!(!matches!(g.try_admit("s1", "k"), StormGuardOutcome::Passed));
        // s2 with same k still passes.
        assert!(matches!(g.try_admit("s2", "k"), StormGuardOutcome::Passed));
        // s1 with a different k still passes.
        assert!(matches!(
            g.try_admit("s1", "other"),
            StormGuardOutcome::Passed
        ));
    }

    #[test]
    fn reset_clears_for_session() {
        let g = StormGuard::new();
        for _ in 0..MAX_PER_WINDOW {
            g.try_admit("s", "k");
        }
        assert!(!matches!(g.try_admit("s", "k"), StormGuardOutcome::Passed));
        g.reset_for_session("s");
        assert!(matches!(g.try_admit("s", "k"), StormGuardOutcome::Passed));
    }
}
