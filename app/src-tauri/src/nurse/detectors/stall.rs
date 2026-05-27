//! `StallDetector` — time-based + post-prompt-silence stall classifier.
//!
//! Two paths:
//! - **Time-based**: `now_wall_ms - last_text_event_ms` exceeds threshold
//!   AND `awaiting_model_for_ms < AWAITING_MODEL_HARD_LIMIT_MS`.
//! - **Post-prompt silence**: `now - last_prompt_sent_at > 60s` AND
//!   `last_event_at < last_prompt_sent_at` (zero events since prompt).
//!
//! Both paths Warn at ~0.6× the configured threshold and Stalled at 1.0×.
//! The post-prompt-silence fast-path catches the "Pi received prompt then
//! 7 minutes of zero events" failure the audit surfaced.

use std::any::Any;

use chrono::Utc;
use smallvec::SmallVec;

use crate::nurse::config::StallDetectorConfig;
use crate::nurse::detector::{Detector, DetectorContext, SignalDelta, TickKind};
use crate::nurse::health::{Severity, Signal};
use crate::pi::events::PiEvent;

pub struct StallDetector;

impl StallDetector {
    pub fn new() -> Self {
        Self
    }
}

impl Default for StallDetector {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Default)]
struct StallState {
    /// Set while `stall` is currently raised so the engine can compute
    /// raise/clear edges without re-scanning `SessionHealth`.
    raised: bool,
    post_prompt_raised: bool,
}

impl Detector for StallDetector {
    fn name(&self) -> &'static str {
        "stall"
    }

    fn description(&self) -> &'static str {
        "Detects sessions that have stopped producing text/thinking events for too long."
    }

    fn on_session_started(&self, _ctx: &DetectorContext<'_>) -> Box<dyn Any + Send + Sync> {
        Box::new(StallState::default())
    }

    fn tick(&self, ctx: &mut DetectorContext<'_>) -> SmallVec<[SignalDelta; 2]> {
        let cfg = config_for(ctx);
        if !cfg.enabled {
            return SmallVec::new();
        }
        let session = match ctx.session.upgrade() {
            Some(s) => s,
            None => return SmallVec::new(),
        };

        let state = ctx
            .state
            .downcast_mut::<StallState>()
            .expect("StallState shape mismatch");

        let mut out: SmallVec<[SignalDelta; 2]> = SmallVec::new();

        // --- Time-based path -------------------------------------------------
        let last_text_ms = session.last_text_event_ms();
        let last_event_ms = session.last_activity_ms();
        let awaiting = session.awaiting_model_for_ms().unwrap_or(0);
        let idle_ms = ctx.now_wall_ms.saturating_sub(last_text_ms);
        let stalled_threshold = cfg.stalled_secs.saturating_mul(1000);
        let warn_threshold = ((cfg.stalled_secs as f64) * 0.6) as u64 * 1000;
        let hard_limit = cfg.awaiting_model_hard_limit_secs.saturating_mul(1000);

        if last_text_ms != 0 && idle_ms >= warn_threshold && awaiting < hard_limit {
            let severity = if idle_ms >= stalled_threshold {
                Severity::Stalled
            } else {
                Severity::Warn
            };
            state.raised = true;
            out.push(SignalDelta::Raise(Signal {
                detector: "stall",
                severity,
                dedup_key: "stall".to_string(),
                summary: format!(
                    "no text/thinking event for {}ms (threshold {}ms)",
                    idle_ms, stalled_threshold
                ),
                raised_at: Utc::now(),
                evidence: serde_json::json!({
                    "path": "time_based",
                    "idle_ms": idle_ms,
                    "threshold_ms": stalled_threshold,
                    "last_text_event_at_ms": last_text_ms,
                    "last_event_at_ms": last_event_ms,
                    "awaiting_model_for_ms": awaiting,
                }),
            }));
        } else if state.raised && idle_ms < warn_threshold {
            // Cleared.
            state.raised = false;
            out.push(SignalDelta::Clear {
                detector: "stall",
                dedup_key: "stall".to_string(),
            });
        }

        // --- Post-prompt silence fast-path -----------------------------------
        let last_prompt = session.last_prompt_sent_ms();
        let post_warn = cfg.post_prompt_warn_secs.saturating_mul(1000);
        let post_stalled = cfg.post_prompt_stalled_secs.saturating_mul(1000);
        if last_prompt > 0 {
            let prompt_age = ctx.now_wall_ms.saturating_sub(last_prompt);
            // Zero events since the prompt: last_event_at < last_prompt_sent_at.
            let no_events_since = last_event_ms < last_prompt;
            if no_events_since && prompt_age >= post_warn {
                let severity = if prompt_age >= post_stalled {
                    Severity::Stalled
                } else {
                    Severity::Warn
                };
                state.post_prompt_raised = true;
                out.push(SignalDelta::Raise(Signal {
                    detector: "stall",
                    severity,
                    dedup_key: "stall:post_prompt_silence".to_string(),
                    summary: format!("no events since prompt sent {}ms ago", prompt_age),
                    raised_at: Utc::now(),
                    evidence: serde_json::json!({
                        "path": "post_prompt_silence",
                        "prompt_age_ms": prompt_age,
                        "last_prompt_sent_at_ms": last_prompt,
                        "last_event_at_ms": last_event_ms,
                    }),
                }));
            } else if state.post_prompt_raised && !no_events_since {
                state.post_prompt_raised = false;
                out.push(SignalDelta::Clear {
                    detector: "stall",
                    dedup_key: "stall:post_prompt_silence".to_string(),
                });
            }
        }

        out
    }

    fn observe(
        &self,
        event: &PiEvent,
        ctx: &mut DetectorContext<'_>,
    ) -> SmallVec<[SignalDelta; 2]> {
        // Any text/thinking event clears the stall.
        let state = ctx
            .state
            .downcast_mut::<StallState>()
            .expect("StallState shape mismatch");
        let mut out: SmallVec<[SignalDelta; 2]> = SmallVec::new();
        if matches!(event, PiEvent::TextDelta(_) | PiEvent::ThinkingDelta(_)) {
            if state.raised {
                state.raised = false;
                out.push(SignalDelta::Clear {
                    detector: "stall",
                    dedup_key: "stall".to_string(),
                });
            }
            if state.post_prompt_raised {
                state.post_prompt_raised = false;
                out.push(SignalDelta::Clear {
                    detector: "stall",
                    dedup_key: "stall:post_prompt_silence".to_string(),
                });
            }
        }
        out
    }

    fn tick_kind(&self) -> TickKind {
        TickKind::Fast
    }

    fn config_schema(&self) -> Vec<crate::nurse::snapshot::TunableDef> {
        use crate::nurse::snapshot::{TunableDef, TunableDirection, TunableKind};
        vec![
            TunableDef {
                name: "stalled_secs".to_string(),
                kind: TunableKind::Stepper,
                unit: "seconds".to_string(),
                direction: TunableDirection::HigherLessSensitive,
                default: serde_json::json!(180),
                safe_range: serde_json::json!({"min": 60, "max": 3600}),
                description:
                    "Idle time before the session is classified as Stalled. Higher = less sensitive."
                        .to_string(),
            },
            TunableDef {
                name: "post_prompt_stalled_secs".to_string(),
                kind: TunableKind::Stepper,
                unit: "seconds".to_string(),
                direction: TunableDirection::HigherLessSensitive,
                default: serde_json::json!(180),
                safe_range: serde_json::json!({"min": 30, "max": 1800}),
                description:
                    "If no events arrive within this many seconds of sending a prompt, the post-prompt-silence fast-path fires."
                        .to_string(),
            },
            TunableDef {
                name: "enabled".to_string(),
                kind: TunableKind::Toggle,
                unit: "".to_string(),
                direction: TunableDirection::Neutral,
                default: serde_json::json!(true),
                safe_range: serde_json::json!({}),
                description: "Master enable for the stall detector on this profile.".to_string(),
            },
        ]
    }
}

