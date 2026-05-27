//! `NurseConfig` + `ProfileConfig` + `NurseProfile`.
//!
//! Per the rewrite plan:
//! - Master-level switches (`enabled`, `mode`, `nurse_model`, `nurse_provider`)
//!   live in [`NurseConfig`] and persist to top-level fields in `config.json`.
//! - Per-context tuning lives in [`ProfileConfig`] keyed by [`NurseProfile`]
//!   under `config.json::nurse_profiles[..]`.
//!
//! Legacy detector-related scalars (`nurse_stall_threshold_secs`,
//! `nurse_tick_interval_secs`) seed the `Default` profile on first
//! startup; subsequent writes go to `nurse_profiles` only.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::nurse::health::Severity;
use crate::pi::session::SessionOwner;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum NurseProfile {
    Tasks,
    Swarm,
    Hivemind,
    Test,
    Default,
}

impl NurseProfile {
    pub fn for_owner(owner: &SessionOwner) -> Self {
        match owner {
            SessionOwner::Task { .. } => Self::Tasks,
            SessionOwner::Swarm { .. } => Self::Swarm,
            SessionOwner::Review { .. } | SessionOwner::Merge { .. } => Self::Hivemind,
            SessionOwner::Unknown => Self::Default,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NurseMode {
    /// Three-tier pipeline runs; interventions dispatch normally.
    Enabled,
    /// Detectors + classifier run, but no interventions dispatch. Useful
    /// for shadow-mode tuning.
    Observe,
    /// Engine still subscribes for telemetry but nothing else.
    Disabled,
}

impl Default for NurseMode {
    fn default() -> Self {
        Self::Enabled
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InterventionMode {
    Auto,
    Observe,
}

impl Default for InterventionMode {
    fn default() -> Self {
        Self::Auto
    }
}

/// Per-detector and lifetime intervention budget. Replaces the legacy
/// single `max_interventions = 3` ceiling, which was structurally
/// insufficient for multi-day unattended swarms.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetConfig {
    /// Initial lifetime cap when the session is first observed.
    pub initial_lifetime_cap: u32,
    /// Cap grows by this many per hour of session age, up to
    /// `max_lifetime_cap`.
    pub decay_per_hour: u32,
    /// Hard ceiling regardless of decay.
    pub max_lifetime_cap: u32,
    /// Per-detector-class sub-budget — prevents one chatty detector
    /// from eating the whole budget.
    pub per_detector_cap: u32,
    /// Per-`dedup_key` cooldown — same key cannot re-fire within this
    /// many seconds regardless of total budget.
    pub per_key_cooldown_secs: u64,
}

impl BudgetConfig {
    pub fn tasks_default() -> Self {
        Self {
            initial_lifetime_cap: 3,
            decay_per_hour: 0,
            max_lifetime_cap: 5,
            per_detector_cap: 2,
            per_key_cooldown_secs: 60,
        }
    }
    pub fn swarm_default() -> Self {
        Self {
            initial_lifetime_cap: 5,
            decay_per_hour: 1,
            max_lifetime_cap: 10,
            per_detector_cap: 4,
            per_key_cooldown_secs: 120,
        }
    }
    pub fn hivemind_default() -> Self {
        Self {
            initial_lifetime_cap: 3,
            decay_per_hour: 0,
            max_lifetime_cap: 5,
            per_detector_cap: 2,
            per_key_cooldown_secs: 180,
        }
    }
    pub fn test_default() -> Self {
        Self {
            initial_lifetime_cap: 5,
            decay_per_hour: 0,
            max_lifetime_cap: 5,
            per_detector_cap: 3,
            per_key_cooldown_secs: 120,
        }
    }
    pub fn default_profile() -> Self {
        Self::tasks_default()
    }
}

// -------------------- Per-detector configs --------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StallDetectorConfig {
    pub warn_secs: u64,
    pub stalled_secs: u64,
    /// Hard upper bound on `awaiting_model_for_ms` before the time-based
    /// check fires regardless of where the timestamps land. Mirrors the
    /// legacy `AWAITING_MODEL_HARD_LIMIT_MS = 10 min`.
    pub awaiting_model_hard_limit_secs: u64,
    /// Post-prompt-silence fast-path: when `now - last_prompt_sent_at`
    /// exceeds this and no events have been observed since, Warn fires.
    pub post_prompt_warn_secs: u64,
    pub post_prompt_stalled_secs: u64,
    pub enabled: bool,
}

impl StallDetectorConfig {
    pub fn for_profile(profile: NurseProfile) -> Self {
        let (warn, stalled) = match profile {
            NurseProfile::Tasks => (120, 180),
            NurseProfile::Swarm => (180, 300),
            NurseProfile::Hivemind => (240, 600),
            NurseProfile::Test => (180, 300),
            NurseProfile::Default => (180, 300),
        };
        Self {
            warn_secs: warn,
            stalled_secs: stalled,
            awaiting_model_hard_limit_secs: 10 * 60,
            post_prompt_warn_secs: 60,
            post_prompt_stalled_secs: 180,
            enabled: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReasoningLoopDetectorConfig {
    pub enabled: bool,
    /// Repeat count of the same SipHash chunk inside the window that
    /// trips `loop:exact`.
    pub siphash_repeats_to_fire: u32,
    pub siphash_window_secs: u64,
    /// Minimum length of a normalised thinking chunk before it's even
    /// considered for SipHash exact-repeat hashing. Without this guard, a
    /// model that emits short fragmentary thinking deltas (single words,
    /// punctuation, even single letters) will trip the detector on
    /// coincidental repeats of trivial tokens like "i", "the", or " ". 64
    /// chars roughly corresponds to "one substantive sentence fragment".
    #[serde(default = "default_siphash_min_chunk_chars")]
    pub siphash_min_chunk_chars: usize,
    pub compression_sample_every_n_events: u32,
    /// Drop threshold for `zstd(buf).len() / buf.len()`. Lower = louder.
    pub compression_ratio_threshold: f64,
    pub compression_consecutive_samples: u32,
    /// MinHash Jaccard threshold for paraphrase-loop detection.
    pub paraphrase_jaccard_threshold: f64,
    /// Skip MinHash if normalised block is smaller than this.
    pub paraphrase_min_block_chars: usize,
    /// Calibration guard — number of consecutive MinHash hits required
    /// before the paraphrase signal escalates past `Info`.
    pub min_paraphrase_raise_count: u32,
}

fn default_siphash_min_chunk_chars() -> usize {
    64
}

impl ReasoningLoopDetectorConfig {
    pub fn for_profile(profile: NurseProfile) -> Self {
        Self {
            enabled: !matches!(profile, NurseProfile::Hivemind),
            siphash_repeats_to_fire: 5,
            siphash_window_secs: 90,
            siphash_min_chunk_chars: default_siphash_min_chunk_chars(),
            compression_sample_every_n_events: 200,
            compression_ratio_threshold: 0.35,
            compression_consecutive_samples: 3,
            paraphrase_jaccard_threshold: 0.7,
            paraphrase_min_block_chars: 512,
            min_paraphrase_raise_count: 2,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolFailureDetectorConfig {
    pub enabled: bool,
    pub window_secs: u64,
    pub min_failures_for_stuck: u32,
    /// `max_signature_count / total_failures` ratio to declare stuck.
    pub stuck_concentration_ratio: f64,
    /// Burst trigger — any tool failing this many times of any signature
    /// in a small window is Stalled regardless of diversity.
    pub burst_count_secs: u64,
    pub burst_count: u32,
    pub tool_call_id_cache_capacity: usize,
}

impl ToolFailureDetectorConfig {
    pub fn for_profile(profile: NurseProfile) -> Self {
        Self {
            enabled: !matches!(profile, NurseProfile::Hivemind),
            window_secs: 300,
            min_failures_for_stuck: 5,
            stuck_concentration_ratio: 0.7,
            burst_count_secs: 60,
            burst_count: 6,
            tool_call_id_cache_capacity: 1024,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderHealthDetectorConfig {
    pub enabled: bool,
}

impl ProviderHealthDetectorConfig {
    pub fn for_profile(_p: NurseProfile) -> Self {
        Self { enabled: true }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextSaturationDetectorConfig {
    pub enabled: bool,
    pub warn_percent: f64,
    pub stalled_percent: f64,
}

impl ContextSaturationDetectorConfig {
    pub fn for_profile(_p: NurseProfile) -> Self {
        Self {
            enabled: true,
            warn_percent: 80.0,
            stalled_percent: 92.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryExhaustionDetectorConfig {
    pub enabled: bool,
    pub distinct_attempts_for_stalled: u32,
    pub window_secs: u64,
    /// Death-loop threshold. Raises a `Critical` `retry:death_loop` signal
    /// when EITHER the consecutive-failure counter (incremented on each
    /// `AutoRetryEnd { success: false }`) OR Pi's reported `attempt`
    /// number reaches this value. Pi consolidates its internal retry
    /// budget into a single `AutoRetryEnd` event whose `attempt` field
    /// reflects how many tries already failed, so checking both fields
    /// catches the case regardless of how Pi batches the signal.
    /// Default `3` matches the legacy `distinct_attempts_for_stalled`.
    #[serde(default = "default_consecutive_failures_for_critical")]
    pub consecutive_failures_for_critical: u32,
}

fn default_consecutive_failures_for_critical() -> u32 {
    3
}

impl RetryExhaustionDetectorConfig {
    pub fn for_profile(_p: NurseProfile) -> Self {
        Self {
            enabled: true,
            distinct_attempts_for_stalled: 3,
            window_secs: 90,
            consecutive_failures_for_critical: default_consecutive_failures_for_critical(),
        }
    }
}

/// Per-context configuration. Only fields that vary per profile live here;
/// master-level switches live on [`NurseConfig`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileConfig {
    pub enabled: bool,
    pub intervention_mode: InterventionMode,
    pub escalation_min_severity: Severity,
    pub budget: BudgetConfig,
    pub stall: StallDetectorConfig,
    pub reasoning_loop: ReasoningLoopDetectorConfig,
    pub tool_failure: ToolFailureDetectorConfig,
    pub provider_health: ProviderHealthDetectorConfig,
    pub context_saturation: ContextSaturationDetectorConfig,
    pub retry_exhaustion: RetryExhaustionDetectorConfig,

    /// Per-profile classifier model override. `None` (the default) falls
    /// back to the engine-wide [`NurseConfig::nurse_model`] via
    /// [`NurseConfig::effective_model`]. Set to a non-empty string to
    /// pin a specific model for this context (e.g. a cheaper model for
    /// `NurseProfile::Tasks` and a stronger one for `NurseProfile::Swarm`).
    /// An empty string or `"none"` is treated identically to `None`.
    #[serde(default)]
    pub nurse_model: Option<String>,

    /// Per-profile provider override. `None` defers to the engine-wide
    /// [`NurseConfig::nurse_provider`] (which itself defers to the
    /// heuristic in `classifier::resolve_provider`). Set to pin a
    /// provider for this context. Empty string is treated as `None`.
    #[serde(default)]
    pub nurse_provider: Option<String>,
}

impl ProfileConfig {
    pub fn default_for(profile: NurseProfile) -> Self {
        let budget = match profile {
            NurseProfile::Tasks => BudgetConfig::tasks_default(),
            NurseProfile::Swarm => BudgetConfig::swarm_default(),
            NurseProfile::Hivemind => BudgetConfig::hivemind_default(),
            NurseProfile::Test => BudgetConfig::test_default(),
            NurseProfile::Default => BudgetConfig::default_profile(),
        };
        let escalation_min_severity = match profile {
            NurseProfile::Tasks => Severity::Stalled,
            NurseProfile::Hivemind => Severity::Stalled,
            _ => Severity::Warn,
        };
        Self {
            enabled: true,
            intervention_mode: InterventionMode::Auto,
            escalation_min_severity,
            budget,
            stall: StallDetectorConfig::for_profile(profile),
            reasoning_loop: ReasoningLoopDetectorConfig::for_profile(profile),
            tool_failure: ToolFailureDetectorConfig::for_profile(profile),
            provider_health: ProviderHealthDetectorConfig::for_profile(profile),
            context_saturation: ContextSaturationDetectorConfig::for_profile(profile),
            retry_exhaustion: RetryExhaustionDetectorConfig::for_profile(profile),
            nurse_model: None,
            nurse_provider: None,
        }
    }
}

/// Master-level configuration — single instance for the whole engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NurseConfig {
    pub enabled: bool,
    pub mode: NurseMode,
    pub nurse_model: Option<String>,
    pub nurse_provider: Option<String>,
    /// Lifetime intervention ceiling fallback (legacy) — retained for the
    /// `NurseServiceConfigSnapshot` wire shape. New code should consult
    /// the per-profile `BudgetConfig`.
    pub max_interventions: u32,
    /// Enables the batched periodic LLM review (see
    /// [`crate::nurse::batch_review::BatchReviewer`]). When `true` (the
    /// default) the engine spawns a ticker that snapshots all active
    /// streaming sessions, batches them into ONE LLM call every
    /// [`crate::tunables::nurse_batch_interval_secs`], and dispatches
    /// per-session decisions. This is the path that catches
    /// repetition / stuck loops the heuristic detectors miss.
    #[serde(default = "default_nurse_batch_enabled")]
    pub nurse_batch_enabled: bool,
    /// User-supplied override for the batch-review tick interval. `None`
    /// falls back to [`crate::tunables::nurse_batch_interval_secs`] (env-var
    /// `HYVEMIND_NURSE_BATCH_INTERVAL_SECS`). Resolved via
    /// [`Self::effective_batch_interval_secs`] which is called every loop
    /// iteration of the ticker so a Settings edit takes effect on the next
    /// scheduled tick — no restart required.
    #[serde(default)]
    pub nurse_batch_interval_secs: Option<u64>,
    /// When `true`, the Nurse engine keeps running detectors and
    /// observability for every session, but the dispatcher
    /// short-circuits any decision whose owner isn't a
    /// [`SessionOwner::Swarm`]. Tasks-view conversations, Hivemind
    /// reviews, and Merges are still observed (so the per-session
    /// timeline keeps populating) but Nurse never steers, restarts,
    /// or cancels them. Default `false` — current behaviour.
    #[serde(default = "default_swarms_only")]
    pub swarms_only: bool,
    #[serde(default)]
    pub profiles: HashMap<NurseProfile, ProfileConfig>,
}

fn default_nurse_batch_enabled() -> bool {
    true
}

fn default_swarms_only() -> bool {
    false
}

impl NurseConfig {
    /// Resolved batch-review tick interval — user override wins, env-var
    /// tunable is the fallback. Clamped to the tunable's safe range so a
    /// bad user value can't break the ticker.
    pub fn effective_batch_interval_secs(&self) -> u64 {
        self.nurse_batch_interval_secs
            .unwrap_or_else(crate::tunables::nurse_batch_interval_secs)
            .clamp(30, 3600)
    }
}

impl Default for NurseConfig {
    fn default() -> Self {
        let mut profiles = HashMap::new();
        for p in [
            NurseProfile::Tasks,
            NurseProfile::Swarm,
            NurseProfile::Hivemind,
            NurseProfile::Test,
            NurseProfile::Default,
        ] {
            profiles.insert(p, ProfileConfig::default_for(p));
        }
        Self {
            enabled: true,
            mode: NurseMode::Enabled,
            nurse_model: None,
            nurse_provider: None,
            max_interventions: 3,
            nurse_batch_enabled: default_nurse_batch_enabled(),
            nurse_batch_interval_secs: None,
            swarms_only: default_swarms_only(),
            profiles,
        }
    }
}

impl NurseConfig {
    /// Lookup with fallback chain `profile → Default → builtin default`.
    pub fn profile(&self, profile: NurseProfile) -> ProfileConfig {
        if let Some(p) = self.profiles.get(&profile) {
            return p.clone();
        }
        if let Some(p) = self.profiles.get(&NurseProfile::Default) {
            return p.clone();
        }
        ProfileConfig::default_for(profile)
    }

    /// Resolve the classifier model for `profile` with the precedence:
    /// per-profile override → engine-wide default. Empty strings and
    /// `"none"` collapse to `None` so the dispatcher's Tier 3 step can
    /// treat all three "unconfigured" representations identically. The
    /// returned `String` is owned to keep call sites simple.
    pub fn effective_model(&self, profile: NurseProfile) -> Option<String> {
        let normalise = |s: &str| -> Option<String> {
            let trimmed = s.trim();
            if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("none") {
                None
            } else {
                Some(trimmed.to_string())
            }
        };
        if let Some(p) = self.profiles.get(&profile) {
            if let Some(m) = p.nurse_model.as_deref().and_then(normalise) {
                return Some(m);
            }
        }
        self.nurse_model.as_deref().and_then(normalise)
    }

    /// Resolve the classifier provider for `profile` with the same
    /// precedence as [`Self::effective_model`]. Empty strings collapse
    /// to `None`; `"none"` is NOT a magic value for providers (a provider
    /// can legitimately be named `none` in tests / mocks), only for models.
    pub fn effective_provider(&self, profile: NurseProfile) -> Option<String> {
        let normalise = |s: &str| -> Option<String> {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        };
        if let Some(p) = self.profiles.get(&profile) {
            if let Some(prov) = p.nurse_provider.as_deref().and_then(normalise) {
                return Some(prov);
            }
        }
        self.nurse_provider.as_deref().and_then(normalise)
    }

    /// Seed `Default` profile from legacy scalar fields. Idempotent: only
    /// fills missing fields.
    pub fn seed_default_profile_from_legacy(
        &mut self,
        stall_threshold_secs: Option<u64>,
        tick_interval_secs: Option<u64>,
    ) {
        let entry = self
            .profiles
            .entry(NurseProfile::Default)
            .or_insert_with(|| ProfileConfig::default_for(NurseProfile::Default));
        if let Some(t) = stall_threshold_secs {
            // Legacy scalar maps to `stall.stalled_secs`. Mirror warn to
            // 2/3 of stalled, matching the legacy implicit ladder.
            entry.stall.stalled_secs = t;
            entry.stall.warn_secs = ((t as f64) * (2.0 / 3.0)).round() as u64;
        }
        // Tick interval is engine-global; not stored on the profile.
        let _ = tick_interval_secs;
    }
}

/// Clamp `stall_threshold_secs` to [`crate::state::config::NURSE_MIN_STALL_THRESHOLD_SECS`].
/// Values below the minimum are silently raised with a WARN log.
///
/// Moved here from `core/nurse_service.rs` (v1) so it remains callable
/// from `commands/nurse.rs::set_nurse_config` after v1 is deleted. The
/// v1 file re-exports this symbol via `pub use crate::nurse::config::clamp_stall_threshold;`
/// during the migration so existing call sites continue to compile.
pub fn clamp_stall_threshold(secs: u64) -> u64 {
    use crate::state::config::NURSE_MIN_STALL_THRESHOLD_SECS;
    if secs < NURSE_MIN_STALL_THRESHOLD_SECS {
        tracing::warn!(
            requested = secs,
            clamped_to = NURSE_MIN_STALL_THRESHOLD_SECS,
            "nurse: stall_threshold_secs below minimum, clamping"
        );
        NURSE_MIN_STALL_THRESHOLD_SECS
    } else {
        secs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_for_owner_routes_correctly() {
        assert_eq!(
            NurseProfile::for_owner(&SessionOwner::Task {
                task_id: "t".into()
            }),
            NurseProfile::Tasks
        );
        assert_eq!(
            NurseProfile::for_owner(&SessionOwner::Swarm {
                swarm_id: "s".into(),
                role: "worker".into()
            }),
            NurseProfile::Swarm
        );
        assert_eq!(
            NurseProfile::for_owner(&SessionOwner::Review { job_id: "j".into() }),
            NurseProfile::Hivemind
        );
        assert_eq!(
            NurseProfile::for_owner(&SessionOwner::Merge {
                job_id: "j".into(),
                round: 1,
                swarm_id: None
            }),
            NurseProfile::Hivemind
        );
        assert_eq!(
            NurseProfile::for_owner(&SessionOwner::Unknown),
            NurseProfile::Default
        );
    }

    #[test]
    fn default_config_seeds_every_profile() {
        let c = NurseConfig::default();
        for p in [
            NurseProfile::Tasks,
            NurseProfile::Swarm,
            NurseProfile::Hivemind,
            NurseProfile::Test,
            NurseProfile::Default,
        ] {
            assert!(c.profiles.contains_key(&p), "missing profile: {:?}", p);
        }
    }

    #[test]
    fn swarm_budget_decays_with_age_others_do_not() {
        assert_eq!(BudgetConfig::tasks_default().decay_per_hour, 0);
        assert_eq!(BudgetConfig::hivemind_default().decay_per_hour, 0);
        assert!(BudgetConfig::swarm_default().decay_per_hour >= 1);
    }

    #[test]
    fn swarms_only_defaults_false_on_legacy_blob() {
        // Legacy config blob written before `swarms_only` existed — must
        // round-trip with `swarms_only = false` (current behaviour).
        let raw = r#"{
            "enabled": true,
            "mode": "enabled",
            "nurse_model": null,
            "nurse_provider": null,
            "max_interventions": 3,
            "profiles": {}
        }"#;
        let cfg: NurseConfig = serde_json::from_str(raw).expect("legacy config deserialises");
        assert!(!cfg.swarms_only);
    }

    #[test]
    fn legacy_scalars_seed_default_profile_idempotent() {
        let mut c = NurseConfig::default();
        c.seed_default_profile_from_legacy(Some(240), Some(10));
        let p = c.profile(NurseProfile::Default);
        assert_eq!(p.stall.stalled_secs, 240);
        assert_eq!(p.stall.warn_secs, 160);
    }
}
