//! `ContextSaturationDetector` — surface the context window approaching
//! its limit so the classifier (or Tier-2 playbook) can nudge the agent
//! to `submit_handoff` before a forced respawn.

use std::any::Any;

use chrono::Utc;
use smallvec::SmallVec;

use crate::nurse::detector::{Detector, DetectorContext, SignalDelta, TickKind};
use crate::nurse::health::{Severity, Signal};
use crate::pi::events::PiEvent;

pub struct ContextSaturationDetector;

impl ContextSaturationDetector {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ContextSaturationDetector {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Default)]
struct ContextSaturationState {
    hot_raised: bool,
    critical_raised: bool,
    last_percent: f64,
    last_context_tokens: u64,
    last_context_window: u64,
}

impl Detector for ContextSaturationDetector {
    fn name(&self) -> &'static str {
        "context_saturation"
    }

    fn description(&self) -> &'static str {
        "Warns when the Pi session's context window approaches saturation."
    }

    fn on_session_started(&self, _ctx: &DetectorContext<'_>) -> Box<dyn Any + Send + Sync> {
        Box::new(ContextSaturationState::default())
    }

    fn observe(
        &self,
        event: &PiEvent,
        ctx: &mut DetectorContext<'_>,
    ) -> SmallVec<[SignalDelta; 2]> {
        let cfg = ctx.profile_config.context_saturation.clone();
        if !cfg.enabled {
            return SmallVec::new();
        }
        let PiEvent::SessionStats(stats) = event else {
            return SmallVec::new();
        };
        let pct = stats.context_percent;
        let state = ctx
            .state
            .downcast_mut::<ContextSaturationState>()
            .expect("ContextSaturationState shape mismatch");
        state.last_percent = pct;
        state.last_context_tokens = stats.context_tokens;
        state.last_context_window = stats.context_window;
        let mut out: SmallVec<[SignalDelta; 2]> = SmallVec::new();

        // Critical (>= stalled threshold).
        if pct >= cfg.stalled_percent {
            if !state.critical_raised {
                state.critical_raised = true;
                out.push(SignalDelta::Raise(Signal {
                    detector: "context_saturation",
                    severity: Severity::Stalled,
                    dedup_key: "ctx:critical".to_string(),
                    summary: format!(
                        "context window at {:.1}% (>= {:.1}% critical)",
                        pct, cfg.stalled_percent
                    ),
                    raised_at: Utc::now(),
                    evidence: serde_json::json!({
                        "context_percent": pct,
                        "threshold": cfg.stalled_percent,
                        "context_tokens": stats.context_tokens,
                        "context_window": stats.context_window,
                    }),
                }));
            }
        } else if state.critical_raised {
            state.critical_raised = false;
            out.push(SignalDelta::Clear {
                detector: "context_saturation",
                dedup_key: "ctx:critical".to_string(),
            });
        }

        // Warn (>= warn threshold).
        if pct >= cfg.warn_percent {
            if !state.hot_raised {
                state.hot_raised = true;
                out.push(SignalDelta::Raise(Signal {
                    detector: "context_saturation",
                    severity: Severity::Warn,
                    dedup_key: "ctx:hot".to_string(),
                    summary: format!(
                        "context window at {:.1}% (>= {:.1}% warn)",
                        pct, cfg.warn_percent
                    ),
                    raised_at: Utc::now(),
                    evidence: serde_json::json!({
                        "context_percent": pct,
                        "threshold": cfg.warn_percent,
                    }),
                }));
            }
        } else if state.hot_raised {
            state.hot_raised = false;
            out.push(SignalDelta::Clear {
                detector: "context_saturation",
                dedup_key: "ctx:hot".to_string(),
            });
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
    use crate::pi::events::PiSessionStats;
    use std::sync::{OnceLock, Weak};

    fn default_profile_config() -> &'static ProfileConfig {
        static CFG: OnceLock<ProfileConfig> = OnceLock::new();
        CFG.get_or_init(|| ProfileConfig::default_for(NurseProfile::Default))
    }

    fn stats(pct: f64) -> PiSessionStats {
        PiSessionStats {
            input: 0,
            output: 0,
            reasoning_tokens: 0,
            cache_read: 0,
            cache_write: 0,
            total_tokens: 0,
            cost: 0.0,
            context_tokens: (pct * 1000.0) as u64,
            context_window: 100_000,
            context_percent: pct,
        }
    }

    #[test]
    fn warn_at_80_stalled_at_92_and_clears_below() {
        let d = ContextSaturationDetector::new();
        let weak: Weak<crate::pi::session::PiSession> = Weak::new();
        let snap = ProviderStateSnapshot::default();
        let mut state_box: Box<dyn Any + Send + Sync> = Box::new(ContextSaturationState::default());
        let mut ctx = DetectorContext {
            session: &weak,
            state: state_box.as_mut(),
            now: std::time::Instant::now(),
            now_wall_ms: 0,
            profile: NurseProfile::Default,
            profile_config: default_profile_config(),
            provider_state: &snap,
            provider: None,
            model_id: None,
        };
        // 50% — no signal.
        let out = d.observe(&PiEvent::SessionStats(stats(50.0)), &mut ctx);
        assert!(out.is_empty());
        // 85% — warn.
        let out = d.observe(&PiEvent::SessionStats(stats(85.0)), &mut ctx);
        assert!(out
            .iter()
            .any(|d| matches!(d, SignalDelta::Raise(s) if s.dedup_key == "ctx:hot")));
        // 95% — stalled added.
        let out = d.observe(&PiEvent::SessionStats(stats(95.0)), &mut ctx);
        assert!(out
            .iter()
            .any(|d| matches!(d, SignalDelta::Raise(s) if s.dedup_key == "ctx:critical")));
        // Below threshold — clears.
        let out = d.observe(&PiEvent::SessionStats(stats(40.0)), &mut ctx);
        let cleared: Vec<&str> = out
            .iter()
            .filter_map(|d| match d {
                SignalDelta::Clear { dedup_key, .. } => Some(dedup_key.as_str()),
                _ => None,
            })
            .collect();
        assert!(cleared.contains(&"ctx:hot"));
        assert!(cleared.contains(&"ctx:critical"));
    }
}