fn config_for(ctx: &DetectorContext<'_>) -> StallDetectorConfig {
    ctx.profile_config.stall.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nurse::config::{NurseProfile, ProfileConfig};
    use crate::nurse::snapshot::{ProviderStateSnapshot, TunableKind};
    use std::sync::{OnceLock, Weak};

    fn default_profile_config() -> &'static ProfileConfig {
        static CFG: OnceLock<ProfileConfig> = OnceLock::new();
        CFG.get_or_init(|| ProfileConfig::default_for(NurseProfile::Default))
    }

    #[test]
    fn config_schema_has_units_and_direction() {
        let d = StallDetector::new();
        let schema = d.config_schema();
        assert!(!schema.is_empty());
        for entry in &schema {
            // Toggle entries don't need a unit, but every numeric one does.
            if matches!(entry.kind, TunableKind::Stepper | TunableKind::NumericRange) {
                assert!(!entry.unit.is_empty(), "missing unit for {}", entry.name);
            }
            assert!(
                !entry.description.is_empty(),
                "missing description for {}",
                entry.name
            );
        }
    }

    #[test]
    fn observe_textdelta_emits_clears_when_stall_was_raised() {
        let d = StallDetector::new();
        let weak: Weak<crate::pi::session::PiSession> = Weak::new();
        let provider_state = ProviderStateSnapshot::default();
        let mut state_box: Box<dyn Any + Send + Sync> = Box::new(StallState {
            raised: true,
            post_prompt_raised: true,
        });
        let mut ctx = DetectorContext {
            session: &weak,
            state: state_box.as_mut(),
            now: std::time::Instant::now(),
            now_wall_ms: 1000,
            profile: NurseProfile::Default,
            profile_config: default_profile_config(),
            provider_state: &provider_state,
            provider: None,
            model_id: None,
        };
        let out = d.observe(&PiEvent::TextDelta("hi".into()), &mut ctx);
        // Two clears: stall + post_prompt_silence.
        assert_eq!(out.len(), 2);
        for delta in &out {
            assert!(matches!(delta, SignalDelta::Clear { .. }));
        }
    }
}
