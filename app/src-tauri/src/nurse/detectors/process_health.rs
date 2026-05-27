//! `ProcessHealthDetector` — `is_alive()` liveness + stderr crash-pattern scan.
//!
//! Two Critical triggers:
//! - `tick()`: `!session.is_alive()` → `process_dead`.
//! - `observe(PiEvent::Error(msg))`: matches SIGSEGV / out of memory /
//!   ENOSPC / EPIPE / stack overflow → `crash_pattern`.
//!
//! Also exposes [`Self::synthetic_session_gone_unobserved`] returned by
//! the engine's `try_upgrade` helper when a tracked `Weak<PiSession>` no
//! longer upgrades. `session_gone_unobserved` is distinct from
//! `process_dead`: there's no Arc left to restart, so the engine's Tier-1
//! action is Cancel rather than Restart.

use std::any::Any;

use chrono::Utc;
use smallvec::{smallvec, SmallVec};

use crate::nurse::detector::{Detector, DetectorContext, SignalDelta, TickKind};
use crate::nurse::health::{Severity, Signal};
use crate::pi::events::PiEvent;

const CRASH_PATTERNS: &[&str] = &[
    "SIGSEGV",
    "out of memory",
    "OutOfMemory",
    "ENOSPC",
    "EPIPE",
    "stack overflow",
    "double free",
    "segmentation fault",
];

pub struct ProcessHealthDetector;

impl ProcessHealthDetector {
    pub fn new() -> Self {
        Self
    }

    /// Build the synthetic `session_gone_unobserved` Critical signal
    /// returned by `try_upgrade` on a Weak<PiSession> upgrade failure.
    pub fn synthetic_session_gone_unobserved() -> SignalDelta {
        SignalDelta::Raise(Signal {
            detector: "process_health",
            severity: Severity::Critical,
            dedup_key: "session_gone_unobserved".to_string(),
            summary: "Weak<PiSession> upgrade failed — the engine's only handle is gone"
                .to_string(),
            raised_at: Utc::now(),
            evidence: serde_json::json!({
                "reason": "weak_upgrade_failed",
            }),
        })
    }
}

impl Default for ProcessHealthDetector {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Default)]
struct ProcessHealthState {
    dead_raised: bool,
    crash_raised: bool,
}

impl Detector for ProcessHealthDetector {
    fn name(&self) -> &'static str {
        "process_health"
    }

    fn description(&self) -> &'static str {
        "Detects dead Pi subprocesses and crash-pattern stderr."
    }

    fn on_session_started(&self, _ctx: &DetectorContext<'_>) -> Box<dyn Any + Send + Sync> {
        Box::new(ProcessHealthState::default())
    }

    fn observe(
        &self,
        event: &PiEvent,
        ctx: &mut DetectorContext<'_>,
    ) -> SmallVec<[SignalDelta; 2]> {
        let state = ctx
            .state
            .downcast_mut::<ProcessHealthState>()
            .expect("ProcessHealthState shape mismatch");
        if let PiEvent::Error(msg) = event {
            for pat in CRASH_PATTERNS {
                if msg.contains(pat) {
                    state.crash_raised = true;
                    return smallvec![SignalDelta::Raise(Signal {
                        detector: "process_health",
                        severity: Severity::Critical,
                        dedup_key: "crash_pattern".to_string(),
                        summary: format!("crash pattern '{}' in Pi stderr", pat),
                        raised_at: Utc::now(),
                        evidence: serde_json::json!({
                            "matched_pattern": pat,
                            "error_message": truncate(msg, 1024),
                        }),
                    })];
                }
            }
        }
        SmallVec::new()
    }

    fn tick(&self, ctx: &mut DetectorContext<'_>) -> SmallVec<[SignalDelta; 2]> {
        let session = match ctx.session.upgrade() {
            Some(s) => s,
            None => return SmallVec::new(),
        };
        let state = ctx
            .state
            .downcast_mut::<ProcessHealthState>()
            .expect("ProcessHealthState shape mismatch");

        let mut out: SmallVec<[SignalDelta; 2]> = SmallVec::new();
        if !session.is_alive() {
            if !state.dead_raised {
                state.dead_raised = true;
                let pid = session.pid();
                out.push(SignalDelta::Raise(Signal {
                    detector: "process_health",
                    severity: Severity::Critical,
                    dedup_key: "process_dead".to_string(),
                    summary: "Pi subprocess is_alive returned false".to_string(),
                    raised_at: Utc::now(),
                    evidence: serde_json::json!({
                        "is_alive": false,
                        "pid": pid,
                    }),
                }));
            }
        } else if state.dead_raised {
            // Don't auto-clear `process_dead` — it's terminal.
        }
        out
    }

    fn tick_kind(&self) -> TickKind {
        TickKind::Slow
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}[truncated]", &s[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nurse::config::{NurseProfile, ProfileConfig};
    use crate::nurse::snapshot::ProviderStateSnapshot;
    use std::sync::{OnceLock, Weak};

    fn default_profile_config() -> &'static ProfileConfig {
        static CFG: OnceLock<ProfileConfig> = OnceLock::new();
        CFG.get_or_init(|| ProfileConfig::default_for(NurseProfile::Default))
    }

    #[test]
    fn crash_pattern_in_error_event_raises_critical() {
        let d = ProcessHealthDetector::new();
        let weak: Weak<crate::pi::session::PiSession> = Weak::new();
        let provider_state = ProviderStateSnapshot::default();
        let mut state_box: Box<dyn Any + Send + Sync> = Box::new(ProcessHealthState::default());
        let mut ctx = DetectorContext {
            session: &weak,
            state: state_box.as_mut(),
            now: std::time::Instant::now(),
            now_wall_ms: 0,
            profile: NurseProfile::Default,
            profile_config: default_profile_config(),
            provider_state: &provider_state,
            provider: None,
            model_id: None,
        };
        let out = d.observe(
            &PiEvent::Error("worker crashed with SIGSEGV at 0x0".into()),
            &mut ctx,
        );
        assert_eq!(out.len(), 1);
        match &out[0] {
            SignalDelta::Raise(sig) => {
                assert_eq!(sig.severity, Severity::Critical);
                assert_eq!(sig.dedup_key, "crash_pattern");
            }
            other => panic!(
                "expected Raise, got {:?}",
                matches!(other, SignalDelta::Clear { .. })
            ),
        }
    }

    #[test]
    fn no_pattern_means_no_signal() {
        let d = ProcessHealthDetector::new();
        let weak: Weak<crate::pi::session::PiSession> = Weak::new();
        let provider_state = ProviderStateSnapshot::default();
        let mut state_box: Box<dyn Any + Send + Sync> = Box::new(ProcessHealthState::default());
        let mut ctx = DetectorContext {
            session: &weak,
            state: state_box.as_mut(),
            now: std::time::Instant::now(),
            now_wall_ms: 0,
            profile: NurseProfile::Default,
            profile_config: default_profile_config(),
            provider_state: &provider_state,
            provider: None,
            model_id: None,
        };
        let out = d.observe(&PiEvent::Error("just a normal error".into()), &mut ctx);
        assert!(out.is_empty());
    }

    #[test]
    fn synthetic_session_gone_is_critical() {
        match ProcessHealthDetector::synthetic_session_gone_unobserved() {
            SignalDelta::Raise(sig) => {
                assert_eq!(sig.severity, Severity::Critical);
                assert_eq!(sig.dedup_key, "session_gone_unobserved");
            }
            _ => panic!("expected Raise"),
        }
    }
}
