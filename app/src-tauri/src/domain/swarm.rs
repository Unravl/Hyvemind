//! Swarm domain types — the canonical data definitions shared by both
//! `core` (the swarm execution engine) and `state` (file persistence,
//! registry, progress log).
//!
//! These types are pure data: `Serialize` / `Deserialize`, small helpers
//! on `impl`, and no dependencies on `pi::*`, `state::*`, or any agent
//! module. Placing them under `domain/` (which neither `core` nor `state`
//! depend back into) breaks the historical `core` ↔ `state` cycle.
//!
//! # Lock-poison policy
//!
//! `SwarmUsageAccumulator` uses a `std::sync::Mutex` to protect a
//! plain numeric `SwarmUsageSummary`. All `lock()` calls in this
//! module follow the project-wide standard of
//! `unwrap_or_else(|e| e.into_inner())` — recovering and continuing
//! after a poisoning panic. The protected data is just additive
//! token/cost/duration counters; even if a previous writer panicked
//! mid-update the worst case is slightly under- or over-reported
//! totals, never an inconsistent or unsafe data structure. Surfacing
//! a panic to every later writer (UI usage tab, agent run completion)
//! would be strictly worse.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

/// Configuration for creating a new swarm.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwarmConfig {
    pub name: String,
    pub description: String,
    pub working_directory: String,
    pub model_settings: ModelSettings,
    pub features: Vec<Feature>,
    pub milestones: Vec<Milestone>,
}

/// Current status of a swarm.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SwarmStatus {
    Planning,
    Implementing,
    Paused,
    Interrupted,
    Completed,
    Failed,
    Cancelled,
}

impl std::fmt::Display for SwarmStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SwarmStatus::Planning => write!(f, "planning"),
            SwarmStatus::Implementing => write!(f, "implementing"),
            SwarmStatus::Paused => write!(f, "paused"),
            SwarmStatus::Interrupted => write!(f, "interrupted"),
            SwarmStatus::Completed => write!(f, "completed"),
            SwarmStatus::Failed => write!(f, "failed"),
            SwarmStatus::Cancelled => write!(f, "cancelled"),
        }
    }
}

impl SwarmStatus {
    /// Returns true for statuses that the user can resume from — either a live
    /// paused queen (Paused), a crash-interrupted state (Interrupted), or a
    /// terminal state that was written to disk and can be rehydrated (Failed,
    /// Cancelled).
    pub fn is_resumable(&self) -> bool {
        matches!(
            self,
            Self::Paused | Self::Interrupted | Self::Failed | Self::Cancelled
        )
    }
}

/// Aggregated token / cost totals for a single swarm.
/// Used by both the DB-backed query and the live in-memory accumulator.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SwarmUsageSummary {
    pub input_tokens: i64,
    pub output_tokens: i64,
    /// Tokens read from prompt cache. For providers like DeepSeek and
    /// Anthropic with caching enabled, this dwarfs `input_tokens` and was
    /// silently dropped before this field existed — leaving the UI showing
    /// roughly 10% of real usage.
    pub cache_read_tokens: i64,
    /// Tokens written to prompt cache (cache_creation_input_tokens on
    /// Anthropic). Billed differently from regular input but still real
    /// tokens the model processed.
    pub cache_write_tokens: i64,
    pub cost: f64,
    pub duration_ms: i64,
}

/// Thread-safe accumulator for real-time swarm token/cost totals.
/// Written to by each agent as it progresses, read by `get_swarm_usage`.
/// Multiple agents can update it concurrently.
#[derive(Clone)]
pub struct SwarmUsageAccumulator(Arc<Mutex<SwarmUsageSummary>>);

impl std::fmt::Debug for SwarmUsageAccumulator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SwarmUsageAccumulator")
            .field("snapshot", &self.snapshot())
            .finish()
    }
}

impl SwarmUsageAccumulator {
    /// Create a new accumulator initialised to zero.
    pub fn new() -> Self {
        Self(Arc::new(Mutex::new(SwarmUsageSummary::default())))
    }

    /// Add token/cost/duration deltas to the live total.
    pub fn add(
        &self,
        input: i64,
        output: i64,
        cache_read: i64,
        cache_write: i64,
        cost: f64,
        duration_ms: i64,
    ) {
        // Lock-poison policy: recover and continue. See module docs.
        let mut inner = self.0.lock().unwrap_or_else(|e| e.into_inner());
        inner.input_tokens += input;
        inner.output_tokens += output;
        inner.cache_read_tokens += cache_read;
        inner.cache_write_tokens += cache_write;
        inner.cost += cost;
        inner.duration_ms += duration_ms;
    }

