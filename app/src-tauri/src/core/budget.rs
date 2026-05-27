//! Hard cost caps for swarm execution.
//!
//! Phase 5A goal: long autonomous swarms can burn unbounded budget. This
//! module provides a small helper that compares the current swarm spend
//! and the current daily spend against optional caps configured on the
//! swarm and globally. Callers (currently `core::queen::run_swarm_full`)
//! consult it between feature batches and pause the swarm if either cap
//! is breached.
//!
//! The check is intentionally side-effect-free: it only compares numbers
//! and returns a verdict. Callers handle pausing, event emission, and
//! persistence themselves so this module stays small and easy to test.

use serde::{Deserialize, Serialize};

/// Optional cost caps applied to a single swarm run. `None` for either
/// field means "unlimited" for that scope.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub struct BudgetCaps {
    /// Hard cap on the lifetime spend of this swarm in USD. Once the
    /// swarm's accumulated cost meets or exceeds this value the queen
    /// pauses the swarm. `None` means unlimited.
    pub swarm_budget_usd: Option<f64>,
    /// Hard cap on aggregate spend across all swarms / hivemind / chat
    /// usage today (UTC). Once today's total meets or exceeds this
    /// value the queen pauses the swarm. `None` means unlimited.
    pub daily_budget_usd: Option<f64>,
}

/// Which budget tripped during a check. Returned by `evaluate_budget`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetBreach {
    /// `swarm_spend >= swarm_budget_usd`.
    Swarm,
    /// `daily_spend >= daily_budget_usd`.
    Daily,
}

impl BudgetBreach {
    /// Human-readable reason string used in pause events / progress logs.
    pub fn reason(self) -> &'static str {
        match self {
            BudgetBreach::Swarm => "swarm budget exceeded",
            BudgetBreach::Daily => "daily budget exceeded",
        }
    }
}

/// Compare the current spend against the configured caps and return the
/// first breach found (swarm budget takes precedence over daily). Returns
/// `None` when both caps are absent or neither has been exceeded.
///
/// Per-swarm budget is opt-in. `None` for either cap means unlimited —
/// the same behaviour as before this phase shipped. Negative spend
/// values are treated as zero (defensive: callers may pass deltas that
/// briefly underflow during accumulator reconciliation).
pub fn evaluate_budget(
    caps: BudgetCaps,
    swarm_spend_usd: f64,
    daily_spend_usd: f64,
) -> Option<BudgetBreach> {
    let swarm = swarm_spend_usd.max(0.0);
    let daily = daily_spend_usd.max(0.0);

    if let Some(cap) = caps.swarm_budget_usd {
        if cap >= 0.0 && swarm >= cap {
            return Some(BudgetBreach::Swarm);
        }
    }
    if let Some(cap) = caps.daily_budget_usd {
        if cap >= 0.0 && daily >= cap {
            return Some(BudgetBreach::Daily);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_caps_means_no_breach() {
        let caps = BudgetCaps::default();
        assert_eq!(evaluate_budget(caps, 0.0, 0.0), None);
        assert_eq!(evaluate_budget(caps, 999.0, 999.0), None);
        assert_eq!(evaluate_budget(caps, f64::MAX, f64::MAX), None);
    }

    #[test]
    fn swarm_cap_unmet_returns_none() {
        let caps = BudgetCaps {
            swarm_budget_usd: Some(5.0),
            daily_budget_usd: None,
        };
        assert_eq!(evaluate_budget(caps, 4.99, 0.0), None);
        assert_eq!(evaluate_budget(caps, 0.0, 0.0), None);
    }

    #[test]
    fn swarm_cap_met_exactly_breaches() {
        // >= boundary: spending exactly the cap is a breach so the queen
        // pauses before the next batch can push spend past the cap.
        let caps = BudgetCaps {
            swarm_budget_usd: Some(5.0),
            daily_budget_usd: None,
        };
        assert_eq!(evaluate_budget(caps, 5.0, 0.0), Some(BudgetBreach::Swarm));
        assert_eq!(evaluate_budget(caps, 5.001, 0.0), Some(BudgetBreach::Swarm));
    }

    #[test]
    fn daily_cap_breach_returned_when_only_daily_set() {
        let caps = BudgetCaps {
            swarm_budget_usd: None,
            daily_budget_usd: Some(50.0),
        };
        assert_eq!(evaluate_budget(caps, 999.0, 49.99), None);
        assert_eq!(evaluate_budget(caps, 0.0, 50.0), Some(BudgetBreach::Daily));
        assert_eq!(evaluate_budget(caps, 0.0, 75.0), Some(BudgetBreach::Daily));
    }

    #[test]
    fn swarm_cap_takes_precedence_over_daily() {
        // If both caps are exceeded simultaneously, swarm is reported
        // first because it's the more actionable single-swarm signal.
        let caps = BudgetCaps {
            swarm_budget_usd: Some(5.0),
            daily_budget_usd: Some(50.0),
        };
        assert_eq!(
            evaluate_budget(caps, 100.0, 100.0),
            Some(BudgetBreach::Swarm)
        );
    }

    #[test]
    fn breach_reason_strings() {
        assert_eq!(BudgetBreach::Swarm.reason(), "swarm budget exceeded");
        assert_eq!(BudgetBreach::Daily.reason(), "daily budget exceeded");
    }

    #[test]
    fn negative_spend_treated_as_zero() {
        let caps = BudgetCaps {
            swarm_budget_usd: Some(5.0),
            daily_budget_usd: Some(50.0),
        };
        // A negative number shouldn't accidentally trip the cap, and
        // shouldn't panic.
        assert_eq!(evaluate_budget(caps, -1.0, -1.0), None);
    }

    #[test]
    fn negative_cap_is_ignored() {
        // Defensive: a negative cap is treated as "unset" rather than
        // tripping on any non-negative spend.
        let caps = BudgetCaps {
            swarm_budget_usd: Some(-1.0),
            daily_budget_usd: None,
        };
        assert_eq!(evaluate_budget(caps, 100.0, 0.0), None);
    }

    #[test]
    fn caps_roundtrip_serde() {
        // Backwards compat: ModelSettings holds a `swarm_budget_usd:
        // Option<f64>` which serialises to null when unset. The cap
        // struct should round-trip cleanly through JSON.
        let caps = BudgetCaps {
            swarm_budget_usd: Some(5.0),
            daily_budget_usd: Some(50.0),
        };
        let json = serde_json::to_string(&caps).unwrap();
        let back: BudgetCaps = serde_json::from_str(&json).unwrap();
        assert_eq!(back, caps);

        let empty = BudgetCaps::default();
        let json = serde_json::to_string(&empty).unwrap();
        let back: BudgetCaps = serde_json::from_str(&json).unwrap();
        assert_eq!(back, empty);
    }
}
