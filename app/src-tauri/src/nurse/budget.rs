//! Per-session intervention budget with detector-class sub-budgets,
//! lifetime cap age-decay, and per-`dedup_key` cooldowns.
//!
//! Replaces the legacy single `max_interventions = 3` counter, which was
//! structurally insufficient for multi-day swarms.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::nurse::config::BudgetConfig;

/// Mutable per-session budget state.
///
/// `Clone` is derived so the dispatcher's two-phase periodic sweep can
/// take a cheap snapshot under the brief `sessions.read()` and then run
/// the per-signal filtering work without holding the engine-level lock.
/// The two `HashMap`s scale with the number of distinct detectors and
/// dedup keys seen on one session — sub-kilobyte in practice.
#[derive(Debug, Clone)]
pub struct BudgetState {
    pub session_first_observed_at: Instant,
    pub lifetime_count: u32,
    pub per_detector_used: HashMap<String, u32>,
    pub per_key_last_fired: HashMap<String, Instant>,
}

impl BudgetState {
    pub fn new(session_first_observed_at: Instant) -> Self {
        Self {
            session_first_observed_at,
            lifetime_count: 0,
            per_detector_used: HashMap::new(),
            per_key_last_fired: HashMap::new(),
        }
    }

    /// Current lifetime cap, given the configured decay rate.
    pub fn current_cap(&self, cfg: &BudgetConfig, now: Instant) -> u32 {
        let age_hours = now
            .saturating_duration_since(self.session_first_observed_at)
            .as_secs()
            / 3600;
        let grown = cfg
            .initial_lifetime_cap
            .saturating_add((age_hours as u32).saturating_mul(cfg.decay_per_hour));
        grown.min(cfg.max_lifetime_cap)
    }

    pub fn try_admit(
        &mut self,
        cfg: &BudgetConfig,
        detector: &str,
        dedup_key: &str,
        now: Instant,
    ) -> BudgetOutcome {
        // 1. Per-key cooldown
        if let Some(&last) = self.per_key_last_fired.get(dedup_key) {
            let elapsed = now.saturating_duration_since(last);
            if elapsed < Duration::from_secs(cfg.per_key_cooldown_secs) {
                let remaining = Duration::from_secs(cfg.per_key_cooldown_secs) - elapsed;
                return BudgetOutcome::Gated(BudgetGateReason::PerKeyCooldown {
                    dedup_key: dedup_key.to_string(),
                    remaining_ms: remaining.as_millis() as u64,
                });
            }
        }
        // 2. Per-detector sub-budget
        let used = *self.per_detector_used.get(detector).unwrap_or(&0);
        if used >= cfg.per_detector_cap {
            return BudgetOutcome::Gated(BudgetGateReason::PerDetectorExhausted {
                detector: detector.to_string(),
                used,
                cap: cfg.per_detector_cap,
            });
        }
        // 3. Lifetime cap
        let cap = self.current_cap(cfg, now);
        if self.lifetime_count >= cap {
            return BudgetOutcome::Gated(BudgetGateReason::LifetimeExhausted {
                used: self.lifetime_count,
                cap,
            });
        }
        BudgetOutcome::Allowed {
            lifetime_used: self.lifetime_count,
            lifetime_cap: cap,
            per_detector_used: used,
            per_detector_cap: cfg.per_detector_cap,
        }
    }

    /// Pure read: has the per-key cooldown elapsed for `dedup_key`? Returns
    /// `true` if no prior fire has been recorded, or if `now` is past
    /// `last_fired + per_key_cooldown_secs`.
    ///
    /// Used by the dispatcher's two-phase periodic sweep to pre-filter
    /// candidates without holding the sessions lock. The authoritative
    /// gate is still [`Self::try_admit`]; this is an advisory check that
    /// avoids spawning a dispatcher task that would immediately gate.
    pub fn is_cooldown_elapsed(
        &self,
        cfg: &BudgetConfig,
        _detector: &str,
        dedup_key: &str,
        now: Instant,
    ) -> bool {
        match self.per_key_last_fired.get(dedup_key) {
            None => true,
            Some(&last) => {
                let elapsed = now.saturating_duration_since(last);
                elapsed >= Duration::from_secs(cfg.per_key_cooldown_secs)
            }
        }
    }