    /// Subtract token/cost/duration values from the live total.
    /// Used after `record_session_usage` writes to the DB to prevent
    /// double-counting: what is now in the DB should no longer be in the
    /// in-memory accumulator.
    pub fn subtract(
        &self,
        input: i64,
        output: i64,
        cache_read: i64,
        cache_write: i64,
        cost: f64,
        duration_ms: i64,
    ) {
        // Lock-poison policy: recover and continue. See module docs.
        let mut inner = self.0.lock().unwrap_or_else(|e| e.into_inner());
        inner.input_tokens = inner.input_tokens.saturating_sub(input);
        inner.output_tokens = inner.output_tokens.saturating_sub(output);
        inner.cache_read_tokens = inner.cache_read_tokens.saturating_sub(cache_read);
        inner.cache_write_tokens = inner.cache_write_tokens.saturating_sub(cache_write);
        inner.cost = (inner.cost - cost).max(0.0);
        inner.duration_ms = inner.duration_ms.saturating_sub(duration_ms);
    }

    /// Snapshot the current live totals.
    pub fn snapshot(&self) -> SwarmUsageSummary {
        // Lock-poison policy: recover and continue. See module docs.
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl Default for SwarmUsageAccumulator {
    fn default() -> Self {
        Self::new()
    }
}

/// Runtime state of an active swarm.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwarmState {
    pub id: String,
    pub name: String,
    pub status: SwarmStatus,
    pub working_directory: String,
    pub model_settings: ModelSettings,
    pub current_phase: String,
    pub current_feature_index: usize,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub error: Option<String>,
    /// Set once the Queen master-plan Hivemind review has been attempted
    /// (success OR explicit skip) for this swarm's current plan. Prevents
    /// re-reviewing on every start_swarm — cloning/resuming a swarm carries
    /// this forward, since the plan is unchanged.
    #[serde(default)]
    pub queen_plan_review_done: bool,
}

impl SwarmState {
    /// Create a new swarm state from a config.
    pub fn from_config(config: &SwarmConfig) -> Self {
        let now = Utc::now();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: config.name.clone(),
            status: SwarmStatus::Planning,
            working_directory: config.working_directory.clone(),
            model_settings: config.model_settings.clone(),
            current_phase: "planning".to_string(),
            current_feature_index: 0,
            created_at: now,
            updated_at: now,
            error: None,
            queen_plan_review_done: false,
        }
    }

    /// Update the status and touch `updated_at`.
    pub fn set_status(&mut self, status: SwarmStatus) {
        self.status = status;
        self.updated_at = Utc::now();
    }

    /// Record an error and set status to Failed.
    pub fn set_error(&mut self, error: String) {
        self.error = Some(error);
        self.status = SwarmStatus::Failed;
        self.updated_at = Utc::now();
    }
}

/// Model configuration for a swarm.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelSettings {
    pub primary_model: String,
    pub scout_model: String,
    #[serde(default)]
    pub guard_model: Option<String>,
    #[serde(default = "default_scout_thinking")]
    pub scout_thinking_level: String,
    #[serde(default = "default_worker_thinking")]
    pub worker_thinking_level: String,
    #[serde(default = "default_guard_thinking")]
    pub guard_thinking_level: String,
    #[serde(default = "default_queen_thinking")]
    pub queen_thinking_level: String,
    pub use_hivemind_on_scout: bool,
    pub use_hivemind_on_queen: bool,
    pub hivemind_id: Option<String>,
    /// Maximum number of features the Queen runs concurrently. Default 1
    /// (sequential). Range 1..=6. Surfaced in the NewSwarm "Advanced" panel.
    #[serde(default = "default_max_concurrent_features")]
    pub max_concurrent_features: u32,
    /// Hard cap on the lifetime spend of this swarm in USD. `None`
    /// (the default) means unlimited; the swarm runs until it finishes
    /// or the user stops it. When the swarm's live cost meets or
    /// exceeds this value, the queen pauses the swarm between feature
    /// batches and emits a `BudgetExceeded` progress event. Opt-in;
    /// backwards compatible with swarms persisted before Phase 5A.
    #[serde(default)]
    pub swarm_budget_usd: Option<f64>,
}

