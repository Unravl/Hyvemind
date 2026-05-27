//! `ProviderHealthDetector` — provider-missing + breaker-state classifier.
//!
//! Two checks:
//! - **Provider missing**: the session's resolved provider is not present
//!   in the snapshot taken under `ProviderRegistry`'s read lock. Catches
//!   the audit's `provider 'crof' not found in registry` failure mode.
//!   Raises `Critical` with `dedup_key = "provider:missing:{name}"` —
//!   Tier-1 Cancel via templated UserNotice to Settings.
//! - **Breaker open**: the provider IS registered but its circuit breaker
//!   is `Open`. Raises `Warn` with `dedup_key = "breaker:{provider}"` so
//!   the Tier-2 playbook can canned-LeaveIt with the upstream's
//!   `retry_after`.
//!
//! Snapshot is built once per engine tick by the engine (under the
//! provider registry's async read lock) and threaded into the
//! `DetectorContext` so this detector can do sync lookups inside the
//! sync `Detector::tick` trait.

use std::any::Any;

use chrono::Utc;
use smallvec::SmallVec;

use crate::nurse::detector::{Detector, DetectorContext, SignalDelta, TickKind};
use crate::nurse::health::{Severity, Signal};
use crate::nurse::snapshot::ProviderBreakerStateDto;

pub struct ProviderHealthDetector;

impl ProviderHealthDetector {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ProviderHealthDetector {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Default)]
struct ProviderHealthState {
    missing_raised: bool,
    breaker_raised: bool,
    last_provider: Option<String>,
}

impl Detector for ProviderHealthDetector {
    fn name(&self) -> &'static str {
        "provider_health"
    }

    fn description(&self) -> &'static str {
        "Flags Pi sessions whose provider is missing from the registry or whose circuit breaker is open."
    }

    fn on_session_started(&self, _ctx: &DetectorContext<'_>) -> Box<dyn Any + Send + Sync> {
        Box::new(ProviderHealthState::default())
    }

    fn tick(&self, ctx: &mut DetectorContext<'_>) -> SmallVec<[SignalDelta; 2]> {
        let provider = match ctx.provider {
            Some(p) if !p.is_empty() => p,
            _ => return SmallVec::new(),
        };
        let state = ctx
            .state
            .downcast_mut::<ProviderHealthState>()
            .expect("ProviderHealthState shape mismatch");
        state.last_provider = Some(provider.to_string());

        let mut out: SmallVec<[SignalDelta; 2]> = SmallVec::new();

        let Some((breaker_state, retry_after_secs)) = ctx.provider_state.0.get(provider) else {
            // Provider missing.
            if !state.missing_raised {
                state.missing_raised = true;
                out.push(SignalDelta::Raise(Signal {
                    detector: "provider_health",
                    severity: Severity::Critical,
                    dedup_key: format!("provider:missing:{}", provider),
                    summary: format!("provider '{}' is not registered", provider),
                    raised_at: Utc::now(),
                    evidence: serde_json::json!({
                        "provider": provider,
                        "error": "not registered",
                        "configured_providers": ctx.provider_state.0.keys().collect::<Vec<_>>(),
                    }),
                }));
            }
            // Don't auto-clear breaker — separate concern.
            return out;
        };

        // Provider exists — clear missing if it was raised.
        if state.missing_raised {
            state.missing_raised = false;
            out.push(SignalDelta::Clear {
                detector: "provider_health",
                dedup_key: format!("provider:missing:{}", provider),
            });
        }

        match breaker_state {
            ProviderBreakerStateDto::Open => {
                if !state.breaker_raised {
                    state.breaker_raised = true;
                    out.push(SignalDelta::Raise(Signal {
                        detector: "provider_health",
                        severity: Severity::Warn,
                        dedup_key: format!("breaker:{}", provider),
                        summary: format!("provider '{}' circuit breaker is open", provider),
                        raised_at: Utc::now(),
                        evidence: serde_json::json!({
                            "provider": provider,
                            "breaker_state": "open",
                            "retry_after_secs": retry_after_secs,
                        }),
                    }));
                }
            }
            ProviderBreakerStateDto::Closed | ProviderBreakerStateDto::HalfOpen => {
                if state.breaker_raised {
                    state.breaker_raised = false;
                    out.push(SignalDelta::Clear {
                        detector: "provider_health",
                        dedup_key: format!("breaker:{}", provider),
                    });
                }
            }
        }
        out
    }

    fn tick_kind(&self) -> TickKind {
        TickKind::Slow
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nurse::config::{NurseProfile, ProfileConfig};
    use crate::nurse::snapshot::ProviderStateSnapshot;
    use std::collections::HashMap;
    use std::sync::{OnceLock, Weak};

    fn default_profile_config() -> &'static ProfileConfig {
        static CFG: OnceLock<ProfileConfig> = OnceLock::new();
        CFG.get_or_init(|| ProfileConfig::default_for(NurseProfile::Default))
    }

    fn ctx_with<'a>(
        weak: &'a Weak<crate::pi::session::PiSession>,
        state: &'a mut ProviderHealthState,
        provider: Option<&'a str>,
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
            provider,
            model_id: None,
        }
    }

    #[test]
    fn missing_provider_raises_critical() {
        let d = ProviderHealthDetector::new();
        let weak: Weak<crate::pi::session::PiSession> = Weak::new();
        let mut state = ProviderHealthState::default();
        let snap = ProviderStateSnapshot::default();
        let mut ctx = ctx_with(&weak, &mut state, Some("crof"), &snap);
        let out = d.tick(&mut ctx);
        assert_eq!(out.len(), 1);
        match &out[0] {
            SignalDelta::Raise(s) => {
                assert_eq!(s.severity, Severity::Critical);
                assert_eq!(s.dedup_key, "provider:missing:crof");
            }
            _ => panic!("expected raise"),
        }
    }

    #[test]
    fn breaker_open_raises_warn_then_clears_on_close() {
        let d = ProviderHealthDetector::new();
        let weak: Weak<crate::pi::session::PiSession> = Weak::new();
        let mut state = ProviderHealthState::default();
        let mut map = HashMap::new();
        map.insert(
            "anthropic".to_string(),
            (ProviderBreakerStateDto::Open, Some(45u64)),
        );
        let snap = ProviderStateSnapshot(map);
        let mut ctx = ctx_with(&weak, &mut state, Some("anthropic"), &snap);
        let out = d.tick(&mut ctx);
        assert!(out
            .iter()
            .any(|d| matches!(d, SignalDelta::Raise(s) if s.dedup_key == "breaker:anthropic")));
        // Transition to Closed.
        let mut map = HashMap::new();
        map.insert(
            "anthropic".to_string(),
            (ProviderBreakerStateDto::Closed, None),
        );
        let snap = ProviderStateSnapshot(map);
        let mut ctx = ctx_with(&weak, &mut state, Some("anthropic"), &snap);
        let out = d.tick(&mut ctx);
        assert!(out
            .iter()
            .any(|d| matches!(d, SignalDelta::Clear { dedup_key, .. } if dedup_key == "breaker:anthropic")));
    }
}
