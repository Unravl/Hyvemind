//! DTOs and snapshot types for the Nurse subsystem IPC surface.
//!
//! Wire shape for every Tauri event and IPC handler is preserved
//! bit-identical to the legacy `core::nurse_service` surface — the
//! atomic cutover in step 5 of the rewrite swaps the producer without
//! touching the renderer's `nurseTypes.ts` mirror.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::pi::session::SessionOwner;

/// Tunable schema entry rendered as a single control on the Profiles tab
/// of the Nurse screen. Every detector that exposes user-tunable knobs
/// returns a `Vec<TunableDef>` from `Detector::config_schema()`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunableDef {
    pub name: String,
    pub kind: TunableKind,
    /// Unit copy shown alongside the control (e.g. `"seconds"`, `"%"`,
    /// `"events"`). Always populated — bare numeric inputs without units
    /// are an explicit regression target in the frontend tests.
    pub unit: String,
    /// Direction copy ("higher = more / less sensitive") shown next to the
    /// control so the operator immediately knows which way to nudge the
    /// value when investigating false positives / negatives.
    pub direction: TunableDirection,
    pub default: serde_json::Value,
    /// Shape depends on `kind`:
    /// - `NumericRange` / `Stepper`: `{ "min": x, "max": y }`
    /// - `Enum`: `{ "variants": ["a", "b"] }`
    /// - `Toggle` / `Text`: `{}`
    pub safe_range: serde_json::Value,
    /// Markdown description rendered under the label.
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TunableKind {
    NumericRange,
    Stepper,
    Enum,
    Toggle,
    Text,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TunableDirection {
    HigherMoreSensitive,
    HigherLessSensitive,
    Neutral,
}

/// Stable serde DTO over [`crate::pi::session::SessionOwner`]. The source
/// enum doesn't derive serde and this rewrite avoids touching it, so we
/// project into this DTO at the IPC boundary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionOwnerDto {
    Task {
        task_id: String,
    },
    Review {
        job_id: String,
    },
    Merge {
        job_id: String,
        round: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        swarm_id: Option<String>,
    },
    Swarm {
        swarm_id: String,
        role: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        feature_id: Option<String>,
    },
    Unknown,
}

impl From<&SessionOwner> for SessionOwnerDto {
    fn from(owner: &SessionOwner) -> Self {
        match owner {
            SessionOwner::Task { task_id } => Self::Task {
                task_id: task_id.clone(),
            },
            SessionOwner::Review { job_id } => Self::Review {
                job_id: job_id.clone(),
            },
            SessionOwner::Merge {
                job_id,
                round,
                swarm_id,
            } => Self::Merge {
                job_id: job_id.clone(),
                round: *round,
                swarm_id: swarm_id.clone(),
            },
            SessionOwner::Swarm { swarm_id, role } => Self::Swarm {
                swarm_id: swarm_id.clone(),
                role: role.clone(),
                feature_id: None,
            },
            SessionOwner::Unknown => Self::Unknown,
        }
    }
}

impl SessionOwnerDto {
    /// Stable identifier extracted from the owner for in-flight-guard
    /// keying. `None` when the owner has no specific session id (e.g.
    /// reviewer dispatch with attribution-only `hm-session-*`).
    pub fn session_key(&self) -> Option<String> {
        match self {
            Self::Task { task_id } => Some(task_id.clone()),
            Self::Review { job_id } => Some(job_id.clone()),
            Self::Merge { job_id, round, .. } => Some(format!("{}-r{}", job_id, round)),
            Self::Swarm { swarm_id, role, .. } => Some(format!("{}:{}", swarm_id, role)),
            Self::Unknown => None,
        }
    }

    /// Return the serde tag string for each variant. Used by analytics
    /// (decision-log rows) so downstream consumers can filter by owner
    /// kind without round-tripping through serde.
    pub fn kind_str(&self) -> &'static str {
        match self {
            Self::Task { .. } => "task",
            Self::Review { .. } => "review",
            Self::Merge { .. } => "merge",
            Self::Swarm { .. } => "swarm",
            Self::Unknown => "unknown",
        }
    }
}

/// Snapshot of `(provider_id → (BreakerState, RetryAfter))` taken under
/// `ProviderRegistry`'s async read lock by the engine BEFORE dispatching
/// detectors. Lets `ProviderHealthDetector::tick` do sync lookups inside
/// the sync `Detector::tick` trait without touching `PiSession`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProviderStateSnapshot(pub HashMap<String, (ProviderBreakerStateDto, Option<u64>)>);

/// Wire-stable mirror of `hivemind::circuit_breaker::BreakerState`.
/// Decoupled from the source enum so the Nurse IPC doesn't pull
/// `hivemind` types into its serde surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderBreakerStateDto {
    Closed,
    Open,
    HalfOpen,
}

