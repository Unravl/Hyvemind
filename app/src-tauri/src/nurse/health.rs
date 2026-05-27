//! Per-session health aggregates: [`Signal`], [`SessionHealth`],
//! [`Severity`], [`EscalationState`].
//!
//! `SessionHealth` is the structured record a detector mutates via
//! [`SignalDelta`](crate::nurse::detector::SignalDelta)s. The engine derives
//! a single `Tier` from the highest active severity, runs that through the
//! three-tier decision pipeline (Deterministic / Templated Steer / LLM
//! classifier), and dispatches an intervention if any tier matches.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::nurse::snapshot::SessionOwnerDto;
use crate::pi::session::SessionOwner;

/// Cap on `SessionHealth.signals` so long-running sessions stay lightweight
/// and Tier 3 classifier prompts stay within byte budgets.
pub const SIGNAL_RING_CAPACITY: usize = 50;

/// Severity ladder consumed by every detector and by the decision pipeline.
///
/// `Critical` is the only tier that bypasses storm-guard / budget gates
/// (process_dead, OOM, missing provider — these MUST act immediately).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Warn,
    Stalled,
    Critical,
}

impl Severity {
    /// Maps to a coarse tier consumed by the decision pipeline. A session's
    /// tier is the max severity of its currently raised signals.
    pub fn tier(self) -> Tier {
        match self {
            Severity::Info => Tier::Quiet,
            Severity::Warn => Tier::Warning,
            Severity::Stalled => Tier::Stalled,
            Severity::Critical => Tier::Critical,
        }
    }
}

/// Coarsest health classification of a single session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    Quiet,
    Warning,
    Stalled,
    Critical,
}

impl Default for Tier {
    fn default() -> Self {
        Self::Quiet
    }
}

/// Long-lived per-session escalation state, used by the engine to gate
/// classifier invocation and to record cooldowns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EscalationState {
    Quiet,
    Concerning,
    Escalated,
    Cooldown,
}

/// A single emitted health signal. Detectors raise and clear these via
/// [`SignalDelta`](crate::nurse::detector::SignalDelta) on every observed
/// event or periodic tick.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Signal {
    pub detector: &'static str,
    pub severity: Severity,
    /// Stable key used both for dedup inside [`SessionHealth`] and as the
    /// playbook lookup key for Tier 2 templated steers.
    pub dedup_key: String,
    pub summary: String,
    pub raised_at: DateTime<Utc>,
    /// Detector-specific evidence the operator (or a future Claude Code
    /// session looking at `signals/{session_id}.jsonl` 30 days later) needs
    /// to understand the raise without reading source code.
    pub evidence: serde_json::Value,
}

/// Truncated payload describing the most recent observation on a session.
/// Carried so post-hoc tools (`get_nurse_session_detail`) and the live UI
/// can show "what was Pi doing when this signal raised".
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PiActivityDigest {
    pub last_event_kind: Option<String>,
    pub last_event_at_ms: u64,
    pub event_count: u64,
    pub text_event_count: u64,
    pub tool_event_count: u64,
    pub awaiting_model_for_ms: Option<u64>,
    pub messages_in_flight: i64,
}

/// Lightweight summary of the last NurseDecision the engine produced for
/// this session. Embedded in `SessionHealth` so snapshot consumers can render
/// "last decision: Steer @ 12:34:56" without a second query.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NurseDecisionSummary {
    pub decision_id: String,
    pub kind: String,
    pub at: DateTime<Utc>,
    pub message_preview: Option<String>,
}

/// Per-session aggregate consumed by the classifier and the snapshot path.
#[derive(Debug, Clone, Serialize)]
pub struct SessionHealth {
    pub session_id: String,
    pub owner: SessionOwnerDto,
    /// Fixed-capacity ring of recent signals. Capacity = [`SIGNAL_RING_CAPACITY`].
    /// FIFO drop at capacity; [`SignalDelta::Clear`] eagerly removes matching
    /// entries via `Vec::retain`.
    pub signals: Vec<Signal>,
    pub tier: Tier,
    pub escalation_state: EscalationState,
    pub intervention_count: u32,
    pub last_classifier_at: Option<DateTime<Utc>>,
    pub last_decision: Option<NurseDecisionSummary>,
    pub last_observation: PiActivityDigest,
}