fn default_scout_thinking() -> String {
    "high".to_string()
}

fn default_worker_thinking() -> String {
    "medium".to_string()
}

fn default_guard_thinking() -> String {
    "medium".to_string()
}

fn default_queen_thinking() -> String {
    "high".to_string()
}

fn default_max_concurrent_features() -> u32 {
    1
}

impl ModelSettings {
    /// Returns the effective guard model (falls back to primary_model).
    pub fn effective_guard_model(&self) -> &str {
        self.guard_model.as_deref().unwrap_or(&self.primary_model)
    }
}

impl Default for ModelSettings {
    fn default() -> Self {
        Self {
            primary_model: "claude-sonnet-4-20250514".to_string(),
            scout_model: "claude-sonnet-4-20250514".to_string(),
            guard_model: None,
            scout_thinking_level: default_scout_thinking(),
            worker_thinking_level: default_worker_thinking(),
            guard_thinking_level: default_guard_thinking(),
            queen_thinking_level: default_queen_thinking(),
            use_hivemind_on_scout: false,
            use_hivemind_on_queen: false,
            hivemind_id: None,
            max_concurrent_features: default_max_concurrent_features(),
            swarm_budget_usd: None,
        }
    }
}

/// A feature to be implemented by the swarm.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Feature {
    pub id: String,
    pub name: String,
    pub description: String,
    pub status: FeatureStatus,
    pub dependencies: Vec<String>,
    pub milestone: Option<String>,
    pub fix_attempt_count: u32,
    pub max_fix_attempts: u32,
    /// The validation assertions this feature is responsible for satisfying.
    /// For impl features, this is usually empty (the validator feature owns
    /// the milestone assertions). For auto-injected validator features
    /// (`id` starts with `validate-`) and Guard-spawned fix features, this
    /// lists the `VAL-*` assertion IDs that must pass.
    ///
    /// Serde default keeps backwards compatibility with old features.json
    /// files persisted before Phase 2.
    #[serde(default)]
    pub fulfills: Vec<String>,
    /// Set to `true` when this feature was marked `Failed` by the crash
    /// reconciler (audit 2.2) because it was in an in-flight state
    /// (`Scouting` / `Implementing` / `Reviewing` / `Validating`) at the
    /// time the host process died. The reconciler folds the JSONL progress
    /// log over the persisted state via `ProgressReader::rebuild_state` —
    /// any feature whose final state from the log is still non-terminal is
    /// promoted to `Failed` with this flag set so the UI can show
    /// "interrupted" badge separately from a genuine validation failure.
    ///
    /// Cleared by `resume_swarm` (via `reset_in_flight_features` →
    /// re-Pending) so a successful retry doesn't carry the marker.
    /// `#[serde(default)]` keeps the field backwards-compatible with
    /// features.json files persisted before audit 2.2.
    #[serde(default)]
    pub interrupted: bool,
    /// Set to `true` alongside `interrupted` on features that the crash
    /// reconciler determined are safe to re-queue via `resume_swarm`
    /// (audit 2.2). The frontend uses this to show a "Resume" affordance
    /// on the swarm card. `#[serde(default)]` keeps backwards-compatible.
    #[serde(default)]
    pub resumable: bool,
}

impl Feature {
    /// Create a new feature with default status and fix settings.
    pub fn new(id: String, name: String, description: String) -> Self {
        Self {
            id,
            name,
            description,
            status: FeatureStatus::Pending,
            dependencies: Vec::new(),
            milestone: None,
            fix_attempt_count: 0,
            max_fix_attempts: 3,
            fulfills: Vec::new(),
            interrupted: false,
            resumable: false,
        }
    }

    /// Whether this feature is a synthetic validator feature auto-injected
    /// by `inject_milestone_validators`. Identified by the `validate-` id
    /// prefix. Validator features skip Scout/Worker and run Guard directly.
    pub fn is_validator(&self) -> bool {
        self.id.starts_with("validate-")
    }

    /// Increment the fix attempt counter.
    pub fn increment_fix_attempts(&mut self) {
        self.fix_attempt_count += 1;
    }
}

/// Status of an individual feature.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FeatureStatus {
    Pending,
    Scouting,
    Implementing,
    Reviewing,
    Validating,
    Completed,
    Failed,
    Skipped,
}

