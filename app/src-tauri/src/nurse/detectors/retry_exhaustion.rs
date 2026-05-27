//! `RetryExhaustionDetector` — flag when Pi's `AutoRetryEnd { success: false }`
//! events accumulate.
//!
//! Tracks distinct `attempt` numbers in a sliding window; each unique
//! failed attempt is a `Warn`, three or more distinct is `Stalled`.
//! A separate counter raises a `Critical` `retry:death_loop` signal when
//! either consecutive `AutoRetryEnd { success: false }` events OR Pi's
//! reported `attempt` number reaches the configured threshold — this
//! catches the "model is in a death loop but Pi is fine" case where Pi
//! consolidates its internal retries into one event with a high `attempt`.
//! Any subsequent `TextDelta` or successful retry clears everything.

use std::any::Any;
use std::collections::{HashSet, VecDeque};
use std::time::Instant;

use chrono::Utc;
use smallvec::SmallVec;

use crate::nurse::detector::{Detector, DetectorContext, SignalDelta, TickKind};
use crate::nurse::health::{Severity, Signal};
use crate::pi::events::PiEvent;

pub struct RetryExhaustionDetector;

impl RetryExhaustionDetector {
    pub fn new() -> Self {
        Self
    }
}

impl Default for RetryExhaustionDetector {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Default)]
struct RetryState {
    window: VecDeque<(Instant, u32)>,
    raised_attempts: HashSet<u32>,
    exhausted_raised: bool,
    /// Count of consecutive `AutoRetryEnd { success: false }` events with
    /// no successful response between them. Reset by `TextDelta` or by
    /// `AutoRetryEnd { success: true }`. Drives the `retry:death_loop`
    /// `Critical` signal in conjunction with Pi's reported `attempt`.
    consecutive_failures: u32,
    /// One-shot flag so the `Critical` signal is raised exactly once per
    /// death-loop episode. Cleared together with `consecutive_failures`.
    death_loop_raised: bool,
}