// ---------------------------------------------------------------------------
// Legacy wire-compatible decision / event types (bit-identical to the
// `core::nurse_service` shapes — preserved so the cutover does not break
// the frontend's `nurseTypes.ts` mirror).
// ---------------------------------------------------------------------------

/// Action classification surfaced on every intervention. Bit-identical to
/// the legacy `core::nurse_service::NurseActionKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NurseActionKind {
    LeaveIt,
    Steer,
    Restart,
    Cancel,
}

/// Default `check_back_secs` when the model omits the field on a LeaveIt.
fn default_check_back_secs() -> u64 {
    60
}

/// Decision returned by the Nurse classifier. Wire-identical to the legacy
/// `core::nurse_service::NurseDecision` — `#[serde(tag = "decision")]`
/// matches the legacy discriminant the renderer and provider parsers expect.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum NurseDecision {
    LeaveIt {
        reasoning: String,
        #[serde(default = "default_check_back_secs")]
        check_back_secs: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        observation: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        action: Option<String>,
    },
    Steer {
        reasoning: String,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        observation: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        action: Option<String>,
    },
    Restart {
        reasoning: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        observation: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        action: Option<String>,
    },
    Cancel {
        reasoning: String,
        #[serde(default)]
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        observation: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        action: Option<String>,
    },
}

impl NurseDecision {
    /// Action-kind classification, mirroring the legacy helper.
    pub fn level(&self) -> NurseActionKind {
        match self {
            Self::LeaveIt { .. } => NurseActionKind::LeaveIt,
            Self::Steer { .. } => NurseActionKind::Steer,
            Self::Restart { .. } => NurseActionKind::Restart,
            Self::Cancel { .. } => NurseActionKind::Cancel,
        }
    }

    pub fn reasoning(&self) -> &str {
        match self {
            Self::LeaveIt { reasoning, .. }
            | Self::Steer { reasoning, .. }
            | Self::Restart { reasoning, .. }
            | Self::Cancel { reasoning, .. } => reasoning,
        }
    }

    pub fn message(&self) -> Option<&str> {
        match self {
            Self::Steer { message, .. } | Self::Cancel { message, .. } => Some(message.as_str()),
            _ => None,
        }
    }

    pub fn observation(&self) -> Option<&str> {
        match self {
            Self::LeaveIt { observation, .. }
            | Self::Steer { observation, .. }
            | Self::Restart { observation, .. }
            | Self::Cancel { observation, .. } => observation.as_deref(),
        }
    }

    pub fn action(&self) -> Option<&str> {
        match self {
            Self::LeaveIt { action, .. }
            | Self::Steer { action, .. }
            | Self::Restart { action, .. }
            | Self::Cancel { action, .. } => action.as_deref(),
        }
    }
}

/// DTO returned by the IPC `check_chat_session` command. Mirrors the
/// legacy shape so the frontend's `useNurseStatus.ts` keeps working without
/// any TypeScript changes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NurseDecisionDto {
    LeaveIt {
        reasoning: String,
        check_back_secs: u64,
    },
    Steer {
        reasoning: String,
        message: String,
    },
    Restart {
        reasoning: String,
    },
    Cancel {
        reasoning: String,
        message: String,
    },
    Noop {
        reasoning: String,
    },
}