impl SessionHealth {
    pub fn new(session_id: String, owner: &SessionOwner) -> Self {
        Self {
            session_id,
            owner: SessionOwnerDto::from(owner),
            signals: Vec::with_capacity(8),
            tier: Tier::Quiet,
            escalation_state: EscalationState::Quiet,
            intervention_count: 0,
            last_classifier_at: None,
            last_decision: None,
            last_observation: PiActivityDigest::default(),
        }
    }

    /// Push a freshly raised signal into the ring, dropping the oldest when
    /// over capacity.
    pub fn push_signal(&mut self, signal: Signal) {
        if self.signals.len() >= SIGNAL_RING_CAPACITY {
            self.signals.remove(0);
        }
        self.signals.push(signal);
        self.recompute_tier();
    }

    /// Remove every signal matching `(detector, dedup_key)`.
    pub fn clear_signal(&mut self, detector: &str, dedup_key: &str) {
        self.signals
            .retain(|s| !(s.detector == detector && s.dedup_key == dedup_key));
        self.recompute_tier();
    }

    pub fn recompute_tier(&mut self) {
        let max = self
            .signals
            .iter()
            .map(|s| s.severity)
            .max()
            .map(|s| s.tier())
            .unwrap_or(Tier::Quiet);
        self.tier = max;
    }

    /// `true` if any currently raised signal has `Severity::Critical`.
    pub fn has_critical(&self) -> bool {
        self.signals
            .iter()
            .any(|s| s.severity == Severity::Critical)
    }

    /// Find the first signal with this `(detector, dedup_key)` key, if any.
    pub fn find_signal(&self, detector: &str, dedup_key: &str) -> Option<&Signal> {
        self.signals
            .iter()
            .find(|s| s.detector == detector && s.dedup_key == dedup_key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn s(detector: &'static str, severity: Severity, key: &str) -> Signal {
        Signal {
            detector,
            severity,
            dedup_key: key.to_string(),
            summary: "test".to_string(),
            raised_at: Utc::now(),
            evidence: serde_json::Value::Null,
        }
    }

    #[test]
    fn ring_drops_oldest_at_capacity() {
        let mut h = SessionHealth::new("sid".into(), &SessionOwner::Unknown);
        for i in 0..100 {
            h.push_signal(s("det", Severity::Info, &format!("k{}", i)));
        }
        assert_eq!(h.signals.len(), SIGNAL_RING_CAPACITY);
        // First retained should be the 50th-from-end (k50).
        assert_eq!(h.signals.first().unwrap().dedup_key, "k50");
        assert_eq!(h.signals.last().unwrap().dedup_key, "k99");
    }

    #[test]
    fn clear_removes_matching_signal_via_retain() {
        let mut h = SessionHealth::new("sid".into(), &SessionOwner::Unknown);
        h.push_signal(s("det", Severity::Warn, "loop:exact:abc"));
        h.push_signal(s("det", Severity::Warn, "loop:exact:xyz"));
        h.clear_signal("det", "loop:exact:abc");
        assert_eq!(h.signals.len(), 1);
        assert_eq!(h.signals[0].dedup_key, "loop:exact:xyz");
        // Recompute keeps Warn -> Warning tier.
        assert_eq!(h.tier, Tier::Warning);
    }

    #[test]
    fn tier_is_max_of_signal_severities() {
        let mut h = SessionHealth::new("sid".into(), &SessionOwner::Unknown);
        assert_eq!(h.tier, Tier::Quiet);
        h.push_signal(s("d", Severity::Info, "a"));
        assert_eq!(h.tier, Tier::Quiet);
        h.push_signal(s("d", Severity::Warn, "b"));
        assert_eq!(h.tier, Tier::Warning);
        h.push_signal(s("d", Severity::Critical, "c"));
        assert_eq!(h.tier, Tier::Critical);
        h.clear_signal("d", "c");
        assert_eq!(h.tier, Tier::Warning);
    }

    #[test]
    fn severity_orders_consistent_with_tier() {
        assert!(Severity::Info < Severity::Warn);
        assert!(Severity::Warn < Severity::Stalled);
        assert!(Severity::Stalled < Severity::Critical);
    }
}