    /// Record an admitted dispatch.
    pub fn record(&mut self, detector: &str, dedup_key: &str, now: Instant) {
        self.lifetime_count = self.lifetime_count.saturating_add(1);
        *self
            .per_detector_used
            .entry(detector.to_string())
            .or_insert(0) = self
            .per_detector_used
            .get(detector)
            .copied()
            .unwrap_or(0)
            .saturating_add(1);
        self.per_key_last_fired.insert(dedup_key.to_string(), now);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BudgetOutcome {
    Allowed {
        lifetime_used: u32,
        lifetime_cap: u32,
        per_detector_used: u32,
        per_detector_cap: u32,
    },
    Gated(BudgetGateReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BudgetGateReason {
    PerKeyCooldown {
        dedup_key: String,
        remaining_ms: u64,
    },
    PerDetectorExhausted {
        detector: String,
        used: u32,
        cap: u32,
    },
    LifetimeExhausted {
        used: u32,
        cap: u32,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nurse::config::BudgetConfig;

    #[test]
    fn age_decay_grows_cap() {
        let mut cfg = BudgetConfig::swarm_default();
        cfg.initial_lifetime_cap = 5;
        cfg.decay_per_hour = 1;
        cfg.max_lifetime_cap = 10;
        let start = Instant::now() - Duration::from_secs(2 * 3600);
        let state = BudgetState::new(start);
        assert_eq!(state.current_cap(&cfg, Instant::now()), 7);
    }

    #[test]
    fn cap_clamps_at_max() {
        let mut cfg = BudgetConfig::swarm_default();
        cfg.initial_lifetime_cap = 5;
        cfg.decay_per_hour = 1;
        cfg.max_lifetime_cap = 10;
        let start = Instant::now() - Duration::from_secs(20 * 3600);
        let state = BudgetState::new(start);
        assert_eq!(state.current_cap(&cfg, Instant::now()), 10);
    }

    #[test]
    fn per_detector_sub_budget_isolates_classes() {
        let mut cfg = BudgetConfig::swarm_default();
        cfg.initial_lifetime_cap = 100;
        cfg.max_lifetime_cap = 100;
        cfg.per_detector_cap = 2;
        cfg.per_key_cooldown_secs = 0;
        let now = Instant::now();
        let mut state = BudgetState::new(now);
        // Admit 2 of "stall".
        state.record("stall", "k1", now);
        state.record("stall", "k2", now);
        // Third "stall" admission gated.
        match state.try_admit(&cfg, "stall", "k3", now) {
            BudgetOutcome::Gated(BudgetGateReason::PerDetectorExhausted { .. }) => {}
            other => panic!("expected per-detector exhaustion, got {:?}", other),
        }
        // Other detector still admitted.
        match state.try_admit(&cfg, "process_health", "px", now) {
            BudgetOutcome::Allowed { .. } => {}
            other => panic!("expected allowed for other detector, got {:?}", other),
        }
    }

    #[test]
    fn per_key_cooldown_blocks_then_reopens() {
        let mut cfg = BudgetConfig::swarm_default();
        cfg.initial_lifetime_cap = 100;
        cfg.max_lifetime_cap = 100;
        cfg.per_detector_cap = 100;
        cfg.per_key_cooldown_secs = 60;
        let now = Instant::now();
        let mut state = BudgetState::new(now);
        state.record("d", "k", now);
        match state.try_admit(&cfg, "d", "k", now) {
            BudgetOutcome::Gated(BudgetGateReason::PerKeyCooldown { .. }) => {}
            other => panic!("expected per-key cooldown, got {:?}", other),
        }
        // Long after the cooldown the key admits again.
        let later = now + Duration::from_secs(120);
        match state.try_admit(&cfg, "d", "k", later) {
            BudgetOutcome::Allowed { .. } => {}
            other => panic!("expected allowed after cooldown, got {:?}", other),
        }
    }
}
