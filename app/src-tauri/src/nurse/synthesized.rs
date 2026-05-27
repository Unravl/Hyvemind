//! `SynthesizedKind` + `describe_synthesized` + `InterventionOwner`.
//!
//! The legacy code had one mega-entry, `evaluate_error_with_llm`, used by
//! both Pi-backed error cases and non-Pi cases (reviewer dispatch, Queen
//! scheduler deadlock). In the rewrite these split into two explicit
//! entry points on `NurseEngine`:
//!
//! - [`NurseEngine::report_synthesized`](crate::nurse::engine::NurseEngine::report_synthesized)
//!   for non-Pi cases (no live session in `engine.sessions`).
//! - [`NurseEngine::report_error`](crate::nurse::engine::NurseEngine::report_error)
//!   for Pi-backed cases — injects a synthetic signal into the session's
//!   health and runs the three-tier pipeline.
//!
//! The mapping table here is the **single source of truth** for the
//! `SynthesizedKind → (NurseActionKind, observation, action)` triple. The
//! shape is preserved verbatim from the legacy `describe_synthesized` in
//! `core::nurse_service` so the frontend wire shape doesn't change.

use serde::{Deserialize, Serialize};

use crate::nurse::health::Severity;
use crate::nurse::snapshot::NurseActionKind;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SynthesizedKind {
    HandoffParseFailure {
        feature_name: String,
    },
    RpcTimeout {
        idle_secs: u64,
    },
    ProcessCrashed {
        #[serde(default)]
        exit_code: Option<i32>,
        stderr: String,
    },
    CircuitBreakerOpen {
        provider: String,
        retry_after_secs: u64,
    },
    PiError {
        message: String,
    },
    SteerFailed {
        reason: String,
    },
    ProtocolViolation {
        agent: String,
        expected_tool: String,
        attempts: u32,
    },
    /// New: a swarm scheduler deadlock detected with no Pi session in
    /// flight — handled via `report_synthesized`.
    SchedulerDeadlock {
        swarm_id: String,
        details: String,
    },
}

/// Routing context for a synthesized intervention. Flat fields mirror the
/// legacy `InterventionOwner` so payload shape is preserved.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InterventionOwner {
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub swarm_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feature_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub round: Option<u32>,
}

/// Pure resolver: `kind → (level, observation, action)`. Wire shape
/// preserved verbatim from legacy.
pub fn describe_synthesized(kind: &SynthesizedKind) -> (NurseActionKind, String, String) {
    match kind {
        SynthesizedKind::HandoffParseFailure { feature_name } => (
            NurseActionKind::Steer,
            format!(
                "worker for feature '{}' finished without a delimited handoff JSON",
                feature_name
            ),
            "marking the feature for retry".to_string(),
        ),
        SynthesizedKind::RpcTimeout { idle_secs } => (
            NurseActionKind::Cancel,
            format!("no activity for {}s", idle_secs),
            "cancelling the run".to_string(),
        ),
        SynthesizedKind::ProcessCrashed { exit_code, stderr } => {
            let code = exit_code
                .map(|c| format!(" with exit code {}", c))
                .unwrap_or_default();
            let snippet: String = stderr.chars().take(200).collect();
            (
                NurseActionKind::Restart,
                format!("pi subprocess exited{}: {}", code, snippet),
                "closing the session for respawn".to_string(),
            )
        }
        SynthesizedKind::CircuitBreakerOpen {
            provider,
            retry_after_secs,
        } => (
            NurseActionKind::LeaveIt,
            format!("provider '{}' tripped its circuit breaker", provider),
            format!("waiting {}s before retrying", retry_after_secs),
        ),
        SynthesizedKind::PiError { message } => {
            let snippet: String = message.chars().take(200).collect();
            (
                NurseActionKind::Steer,
                format!("pi reported error: {}", snippet),
                "surfacing the error while keeping the session alive".to_string(),
            )
        }
        SynthesizedKind::SteerFailed { reason } => {
            let snippet: String = reason.chars().take(200).collect();
            (
                NurseActionKind::Cancel,
                format!("steer command failed: {}", snippet),
                "cancelling the run".to_string(),
            )
        }
        SynthesizedKind::ProtocolViolation {
            agent,
            expected_tool,
            attempts,
        } => (
            NurseActionKind::Cancel,
            format!(
                "agent '{}' never called required tool '{}' after {} attempts",
                agent, expected_tool, attempts
            ),
            "cancelling the run".to_string(),
        ),
        SynthesizedKind::SchedulerDeadlock { swarm_id, details } => (
            NurseActionKind::Cancel,
            format!("swarm '{}' scheduler deadlock: {}", swarm_id, details),
            "cancelling the swarm to break the deadlock".to_string(),
        ),
    }
}