impl Detector for RetryExhaustionDetector {
    fn name(&self) -> &'static str {
        "retry_exhaustion"
    }

    fn description(&self) -> &'static str {
        "Tracks failed AutoRetry attempts; flags when distinct attempts pile up in a window."
    }

    fn on_session_started(&self, _ctx: &DetectorContext<'_>) -> Box<dyn Any + Send + Sync> {
        Box::new(RetryState::default())
    }

    fn observe(
        &self,
        event: &PiEvent,
        ctx: &mut DetectorContext<'_>,
    ) -> SmallVec<[SignalDelta; 2]> {
        let cfg = ctx.profile_config.retry_exhaustion.clone();
        if !cfg.enabled {
            return SmallVec::new();
        }
        let state = ctx
            .state
            .downcast_mut::<RetryState>()
            .expect("RetryState shape mismatch");

        let mut out: SmallVec<[SignalDelta; 2]> = SmallVec::new();

        match event {
            PiEvent::AutoRetryEnd {
                success: false,
                attempt,
            } => {
                // Trim window.
                let now = ctx.now;
                let cutoff = std::time::Duration::from_secs(cfg.window_secs);
                while let Some(&(t, _)) = state.window.front() {
                    if now.saturating_duration_since(t) >= cutoff {
                        state.window.pop_front();
                    } else {
                        break;
                    }
                }
                state.window.push_back((now, *attempt));
                state.consecutive_failures = state.consecutive_failures.saturating_add(1);
                let distinct: HashSet<u32> = state.window.iter().map(|(_, a)| *a).collect();
                let key = format!("retry:{}", attempt);
                if !state.raised_attempts.contains(attempt) {
                    state.raised_attempts.insert(*attempt);
                    out.push(SignalDelta::Raise(Signal {
                        detector: "retry_exhaustion",
                        severity: Severity::Warn,
                        dedup_key: key,
                        summary: format!(
                            "auto-retry attempt {} failed (distinct attempts in window: {})",
                            attempt,
                            distinct.len()
                        ),
                        raised_at: Utc::now(),
                        evidence: serde_json::json!({
                            "attempt": attempt,
                            "attempts_in_window": distinct.iter().collect::<Vec<_>>(),
                            "window_secs": cfg.window_secs,
                        }),
                    }));
                }
                if distinct.len() as u32 >= cfg.distinct_attempts_for_stalled
                    && !state.exhausted_raised
                {
                    state.exhausted_raised = true;
                    out.push(SignalDelta::Raise(Signal {
                        detector: "retry_exhaustion",
                        severity: Severity::Stalled,
                        dedup_key: "retry:exhausted".to_string(),
                        summary: format!(
                            "{} distinct failed retry attempts in {}s window",
                            distinct.len(),
                            cfg.window_secs
                        ),
                        raised_at: Utc::now(),
                        evidence: serde_json::json!({
                            "attempts_in_window": distinct.iter().collect::<Vec<_>>(),
                            "window_secs": cfg.window_secs,
                            "threshold": cfg.distinct_attempts_for_stalled,
                        }),
                    }));
                }
                // Death-loop escalation. Use the larger of the two signals
                // we have (Pi's reported attempt vs our consecutive
                // counter). Pi sometimes batches its internal retries
                // into a single event with a high `attempt`; sometimes it
                // emits a sequence of single-attempt events. Either path
                // means the model is in a death loop with no path forward.
                let effective = state.consecutive_failures.max(*attempt);
                if effective >= cfg.consecutive_failures_for_critical && !state.death_loop_raised {
                    state.death_loop_raised = true;
                    out.push(SignalDelta::Raise(Signal {
                        detector: "retry_exhaustion",
                        severity: Severity::Critical,
                        dedup_key: "retry:death_loop".to_string(),
                        summary: format!(
                            "model is in an auto-retry death loop \
                             (attempt={}, consecutive_failures={}, threshold={})",
                            attempt,
                            state.consecutive_failures,
                            cfg.consecutive_failures_for_critical
                        ),
                        raised_at: Utc::now(),
                        evidence: serde_json::json!({
                            "attempt": attempt,
                            "consecutive_failures": state.consecutive_failures,
                            "threshold": cfg.consecutive_failures_for_critical,
                            "attempts_in_window": distinct.iter().collect::<Vec<_>>(),
                            "window_secs": cfg.window_secs,
                        }),
                    }));
                }
            }
            PiEvent::AutoRetryEnd { success: true, .. } => {
                // Successful retry resets the death-loop counter.
                state.consecutive_failures = 0;
                if state.death_loop_raised {
                    state.death_loop_raised = false;
                    out.push(SignalDelta::Clear {
                        detector: "retry_exhaustion",
                        dedup_key: "retry:death_loop".to_string(),
                    });
                }
            }
            PiEvent::TextDelta(_) => {
                // Clear everything on any real text.
                for a in std::mem::take(&mut state.raised_attempts) {
                    out.push(SignalDelta::Clear {
                        detector: "retry_exhaustion",
                        dedup_key: format!("retry:{}", a),
                    });
                }
                if state.exhausted_raised {
                    state.exhausted_raised = false;
                    out.push(SignalDelta::Clear {
                        detector: "retry_exhaustion",
                        dedup_key: "retry:exhausted".to_string(),
                    });
                }
                if state.death_loop_raised {
                    state.death_loop_raised = false;
                    out.push(SignalDelta::Clear {
                        detector: "retry_exhaustion",
                        dedup_key: "retry:death_loop".to_string(),
                    });
                }
                state.consecutive_failures = 0;
                state.window.clear();
            }
            _ => {}
        }
        out
    }

    fn tick_kind(&self) -> TickKind {
        TickKind::Fast
    }
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

    fn ctx_fixture<'a>(
        weak: &'a Weak<crate::pi::session::PiSession>,
        state: &'a mut RetryState,
        provider_state: &'a ProviderStateSnapshot,
    ) -> DetectorContext<'a> {
        DetectorContext {
            session: weak,
            state,
            now: std::time::Instant::now(),
            now_wall_ms: 0,
            profile: NurseProfile::Default,
            profile_config: default_profile_config(),
            provider_state,
            provider: None,
            model_id: None,
        }
    }

    #[test]
    fn three_distinct_attempts_fire_stalled_then_text_clears_all() {
        let d = RetryExhaustionDetector::new();
        let weak: Weak<crate::pi::session::PiSession> = Weak::new();
        let snap = ProviderStateSnapshot::default();
        let mut state = RetryState::default();
        let mut ctx = ctx_fixture(&weak, &mut state, &snap);
        let out = d.observe(
            &PiEvent::AutoRetryEnd {
                success: false,
                attempt: 1,
            },
            &mut ctx,
        );
        assert!(out
            .iter()
            .any(|d| matches!(d, SignalDelta::Raise(s) if s.dedup_key == "retry:1")));
        let _ = d.observe(
            &PiEvent::AutoRetryEnd {
                success: false,
                attempt: 2,
            },
            &mut ctx,
        );
        let out = d.observe(
            &PiEvent::AutoRetryEnd {
                success: false,
                attempt: 3,
            },
            &mut ctx,
        );
        assert!(out
            .iter()
            .any(|d| matches!(d, SignalDelta::Raise(s) if s.dedup_key == "retry:exhausted")));

        // Now a TextDelta clears everything.
        let cleared = d.observe(&PiEvent::TextDelta("ok".into()), &mut ctx);
        assert!(cleared.iter().any(
            |d| matches!(d, SignalDelta::Clear { dedup_key, .. } if dedup_key == "retry:exhausted")
        ));
        assert!(cleared
            .iter()
            .filter(|d| matches!(d, SignalDelta::Clear { dedup_key, .. } if dedup_key.starts_with("retry:") && dedup_key != "retry:exhausted"))
            .count()
            >= 3);
    }

    /// The user's session `399ddf42…` had Pi emit ONE
    /// `AutoRetryEnd { success: false, attempt: 3 }` event after
    /// internally batching its retry budget. The old detector raised
    /// only Warn (`retry:3`) and Stalled required 3 *distinct* attempts.
    /// Confirm that a single high-`attempt` event now also raises the
    /// `Critical` `retry:death_loop` signal.
    #[test]
    fn single_high_attempt_event_fires_critical_death_loop() {
        let d = RetryExhaustionDetector::new();
        let weak: Weak<crate::pi::session::PiSession> = Weak::new();
        let snap = ProviderStateSnapshot::default();
        let mut state = RetryState::default();
        let mut ctx = ctx_fixture(&weak, &mut state, &snap);
        let out = d.observe(
            &PiEvent::AutoRetryEnd {
                success: false,
                attempt: 3,
            },
            &mut ctx,
        );
        let death_loop = out.iter().find_map(|d| match d {
            SignalDelta::Raise(s) if s.dedup_key == "retry:death_loop" => Some(s),
            _ => None,
        });
        let raised = death_loop.expect("retry:death_loop should fire on attempt=3");
        assert_eq!(raised.severity, Severity::Critical);
        assert_eq!(raised.evidence["attempt"], 3);
        assert_eq!(raised.evidence["threshold"], 3);
    }

    /// Three consecutive `AutoRetryEnd { success: false, attempt: 1 }`
    /// events (Pi emitting per-attempt rather than batched) must also
    /// fire the death-loop Critical.
    #[test]
    fn three_consecutive_single_attempt_events_fire_critical_death_loop() {
        let d = RetryExhaustionDetector::new();
        let weak: Weak<crate::pi::session::PiSession> = Weak::new();
        let snap = ProviderStateSnapshot::default();
        let mut state = RetryState::default();
        let mut ctx = ctx_fixture(&weak, &mut state, &snap);
        let evt = PiEvent::AutoRetryEnd {
            success: false,
            attempt: 1,
        };
        let _ = d.observe(&evt, &mut ctx);
        let _ = d.observe(&evt, &mut ctx);
        let out = d.observe(&evt, &mut ctx);
        let death_loop = out.iter().find_map(|d| match d {
            SignalDelta::Raise(s) if s.dedup_key == "retry:death_loop" => Some(s),
            _ => None,
        });
        let raised = death_loop.expect("retry:death_loop should fire after 3 consecutive failures");
        assert_eq!(raised.severity, Severity::Critical);
        assert_eq!(raised.evidence["consecutive_failures"], 3);
    }

    /// A successful retry between two failures resets the counter, so
    /// the death-loop Critical does not fire on a single subsequent
    /// failure that would otherwise hit the threshold.
    #[test]
    fn successful_retry_resets_consecutive_counter() {
        let d = RetryExhaustionDetector::new();
        let weak: Weak<crate::pi::session::PiSession> = Weak::new();
        let snap = ProviderStateSnapshot::default();
        let mut state = RetryState::default();
        let mut ctx = ctx_fixture(&weak, &mut state, &snap);
        let fail = PiEvent::AutoRetryEnd {
            success: false,
            attempt: 1,
        };
        let ok = PiEvent::AutoRetryEnd {
            success: true,
            attempt: 1,
        };
        let _ = d.observe(&fail, &mut ctx);
        let _ = d.observe(&fail, &mut ctx);
        let _ = d.observe(&ok, &mut ctx);
        let out = d.observe(&fail, &mut ctx);
        assert!(
            !out.iter()
                .any(|d| matches!(d, SignalDelta::Raise(s) if s.dedup_key == "retry:death_loop")),
            "death-loop should not fire after counter reset"
        );
    }

    /// `TextDelta` clears `retry:death_loop` along with the rest.
    #[test]
    fn text_delta_clears_death_loop_signal() {
        let d = RetryExhaustionDetector::new();
        let weak: Weak<crate::pi::session::PiSession> = Weak::new();
        let snap = ProviderStateSnapshot::default();
        let mut state = RetryState::default();
        let mut ctx = ctx_fixture(&weak, &mut state, &snap);
        let _ = d.observe(
            &PiEvent::AutoRetryEnd {
                success: false,
                attempt: 3,
            },
            &mut ctx,
        );
        let cleared = d.observe(&PiEvent::TextDelta("recovered".into()), &mut ctx);
        assert!(
            cleared.iter().any(
                |d| matches!(d, SignalDelta::Clear { dedup_key, .. } if dedup_key == "retry:death_loop")
            ),
            "TextDelta should clear retry:death_loop"
        );
    }
}