impl std::fmt::Display for FeatureStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FeatureStatus::Pending => write!(f, "pending"),
            FeatureStatus::Scouting => write!(f, "scouting"),
            FeatureStatus::Implementing => write!(f, "implementing"),
            FeatureStatus::Reviewing => write!(f, "reviewing"),
            FeatureStatus::Validating => write!(f, "validating"),
            FeatureStatus::Completed => write!(f, "completed"),
            FeatureStatus::Failed => write!(f, "failed"),
            FeatureStatus::Skipped => write!(f, "skipped"),
        }
    }
}

impl FeatureStatus {
    /// Whether this status represents a terminal state.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            FeatureStatus::Completed | FeatureStatus::Failed | FeatureStatus::Skipped
        )
    }
}

/// A milestone grouping features with validation assertions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Milestone {
    pub id: String,
    pub name: String,
    pub features: Vec<String>,
    pub assertions: Vec<String>,
    /// Once a milestone validator passes, the scheduler refuses to inject
    /// additional features into it. Phase 2 enforcement; default false for
    /// backwards compat with swarms persisted before this field existed.
    #[serde(default)]
    pub sealed: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------------
    // Feature tests
    // ------------------------------------------------------------------

    #[test]
    fn test_feature_new_defaults() {
        let f = Feature::new("f1".into(), "Feature 1".into(), "A test feature".into());
        assert_eq!(f.id, "f1");
        assert_eq!(f.name, "Feature 1");
        assert_eq!(f.status, FeatureStatus::Pending);
        assert!(f.dependencies.is_empty());
        assert!(f.milestone.is_none());
        assert_eq!(f.fix_attempt_count, 0);
        assert_eq!(f.max_fix_attempts, 3);
        assert!(f.fulfills.is_empty());
        assert!(!f.is_validator());
    }

    #[test]
    fn test_feature_is_validator_by_id_prefix() {
        let mut f = Feature::new("validate-m1".into(), "Validate M1".into(), "".into());
        assert!(f.is_validator());
        f.id = "feat-001".into();
        assert!(!f.is_validator());
        f.id = "validate".into(); // no dash
        assert!(!f.is_validator());
    }

    #[test]
    fn test_feature_fulfills_serde_default() {
        // Backwards compat: features.json written before Phase 2 has no
        // `fulfills` field; deserialisation must default it to [].
        let json = r#"{
            "id": "f1",
            "name": "F1",
            "description": "d",
            "status": "pending",
            "dependencies": [],
            "milestone": null,
            "fix_attempt_count": 0,
            "max_fix_attempts": 3
        }"#;
        let f: Feature = serde_json::from_str(json).expect("deserialise legacy feature");
        assert_eq!(f.id, "f1");
        assert!(f.fulfills.is_empty());
    }

    // ------------------------------------------------------------------
    // FeatureStatus tests
    // ------------------------------------------------------------------

    #[test]
    fn test_feature_status_is_terminal() {
        assert!(FeatureStatus::Completed.is_terminal());
        assert!(FeatureStatus::Failed.is_terminal());
        assert!(FeatureStatus::Skipped.is_terminal());

        assert!(!FeatureStatus::Pending.is_terminal());
        assert!(!FeatureStatus::Scouting.is_terminal());
        assert!(!FeatureStatus::Implementing.is_terminal());
        assert!(!FeatureStatus::Reviewing.is_terminal());
        assert!(!FeatureStatus::Validating.is_terminal());
    }

    #[test]
    fn test_feature_status_display() {
        assert_eq!(FeatureStatus::Pending.to_string(), "pending");
        assert_eq!(FeatureStatus::Scouting.to_string(), "scouting");
        assert_eq!(FeatureStatus::Implementing.to_string(), "implementing");
        assert_eq!(FeatureStatus::Reviewing.to_string(), "reviewing");
        assert_eq!(FeatureStatus::Validating.to_string(), "validating");
        assert_eq!(FeatureStatus::Completed.to_string(), "completed");
        assert_eq!(FeatureStatus::Failed.to_string(), "failed");
        assert_eq!(FeatureStatus::Skipped.to_string(), "skipped");
    }

    // ------------------------------------------------------------------
    // SwarmStatus tests
    // ------------------------------------------------------------------

    #[test]
    fn test_swarm_status_display() {
        assert_eq!(SwarmStatus::Planning.to_string(), "planning");
        assert_eq!(SwarmStatus::Implementing.to_string(), "implementing");
        assert_eq!(SwarmStatus::Paused.to_string(), "paused");
        assert_eq!(SwarmStatus::Interrupted.to_string(), "interrupted");
        assert_eq!(SwarmStatus::Completed.to_string(), "completed");
        assert_eq!(SwarmStatus::Failed.to_string(), "failed");
        assert_eq!(SwarmStatus::Cancelled.to_string(), "cancelled");
    }

    #[test]
    fn test_is_resumable() {
        assert!(SwarmStatus::Paused.is_resumable());
        assert!(SwarmStatus::Interrupted.is_resumable());
        assert!(SwarmStatus::Failed.is_resumable());
        assert!(SwarmStatus::Cancelled.is_resumable());
        assert!(!SwarmStatus::Planning.is_resumable());
        assert!(!SwarmStatus::Implementing.is_resumable());
        assert!(!SwarmStatus::Completed.is_resumable());
    }

    // ------------------------------------------------------------------
    // SwarmState tests
    // ------------------------------------------------------------------

    #[test]
    fn test_swarm_state_from_config() {
        let config = SwarmConfig {
            name: "Test Swarm".into(),
            description: "A test".into(),
            working_directory: "/tmp/test".into(),
            model_settings: ModelSettings::default(),
            features: vec![],
            milestones: vec![],
        };

        let state = SwarmState::from_config(&config);
        assert_eq!(state.name, "Test Swarm");
        assert_eq!(state.status, SwarmStatus::Planning);
        assert_eq!(state.current_phase, "planning");
        assert_eq!(state.current_feature_index, 0);
        assert_eq!(state.working_directory, "/tmp/test");
        assert!(state.error.is_none());
        assert!(!state.id.is_empty());
    }

    #[test]
    fn test_swarm_state_set_status() {
        let config = SwarmConfig {
            name: "Test".into(),
            description: "".into(),
            working_directory: "/tmp".into(),
            model_settings: ModelSettings::default(),
            features: vec![],
            milestones: vec![],
        };
        let mut state = SwarmState::from_config(&config);
        let original_updated = state.updated_at;

        std::thread::sleep(std::time::Duration::from_millis(1));

        state.set_status(SwarmStatus::Implementing);
        assert_eq!(state.status, SwarmStatus::Implementing);
        assert!(state.updated_at > original_updated);
    }

    #[test]
    fn test_swarm_state_set_error() {
        let config = SwarmConfig {
            name: "Test".into(),
            description: "".into(),
            working_directory: "/tmp".into(),
            model_settings: ModelSettings::default(),
            features: vec![],
            milestones: vec![],
        };
        let mut state = SwarmState::from_config(&config);

        state.set_error("something broke".into());
        assert_eq!(state.status, SwarmStatus::Failed);
        assert_eq!(state.error, Some("something broke".into()));
    }

    // ------------------------------------------------------------------
    // ModelSettings tests
    // ------------------------------------------------------------------

    #[test]
    fn test_model_settings_effective_guard_model_fallback() {
        let settings = ModelSettings {
            primary_model: "claude-sonnet-4".into(),
            guard_model: None,
            ..ModelSettings::default()
        };
        assert_eq!(settings.effective_guard_model(), "claude-sonnet-4");
    }

    #[test]
    fn test_model_settings_effective_guard_model_explicit() {
        let settings = ModelSettings {
            primary_model: "claude-sonnet-4".into(),
            guard_model: Some("claude-opus-4".into()),
            ..ModelSettings::default()
        };
        assert_eq!(settings.effective_guard_model(), "claude-opus-4");
    }

    #[test]
    fn test_model_settings_defaults() {
        let settings = ModelSettings::default();
        assert_eq!(settings.scout_thinking_level, "high");
        assert_eq!(settings.worker_thinking_level, "medium");
        assert_eq!(settings.guard_thinking_level, "medium");
        assert_eq!(settings.queen_thinking_level, "high");
        assert!(!settings.use_hivemind_on_scout);
        assert!(!settings.use_hivemind_on_queen);
        assert!(settings.hivemind_id.is_none());
        assert!(settings.guard_model.is_none());
    }

    // ------------------------------------------------------------------
    // Milestone tests
    // ------------------------------------------------------------------

    #[test]
    fn test_milestone_deserialises_without_sealed_field() {
        // Backwards compat: milestones.json written before the `sealed` field
        // existed must still deserialise, defaulting `sealed` to false.
        let json = r#"{"id":"m1","name":"M1","features":["f1"],"assertions":["a1"]}"#;
        let m: Milestone = serde_json::from_str(json).expect("deserialise legacy milestone");
        assert_eq!(m.id, "m1");
        assert_eq!(m.name, "M1");
        assert_eq!(m.features, vec!["f1"]);
        assert_eq!(m.assertions, vec!["a1"]);
        assert!(!m.sealed);
    }

    #[test]
    fn test_model_settings_deserialises_without_swarm_budget() {
        // Backwards compat: ModelSettings persisted before swarm_budget_usd
        // existed must still deserialise, defaulting to None (unlimited).
        let json = r#"{
            "primary_model": "claude-sonnet-4-20250514",
            "scout_model": "claude-sonnet-4-20250514",
            "use_hivemind_on_scout": false,
            "use_hivemind_on_queen": false,
            "hivemind_id": null
        }"#;
        let settings: ModelSettings =
            serde_json::from_str(json).expect("deserialise legacy model settings");
        assert!(settings.swarm_budget_usd.is_none());
    }

    #[test]
    fn test_model_settings_roundtrips_with_swarm_budget() {
        let mut s = ModelSettings::default();
        s.swarm_budget_usd = Some(5.0);
        let json = serde_json::to_string(&s).expect("serialise");
        let back: ModelSettings = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(back.swarm_budget_usd, Some(5.0));
    }

    #[test]
    fn test_model_settings_deserialises_without_max_concurrent_features() {
        // Backwards compat: ModelSettings persisted before max_concurrent_features
        // existed must still deserialise, defaulting to 1 (sequential).
        let json = r#"{
            "primary_model": "claude-sonnet-4-20250514",
            "scout_model": "claude-sonnet-4-20250514",
            "use_hivemind_on_scout": false,
            "use_hivemind_on_queen": false,
            "hivemind_id": null
        }"#;
        let settings: ModelSettings =
            serde_json::from_str(json).expect("deserialise legacy model settings");
        assert_eq!(settings.max_concurrent_features, 1);
        assert_eq!(settings.primary_model, "claude-sonnet-4-20250514");
    }

    // ------------------------------------------------------------------
    // SwarmUsageAccumulator tests
    // ------------------------------------------------------------------

    #[test]
    fn test_usage_accumulator_defaults() {
        let acc = SwarmUsageAccumulator::new();
        let snap = acc.snapshot();
        assert_eq!(snap.input_tokens, 0);
        assert_eq!(snap.output_tokens, 0);
        assert_eq!(snap.cost, 0.0);
        assert_eq!(snap.duration_ms, 0);
    }

    #[test]
    fn test_usage_accumulator_add_and_snapshot() {
        let acc = SwarmUsageAccumulator::new();
        acc.add(100, 200, 1000, 50, 0.5, 1500);
        let snap = acc.snapshot();
        assert_eq!(snap.input_tokens, 100);
        assert_eq!(snap.output_tokens, 200);
        assert_eq!(snap.cache_read_tokens, 1000);
        assert_eq!(snap.cache_write_tokens, 50);
        assert_eq!(snap.cost, 0.5);
        assert_eq!(snap.duration_ms, 1500);
    }

    #[test]
    fn test_usage_accumulator_accumulates() {
        let acc = SwarmUsageAccumulator::new();
        acc.add(100, 200, 1000, 50, 0.5, 1500);
        acc.add(50, 300, 2000, 0, 1.0, 500);
        let snap = acc.snapshot();
        assert_eq!(snap.input_tokens, 150);
        assert_eq!(snap.output_tokens, 500);
        assert_eq!(snap.cache_read_tokens, 3000);
        assert_eq!(snap.cache_write_tokens, 50);
        assert_eq!(snap.cost, 1.5);
        assert_eq!(snap.duration_ms, 2000);
    }

    #[test]
    fn test_usage_accumulator_clone_shares_data() {
        let acc = SwarmUsageAccumulator::new();
        acc.add(100, 200, 1000, 0, 0.5, 1500);
        let acc2 = acc.clone();
        acc2.add(50, 50, 500, 0, 0.2, 500);
        // Clone shares the same Arc, so both see the cumulative total
        let snap = acc.snapshot();
        assert_eq!(snap.input_tokens, 150);
        assert_eq!(snap.cache_read_tokens, 1500);
        assert_eq!(snap.cost, 0.7);
        let snap2 = acc2.snapshot();
        assert_eq!(snap2.input_tokens, 150);
    }
}