/// Severity classification, used by `report_error` to inject a synthetic
/// signal into the session's `SessionHealth` so the three-tier pipeline
/// can route it.
pub fn severity_for(kind: &SynthesizedKind) -> Severity {
    match kind {
        SynthesizedKind::ProcessCrashed { .. } => Severity::Critical,
        SynthesizedKind::SchedulerDeadlock { .. } => Severity::Critical,
        SynthesizedKind::CircuitBreakerOpen { .. } => Severity::Warn,
        SynthesizedKind::SteerFailed { .. } => Severity::Warn,
        SynthesizedKind::HandoffParseFailure { .. } => Severity::Warn,
        SynthesizedKind::RpcTimeout { .. } => Severity::Stalled,
        SynthesizedKind::PiError { .. } => Severity::Stalled,
        SynthesizedKind::ProtocolViolation { .. } => Severity::Stalled,
    }
}

/// Stable dedup key. Used as the storm-guard trigger kind AND as the
/// `Signal::dedup_key` injected by `report_error`.
pub fn dedup_key(kind: &SynthesizedKind) -> String {
    match kind {
        SynthesizedKind::HandoffParseFailure { feature_name } => {
            format!("synthesized:handoff_parse:{}", feature_name)
        }
        SynthesizedKind::RpcTimeout { .. } => "synthesized:rpc_timeout".to_string(),
        SynthesizedKind::ProcessCrashed { .. } => "synthesized:process_crashed".to_string(),
        SynthesizedKind::CircuitBreakerOpen { provider, .. } => {
            format!("synthesized:breaker:{}", provider)
        }
        SynthesizedKind::PiError { .. } => "synthesized:pi_error".to_string(),
        SynthesizedKind::SteerFailed { .. } => "synthesized:steer_failed".to_string(),
        SynthesizedKind::ProtocolViolation {
            agent,
            expected_tool,
            ..
        } => {
            format!("synthesized:protocol_violation:{}:{}", agent, expected_tool)
        }
        SynthesizedKind::SchedulerDeadlock { swarm_id, .. } => {
            format!("synthesized:scheduler_deadlock:{}", swarm_id)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describe_synthesized_covers_every_variant() {
        let cases = vec![
            SynthesizedKind::HandoffParseFailure {
                feature_name: "f".into(),
            },
            SynthesizedKind::RpcTimeout { idle_secs: 90 },
            SynthesizedKind::ProcessCrashed {
                exit_code: Some(1),
                stderr: "boom".into(),
            },
            SynthesizedKind::CircuitBreakerOpen {
                provider: "anthropic".into(),
                retry_after_secs: 30,
            },
            SynthesizedKind::PiError {
                message: "oops".into(),
            },
            SynthesizedKind::SteerFailed {
                reason: "closed pipe".into(),
            },
            SynthesizedKind::ProtocolViolation {
                agent: "scout".into(),
                expected_tool: "submit_scout_result".into(),
                attempts: 3,
            },
            SynthesizedKind::SchedulerDeadlock {
                swarm_id: "s".into(),
                details: "cycle".into(),
            },
        ];
        for k in cases {
            let (level, obs, act) = describe_synthesized(&k);
            assert!(!obs.is_empty());
            assert!(!act.is_empty());
            let _ = level;
        }
    }

    #[test]
    fn severity_routes_crashes_to_critical() {
        assert_eq!(
            severity_for(&SynthesizedKind::ProcessCrashed {
                exit_code: None,
                stderr: "".into()
            }),
            Severity::Critical
        );
        assert_eq!(
            severity_for(&SynthesizedKind::CircuitBreakerOpen {
                provider: "p".into(),
                retry_after_secs: 1
            }),
            Severity::Warn
        );
        assert_eq!(
            severity_for(&SynthesizedKind::RpcTimeout { idle_secs: 1 }),
            Severity::Stalled
        );
    }
}