impl From<&NurseDecision> for NurseDecisionDto {
    fn from(d: &NurseDecision) -> Self {
        match d {
            NurseDecision::LeaveIt {
                reasoning,
                check_back_secs,
                ..
            } => Self::LeaveIt {
                reasoning: reasoning.clone(),
                check_back_secs: *check_back_secs,
            },
            NurseDecision::Steer {
                reasoning, message, ..
            } => Self::Steer {
                reasoning: reasoning.clone(),
                message: message.clone(),
            },
            NurseDecision::Restart { reasoning, .. } => Self::Restart {
                reasoning: reasoning.clone(),
            },
            NurseDecision::Cancel {
                reasoning, message, ..
            } => Self::Cancel {
                reasoning: reasoning.clone(),
                message: message.clone(),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Tauri-event surface (`nurse-event`) — bit-identical to legacy
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NurseLifecycleStatus {
    Started,
    Reasoning,
    Completed,
    Failed,
}

/// Which tier dispatched the intervention. Surfaced in
/// `NurseInterventionRecord` and the Intervention Log UI so operators can
/// see at a glance whether the action came from a hardcoded Tier-1 path,
/// the Tier-2 playbook, or the Tier-3 LLM classifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NurseDispatchTier {
    Deterministic,
    Templated,
    Llm,
    Synthesized,
    Manual,
}

/// Payload of every `nurse-event` Tauri emit of the Lifecycle variant.
/// Bit-identical to the legacy `core::nurse_service::NurseLifecyclePayload`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NurseLifecyclePayload {
    pub intervention_id: String,
    pub status: NurseLifecycleStatus,
    pub level: NurseActionKind,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub swarm_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feature_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_id: Option<String>,
    pub observation: String,
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_delta: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_reasoning: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NurseSessionAction {
    pub level: NurseActionKind,
    pub session_id: String,
    pub message: String,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NurseInterventionRecord {
    pub id: String,
    pub session_id: String,
    pub timestamp: DateTime<Utc>,
    pub level: NurseActionKind,
    pub analysis: String,
    pub action_taken: NurseSessionAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
}

/// Tagged `nurse-event` Tauri-event envelope. Bit-identical to legacy.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event_type")]
pub enum NurseEvent {
    StatusUpdate(NurseStatusSnapshot),
    Intervention(NurseInterventionRecord),
    UserNotice {
        session_id: String,
        level: NurseActionKind,
        message: String,
        timestamp: DateTime<Utc>,
    },
    Lifecycle(NurseLifecyclePayload),
}

// ---------------------------------------------------------------------------
// Status snapshot returned by `get_nurse_status` IPC.
// ---------------------------------------------------------------------------

/// Bit-identical shape to legacy `NurseServiceConfigSnapshot`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NurseServiceConfigSnapshot {
    pub enabled: bool,
    pub stall_threshold_secs: u64,
    pub nurse_model: String,
    pub max_interventions: u32,
    pub tick_interval_secs: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nurse_provider: Option<String>,
    /// When `true`, Nurse keeps observing every session but suppresses
    /// every intervention whose owner isn't `SessionOwner::Swarm`.
    /// Optional on the wire for back-compat with older backends.
    #[serde(default)]
    pub swarms_only: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NurseStats {
    pub monitored_count: usize,
    pub stall_count: usize,
    pub intervention_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_check_at: Option<DateTime<Utc>>,
    pub is_running: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NurseHealthSnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_tick_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_successful_tick_at: Option<DateTime<Utc>>,
    pub consecutive_failed_ticks: u32,
    pub consecutive_bad_parse_ticks: u32,
    pub consecutive_skipped_ticks: u32,
    pub degraded: bool,
    #[serde(default)]
    pub tier3_skipped_no_model: u64,
    #[serde(default)]
    pub intervention_writer_dropped: u64,
    #[serde(default)]
    pub observability_dropped: u64,
}

/// Per-session entry surfaced by `get_nurse_status`. Legacy shape preserved
/// — note the flat health-status enum, not the new `Tier` enum (the new
/// per-session tier is exposed through the additive `tier` field for
/// newer frontends to consume).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitoredSessionSnapshot {
    pub session_id: String,
    pub last_activity_ms: u64,
    pub event_count: u64,
    pub is_alive: bool,
    pub is_busy: bool,
    pub status: SessionHealthStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stall_detected_at: Option<DateTime<Utc>>,
    pub intervention_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_check_at: Option<DateTime<Utc>>,
    /// Additive field for the new Nurse UI. Older frontends ignore it.
    #[serde(default)]
    pub tier: crate::nurse::health::Tier,
    /// Additive field for the new Nurse UI. Older frontends ignore it.
    #[serde(default)]
    pub owner: Option<SessionOwnerDto>,
    /// Active signals projected from the new `SessionHealth`. Empty on
    /// older paths; populated for the new engine.
    #[serde(default)]
    pub active_signals: Vec<NurseActiveSignal>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionHealthStatus {
    Healthy,
    Warning,
    Stalled,
    Intervening,
    Resolved,
    Failed,
}

impl Default for SessionHealthStatus {
    fn default() -> Self {
        Self::Healthy
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NurseActiveSignal {
    pub detector: String,
    pub severity: crate::nurse::health::Severity,
    pub dedup_key: String,
    pub summary: String,
    pub raised_at: DateTime<Utc>,
}

/// Full status snapshot. Bit-identical envelope to legacy except for the
/// optional `batch` field, which is `#[serde(default)]` so the wire shape
/// stays backward-compatible (a frontend that doesn't know about it
/// simply ignores it).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NurseStatusSnapshot {
    pub stats: NurseStats,
    pub sessions: Vec<MonitoredSessionSnapshot>,
    pub recent_interventions: Vec<NurseInterventionRecord>,
    pub config: NurseServiceConfigSnapshot,
    pub health: NurseHealthSnapshot,
    /// Status of the batched LLM reviewer. `None` when the reviewer was
    /// never attached (e.g. tests, or when the user disabled it).
    /// Powers the topbar countdown progress bar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch: Option<BatchTickSnapshotDto>,
}

/// Wire-shape DTO mirroring [`crate::nurse::batch_review::BatchTickSnapshot`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchTickSnapshotDto {
    pub enabled: bool,
    pub interval_secs: u64,
    pub last_tick_at_unix_ms: u64,
    pub last_tick_duration_ms: u64,
    pub next_tick_at_unix_ms: u64,
    pub last_tick_session_count: u64,
    /// Cumulative nurse-LLM provider calls this process lifetime —
    /// covers the Tier 3 single-session classifier AND the batched
    /// reviewer. Surfaces in the topbar pill popover so the user can
    /// see how many nurse API requests they've spent. Resets to 0 on
    /// app start (not persisted).
    #[serde(default)]
    pub llm_calls_total: u64,
}

impl Default for NurseStatusSnapshot {
    /// Returned by `get_nurse_status` during the startup sliver before the
    /// engine has been attached. Mirrors the v1 default-ish "Nurse off" state
    /// so the frontend's early poll doesn't surface a confusing error.
    fn default() -> Self {
        Self {
            stats: NurseStats::default(),
            sessions: Vec::new(),
            recent_interventions: Vec::new(),
            config: NurseServiceConfigSnapshot {
                enabled: false,
                stall_threshold_secs: 180,
                nurse_model: String::new(),
                max_interventions: 3,
                tick_interval_secs: 10,
                nurse_provider: None,
                swarms_only: false,
            },
            health: NurseHealthSnapshot::default(),
            batch: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_owner_dto_kind_str_maps_variants() {
        assert_eq!(
            SessionOwnerDto::Task {
                task_id: "t".into()
            }
            .kind_str(),
            "task"
        );
        assert_eq!(
            SessionOwnerDto::Review { job_id: "j".into() }.kind_str(),
            "review"
        );
        assert_eq!(
            SessionOwnerDto::Merge {
                job_id: "j".into(),
                round: 1,
                swarm_id: None
            }
            .kind_str(),
            "merge"
        );
        assert_eq!(
            SessionOwnerDto::Swarm {
                swarm_id: "s".into(),
                role: "worker".into(),
                feature_id: None
            }
            .kind_str(),
            "swarm"
        );
        assert_eq!(SessionOwnerDto::Unknown.kind_str(), "unknown");
    }

    #[test]
    fn session_owner_dto_serde_round_trip() {
        let cases = vec![
            SessionOwnerDto::Task {
                task_id: "t1".into(),
            },
            SessionOwnerDto::Review {
                job_id: "j1".into(),
            },
            SessionOwnerDto::Merge {
                job_id: "j1".into(),
                round: 2,
                swarm_id: Some("s1".into()),
            },
            SessionOwnerDto::Swarm {
                swarm_id: "s1".into(),
                role: "worker".into(),
                feature_id: Some("f1".into()),
            },
            SessionOwnerDto::Unknown,
        ];
        for c in cases {
            let json = serde_json::to_string(&c).unwrap();
            let back: SessionOwnerDto = serde_json::from_str(&json).unwrap();
            assert_eq!(c, back, "round-trip failed for {:?}", c);
        }
    }

    #[test]
    fn nurse_decision_legacy_wire_shape() {
        // Wire-shape regression — the renderer parses on `decision` tag.
        let d = NurseDecision::Steer {
            reasoning: "r".into(),
            message: "m".into(),
            observation: None,
            action: None,
        };
        let v = serde_json::to_value(&d).unwrap();
        assert_eq!(v["decision"], "steer");
        assert_eq!(v["reasoning"], "r");
        assert_eq!(v["message"], "m");
    }

    #[test]
    fn nurse_decision_leave_it_defaults_check_back_secs() {
        let raw = r#"{ "decision": "leave_it", "reasoning": "fine" }"#;
        let parsed: NurseDecision = serde_json::from_str(raw).unwrap();
        match parsed {
            NurseDecision::LeaveIt {
                check_back_secs, ..
            } => assert_eq!(check_back_secs, 60),
            other => panic!("expected LeaveIt, got {:?}", other),
        }
    }

    #[test]
    fn nurse_lifecycle_payload_carries_optional_flat_owner_fields() {
        let p = NurseLifecyclePayload {
            intervention_id: "i".into(),
            status: NurseLifecycleStatus::Started,
            level: NurseActionKind::Steer,
            session_id: "sid".into(),
            task_id: Some("t".into()),
            swarm_id: None,
            feature_id: None,
            review_id: None,
            observation: "o".into(),
            action: "a".into(),
            reasoning_delta: None,
            full_reasoning: None,
            error: None,
            timestamp: Utc::now(),
        };
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["task_id"], "t");
        assert!(v.get("swarm_id").is_none(), "None fields are skipped");
    }

    #[test]
    fn nurse_event_tagged_with_event_type() {
        let e = NurseEvent::UserNotice {
            session_id: "sid".into(),
            level: NurseActionKind::Restart,
            message: "m".into(),
            timestamp: Utc::now(),
        };
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["event_type"], "UserNotice");
    }
}
