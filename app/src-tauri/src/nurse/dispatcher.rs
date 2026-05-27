//! Three-tier Nurse decision dispatcher.
//!
//! The dispatcher is the single funnel through which every per-session
//! `Signal` raise (or external `report_error` / synthesized raise) is run
//! through the Tier 1 / Tier 2 / Tier 3 pipeline and, when admitted,
//! handed to the [`ActionApplier`] for the actual Pi-side effect.
//!
//! ## Lock discipline
//!
//! - `engine.config` is `tokio::sync::RwLock` (async). The dispatcher
//!   reads it ONCE per `handle_signal`, clones, and drops the guard
//!   BEFORE grabbing the sync `engine.sessions` lock — never the other
//!   way round.
//! - `engine.sessions` is `std::sync::RwLock`. Held only across
//!   non-`await` work and dropped before any `.await`. Poison policy is
//!   `unwrap_or_else(|p| p.into_inner())` (project-wide).
//! - `intervention_ctx.in_flight` is `std::sync::Mutex<HashMap<…>>`.
//!   Per-session entries are inserted on admission and removed by
//!   [`InFlightGuard::drop`] on ANY exit path — including timeout
//!   cancellation and panic unwind.
//!
//! ## Self-kill grace
//!
//! When an applier kills a session (Cancel / Restart) and IMMEDIATELY
//! after the kill receives a fresh `process_health/process_dead` raise
//! about that same session id, the dispatcher would otherwise dispatch
//! a second action against a session that is already gone — a "we
//! killed it, now we're killing it again" loop. The applier records the
//! kill in `self_killed`; this dispatcher honours that mark for
//! [`SELF_KILL_GRACE`] and gates the second raise with
//! `decision_finalised{status="gated_self_kill_grace"}`.

use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use futures::FutureExt;
use std::sync::atomic::{AtomicU32, Ordering as AtomicOrdering};
use tauri::AppHandle;

use crate::nurse::budget::{BudgetGateReason, BudgetOutcome};
use crate::nurse::classifier::{ClassifyOutput, LlmClassifier};
use crate::nurse::config::{NurseConfig, NurseProfile};
use crate::nurse::engine::NurseEngine;
use crate::nurse::health::{SessionHealth, Severity, Signal, Tier};
use crate::nurse::intervention::SessionKiller;
use crate::nurse::observability::decision_log::events as devts;
use crate::nurse::observability::decision_log::DecisionLogger;
use crate::nurse::playbook::PlaybookAction;
use crate::nurse::snapshot::{
    NurseActionKind, NurseDecision, NurseDispatchTier, NurseLifecyclePayload, NurseLifecycleStatus,
    SessionOwnerDto,
};
use crate::pi::session::SessionOwner;
use crate::tunables;

/// Grace window during which a fresh `process_dead` / `synthesized:process_crashed`
/// raise about a session we just killed is gated as a duplicate.
pub const SELF_KILL_GRACE: Duration = Duration::from_secs(30);

/// Monotonic per-decision sequence counter. The dispatcher allocates a
/// fresh counter inside every `handle_signal` so the rows for one
/// decision are densely numbered starting at 0 regardless of concurrent
/// decisions on other sessions.
pub struct EventSeq(AtomicU32);

impl Default for EventSeq {
    fn default() -> Self {
        Self::new()
    }
}

impl EventSeq {
    pub fn new() -> Self {
        Self(AtomicU32::new(0))
    }
    /// Allocate and return the next event_seq for this decision.
    pub fn next(&self) -> u32 {
        self.0.fetch_add(1, AtomicOrdering::Relaxed)
    }
    /// Current count (number of events emitted so far). Used to populate
    /// `decision_finalised.num_events_in_chain` AFTER the finalised row
    /// itself is allocated.
    pub fn current(&self) -> u32 {
        self.0.load(AtomicOrdering::Relaxed)
    }
}

/// Tier 3 backend abstraction. Production is `Arc<LlmClassifier>`;
/// dispatcher tests inject a mock to control the classifier outcome
/// without spinning up a real provider.
#[async_trait]
pub trait ClassifierBackend: Send + Sync {
    fn build_prompt(&self, cfg: &NurseConfig, health: &SessionHealth) -> String;
    async fn classify_prepared(
        &self,
        cfg: &NurseConfig,
        health: &SessionHealth,
        prompt: &str,
    ) -> anyhow::Result<Option<ClassifyOutput>>;
}

#[async_trait]
impl ClassifierBackend for Arc<LlmClassifier> {
    fn build_prompt(&self, cfg: &NurseConfig, health: &SessionHealth) -> String {
        LlmClassifier::build_prompt(self, cfg, health)
    }
    async fn classify_prepared(
        &self,
        cfg: &NurseConfig,
        health: &SessionHealth,
        prompt: &str,
    ) -> anyhow::Result<Option<ClassifyOutput>> {
        LlmClassifier::classify_prepared(self, cfg, health, prompt).await
    }
}

/// Where the signal that triggered this dispatch came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchOrigin {
    /// Detector raised a `SignalDelta::Raise`.
    DetectorRaise,
    /// External `report_error` or synthesized raise.
    ReportError,
    /// Periodic watchdog tick noticed a long-active signal.
    Watchdog,
    /// Periodic sweep filtered a candidate signal worth re-dispatching.
    PeriodicSweep,
    /// Batched periodic review (one LLM call across all sessions). The
    /// decision was made by the batch reviewer; Tier 1/2/3 evaluation
    /// is bypassed because the LLM already chose an action.
    BatchReview,
}

impl DispatchOrigin {
    fn as_str(&self) -> &'static str {
        match self {
            Self::DetectorRaise => "detector_raise",
            Self::ReportError => "report_error",
            Self::Watchdog => "watchdog",
            Self::PeriodicSweep => "periodic_sweep",
            Self::BatchReview => "batch_review",
        }
    }
}

/// Input bundle passed to [`Dispatcher::handle_signal`].
pub struct DispatchInput {
    pub decision_id: String,
    pub session_id: String,
    pub trigger_signal: Signal,
    pub origin: DispatchOrigin,
}

/// Outcome of one [`Dispatcher::handle_signal`] call.
#[derive(Debug)]
pub struct DispatchResult {
    pub decision_id: String,
    pub kind: DispatchResultKind,
}

#[derive(Debug)]
pub enum DispatchResultKind {
    Dispatched(NurseDecision, ActionOutcome),
    GatedSeverity,
    GatedDisabled,
    GatedInFlight {
        existing: String,
    },
    GatedPostLag,
    GatedStormGuard,
    GatedBudget(BudgetGateReason),
    GatedSelfKillGrace,
    GatedSwarmsOnly,
    NoSession,
    ClassifierSkippedNoModel,
    ClassifierFailed(String),
    /// Watchdog fast-path returned LeaveIt without consulting tiers.
    FastPathLeaveIt(NurseDecision),
    /// The `NurseEngine` was dropped between dispatcher construction and
    /// dispatch — only happens in shutdown races and (intentionally) in
    /// the `EngineGone` test.
    EngineGone,
    Panic(String),
}

/// Whether the action consumed budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BudgetCharge {
    #[default]
    Charge,
    Free,
}

/// Result of an [`ActionApplier::apply`] call.
#[derive(Debug)]
pub struct ActionOutcome {
    pub budget_charge: BudgetCharge,
    pub completed_status: NurseLifecycleStatus,
    pub outcome_string: String,
    pub lifecycle_payload: NurseLifecyclePayload,
}

/// All the borrowed state an applier needs to perform an action.
pub struct ApplyActionCtx<'a> {
    pub decision: NurseDecision,
    pub session_id: &'a str,
    pub owner: SessionOwner,
    pub decision_id: &'a str,
    pub tier_used: NurseDispatchTier,
    pub decision_logger: &'a Arc<DecisionLogger>,
    pub event_seq: &'a EventSeq,
    pub owner_dto: SessionOwnerDto,
    pub profile: NurseProfile,
    /// `None` when the engine's [`crate::nurse::intervention::InterventionContext`]
    /// hasn't been attached yet (only happens in dispatcher unit tests and
    /// during a sliver of startup before `lib.rs` calls `attach_app_handle`).
    /// Production appliers should treat `None` as a no-op for any Tauri
    /// event emission.
    pub app: Option<&'a AppHandle>,
    pub pi_manager: &'a Arc<dyn SessionKiller>,
    pub self_killed: &'a Arc<Mutex<HashMap<String, Instant>>>,
}

/// Action applier — Step 9 supplies the production impl; dispatcher
/// tests inject a mock.
#[async_trait]
pub trait ActionApplier: Send + Sync {
    async fn apply(&self, ctx: ApplyActionCtx<'_>) -> ActionOutcome;
}

/// Stable identifier for every Tier 1 deterministic table row. Surfaced
/// as `entry_id` on `tier1_evaluated` for post-hoc diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier1EntryId {
    ProcessDead,
    CrashPattern,
    SessionGoneUnobserved,
    NoProvidersConfigured,
    SynthesizedProcessCrashed,
    SynthesizedSchedulerDeadlock,
    RetryDeathLoop,
}

impl Tier1EntryId {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ProcessDead => "process_dead",
            Self::CrashPattern => "crash_pattern",
            Self::SessionGoneUnobserved => "session_gone_unobserved",
            Self::NoProvidersConfigured => "no_providers_configured",
            Self::SynthesizedProcessCrashed => "synthesized_process_crashed",
            Self::SynthesizedSchedulerDeadlock => "synthesized_scheduler_deadlock",
            Self::RetryDeathLoop => "retry_death_loop",
        }
    }
}

/// Reasons a Tier 1 or Tier 3 decision is downgraded for the matched
/// owner kind (e.g. `Restart` is meaningless for a Hivemind session
/// because there is no Pi subprocess to respawn).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DowngradeReason {
    RestartNotMeaningfulForHivemind,
}

impl DowngradeReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::RestartNotMeaningfulForHivemind => "restart_not_meaningful_for_hivemind",
        }
    }
}

/// Pure lookup over the Tier 1 deterministic table.
///
/// Returns `Some((action, entry_id, downgrade_reason))` on hit. The
/// `downgrade_reason` is `Some` when the matched row's natural action
/// was downgraded because of the owner kind (e.g. `Restart` →
/// `Cancel` for `SessionOwner::Review` / `SessionOwner::Merge`).
pub fn tier1_lookup(
    detector: &str,
    dedup_key: &str,
    owner: &SessionOwner,
) -> Option<(NurseActionKind, Tier1EntryId, Option<DowngradeReason>)> {
    let is_hivemind = matches!(
        owner,
        SessionOwner::Review { .. } | SessionOwner::Merge { .. }
    );
    match (detector, dedup_key) {
        ("process_health", "process_dead") => {
            if is_hivemind {
                Some((
                    NurseActionKind::Cancel,
                    Tier1EntryId::ProcessDead,
                    Some(DowngradeReason::RestartNotMeaningfulForHivemind),
                ))
            } else {
                Some((NurseActionKind::Restart, Tier1EntryId::ProcessDead, None))
            }
        }
        ("process_health", "crash_pattern") => {
            Some((NurseActionKind::Cancel, Tier1EntryId::CrashPattern, None))
        }
        ("process_health", "session_gone_unobserved") => Some((
            NurseActionKind::Cancel,
            Tier1EntryId::SessionGoneUnobserved,
            None,
        )),
        ("provider_health", "no_providers_configured") => Some((
            NurseActionKind::Cancel,
            Tier1EntryId::NoProvidersConfigured,
            None,
        )),
        ("synthesized", "synthesized:process_crashed") => {
            if is_hivemind {
                Some((
                    NurseActionKind::Cancel,
                    Tier1EntryId::SynthesizedProcessCrashed,
                    Some(DowngradeReason::RestartNotMeaningfulForHivemind),
                ))
            } else {
                Some((
                    NurseActionKind::Restart,
                    Tier1EntryId::SynthesizedProcessCrashed,
                    None,
                ))
            }
        }
        ("synthesized", key) if key.starts_with("synthesized:scheduler_deadlock:") => Some((
            NurseActionKind::Cancel,
            Tier1EntryId::SynthesizedSchedulerDeadlock,
            None,
        )),
        ("retry_exhaustion", "retry:death_loop") => {
            if is_hivemind {
                Some((
                    NurseActionKind::Cancel,
                    Tier1EntryId::RetryDeathLoop,
                    Some(DowngradeReason::RestartNotMeaningfulForHivemind),
                ))
            } else {
                Some((NurseActionKind::Restart, Tier1EntryId::RetryDeathLoop, None))
            }
        }
        _ => None,
    }
}

/// Fast-path checks bound to `DispatchOrigin::Watchdog`. Ported from the
/// retired v1 `core/nurse_service.rs::evaluate_session_now` short-circuits.
/// The watchdog (frontend `check_chat_session` IPC) fires whenever the
/// user idles past the configured `chat_check_in_secs` — for sessions
/// that are merely waiting on a slow provider call OR actively streaming
/// tokens, calling the Tier 3 LLM classifier wastes tokens AND can
/// cancel a session that's legitimately producing a multi-kilobyte tool
/// call argument in silence.
pub mod fast_path {
    use crate::nurse::health::SessionHealth;
    use crate::pi::session::PiSession;
    use std::sync::Weak;

    /// 10 minutes — past this point, even a "legitimately silent" model
    /// call is treated as a real stall. Mirrors v1's AWAITING_MODEL_HARD_LIMIT_MS.
    const AWAITING_MODEL_HARD_LIMIT_MS: u64 = 10 * 60 * 1000;

    /// Returns `Some((reasoning_string, check_back_secs))` if Pi is
    /// mid-assistant-message AND that await time is under the hard limit.
    /// The caller wraps this in a `FastPathLeaveIt` result.
    pub fn awaiting_model(
        session: &Weak<PiSession>,
        _health: &SessionHealth,
        stall_threshold_secs: u64,
    ) -> Option<(String, u64)> {
        let s = session.upgrade()?;
        let await_ms = s.awaiting_model_for_ms()?;
        if await_ms >= AWAITING_MODEL_HARD_LIMIT_MS {
            return None;
        }
        let reasoning = format!(
            "assistant message in flight for {}s — provider still composing",
            await_ms / 1000
        );
        Some((reasoning, stall_threshold_secs.max(60)))
    }

    /// Returns `Some((reasoning_string, check_back_secs))` if Pi is busy
    /// AND has produced text recently (idle < 1/4 of stall threshold).
    pub fn healthy_streaming(
        session: &Weak<PiSession>,
        _health: &SessionHealth,
        stall_threshold_secs: u64,
    ) -> Option<(String, u64)> {
        let s = session.upgrade()?;
        if !s.is_busy() {
            return None;
        }
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let last_text_ms = s.last_text_event_ms();
        let idle_ms = if last_text_ms == 0 {
            now_ms.saturating_sub(s.last_activity_ms())
        } else {
            now_ms.saturating_sub(last_text_ms)
        };
        let stall_threshold_ms = stall_threshold_secs.saturating_mul(1000);
        if idle_ms >= stall_threshold_ms / 4 {
            return None;
        }
        Some((
            "session actively producing tokens".to_string(),
            stall_threshold_secs.max(60),
        ))
    }
}

/// RAII guard removing the per-session in-flight entry on drop. Held
/// across the entire `handle_signal` body so even panic-unwind or
/// timeout cancellation releases the slot.
pub(crate) struct InFlightGuard {
    map: Arc<Mutex<HashMap<String, String>>>,
    session_id: String,
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        let mut g = self.map.lock().unwrap_or_else(|e| e.into_inner());
        g.remove(&self.session_id);
    }
}

/// The dispatcher itself. Holds `Weak<NurseEngine>` so the engine can
/// be dropped (and unit-tests can construct dispatchers without a real
/// engine via `Weak::new()`).
pub struct Dispatcher {
    engine: Weak<NurseEngine>,
    classifier: Arc<dyn ClassifierBackend>,
    applier: Arc<dyn ActionApplier>,
    pi_manager: Arc<dyn SessionKiller>,
    self_killed: Arc<Mutex<HashMap<String, Instant>>>,
}

impl Dispatcher {
    pub fn new(
        engine: Weak<NurseEngine>,
        classifier: Arc<dyn ClassifierBackend>,
        applier: Arc<dyn ActionApplier>,
        pi_manager: Arc<dyn SessionKiller>,
    ) -> Self {
        Self {
            engine,
            classifier,
            applier,
            pi_manager,
            self_killed: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Shared `self_killed` map — applier impls call
    /// `self_killed.lock().insert(session_id, Instant::now())` immediately
    /// after issuing a kill so the dispatcher can gate the inevitable
    /// follow-up `process_dead` raise.
    pub fn self_killed_handle(&self) -> Arc<Mutex<HashMap<String, Instant>>> {
        Arc::clone(&self.self_killed)
    }

    /// Dispatch a decision that was pre-decided by the batched periodic
    /// reviewer. Bypasses Tier 1/2/3 (the LLM already chose an action)
    /// and the per-(session, dedup_key) storm guard (the batch ticker
    /// is itself rate-limited by `nurse_batch_interval_secs`), but
    /// HONOURS:
    ///   - in_flight gate — never collide with a signal-driven decision
    ///     that's mid-flight on the same session.
    ///   - SELF_KILL_GRACE — never re-kill a session inside the 30s
    ///     post-kill window (the previous kill is still settling).
    ///   - escalation_min_severity — batch decisions count as `Stalled`
    ///     severity by convention; any profile that requires Critical
    ///     skips the batch action.
    ///   - the same applier — same kill-verification, same nurse-event
    ///     emission, same budget charge bookkeeping.
    ///
    /// Records the full decision chain on disk
    /// (`decision_started{origin:"batch_review"}` →
    ///  `intervention_dispatched` → `decision_finalised`) so post-hoc
    /// diagnosis is identical to signal-driven decisions.
    pub async fn dispatch_batch_decision(
        &self,
        session_id: &str,
        profile_hint: NurseProfile,
        decision: NurseDecision,
        reasoning: Option<String>,
    ) -> DispatchResult {
        let decision_id = uuid::Uuid::new_v4().simple().to_string();
        let started = Instant::now();
        let event_seq = EventSeq::new();
        let session_id_owned = session_id.to_string();
        let _ = profile_hint; // resolved from engine below; hint is for callers that already know

        // Upgrade engine; nothing to do if it's gone.
        let Some(engine) = self.engine.upgrade() else {
            return DispatchResult {
                decision_id,
                kind: DispatchResultKind::EngineGone,
            };
        };
        let decisions_logger = Arc::clone(&engine.observability.decisions);

        // Resolve owner / profile FIRST so envelope is populated for
        // every subsequent log row. (Inverted vs handle_signal which can
        // log decision_started before lookup because it has a real
        // trigger_signal with detector/severity to record up front.)
        let nurse_cfg = engine.config.read().await.clone();

        // Engine-disabled gate — log an opener + finaliser even though we
        // never look at the session.
        if !nurse_cfg.enabled {
            decisions_logger.write(devts::decision_started(
                &decision_id,
                &Some(session_id_owned.clone()),
                &None,
                &None,
                event_seq.next(),
                DispatchOrigin::BatchReview.as_str(),
                "stalled",
                "batch_review",
                Severity::Stalled,
                "batch_review:disabled",
            ));
            let final_seq = event_seq.next();
            let chain_len = event_seq.current();
            decisions_logger.write(devts::decision_finalised(
                &decision_id,
                &Some(session_id_owned),
                &None,
                &None,
                final_seq,
                "gated_disabled",
                started.elapsed().as_millis() as u64,
                chain_len,
                serde_json::json!({}),
            ));
            return DispatchResult {
                decision_id,
                kind: DispatchResultKind::GatedDisabled,
            };
        }

        let owner = {
            let sessions = engine.sessions.read().unwrap_or_else(|p| p.into_inner());
            match sessions.get(&session_id_owned) {
                Some(state) => state.owner.clone(),
                None => {
                    // No session — log opener + finaliser and return.
                    decisions_logger.write(devts::decision_started(
                        &decision_id,
                        &Some(session_id_owned.clone()),
                        &None,
                        &None,
                        event_seq.next(),
                        DispatchOrigin::BatchReview.as_str(),
                        "stalled",
                        "batch_review",
                        Severity::Stalled,
                        "batch_review:no_session",
                    ));
                    let final_seq = event_seq.next();
                    let chain_len = event_seq.current();
                    decisions_logger.write(devts::decision_finalised(
                        &decision_id,
                        &Some(session_id_owned),
                        &None,
                        &None,
                        final_seq,
                        "no_session",
                        started.elapsed().as_millis() as u64,
                        chain_len,
                        serde_json::json!({}),
                    ));
                    return DispatchResult {
                        decision_id,
                        kind: DispatchResultKind::NoSession,
                    };
                }
            }
        };
        let profile = NurseProfile::for_owner(&owner);
        let profile_config = nurse_cfg.profile(profile);
        let owner_dto = SessionOwnerDto::from(&owner);
        let profile_str_owned = profile_str(profile);
        let env_session_id: Option<String> = Some(session_id_owned.clone());
        let env_owner_dto: Option<SessionOwnerDto> = Some(owner_dto.clone());
        let env_profile_str: Option<String> = Some(profile_str_owned.clone());

        // Swarms-only gate. Mirrors the gate inside `run_pipeline` but
        // emits the full `decision_started` → `decision_finalised` pair
        // so the batch-review chain is complete on disk for analytics.
        if nurse_cfg.swarms_only && !matches!(owner, SessionOwner::Swarm { .. }) {
            decisions_logger.write(devts::decision_started(
                &decision_id,
                &env_session_id,
                &env_owner_dto,
                &env_profile_str,
                event_seq.next(),
                DispatchOrigin::BatchReview.as_str(),
                "stalled",
                "batch_review",
                Severity::Stalled,
                "batch_review:swarms_only",
            ));
            let final_seq = event_seq.next();
            let chain_len = event_seq.current();
            decisions_logger.write(devts::decision_finalised(
                &decision_id,
                &env_session_id,
                &env_owner_dto,
                &env_profile_str,
                final_seq,
                "gated_swarms_only",
                started.elapsed().as_millis() as u64,
                chain_len,
                serde_json::json!({"owner_kind": owner_dto.kind_str()}),
            ));
            return DispatchResult {
                decision_id,
                kind: DispatchResultKind::GatedSwarmsOnly,
            };
        }

        // Anchor the chain — now with full envelope populated.
        let synthetic_reason = reasoning.clone().unwrap_or_else(|| "batch review".into());
        decisions_logger.write(devts::decision_started(
            &decision_id,
            &env_session_id,
            &env_owner_dto,
            &env_profile_str,
            event_seq.next(),
            DispatchOrigin::BatchReview.as_str(),
            "stalled",
            "batch_review",
            Severity::Stalled,
            &format!("batch_review:{}", kind_to_str(decision_to_kind(&decision))),
        ));

        let finalize = |status: &str, extra: serde_json::Value, kind: DispatchResultKind| {
            let final_seq = event_seq.next();
            let chain_len = event_seq.current();
            decisions_logger.write(devts::decision_finalised(
                &decision_id,
                &env_session_id,
                &env_owner_dto,
                &env_profile_str,
                final_seq,
                status,
                started.elapsed().as_millis() as u64,
                chain_len,
                extra,
            ));
            DispatchResult {
                decision_id: decision_id.clone(),
                kind,
            }
        };

        // Severity gate using the synthetic Stalled level.
        if Severity::Stalled < profile_config.escalation_min_severity {
            return finalize(
                "gated_severity",
                serde_json::json!({"min_required": profile_config.escalation_min_severity}),
                DispatchResultKind::GatedSeverity,
            );
        }

        // Self-kill grace gate for kill-class actions only. Batch
        // reviewer can't reach the keyed `is_self_kill_repeat` because
        // it has no detector/dedup_key to match, so check the timestamp
        // directly.
        prune_self_killed(&self.self_killed);
        if matches!(
            decision,
            NurseDecision::Cancel { .. } | NurseDecision::Restart { .. }
        ) {
            let in_grace = {
                let guard = self.self_killed.lock().unwrap_or_else(|e| e.into_inner());
                matches!(
                    guard.get(&session_id_owned),
                    Some(&ts) if Instant::now() < ts + SELF_KILL_GRACE
                )
            };
            if in_grace {
                return finalize(
                    "gated_self_kill_grace",
                    serde_json::json!({}),
                    DispatchResultKind::GatedSelfKillGrace,
                );
            }
        }

        // In-flight gate — never collide with a signal-driven decision.
        // The InFlightGuard is held for the rest of this fn so a panic
        // inside `applier.apply` still releases the slot.
        let _guard = {
            let mut in_flight = engine.in_flight.lock().unwrap_or_else(|e| e.into_inner());
            use std::collections::hash_map::Entry;
            match in_flight.entry(session_id_owned.clone()) {
                Entry::Occupied(occ) => {
                    let existing = occ.get().clone();
                    drop(in_flight);
                    return finalize(
                        "gated_in_flight",
                        serde_json::json!({"existing_decision_id": existing}),
                        DispatchResultKind::GatedInFlight { existing },
                    );
                }
                Entry::Vacant(vac) => {
                    vac.insert(decision_id.clone());
                }
            }
            InFlightGuard {
                map: Arc::clone(&engine.in_flight),
                session_id: session_id_owned.clone(),
            }
        };

        // Apply.
        let attached_ctx = engine.intervention_ctx.get();
        let app_ref: Option<&AppHandle> = attached_ctx.map(|c| &c.app);
        let tier_used = NurseDispatchTier::Llm;
        let outcome = self
            .applier
            .apply(ApplyActionCtx {
                decision: decision.clone(),
                session_id: &session_id_owned,
                owner: owner.clone(),
                decision_id: &decision_id,
                tier_used,
                decision_logger: &decisions_logger,
                event_seq: &event_seq,
                owner_dto: owner_dto.clone(),
                profile,
                app: app_ref,
                pi_manager: &self.pi_manager,
                self_killed: &self.self_killed,
            })
            .await;

        // Budget-charge bookkeeping. Treat batch decisions as
        // detector="batch_review", dedup_key="batch_review" so they share
        // a single per-detector quota in `BudgetState`.
        if outcome.budget_charge == BudgetCharge::Charge {
            let mut sessions = engine.sessions.write().unwrap_or_else(|p| p.into_inner());
            if let Some(state) = sessions.get_mut(&session_id_owned) {
                state
                    .budget
                    .record("batch_review", "batch_review", Instant::now());
                state.intervention_count = state.intervention_count.saturating_add(1);
            }
        }

        decisions_logger.write(devts::intervention_dispatched(
            &decision_id,
            &env_session_id,
            &env_owner_dto,
            &env_profile_str,
            event_seq.next(),
            tier_to_str(tier_used),
            kind_to_str(decision_to_kind(&decision)),
            &outcome.outcome_string,
        ));

        let record = crate::nurse::intervention::record_from_payload(&outcome.lifecycle_payload);
        engine.intervention_writer.send(record);

        finalize(
            "dispatched",
            serde_json::json!({
                "tier_used": tier_to_str(tier_used),
                "action": kind_to_str(decision_to_kind(&decision)),
                "reasoning": synthetic_reason,
            }),
            DispatchResultKind::Dispatched(decision, outcome),
        )
    }

    /// Run one `(session_id, trigger_signal)` through the pipeline.
    pub async fn handle_signal(&self, input: DispatchInput) -> DispatchResult {
        let DispatchInput {
            decision_id,
            session_id,
            trigger_signal,
            origin,
        } = input;
        let started = Instant::now();
        let event_seq = EventSeq::new();

        let classifier = Arc::clone(&self.classifier);
        let applier = Arc::clone(&self.applier);
        let pi_manager = Arc::clone(&self.pi_manager);
        let self_killed = Arc::clone(&self.self_killed);
        let engine_weak = self.engine.clone();

        // Cap on the full pipeline runtime so a hung classifier or stuck
        // applier can never wedge a session_id in_flight forever. The
        // worst-case budget is min(5 * provider timeout, 600s).
        let timeout_secs = (5 * tunables::nurse_provider_timeout_secs()).min(600);
        let timeout = Duration::from_secs(timeout_secs);

        let dec_id_for_panic = decision_id.clone();
        let sid_for_panic = session_id.clone();

        let work = async move {
            run_pipeline(
                engine_weak,
                classifier,
                applier,
                pi_manager,
                self_killed,
                decision_id,
                session_id,
                trigger_signal,
                origin,
                started,
                event_seq,
            )
            .await
        };

        // catch_unwind around the entire body so a panic in any inner
        // step releases the InFlightGuard via the future being dropped
        // and surfaces as DispatchResultKind::Panic rather than
        // poisoning the broader engine task.
        match tokio::time::timeout(timeout, AssertUnwindSafe(work).catch_unwind()).await {
            Ok(Ok(result)) => result,
            Ok(Err(panic)) => {
                let msg = panic_message(panic);
                tracing::error!(
                    decision_id = %dec_id_for_panic,
                    session_id = %sid_for_panic,
                    panic = %msg,
                    "nurse dispatcher pipeline panicked"
                );
                DispatchResult {
                    decision_id: dec_id_for_panic,
                    kind: DispatchResultKind::Panic(msg),
                }
            }
            Err(_) => {
                tracing::error!(
                    decision_id = %dec_id_for_panic,
                    session_id = %sid_for_panic,
                    timeout_secs,
                    "nurse dispatcher pipeline timed out"
                );
                DispatchResult {
                    decision_id: dec_id_for_panic,
                    kind: DispatchResultKind::Panic(format!(
                        "dispatcher pipeline exceeded {}s budget",
                        timeout_secs
                    )),
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_pipeline(
    engine_weak: Weak<NurseEngine>,
    classifier: Arc<dyn ClassifierBackend>,
    applier: Arc<dyn ActionApplier>,
    pi_manager: Arc<dyn SessionKiller>,
    self_killed: Arc<Mutex<HashMap<String, Instant>>>,
    decision_id: String,
    session_id: String,
    trigger_signal: Signal,
    origin: DispatchOrigin,
    started: Instant,
    event_seq: EventSeq,
) -> DispatchResult {
    tracing::debug!(
        decision_id = %decision_id,
        session_id = %session_id,
        detector = trigger_signal.detector,
        dedup_key = %trigger_signal.dedup_key,
        severity = ?trigger_signal.severity,
        origin = ?origin,
        "nurse dispatcher: pipeline entry"
    );

    // ── Step 0: emit decision_started immediately so the chain is
    //           always anchored on disk regardless of which gate fires.
    let Some(engine) = engine_weak.upgrade() else {
        tracing::warn!(decision_id = %decision_id, "nurse dispatcher: engine gone");
        return DispatchResult {
            decision_id,
            kind: DispatchResultKind::EngineGone,
        };
    };
    let decisions_logger = Arc::clone(&engine.observability.decisions);

    // Envelope fields — owner / profile populated once we resolve them
    // from the session lookup in Step 1.
    let mut env_session_id: Option<String> = Some(session_id.clone());
    let mut env_owner_dto: Option<SessionOwnerDto> = None;
    let mut env_profile_str: Option<String> = None;

    decisions_logger.write(devts::decision_started(
        &decision_id,
        &env_session_id,
        &env_owner_dto,
        &env_profile_str,
        event_seq.next(),
        origin.as_str(),
        severity_tier_str(trigger_signal.severity),
        trigger_signal.detector,
        trigger_signal.severity,
        &trigger_signal.dedup_key,
    ));

    // Helper to emit decision_finalised + return — used on every gate.
    let finalize_and_return = |logger: &Arc<DecisionLogger>,
                               env_sid: &Option<String>,
                               env_own: &Option<SessionOwnerDto>,
                               env_prof: &Option<String>,
                               seq: &EventSeq,
                               status: &str,
                               extra: serde_json::Value,
                               kind: DispatchResultKind|
     -> DispatchResult {
        let final_seq = seq.next();
        let chain_len = seq.current();
        logger.write(devts::decision_finalised(
            &decision_id,
            env_sid,
            env_own,
            env_prof,
            final_seq,
            status,
            started.elapsed().as_millis() as u64,
            chain_len,
            extra,
        ));
        DispatchResult {
            decision_id: decision_id.clone(),
            kind,
        }
    };

    // ── Step 1: snapshot config, then session.
    let nurse_cfg = engine.config.read().await.clone();
    if !nurse_cfg.enabled {
        return finalize_and_return(
            &decisions_logger,
            &env_session_id,
            &env_owner_dto,
            &env_profile_str,
            &event_seq,
            "gated_disabled",
            serde_json::json!({}),
            DispatchResultKind::GatedDisabled,
        );
    }

    let (owner, health_snap, post_lag_until): (SessionOwner, SessionHealth, Option<Instant>) = {
        let sessions = engine.sessions.read().unwrap_or_else(|p| p.into_inner());
        let Some(state) = sessions.get(&session_id) else {
            return finalize_and_return(
                &decisions_logger,
                &env_session_id,
                &env_owner_dto,
                &env_profile_str,
                &event_seq,
                "no_session",
                serde_json::json!({}),
                DispatchResultKind::NoSession,
            );
        };
        (
            state.owner.clone(),
            state.health.clone(),
            state.post_lag_until,
        )
    };

    let profile = NurseProfile::for_owner(&owner);
    let profile_config = nurse_cfg.profile(profile);
    let owner_dto = SessionOwnerDto::from(&owner);
    let profile_str = profile_str(profile);
    env_session_id = Some(session_id.clone());
    env_owner_dto = Some(owner_dto.clone());
    env_profile_str = Some(profile_str.clone());

    // ── Step 1.5: swarms-only gate. When the user opts into "Swarms
    //           only" mode the dispatcher suppresses every intervention
    //           whose owner isn't a swarm agent. Detectors keep running
    //           so the per-session timeline still populates — we just
    //           never act.
    if nurse_cfg.swarms_only && !matches!(owner, SessionOwner::Swarm { .. }) {
        return finalize_and_return(
            &decisions_logger,
            &env_session_id,
            &env_owner_dto,
            &env_profile_str,
            &event_seq,
            "gated_swarms_only",
            serde_json::json!({"owner_kind": owner_dto.kind_str()}),
            DispatchResultKind::GatedSwarmsOnly,
        );
    }

    // ── Step 2: severity gate.
    if trigger_signal.severity < profile_config.escalation_min_severity {
        return finalize_and_return(
            &decisions_logger,
            &env_session_id,
            &env_owner_dto,
            &env_profile_str,
            &event_seq,
            "gated_severity",
            serde_json::json!({
                "min_required": profile_config.escalation_min_severity,
            }),
            DispatchResultKind::GatedSeverity,
        );
    }

    // ── Step 3: self-kill grace window.
    prune_self_killed(&self_killed);
    if is_self_kill_repeat(
        &self_killed,
        &session_id,
        &trigger_signal.detector,
        &trigger_signal.dedup_key,
    ) {
        let killed_at_ms = self_killed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&session_id)
            .map(|t| instant_to_unix_ms(*t))
            .unwrap_or(0);
        return finalize_and_return(
            &decisions_logger,
            &env_session_id,
            &env_owner_dto,
            &env_profile_str,
            &event_seq,
            "gated_self_kill_grace",
            serde_json::json!({"killed_at_unix_ms": killed_at_ms}),
            DispatchResultKind::GatedSelfKillGrace,
        );
    }

    // ── Step 4: in-flight gate. The InFlightGuard MUST be constructed
    //           before any further await so a panic releases the slot.
    //           The in-flight map lives on the engine, NOT on
    //           InterventionContext — it must work even before the
    //           AppHandle is attached.
    let _guard = {
        let mut in_flight = engine.in_flight.lock().unwrap_or_else(|e| e.into_inner());
        use std::collections::hash_map::Entry;
        match in_flight.entry(session_id.clone()) {
            Entry::Occupied(occ) => {
                let existing = occ.get().clone();
                drop(in_flight);
                return finalize_and_return(
                    &decisions_logger,
                    &env_session_id,
                    &env_owner_dto,
                    &env_profile_str,
                    &event_seq,
                    "gated_in_flight",
                    serde_json::json!({"existing_decision_id": existing}),
                    DispatchResultKind::GatedInFlight { existing },
                );
            }
            Entry::Vacant(vac) => {
                vac.insert(decision_id.clone());
            }
        }
        InFlightGuard {
            map: Arc::clone(&engine.in_flight),
            session_id: session_id.clone(),
        }
    };

    // Optional AppHandle-bound context. `None` is OK in dispatcher unit
    // tests — the mock applier doesn't emit Tauri events. In production
    // `intervention_ctx` is attached during `lib.rs::setup` before the
    // engine ever sees a real signal.
    let attached_ctx = engine.intervention_ctx.get();

    // ── Step 5: storm guard (Tier 1 + Critical bypass it).
    let tier1_hit = tier1_lookup(&trigger_signal.detector, &trigger_signal.dedup_key, &owner);
    let critical = trigger_signal.severity == Severity::Critical;
    if critical {
        decisions_logger.write(devts::storm_guard_evaluated(
            &decision_id,
            &env_session_id,
            &env_owner_dto,
            &env_profile_str,
            event_seq.next(),
            "bypassed_critical",
            None,
            None,
        ));
    } else if tier1_hit.is_some() {
        decisions_logger.write(devts::storm_guard_evaluated(
            &decision_id,
            &env_session_id,
            &env_owner_dto,
            &env_profile_str,
            event_seq.next(),
            "bypassed_tier1",
            None,
            None,
        ));
    } else {
        use crate::nurse::storm_guard::StormGuardOutcome;
        match engine
            .storm_guard
            .try_admit(&session_id, &trigger_signal.dedup_key)
        {
            StormGuardOutcome::Passed => {
                decisions_logger.write(devts::storm_guard_evaluated(
                    &decision_id,
                    &env_session_id,
                    &env_owner_dto,
                    &env_profile_str,
                    event_seq.next(),
                    "passed",
                    None,
                    None,
                ));
            }
            StormGuardOutcome::Gated {
                recent_count,
                skip_until_unix_ms,
                ..
            } => {
                decisions_logger.write(devts::storm_guard_evaluated(
                    &decision_id,
                    &env_session_id,
                    &env_owner_dto,
                    &env_profile_str,
                    event_seq.next(),
                    "gated",
                    Some(recent_count),
                    Some(skip_until_unix_ms),
                ));
                return finalize_and_return(
                    &decisions_logger,
                    &env_session_id,
                    &env_owner_dto,
                    &env_profile_str,
                    &event_seq,
                    "gated_storm_guard",
                    serde_json::json!({
                        "recent_count": recent_count,
                        "skip_until_unix_ms": skip_until_unix_ms,
                    }),
                    DispatchResultKind::GatedStormGuard,
                );
            }
        }
    }

    // ── Step 6: Watchdog fast-paths (only when origin == Watchdog).
    //
    // The frontend's `check_chat_session` IPC dispatches with
    // origin=Watchdog at the chat_check_in_secs cadence. Most of those
    // checks fire on sessions that are healthy (mid-stream or awaiting
    // a slow model call). We don't want to spend Tier 3 tokens on those
    // — short-circuit to LeaveIt with a sensible check_back.
    if origin == DispatchOrigin::Watchdog {
        // Look up the live Pi session via the engine.sessions map.
        let session_weak = {
            let sessions = engine.sessions.read().unwrap_or_else(|p| p.into_inner());
            sessions.get(&session_id).map(|s| s.session.clone())
        };
        if let Some(weak) = session_weak {
            // Resolve the stall threshold from the profile config
            // (already in scope).
            let stall_threshold_secs = profile_config.stall.stalled_secs;

            if let Some((reasoning, check_back_secs)) =
                fast_path::awaiting_model(&weak, &health_snap, stall_threshold_secs)
            {
                let final_seq = event_seq.next();
                let chain_len = event_seq.current();
                decisions_logger.write(devts::decision_finalised(
                    &decision_id,
                    &env_session_id,
                    &env_owner_dto,
                    &env_profile_str,
                    final_seq,
                    "fast_path_awaiting_model",
                    started.elapsed().as_millis() as u64,
                    chain_len,
                    serde_json::json!({
                        "reasoning": &reasoning,
                        "check_back_secs": check_back_secs,
                    }),
                ));
                return DispatchResult {
                    decision_id,
                    kind: DispatchResultKind::FastPathLeaveIt(NurseDecision::LeaveIt {
                        reasoning,
                        check_back_secs,
                        observation: None,
                        action: None,
                    }),
                };
            }

            if let Some((reasoning, check_back_secs)) =
                fast_path::healthy_streaming(&weak, &health_snap, stall_threshold_secs)
            {
                let final_seq = event_seq.next();
                let chain_len = event_seq.current();
                decisions_logger.write(devts::decision_finalised(
                    &decision_id,
                    &env_session_id,
                    &env_owner_dto,
                    &env_profile_str,
                    final_seq,
                    "fast_path_healthy_streaming",
                    started.elapsed().as_millis() as u64,
                    chain_len,
                    serde_json::json!({
                        "reasoning": &reasoning,
                        "check_back_secs": check_back_secs,
                    }),
                ));
                return DispatchResult {
                    decision_id,
                    kind: DispatchResultKind::FastPathLeaveIt(NurseDecision::LeaveIt {
                        reasoning,
                        check_back_secs,
                        observation: None,
                        action: None,
                    }),
                };
            }
        }
        // No fast-path matched — fall through to normal pipeline.
    }

    // ── Step 7: Tier 1.
    let decision: NurseDecision;
    let tier_used: NurseDispatchTier;
    if let Some((action, entry_id, downgrade)) = tier1_hit {
        decisions_logger.write(devts::tier1_evaluated(
            &decision_id,
            &env_session_id,
            &env_owner_dto,
            &env_profile_str,
            event_seq.next(),
            true,
            Some(kind_to_str(action)),
            Some(entry_id.as_str()),
            downgrade.map(|d| d.as_str()),
        ));
        decision = match action {
            NurseActionKind::LeaveIt => NurseDecision::LeaveIt {
                reasoning: "tier1 deterministic".to_string(),
                check_back_secs: 60,
                observation: None,
                action: None,
            },
            NurseActionKind::Steer => NurseDecision::Steer {
                reasoning: format!("tier1: {}", entry_id.as_str()),
                message: String::new(),
                observation: None,
                action: None,
            },
            NurseActionKind::Restart => NurseDecision::Restart {
                reasoning: format!("tier1: {}", entry_id.as_str()),
                observation: None,
                action: None,
            },
            NurseActionKind::Cancel => NurseDecision::Cancel {
                reasoning: format!("tier1: {}", entry_id.as_str()),
                message: String::new(),
                observation: None,
                action: None,
            },
        };
        tier_used = NurseDispatchTier::Deterministic;
    } else {
        decisions_logger.write(devts::tier1_evaluated(
            &decision_id,
            &env_session_id,
            &env_owner_dto,
            &env_profile_str,
            event_seq.next(),
            false,
            None,
            None,
            None,
        ));

        // ── Step 8: post-lag gate, then Tier 2, then Tier 3.
        if let Some(until) = post_lag_until {
            if Instant::now() < until && !critical {
                return finalize_and_return(
                    &decisions_logger,
                    &env_session_id,
                    &env_owner_dto,
                    &env_profile_str,
                    &event_seq,
                    "gated_post_lag",
                    serde_json::json!({
                        "until_unix_ms": instant_to_unix_ms(until),
                    }),
                    DispatchResultKind::GatedPostLag,
                );
            }
        }

        // Tier 2 playbook.
        if let Some(entry) = engine
            .playbook
            .lookup(&trigger_signal.detector, &trigger_signal.dedup_key)
        {
            decisions_logger.write(devts::playbook_evaluated(
                &decision_id,
                &env_session_id,
                &env_owner_dto,
                &env_profile_str,
                event_seq.next(),
                true,
                Some(entry.key_prefix),
                Some(entry.rationale),
            ));
            decision = match &entry.action {
                PlaybookAction::Steer { message } => NurseDecision::Steer {
                    reasoning: "tier2 playbook".to_string(),
                    message: message.clone(),
                    observation: None,
                    action: None,
                },
                PlaybookAction::LeaveIt { check_back_secs } => NurseDecision::LeaveIt {
                    reasoning: "tier2 playbook".to_string(),
                    check_back_secs: *check_back_secs,
                    observation: None,
                    action: None,
                },
            };
            tier_used = NurseDispatchTier::Templated;
        } else {
            decisions_logger.write(devts::playbook_evaluated(
                &decision_id,
                &env_session_id,
                &env_owner_dto,
                &env_profile_str,
                event_seq.next(),
                false,
                None,
                None,
            ));

            // ── Tier 3 LLM classifier.
            let effective_model = nurse_cfg.effective_model(profile);
            let Some(_model) = effective_model.clone() else {
                engine
                    .health
                    .tier3_skipped_no_model
                    .fetch_add(1, AtomicOrdering::Relaxed);
                return finalize_and_return(
                    &decisions_logger,
                    &env_session_id,
                    &env_owner_dto,
                    &env_profile_str,
                    &event_seq,
                    "classifier_skipped_no_model",
                    serde_json::json!({}),
                    DispatchResultKind::ClassifierSkippedNoModel,
                );
            };
            let effective_provider = nurse_cfg
                .effective_provider(profile)
                .unwrap_or_else(|| "unknown".to_string());

            decisions_logger.write(devts::classifier_invoked(
                &decision_id,
                &env_session_id,
                &env_owner_dto,
                &env_profile_str,
                event_seq.next(),
                &effective_provider,
                effective_model.as_deref().unwrap_or(""),
            ));

            let prompt = classifier.build_prompt(&nurse_cfg, &health_snap);

            // Capture prompt synchronously off the runtime so a mid-call
            // crash leaves it on disk.
            let captures_for_prompt = Arc::clone(&engine.observability.captures);
            let did_clone = decision_id.clone();
            let prompt_clone = prompt.clone();
            let _ = tokio::task::spawn_blocking(move || {
                if let Err(e) = captures_for_prompt.write_prompt_sync(&did_clone, &prompt_clone) {
                    tracing::warn!(error = %e, "nurse dispatcher: prompt capture failed");
                }
            })
            .await;

            let classifier_result = classifier
                .classify_prepared(&nurse_cfg, &health_snap, &prompt)
                .await;

            match classifier_result {
                Err(e) => {
                    // If this was a parse failure (provider call succeeded but
                    // the response couldn't be decoded into a NurseDecision),
                    // emit a richer `classifier_returned_unparseable` event
                    // that carries the cache stats + raw_len so the user can
                    // still see whether the call hit the prompt cache, and
                    // best-effort write the raw response to the capture file
                    // so they can inspect what came back.
                    if let Some(pf) =
                        e.downcast_ref::<crate::nurse::classifier::ClassifierParseFailure>()
                    {
                        let _ = engine
                            .observability
                            .captures
                            .write_response(&decision_id, &pf.raw)
                            .await;
                        decisions_logger.write(devts::classifier_returned_unparseable(
                            &decision_id,
                            &env_session_id,
                            &env_owner_dto,
                            &env_profile_str,
                            event_seq.next(),
                            &pf.provider,
                            &pf.model,
                            pf.duration_ms,
                            pf.raw.len(),
                            &pf.parse_error,
                            pf.cache_hit_tokens,
                            pf.cache_write_tokens,
                        ));
                    }
                    return finalize_and_return(
                        &decisions_logger,
                        &env_session_id,
                        &env_owner_dto,
                        &env_profile_str,
                        &event_seq,
                        "classifier_failed",
                        serde_json::json!({"error": e.to_string()}),
                        DispatchResultKind::ClassifierFailed(e.to_string()),
                    );
                }
                Ok(None) => {
                    engine
                        .health
                        .tier3_skipped_no_model
                        .fetch_add(1, AtomicOrdering::Relaxed);
                    return finalize_and_return(
                        &decisions_logger,
                        &env_session_id,
                        &env_owner_dto,
                        &env_profile_str,
                        &event_seq,
                        "classifier_skipped_no_model",
                        serde_json::json!({}),
                        DispatchResultKind::ClassifierSkippedNoModel,
                    );
                }
                Ok(Some(out)) => {
                    // Best-effort response write.
                    let _ = engine
                        .observability
                        .captures
                        .write_response(&decision_id, &out.raw_response)
                        .await;
                    decisions_logger.write(devts::classifier_returned(
                        &decision_id,
                        &env_session_id,
                        &env_owner_dto,
                        &env_profile_str,
                        event_seq.next(),
                        kind_to_str(decision_to_kind(&out.decision)),
                        None,
                        None,
                        None,
                        out.duration_ms,
                        &out.provider,
                        &out.model,
                        out.cache_hit_tokens,
                        out.cache_write_tokens,
                    ));

                    // Tier 3 owner-aware downgrade: Restart → Cancel on
                    // Hivemind sessions because there is no Pi process to
                    // respawn.
                    let is_hivemind = matches!(
                        owner,
                        SessionOwner::Review { .. } | SessionOwner::Merge { .. }
                    );
                    if matches!(out.decision, NurseDecision::Restart { .. }) && is_hivemind {
                        let original_reasoning = out.decision.reasoning().to_string();
                        decision = NurseDecision::Cancel {
                            reasoning: format!("downgraded from restart: {}", original_reasoning),
                            message: String::new(),
                            observation: None,
                            action: None,
                        };
                        decisions_logger.write(devts::classifier_decision_downgraded(
                            &decision_id,
                            &env_session_id,
                            &env_owner_dto,
                            &env_profile_str,
                            event_seq.next(),
                            DowngradeReason::RestartNotMeaningfulForHivemind.as_str(),
                        ));
                    } else {
                        decision = out.decision;
                    }
                    tier_used = NurseDispatchTier::Llm;
                }
            }
        }
    }

    // ── Step 9: budget gate (action-aware).
    let detector_str: String = trigger_signal.detector.to_string();
    let dedup_str: String = trigger_signal.dedup_key.clone();
    let admitted_budget_meta: Option<(u32, u32, u32, u32)>;
    if matches!(decision, NurseDecision::LeaveIt { .. }) {
        decisions_logger.write(devts::budget_evaluated(
            &decision_id,
            &env_session_id,
            &env_owner_dto,
            &env_profile_str,
            event_seq.next(),
            "skipped_leave_it",
            None,
            None,
            None,
            None,
            None,
        ));
        admitted_budget_meta = None;
    } else {
        let outcome = {
            let mut sessions = engine.sessions.write().unwrap_or_else(|p| p.into_inner());
            match sessions.get_mut(&session_id) {
                Some(state) => state.budget.try_admit(
                    &profile_config.budget,
                    &detector_str,
                    &dedup_str,
                    Instant::now(),
                ),
                None => {
                    drop(sessions);
                    return finalize_and_return(
                        &decisions_logger,
                        &env_session_id,
                        &env_owner_dto,
                        &env_profile_str,
                        &event_seq,
                        "no_session",
                        serde_json::json!({}),
                        DispatchResultKind::NoSession,
                    );
                }
            }
        };
        match outcome {
            BudgetOutcome::Allowed {
                lifetime_used,
                lifetime_cap,
                per_detector_used,
                per_detector_cap,
            } => {
                decisions_logger.write(devts::budget_evaluated(
                    &decision_id,
                    &env_session_id,
                    &env_owner_dto,
                    &env_profile_str,
                    event_seq.next(),
                    "allowed",
                    None,
                    Some(lifetime_used),
                    Some(lifetime_cap),
                    Some(per_detector_used),
                    Some(per_detector_cap),
                ));
                admitted_budget_meta = Some((
                    lifetime_used,
                    lifetime_cap,
                    per_detector_used,
                    per_detector_cap,
                ));
            }
            BudgetOutcome::Gated(reason) => {
                let reason_str = budget_gate_reason_str(&reason);
                decisions_logger.write(devts::budget_evaluated(
                    &decision_id,
                    &env_session_id,
                    &env_owner_dto,
                    &env_profile_str,
                    event_seq.next(),
                    "gated",
                    Some(reason_str),
                    None,
                    None,
                    None,
                    None,
                ));
                return finalize_and_return(
                    &decisions_logger,
                    &env_session_id,
                    &env_owner_dto,
                    &env_profile_str,
                    &event_seq,
                    "gated_budget",
                    serde_json::json!({"reason": reason_str}),
                    DispatchResultKind::GatedBudget(reason),
                );
            }
        }
    }
    let _ = admitted_budget_meta;

    // ── Step 10: apply.
    let app_ref: Option<&AppHandle> = attached_ctx.map(|c| &c.app);
    let outcome = applier
        .apply(ApplyActionCtx {
            decision: decision.clone(),
            session_id: &session_id,
            owner: owner.clone(),
            decision_id: &decision_id,
            tier_used,
            decision_logger: &decisions_logger,
            event_seq: &event_seq,
            owner_dto: owner_dto.clone(),
            profile,
            app: app_ref,
            pi_manager: &pi_manager,
            self_killed: &self_killed,
        })
        .await;

    // ── Step 11: outcome accounting.
    if outcome.budget_charge == BudgetCharge::Charge {
        let mut sessions = engine.sessions.write().unwrap_or_else(|p| p.into_inner());
        if let Some(state) = sessions.get_mut(&session_id) {
            state
                .budget
                .record(&detector_str, &dedup_str, Instant::now());
            state.intervention_count = state.intervention_count.saturating_add(1);
        }
    }

    decisions_logger.write(devts::intervention_dispatched(
        &decision_id,
        &env_session_id,
        &env_owner_dto,
        &env_profile_str,
        event_seq.next(),
        tier_to_str(tier_used),
        kind_to_str(decision_to_kind(&decision)),
        &outcome.outcome_string,
    ));

    let record = crate::nurse::intervention::record_from_payload(&outcome.lifecycle_payload);
    engine.intervention_writer.send(record);

    let final_seq = event_seq.next();
    let chain_len = event_seq.current();
    decisions_logger.write(devts::decision_finalised(
        &decision_id,
        &env_session_id,
        &env_owner_dto,
        &env_profile_str,
        final_seq,
        "dispatched",
        started.elapsed().as_millis() as u64,
        chain_len,
        serde_json::json!({
            "tier_used": tier_to_str(tier_used),
            "action": kind_to_str(decision_to_kind(&decision)),
        }),
    ));

    DispatchResult {
        decision_id,
        kind: DispatchResultKind::Dispatched(decision, outcome),
    }
}

// ───────────────────────────── helpers ─────────────────────────────────

fn kind_to_str(kind: NurseActionKind) -> &'static str {
    match kind {
        NurseActionKind::LeaveIt => "leave_it",
        NurseActionKind::Steer => "steer",
        NurseActionKind::Restart => "restart",
        NurseActionKind::Cancel => "cancel",
    }
}

fn decision_to_kind(d: &NurseDecision) -> NurseActionKind {
    match d {
        NurseDecision::LeaveIt { .. } => NurseActionKind::LeaveIt,
        NurseDecision::Steer { .. } => NurseActionKind::Steer,
        NurseDecision::Restart { .. } => NurseActionKind::Restart,
        NurseDecision::Cancel { .. } => NurseActionKind::Cancel,
    }
}

fn tier_to_str(t: NurseDispatchTier) -> &'static str {
    match t {
        NurseDispatchTier::Deterministic => "deterministic",
        NurseDispatchTier::Templated => "templated",
        NurseDispatchTier::Llm => "llm",
        NurseDispatchTier::Synthesized => "synthesized",
        NurseDispatchTier::Manual => "manual",
    }
}

fn severity_tier_str(s: Severity) -> &'static str {
    match s.tier() {
        Tier::Quiet => "quiet",
        Tier::Warning => "warning",
        Tier::Stalled => "stalled",
        Tier::Critical => "critical",
    }
}

fn profile_str(p: NurseProfile) -> String {
    match p {
        NurseProfile::Tasks => "tasks",
        NurseProfile::Swarm => "swarm",
        NurseProfile::Hivemind => "hivemind",
        NurseProfile::Test => "test",
        NurseProfile::Default => "default",
    }
    .to_string()
}

fn budget_gate_reason_str(r: &BudgetGateReason) -> &'static str {
    match r {
        BudgetGateReason::PerKeyCooldown { .. } => "per_key_cooldown",
        BudgetGateReason::PerDetectorExhausted { .. } => "per_detector_exhausted",
        BudgetGateReason::LifetimeExhausted { .. } => "lifetime_exhausted",
    }
}

fn instant_to_unix_ms(t: Instant) -> u64 {
    let now = Instant::now();
    let sys_now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    if t >= now {
        sys_now.saturating_add(t.saturating_duration_since(now).as_millis() as u64)
    } else {
        sys_now.saturating_sub(now.saturating_duration_since(t).as_millis() as u64)
    }
}

fn prune_self_killed(self_killed: &Arc<Mutex<HashMap<String, Instant>>>) {
    let cutoff = Instant::now()
        .checked_sub(SELF_KILL_GRACE.saturating_mul(2))
        .unwrap_or_else(Instant::now);
    let mut guard = self_killed.lock().unwrap_or_else(|e| e.into_inner());
    guard.retain(|_, ts| *ts > cutoff);
}

fn is_self_kill_repeat(
    self_killed: &Arc<Mutex<HashMap<String, Instant>>>,
    session_id: &str,
    detector: &str,
    dedup_key: &str,
) -> bool {
    let kill_related = matches!(detector, "process_health" | "synthesized")
        && matches!(
            dedup_key,
            "process_dead" | "synthesized:process_crashed" | "session_gone_unobserved"
        );
    if !kill_related {
        return false;
    }
    let guard = self_killed.lock().unwrap_or_else(|e| e.into_inner());
    match guard.get(session_id) {
        Some(&ts) => Instant::now() < ts + SELF_KILL_GRACE,
        None => false,
    }
}

fn panic_message(any: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = any.downcast_ref::<&'static str>() {
        return (*s).to_string();
    }
    if let Some(s) = any.downcast_ref::<String>() {
        return s.clone();
    }
    "<non-string panic payload>".to_string()
}

// ────────────────────────────── tests ──────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nurse::bus::NurseBus;
    use crate::nurse::engine::SessionState;
    use crate::nurse::intervention::KillableSession;
    use crate::pi::manager::{PiManager, PiManagerError};
    use crate::pi::rpc::PiRpcError;
    use async_trait::async_trait;
    use chrono::Utc;
    use std::sync::Arc;

    // ── helpers ────────────────────────────────────────────────────────

    fn signal(detector: &'static str, severity: Severity, dedup_key: &str) -> Signal {
        Signal {
            detector,
            severity,
            dedup_key: dedup_key.to_string(),
            summary: format!("test signal {}", dedup_key),
            raised_at: Utc::now(),
            evidence: serde_json::Value::Null,
        }
    }

    fn dispatch_input(session_id: &str, sig: Signal) -> DispatchInput {
        DispatchInput {
            decision_id: uuid::Uuid::new_v4().simple().to_string(),
            session_id: session_id.to_string(),
            trigger_signal: sig,
            origin: DispatchOrigin::DetectorRaise,
        }
    }

    fn test_engine(cfg: NurseConfig) -> Arc<NurseEngine> {
        let bus = Arc::new(NurseBus::new());
        let pi = Arc::new(PiManager::new_for_tests());
        // NOTE: we deliberately do NOT call `engine.attach_app_handle`
        // — the dispatcher uses `engine.in_flight` (no AppHandle needed)
        // and the MockApplier ignores the optional `ApplyActionCtx.app`.
        Arc::new(NurseEngine::new(bus, pi, cfg).expect("engine"))
    }

    fn register_session(engine: &Arc<NurseEngine>, session_id: &str, owner: SessionOwner) {
        use std::sync::Weak;
        let weak: Weak<crate::pi::session::PiSession> = Weak::new();
        let mut state = SessionState::new(session_id.to_string(), owner, weak);
        // Default state — tests override fields after registration.
        state.health.signals.clear();
        let mut sessions = engine.sessions.write().unwrap_or_else(|p| p.into_inner());
        sessions.insert(session_id.to_string(), state);
    }

    // ── mock backends ──────────────────────────────────────────────────

    #[derive(Default)]
    struct MockClassifier {
        build_prompt_output: std::sync::Mutex<String>,
        // Each call pops the front of the queue.
        results:
            std::sync::Mutex<std::collections::VecDeque<anyhow::Result<Option<ClassifyOutput>>>>,
    }

    impl MockClassifier {
        fn new() -> Self {
            Self::default()
        }
        fn set_prompt(&self, p: &str) {
            *self.build_prompt_output.lock().unwrap() = p.to_string();
        }
        fn push_result(&self, r: anyhow::Result<Option<ClassifyOutput>>) {
            self.results.lock().unwrap().push_back(r);
        }
    }

    #[async_trait]
    impl ClassifierBackend for Arc<MockClassifier> {
        fn build_prompt(&self, _cfg: &NurseConfig, _health: &SessionHealth) -> String {
            self.build_prompt_output.lock().unwrap().clone()
        }
        async fn classify_prepared(
            &self,
            _cfg: &NurseConfig,
            _health: &SessionHealth,
            _prompt: &str,
        ) -> anyhow::Result<Option<ClassifyOutput>> {
            let mut q = self.results.lock().unwrap();
            match q.pop_front() {
                Some(r) => r,
                None => Ok(None),
            }
        }
    }

    struct ApplyCall {
        decision_kind: NurseActionKind,
        owner_dto: SessionOwnerDto,
        session_id: String,
        tier_used: NurseDispatchTier,
    }

    #[derive(Default)]
    struct MockApplier {
        calls: std::sync::Mutex<Vec<ApplyCall>>,
        panic_on_apply: std::sync::atomic::AtomicBool,
        budget_charge: std::sync::Mutex<BudgetCharge>,
    }

    impl MockApplier {
        fn new() -> Self {
            Self {
                calls: std::sync::Mutex::new(Vec::new()),
                panic_on_apply: std::sync::atomic::AtomicBool::new(false),
                budget_charge: std::sync::Mutex::new(BudgetCharge::Charge),
            }
        }
        fn arm_panic(&self) {
            self.panic_on_apply
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
        fn set_budget_charge(&self, bc: BudgetCharge) {
            *self.budget_charge.lock().unwrap() = bc;
        }
        fn last_call(&self) -> Option<ApplyCall> {
            self.calls.lock().unwrap().pop()
        }
        fn call_count(&self) -> usize {
            self.calls.lock().unwrap().len()
        }
    }

    #[async_trait]
    impl ActionApplier for Arc<MockApplier> {
        async fn apply(&self, ctx: ApplyActionCtx<'_>) -> ActionOutcome {
            if self
                .panic_on_apply
                .load(std::sync::atomic::Ordering::SeqCst)
            {
                panic!("mock applier panic");
            }
            let kind = decision_to_kind(&ctx.decision);
            self.calls.lock().unwrap().push(ApplyCall {
                decision_kind: kind,
                owner_dto: ctx.owner_dto.clone(),
                session_id: ctx.session_id.to_string(),
                tier_used: ctx.tier_used,
            });
            ActionOutcome {
                budget_charge: *self.budget_charge.lock().unwrap(),
                completed_status: NurseLifecycleStatus::Completed,
                outcome_string: format!("mock applied {}", kind_to_str(kind)),
                lifecycle_payload: NurseLifecyclePayload {
                    intervention_id: uuid::Uuid::new_v4().simple().to_string(),
                    status: NurseLifecycleStatus::Completed,
                    level: kind,
                    session_id: ctx.session_id.to_string(),
                    task_id: None,
                    swarm_id: None,
                    feature_id: None,
                    review_id: None,
                    observation: "mock".into(),
                    action: format!("mock {}", kind_to_str(kind)),
                    reasoning_delta: None,
                    full_reasoning: None,
                    error: None,
                    timestamp: Utc::now(),
                },
            }
        }
    }

    // ── mock SessionKiller ────────────────────────────────────────────

    struct MockKillableSession;
    #[async_trait]
    impl KillableSession for MockKillableSession {
        async fn abort(&self) -> Result<(), PiRpcError> {
            Ok(())
        }
        fn is_alive(&self) -> bool {
            false
        }
    }

    #[derive(Default)]
    struct MockKiller;
    #[async_trait]
    impl SessionKiller for MockKiller {
        async fn get_session(&self, _session_id: &str) -> Option<Arc<dyn KillableSession>> {
            None
        }
        async fn kill_session(&self, session_id: &str) -> Result<(), PiManagerError> {
            Err(PiManagerError::SessionNotFound {
                session_id: session_id.to_string(),
            })
        }
    }

    fn make_dispatcher(
        engine: &Arc<NurseEngine>,
        classifier: Arc<MockClassifier>,
        applier: Arc<MockApplier>,
    ) -> Dispatcher {
        let weak = Arc::downgrade(engine);
        let c: Arc<dyn ClassifierBackend> = Arc::new(classifier);
        let a: Arc<dyn ActionApplier> = Arc::new(applier);
        let killer: Arc<dyn SessionKiller> = Arc::new(MockKiller);
        Dispatcher::new(weak, c, a, killer)
    }

    async fn read_chain(engine: &Arc<NurseEngine>, decision_id: &str) -> Vec<serde_json::Value> {
        use std::path::PathBuf;
        // Decisions logger writes to today's file under the obs root.
        let root: PathBuf = engine.observability.decisions.root().to_path_buf();
        let today = crate::nurse::observability::writer::today_yyyy_mm_dd();
        let path = root.join(format!("decisions.jsonl.{}", today));
        // Poll the file up to ~2s — the writer task drains async; std
        // sleep won't yield to tokio in single-thread runtime tests.
        for _ in 0..40 {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let txt = std::fs::read_to_string(&path).unwrap_or_default();
            let rows: Vec<serde_json::Value> = txt
                .lines()
                .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
                .filter(|v| v.get("decision_id").and_then(|x| x.as_str()) == Some(decision_id))
                .collect();
            if rows
                .iter()
                .any(|v| v.get("event").and_then(|s| s.as_str()) == Some("decision_finalised"))
            {
                return rows;
            }
        }
        // Final read regardless of completeness.
        let txt = std::fs::read_to_string(&path).unwrap_or_default();
        txt.lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|v| v.get("decision_id").and_then(|x| x.as_str()) == Some(decision_id))
            .collect()
    }

    fn chain_events(chain: &[serde_json::Value]) -> Vec<String> {
        chain
            .iter()
            .filter_map(|v| v.get("event").and_then(|x| x.as_str()).map(String::from))
            .collect()
    }

    fn chain_final_status(chain: &[serde_json::Value]) -> Option<String> {
        chain.iter().rev().find_map(|v| {
            if v.get("event").and_then(|x| x.as_str()) == Some("decision_finalised") {
                v.get("data")
                    .and_then(|d| d.get("status"))
                    .and_then(|s| s.as_str())
                    .map(String::from)
            } else {
                None
            }
        })
    }

    // ── Test 1: severity gate drops below-threshold raise ─────────────
    #[tokio::test]
    async fn test1_severity_gate_drops_below_threshold() {
        let cfg = NurseConfig::default();
        let engine = test_engine(cfg);
        register_session(
            &engine,
            "sess-sev",
            SessionOwner::Task {
                task_id: "t".into(),
            },
        );

        let cls = Arc::new(MockClassifier::new());
        let app = Arc::new(MockApplier::new());
        let dispatcher = make_dispatcher(&engine, cls, app.clone());

        // Tasks profile defaults: escalation_min_severity = Stalled.
        let sig = signal("stall", Severity::Info, "stall");
        let input = dispatch_input("sess-sev", sig);
        let did = input.decision_id.clone();
        let result = dispatcher.handle_signal(input).await;
        assert!(matches!(result.kind, DispatchResultKind::GatedSeverity));
        let chain = read_chain(&engine, &did).await;
        assert_eq!(
            chain_final_status(&chain).as_deref(),
            Some("gated_severity")
        );
        assert_eq!(app.call_count(), 0);
    }

    // ── Test 2: in-flight gate rejects duplicate ──────────────────────
    #[tokio::test]
    async fn test2_in_flight_gate_rejects_duplicate() {
        let cfg = NurseConfig::default();
        let engine = test_engine(cfg);
        register_session(
            &engine,
            "sess-if",
            SessionOwner::Swarm {
                swarm_id: "s".into(),
                role: "worker".into(),
            },
        );

        // Pre-populate the engine in_flight slot.
        engine
            .in_flight
            .lock()
            .unwrap()
            .insert("sess-if".to_string(), "EXISTING".to_string());

        let cls = Arc::new(MockClassifier::new());
        let app = Arc::new(MockApplier::new());
        let dispatcher = make_dispatcher(&engine, cls, app.clone());

        let sig = signal("stall", Severity::Stalled, "stall");
        let input = dispatch_input("sess-if", sig);
        let did = input.decision_id.clone();
        let result = dispatcher.handle_signal(input).await;
        match result.kind {
            DispatchResultKind::GatedInFlight { existing } => {
                assert_eq!(existing, "EXISTING");
            }
            other => panic!("expected GatedInFlight, got {:?}", other),
        }
        let chain = read_chain(&engine, &did).await;
        assert_eq!(
            chain_final_status(&chain).as_deref(),
            Some("gated_in_flight")
        );
    }

    // ── Test 3: storm guard bypassed for Tier 1 matched signal ───────
    #[tokio::test]
    async fn test3_storm_guard_bypassed_for_tier1() {
        let cfg = NurseConfig::default();
        let engine = test_engine(cfg);
        register_session(
            &engine,
            "sess-sg",
            SessionOwner::Task {
                task_id: "t".into(),
            },
        );

        // Saturate the storm guard for (sess-sg, process_dead).
        for _ in 0..5 {
            let _ = engine.storm_guard.try_admit("sess-sg", "process_dead");
        }

        let cls = Arc::new(MockClassifier::new());
        let app = Arc::new(MockApplier::new());
        let dispatcher = make_dispatcher(&engine, cls, app.clone());

        let sig = signal("process_health", Severity::Critical, "process_dead");
        let input = dispatch_input("sess-sg", sig);
        let did = input.decision_id.clone();
        let _ = dispatcher.handle_signal(input).await;
        let chain = read_chain(&engine, &did).await;
        let events = chain_events(&chain);
        assert!(
            events.iter().any(|e| e == "storm_guard_evaluated"),
            "expected storm_guard_evaluated in {:?}",
            events
        );
        // Look up the storm_guard row's outcome.
        let outcome = chain
            .iter()
            .find(|v| v.get("event").and_then(|s| s.as_str()) == Some("storm_guard_evaluated"))
            .and_then(|v| v.get("data"))
            .and_then(|d| d.get("outcome"))
            .and_then(|o| o.as_str())
            .map(String::from);
        // Critical bypasses storm guard with priority over tier1 bypass.
        assert!(
            matches!(
                outcome.as_deref(),
                Some("bypassed_critical") | Some("bypassed_tier1")
            ),
            "unexpected outcome {:?}",
            outcome
        );
        // Tier 1 hit so applier got called.
        assert!(app.call_count() >= 1);
    }

    // ── Test 4: storm guard bypass for Critical ───────────────────────
    #[tokio::test]
    async fn test4_storm_guard_bypass_for_critical() {
        let cfg = NurseConfig::default();
        let engine = test_engine(cfg);
        register_session(
            &engine,
            "sess-crit",
            SessionOwner::Swarm {
                swarm_id: "s".into(),
                role: "worker".into(),
            },
        );

        // Saturate storm guard on a different key (non-tier1).
        for _ in 0..5 {
            let _ = engine.storm_guard.try_admit("sess-crit", "some_key");
        }

        let cls = Arc::new(MockClassifier::new());
        // Classifier returns Cancel so it doesn't hit no-model gate.
        cls.push_result(Ok(Some(ClassifyOutput {
            decision: NurseDecision::Cancel {
                reasoning: "go".into(),
                message: "stop".into(),
                observation: None,
                action: None,
            },
            raw_response: "{}".into(),
            provider: "mock".into(),
            model: "mock-m".into(),
            duration_ms: 1,
            cache_hit_tokens: 0,
            cache_write_tokens: 0,
        })));
        let app = Arc::new(MockApplier::new());
        let dispatcher = make_dispatcher(&engine, cls.clone(), app.clone());

        // Configure a nurse_model so Tier 3 fires.
        {
            let mut c = engine.config.write().await;
            c.nurse_model = Some("mock-m".into());
        }

        let sig = signal("custom_det", Severity::Critical, "some_key");
        let input = dispatch_input("sess-crit", sig);
        let did = input.decision_id.clone();
        let _ = dispatcher.handle_signal(input).await;
        let chain = read_chain(&engine, &did).await;
        let outcome = chain
            .iter()
            .find(|v| v.get("event").and_then(|s| s.as_str()) == Some("storm_guard_evaluated"))
            .and_then(|v| v.get("data"))
            .and_then(|d| d.get("outcome"))
            .and_then(|o| o.as_str())
            .map(String::from);
        assert_eq!(outcome.as_deref(), Some("bypassed_critical"));
    }

    // ── Test 5: post-lag suppression ─────────────────────────────────
    #[tokio::test]
    async fn test5_post_lag_suppression() {
        let cfg = NurseConfig::default();
        let engine = test_engine(cfg);
        register_session(
            &engine,
            "sess-pl",
            SessionOwner::Swarm {
                swarm_id: "s".into(),
                role: "worker".into(),
            },
        );
        // Configure Tier 3 model so we can reach the post-lag check.
        {
            let mut c = engine.config.write().await;
            c.nurse_model = Some("mock-m".into());
        }
        {
            let mut sessions = engine.sessions.write().unwrap();
            let state = sessions.get_mut("sess-pl").unwrap();
            state.post_lag_until = Some(Instant::now() + Duration::from_secs(60));
        }
        let cls = Arc::new(MockClassifier::new());
        let app = Arc::new(MockApplier::new());
        let dispatcher = make_dispatcher(&engine, cls, app.clone());

        // Non-Tier1 detector with Stalled (not Critical) — should be gated.
        let sig = signal("custom_det", Severity::Stalled, "some_key");
        let input = dispatch_input("sess-pl", sig);
        let did = input.decision_id.clone();
        let result = dispatcher.handle_signal(input).await;
        assert!(matches!(result.kind, DispatchResultKind::GatedPostLag));
        let chain = read_chain(&engine, &did).await;
        assert_eq!(
            chain_final_status(&chain).as_deref(),
            Some("gated_post_lag")
        );
    }

    // ── Test 6: budget gate (LeaveIt skipped) ─────────────────────────
    #[tokio::test]
    async fn test6_budget_gate_leave_it_skipped() {
        let cfg = NurseConfig::default();
        let engine = test_engine(cfg);
        register_session(
            &engine,
            "sess-bd",
            SessionOwner::Swarm {
                swarm_id: "s".into(),
                role: "worker".into(),
            },
        );
        // Exhaust per_detector budget so a non-LeaveIt would be gated.
        {
            let mut sessions = engine.sessions.write().unwrap();
            let state = sessions.get_mut("sess-bd").unwrap();
            for _ in 0..100 {
                state.budget.record("custom_det", "k", Instant::now());
            }
        }
        let cls = Arc::new(MockClassifier::new());
        cls.push_result(Ok(Some(ClassifyOutput {
            decision: NurseDecision::LeaveIt {
                reasoning: "ok".into(),
                check_back_secs: 30,
                observation: None,
                action: None,
            },
            raw_response: "{}".into(),
            provider: "mock".into(),
            model: "mock-m".into(),
            duration_ms: 1,
            cache_hit_tokens: 0,
            cache_write_tokens: 0,
        })));
        {
            let mut c = engine.config.write().await;
            c.nurse_model = Some("mock-m".into());
        }
        let app = Arc::new(MockApplier::new());
        let dispatcher = make_dispatcher(&engine, cls, app.clone());

        let sig = signal("custom_det", Severity::Stalled, "k");
        let input = dispatch_input("sess-bd", sig);
        let did = input.decision_id.clone();
        let _ = dispatcher.handle_signal(input).await;
        let chain = read_chain(&engine, &did).await;
        let budget_row = chain
            .iter()
            .find(|v| v.get("event").and_then(|s| s.as_str()) == Some("budget_evaluated"))
            .expect("budget_evaluated row");
        assert_eq!(
            budget_row
                .get("data")
                .and_then(|d| d.get("outcome"))
                .and_then(|o| o.as_str()),
            Some("skipped_leave_it")
        );
        assert_eq!(app.call_count(), 1, "applier still called for LeaveIt");
    }

    // ── Test 7: Tier 1 deterministic ──────────────────────────────────
    #[tokio::test]
    async fn test7_tier1_deterministic() {
        let cfg = NurseConfig::default();
        let engine = test_engine(cfg);
        register_session(
            &engine,
            "sess-t1",
            SessionOwner::Task {
                task_id: "t".into(),
            },
        );
        let cls = Arc::new(MockClassifier::new());
        let app = Arc::new(MockApplier::new());
        let dispatcher = make_dispatcher(&engine, cls, app.clone());

        let sig = signal("process_health", Severity::Critical, "process_dead");
        let input = dispatch_input("sess-t1", sig);
        let did = input.decision_id.clone();
        let _ = dispatcher.handle_signal(input).await;
        let chain = read_chain(&engine, &did).await;
        let events = chain_events(&chain);
        let t1 = chain
            .iter()
            .find(|v| v.get("event").and_then(|s| s.as_str()) == Some("tier1_evaluated"))
            .expect("tier1_evaluated");
        assert_eq!(
            t1.get("data").and_then(|d| d.get("matched")),
            Some(&serde_json::Value::Bool(true))
        );
        assert_eq!(
            t1.get("data")
                .and_then(|d| d.get("action"))
                .and_then(|a| a.as_str()),
            Some("restart")
        );
        assert!(
            !events.iter().any(|e| e == "playbook_evaluated"),
            "playbook should not run after tier1 hit: {:?}",
            events
        );
        assert!(!events.iter().any(|e| e == "classifier_invoked"));
        let last = app.last_call().expect("applier called");
        assert_eq!(last.decision_kind, NurseActionKind::Restart);
        assert_eq!(last.tier_used, NurseDispatchTier::Deterministic);
    }

    // ── Test 8: Tier 1 owner-aware downgrade combinations ────────────
    #[tokio::test]
    async fn test8_tier1_owner_aware_downgrade() {
        // ProcessDead + Review → Cancel + downgrade row.
        {
            let cfg = NurseConfig::default();
            let engine = test_engine(cfg);
            register_session(&engine, "s1", SessionOwner::Review { job_id: "j".into() });
            let cls = Arc::new(MockClassifier::new());
            let app = Arc::new(MockApplier::new());
            let dispatcher = make_dispatcher(&engine, cls, app.clone());
            let sig = signal("process_health", Severity::Critical, "process_dead");
            let input = dispatch_input("s1", sig);
            let did = input.decision_id.clone();
            let _ = dispatcher.handle_signal(input).await;
            let chain = read_chain(&engine, &did).await;
            let t1 = chain
                .iter()
                .find(|v| v.get("event").and_then(|s| s.as_str()) == Some("tier1_evaluated"))
                .expect("tier1");
            assert_eq!(
                t1.get("data")
                    .and_then(|d| d.get("action"))
                    .and_then(|a| a.as_str()),
                Some("cancel")
            );
            assert_eq!(
                t1.get("data")
                    .and_then(|d| d.get("downgrade_reason"))
                    .and_then(|r| r.as_str()),
                Some("restart_not_meaningful_for_hivemind")
            );
            let last = app.last_call().unwrap();
            assert_eq!(last.decision_kind, NurseActionKind::Cancel);
        }

        // ProcessDead + Task → Restart, no downgrade.
        {
            let cfg = NurseConfig::default();
            let engine = test_engine(cfg);
            register_session(
                &engine,
                "s2",
                SessionOwner::Task {
                    task_id: "t".into(),
                },
            );
            let cls = Arc::new(MockClassifier::new());
            let app = Arc::new(MockApplier::new());
            let dispatcher = make_dispatcher(&engine, cls, app.clone());
            let sig = signal("process_health", Severity::Critical, "process_dead");
            let input = dispatch_input("s2", sig);
            let did = input.decision_id.clone();
            let _ = dispatcher.handle_signal(input).await;
            let chain = read_chain(&engine, &did).await;
            let t1 = chain
                .iter()
                .find(|v| v.get("event").and_then(|s| s.as_str()) == Some("tier1_evaluated"))
                .unwrap();
            assert_eq!(
                t1.get("data")
                    .and_then(|d| d.get("action"))
                    .and_then(|a| a.as_str()),
                Some("restart")
            );
            assert!(t1
                .get("data")
                .and_then(|d| d.get("downgrade_reason"))
                .is_none());
        }

        // CrashPattern + Review → Cancel, no downgrade.
        {
            let cfg = NurseConfig::default();
            let engine = test_engine(cfg);
            register_session(&engine, "s3", SessionOwner::Review { job_id: "j".into() });
            let cls = Arc::new(MockClassifier::new());
            let app = Arc::new(MockApplier::new());
            let dispatcher = make_dispatcher(&engine, cls, app.clone());
            let sig = signal("process_health", Severity::Critical, "crash_pattern");
            let input = dispatch_input("s3", sig);
            let did = input.decision_id.clone();
            let _ = dispatcher.handle_signal(input).await;
            let chain = read_chain(&engine, &did).await;
            let t1 = chain
                .iter()
                .find(|v| v.get("event").and_then(|s| s.as_str()) == Some("tier1_evaluated"))
                .unwrap();
            assert_eq!(
                t1.get("data")
                    .and_then(|d| d.get("action"))
                    .and_then(|a| a.as_str()),
                Some("cancel")
            );
            assert!(t1
                .get("data")
                .and_then(|d| d.get("downgrade_reason"))
                .is_none());
        }

        // SchedulerDeadlock + Swarm → Cancel, no downgrade.
        {
            let cfg = NurseConfig::default();
            let engine = test_engine(cfg);
            register_session(
                &engine,
                "s4",
                SessionOwner::Swarm {
                    swarm_id: "sw".into(),
                    role: "queen".into(),
                },
            );
            let cls = Arc::new(MockClassifier::new());
            let app = Arc::new(MockApplier::new());
            let dispatcher = make_dispatcher(&engine, cls, app.clone());
            let sig = signal(
                "synthesized",
                Severity::Critical,
                "synthesized:scheduler_deadlock:sw",
            );
            let input = dispatch_input("s4", sig);
            let did = input.decision_id.clone();
            let _ = dispatcher.handle_signal(input).await;
            let chain = read_chain(&engine, &did).await;
            let t1 = chain
                .iter()
                .find(|v| v.get("event").and_then(|s| s.as_str()) == Some("tier1_evaluated"))
                .unwrap();
            assert_eq!(
                t1.get("data")
                    .and_then(|d| d.get("action"))
                    .and_then(|a| a.as_str()),
                Some("cancel")
            );
        }
    }

    // ── Tier 1 routing for `retry:death_loop` (added after the
    //    `399ddf42…` investigation: model died in an auto-retry loop
    //    but the existing detector only raised Warn, so the dispatcher
    //    let Tier 3 say leave_it 4 times in a row).
    #[tokio::test]
    async fn tier1_retry_death_loop_routes_to_restart_for_tasks() {
        let cfg = NurseConfig::default();
        let engine = test_engine(cfg);
        register_session(
            &engine,
            "sess-deathloop-task",
            SessionOwner::Task {
                task_id: "t".into(),
            },
        );
        let cls = Arc::new(MockClassifier::new());
        let app = Arc::new(MockApplier::new());
        let dispatcher = make_dispatcher(&engine, cls, app.clone());

        let sig = signal("retry_exhaustion", Severity::Critical, "retry:death_loop");
        let input = dispatch_input("sess-deathloop-task", sig);
        let did = input.decision_id.clone();
        let _ = dispatcher.handle_signal(input).await;

        let chain = read_chain(&engine, &did).await;
        let events = chain_events(&chain);
        let t1 = chain
            .iter()
            .find(|v| v.get("event").and_then(|s| s.as_str()) == Some("tier1_evaluated"))
            .expect("tier1_evaluated");
        assert_eq!(
            t1.get("data").and_then(|d| d.get("matched")),
            Some(&serde_json::Value::Bool(true))
        );
        assert_eq!(
            t1.get("data")
                .and_then(|d| d.get("action"))
                .and_then(|a| a.as_str()),
            Some("restart")
        );
        assert_eq!(
            t1.get("data")
                .and_then(|d| d.get("entry_id"))
                .and_then(|s| s.as_str()),
            Some("retry_death_loop")
        );
        assert!(
            !events.iter().any(|e| e == "classifier_invoked"),
            "Tier 1 must short-circuit the LLM classifier"
        );
        let last = app.last_call().expect("applier called");
        assert_eq!(last.decision_kind, NurseActionKind::Restart);
        assert_eq!(last.tier_used, NurseDispatchTier::Deterministic);
    }

    #[tokio::test]
    async fn tier1_retry_death_loop_downgrades_to_cancel_for_hivemind() {
        let cfg = NurseConfig::default();
        let engine = test_engine(cfg);
        register_session(
            &engine,
            "sess-deathloop-rev",
            SessionOwner::Review { job_id: "j".into() },
        );
        let cls = Arc::new(MockClassifier::new());
        let app = Arc::new(MockApplier::new());
        let dispatcher = make_dispatcher(&engine, cls, app.clone());

        let sig = signal("retry_exhaustion", Severity::Critical, "retry:death_loop");
        let input = dispatch_input("sess-deathloop-rev", sig);
        let did = input.decision_id.clone();
        let _ = dispatcher.handle_signal(input).await;

        let chain = read_chain(&engine, &did).await;
        let t1 = chain
            .iter()
            .find(|v| v.get("event").and_then(|s| s.as_str()) == Some("tier1_evaluated"))
            .expect("tier1_evaluated");
        assert_eq!(
            t1.get("data")
                .and_then(|d| d.get("action"))
                .and_then(|a| a.as_str()),
            Some("cancel")
        );
        assert_eq!(
            t1.get("data")
                .and_then(|d| d.get("downgrade_reason"))
                .and_then(|r| r.as_str()),
            Some("restart_not_meaningful_for_hivemind")
        );
        let last = app.last_call().unwrap();
        assert_eq!(last.decision_kind, NurseActionKind::Cancel);
    }

    // ── Test 9: Tier 3 owner-aware downgrade ──────────────────────────
    #[tokio::test]
    async fn test9_tier3_owner_aware_downgrade() {
        let cfg = NurseConfig::default();
        let engine = test_engine(cfg);
        register_session(
            &engine,
            "sess-tier3",
            SessionOwner::Review { job_id: "j".into() },
        );
        {
            let mut c = engine.config.write().await;
            c.nurse_model = Some("mock-m".into());
        }
        let cls = Arc::new(MockClassifier::new());
        cls.push_result(Ok(Some(ClassifyOutput {
            decision: NurseDecision::Restart {
                reasoning: "model said restart".into(),
                observation: None,
                action: None,
            },
            raw_response: "{}".into(),
            provider: "mock".into(),
            model: "mock-m".into(),
            duration_ms: 1,
            cache_hit_tokens: 0,
            cache_write_tokens: 0,
        })));
        let app = Arc::new(MockApplier::new());
        let dispatcher = make_dispatcher(&engine, cls, app.clone());

        // Use a non-Tier1 signal so we reach Tier 3.
        let sig = signal("custom_det", Severity::Stalled, "k");
        let input = dispatch_input("sess-tier3", sig);
        let did = input.decision_id.clone();
        let _ = dispatcher.handle_signal(input).await;
        let chain = read_chain(&engine, &did).await;
        let events = chain_events(&chain);
        assert!(
            events.iter().any(|e| e == "classifier_decision_downgraded"),
            "missing downgrade row in {:?}",
            events
        );
        let last = app.last_call().unwrap();
        assert_eq!(last.decision_kind, NurseActionKind::Cancel);
        assert_eq!(last.tier_used, NurseDispatchTier::Llm);
    }

    // ── Test 10: Tier 2 playbook hit ──────────────────────────────────
    #[tokio::test]
    async fn test10_tier2_playbook_hit() {
        let cfg = NurseConfig::default();
        let engine = test_engine(cfg);
        register_session(
            &engine,
            "sess-pb",
            SessionOwner::Task {
                task_id: "t".into(),
            },
        );
        // Default playbook seeded — context_saturation/ctx:critical matches.
        let cls = Arc::new(MockClassifier::new());
        let app = Arc::new(MockApplier::new());
        let dispatcher = make_dispatcher(&engine, cls, app.clone());

        let sig = signal("context_saturation", Severity::Stalled, "ctx:critical");
        let input = dispatch_input("sess-pb", sig);
        let did = input.decision_id.clone();
        let _ = dispatcher.handle_signal(input).await;
        let chain = read_chain(&engine, &did).await;
        let events = chain_events(&chain);
        let pb = chain
            .iter()
            .find(|v| v.get("event").and_then(|s| s.as_str()) == Some("playbook_evaluated"))
            .expect("playbook_evaluated row");
        assert_eq!(
            pb.get("data").and_then(|d| d.get("matched")),
            Some(&serde_json::Value::Bool(true))
        );
        assert!(
            !events.iter().any(|e| e == "classifier_invoked"),
            "classifier should not run after playbook hit: {:?}",
            events
        );
        let last = app.last_call().unwrap();
        assert_eq!(last.decision_kind, NurseActionKind::Steer);
        assert_eq!(last.tier_used, NurseDispatchTier::Templated);
    }

    // ── Test 11: Tier 3 classifier skip on no model ───────────────────
    #[tokio::test]
    async fn test11_tier3_skip_no_model() {
        let mut cfg = NurseConfig::default();
        cfg.nurse_model = None;
        let engine = test_engine(cfg);
        // Clear per-profile overrides too.
        {
            let mut c = engine.config.write().await;
            for p in c.profiles.values_mut() {
                p.nurse_model = None;
            }
        }
        register_session(
            &engine,
            "sess-nm",
            SessionOwner::Task {
                task_id: "t".into(),
            },
        );
        // Make sure the empty playbook path is hit by using an entirely
        // unmatched detector/dedup.
        let cls = Arc::new(MockClassifier::new());
        let app = Arc::new(MockApplier::new());
        let dispatcher = make_dispatcher(&engine, cls, app.clone());

        let sig = signal("custom_det", Severity::Stalled, "kk");
        let input = dispatch_input("sess-nm", sig);
        let did = input.decision_id.clone();
        let result = dispatcher.handle_signal(input).await;
        assert!(matches!(
            result.kind,
            DispatchResultKind::ClassifierSkippedNoModel
        ));
        let chain = read_chain(&engine, &did).await;
        assert_eq!(
            chain_final_status(&chain).as_deref(),
            Some("classifier_skipped_no_model")
        );
        assert!(
            engine
                .health
                .tier3_skipped_no_model
                .load(AtomicOrdering::Relaxed)
                >= 1
        );
    }

    // ── Test 12: Tier 3 prompt capture ───────────────────────────────
    #[tokio::test]
    async fn test12_tier3_prompt_capture() {
        let cfg = NurseConfig::default();
        let engine = test_engine(cfg);
        {
            let mut c = engine.config.write().await;
            c.nurse_model = Some("mock-m".into());
        }
        register_session(
            &engine,
            "sess-cap",
            SessionOwner::Task {
                task_id: "t".into(),
            },
        );
        let cls = Arc::new(MockClassifier::new());
        cls.set_prompt("CAPTURED_PROMPT_BODY");
        // Force an error so no response is written.
        cls.push_result(Err(anyhow::anyhow!("provider boom")));
        let app = Arc::new(MockApplier::new());
        let dispatcher = make_dispatcher(&engine, cls.clone(), app.clone());

        let sig = signal("custom_det", Severity::Stalled, "kk");
        let input = dispatch_input("sess-cap", sig);
        let did = input.decision_id.clone();
        let _ = dispatcher.handle_signal(input).await;

        let prompt_path = engine.observability.captures.prompt_path(&did);
        let response_path = engine.observability.captures.response_path(&did);
        assert!(prompt_path.exists(), "prompt file should exist");
        let body = std::fs::read_to_string(&prompt_path).unwrap();
        assert_eq!(body, "CAPTURED_PROMPT_BODY");
        assert!(
            !response_path.exists(),
            "response file should NOT exist after classifier err"
        );
    }

    // ── Test 14: in-flight RAII guard releases on panic ───────────────
    #[tokio::test]
    async fn test14_in_flight_releases_on_panic() {
        let cfg = NurseConfig::default();
        let engine = test_engine(cfg);
        register_session(
            &engine,
            "sess-pn",
            SessionOwner::Task {
                task_id: "t".into(),
            },
        );
        let cls = Arc::new(MockClassifier::new());
        let app = Arc::new(MockApplier::new());
        app.arm_panic();
        let dispatcher = make_dispatcher(&engine, cls, app.clone());

        // Critical so we get past severity and reach apply quickly via
        // Tier 1.
        let sig = signal("process_health", Severity::Critical, "process_dead");
        let input = dispatch_input("sess-pn", sig);
        let result = dispatcher.handle_signal(input).await;
        assert!(matches!(result.kind, DispatchResultKind::Panic(_)));

        let g = engine.in_flight.lock().unwrap();
        assert!(
            !g.contains_key("sess-pn"),
            "in_flight slot should be released on panic"
        );
    }

    // ── Test 20: EngineGone path ──────────────────────────────────────
    #[tokio::test]
    async fn test20_engine_gone() {
        let cls: Arc<dyn ClassifierBackend> = Arc::new(Arc::new(MockClassifier::new()));
        let app: Arc<dyn ActionApplier> = Arc::new(Arc::new(MockApplier::new()));
        let killer: Arc<dyn SessionKiller> = Arc::new(MockKiller);
        let dispatcher = Dispatcher::new(Weak::new(), cls, app, killer);

        let sig = signal("anything", Severity::Critical, "k");
        let input = dispatch_input("gone-sid", sig);
        let result = dispatcher.handle_signal(input).await;
        assert!(matches!(result.kind, DispatchResultKind::EngineGone));
    }

    // ── Test 21: InFlightGuard poison safety ──────────────────────────
    #[tokio::test]
    async fn test21_in_flight_poison_safety() {
        let cfg = NurseConfig::default();
        let engine = test_engine(cfg);
        register_session(
            &engine,
            "sess-ps",
            SessionOwner::Task {
                task_id: "t".into(),
            },
        );

        // Poison the engine in_flight mutex via a panic in a held lock.
        let m = Arc::clone(&engine.in_flight);
        let _ = std::thread::spawn(move || {
            let _g = m.lock().unwrap();
            panic!("intentional poison");
        })
        .join();

        let cls = Arc::new(MockClassifier::new());
        let app = Arc::new(MockApplier::new());
        let dispatcher = make_dispatcher(&engine, cls, app.clone());
        let sig = signal("process_health", Severity::Critical, "process_dead");
        let input = dispatch_input("sess-ps", sig);
        let result = dispatcher.handle_signal(input).await;
        // Must not panic on the poisoned lock.
        assert!(!matches!(result.kind, DispatchResultKind::Panic(_)));
        // in_flight should end clean.
        let g = engine.in_flight.lock().unwrap_or_else(|e| e.into_inner());
        assert!(!g.contains_key("sess-ps"));
    }

    // ── Test 18: watchdog with no live session does NOT short-circuit ──
    //
    // We can't construct a real PiSession easily in tests, so this test
    // asserts the simpler invariant: when origin == Watchdog AND the
    // session's Weak<PiSession> fails to upgrade (no live PiSession), the
    // dispatcher does NOT hit the fast-path branches and falls through
    // to the normal pipeline (here: Tier 1 deterministic for
    // process_health/process_dead). The full fast-path behaviour with
    // a real PiSession returning Some(<10min) for awaiting_model_for_ms
    // is covered by Step 20 manual acceptance via the Test Nurse ▾
    // dropdown.
    #[tokio::test]
    async fn test18_watchdog_no_session_does_not_short_circuit() {
        let cfg = NurseConfig::default();
        let engine = test_engine(cfg);
        register_session(
            &engine,
            "sess-wd",
            SessionOwner::Task {
                task_id: "t".into(),
            },
        );
        // The default `register_session` helper uses `Weak::new()`, so
        // any upgrade() in the fast-path will return None and we fall
        // through to the normal pipeline.
        let cls = Arc::new(MockClassifier::new());
        let app = Arc::new(MockApplier::new());
        let dispatcher = make_dispatcher(&engine, cls, app.clone());

        // Critical so it bypasses storm guard and hits Tier 1.
        let sig = signal("process_health", Severity::Critical, "process_dead");
        let input = DispatchInput {
            decision_id: uuid::Uuid::new_v4().simple().to_string(),
            session_id: "sess-wd".to_string(),
            trigger_signal: sig,
            origin: DispatchOrigin::Watchdog,
        };
        let result = dispatcher.handle_signal(input).await;
        // Should NOT be a FastPathLeaveIt — falls through to Tier 1.
        assert!(
            !matches!(result.kind, DispatchResultKind::FastPathLeaveIt(_)),
            "no live PiSession should NOT short-circuit to fast-path, got {:?}",
            result.kind
        );
        // Should have dispatched normally via Tier 1.
        assert_eq!(app.call_count(), 1, "applier should have been called once");
        let call = app.last_call().expect("applier call");
        assert_eq!(call.tier_used, NurseDispatchTier::Deterministic);
    }

    // ── Swarms-only gate ───────────────────────────────────
    #[tokio::test]
    async fn swarms_only_gates_task_signal() {
        let mut cfg = NurseConfig::default();
        cfg.swarms_only = true;
        let engine = test_engine(cfg);
        register_session(
            &engine,
            "sess-sw-task",
            SessionOwner::Task {
                task_id: "t".into(),
            },
        );
        let cls = Arc::new(MockClassifier::new());
        let app = Arc::new(MockApplier::new());
        let dispatcher = make_dispatcher(&engine, cls, app.clone());

        // Critical so a non-gated run would otherwise dispatch via Tier 1.
        let sig = signal("process_health", Severity::Critical, "process_dead");
        let input = dispatch_input("sess-sw-task", sig);
        let did = input.decision_id.clone();
        let result = dispatcher.handle_signal(input).await;
        assert!(
            matches!(result.kind, DispatchResultKind::GatedSwarmsOnly),
            "expected GatedSwarmsOnly, got {:?}",
            result.kind
        );
        let chain = read_chain(&engine, &did).await;
        assert_eq!(
            chain_final_status(&chain).as_deref(),
            Some("gated_swarms_only")
        );
        assert_eq!(app.call_count(), 0);
    }

    #[tokio::test]
    async fn swarms_only_allows_swarm_signal() {
        let mut cfg = NurseConfig::default();
        cfg.swarms_only = true;
        let engine = test_engine(cfg);
        register_session(
            &engine,
            "sess-sw-swarm",
            SessionOwner::Swarm {
                swarm_id: "s".into(),
                role: "worker".into(),
            },
        );
        let cls = Arc::new(MockClassifier::new());
        let app = Arc::new(MockApplier::new());
        let dispatcher = make_dispatcher(&engine, cls, app.clone());

        let sig = signal("process_health", Severity::Critical, "process_dead");
        let input = dispatch_input("sess-sw-swarm", sig);
        let did = input.decision_id.clone();
        let result = dispatcher.handle_signal(input).await;
        assert!(
            matches!(result.kind, DispatchResultKind::Dispatched(_, _)),
            "expected Dispatched, got {:?}",
            result.kind
        );
        let chain = read_chain(&engine, &did).await;
        // No `gated_swarms_only` row should appear in the chain.
        let events = chain_events(&chain);
        assert!(
            !events.iter().any(|e| {
                chain.iter().any(|v| {
                    v.get("data")
                        .and_then(|d| d.get("status"))
                        .and_then(|s| s.as_str())
                        == Some("gated_swarms_only")
                        && v.get("event").and_then(|x| x.as_str()) == Some(e.as_str())
                })
            }),
            "swarm signal should not produce gated_swarms_only row"
        );
        assert_eq!(app.call_count(), 1);
    }

    #[tokio::test]
    async fn swarms_only_disabled_does_not_gate() {
        // Regression guard — with swarms_only=false the gate must be
        // inert for both Task and Swarm owners.
        for (label, owner) in [
            (
                "task",
                SessionOwner::Task {
                    task_id: "t".into(),
                },
            ),
            (
                "swarm",
                SessionOwner::Swarm {
                    swarm_id: "s".into(),
                    role: "worker".into(),
                },
            ),
        ] {
            let cfg = NurseConfig::default();
            assert!(
                !cfg.swarms_only,
                "default config must have swarms_only=false"
            );
            let engine = test_engine(cfg);
            let sid = format!("sess-off-{}", label);
            register_session(&engine, &sid, owner);
            let cls = Arc::new(MockClassifier::new());
            let app = Arc::new(MockApplier::new());
            let dispatcher = make_dispatcher(&engine, cls, app.clone());
            let sig = signal("process_health", Severity::Critical, "process_dead");
            let input = dispatch_input(&sid, sig);
            let result = dispatcher.handle_signal(input).await;
            assert!(
                matches!(result.kind, DispatchResultKind::Dispatched(_, _)),
                "{label}: expected Dispatched, got {:?}",
                result.kind
            );
            assert_eq!(app.call_count(), 1, "{label}: applier must run");
        }
    }

    // ── Test 22: self_killed pruning ─────────────────────────────────
    #[tokio::test]
    async fn test22_self_killed_pruning() {
        let cfg = NurseConfig::default();
        let engine = test_engine(cfg);
        register_session(
            &engine,
            "sess-prune",
            SessionOwner::Task {
                task_id: "t".into(),
            },
        );
        let cls = Arc::new(MockClassifier::new());
        let app = Arc::new(MockApplier::new());
        let dispatcher = make_dispatcher(&engine, cls, app.clone());

        // Insert a stale entry well outside 2× SELF_KILL_GRACE.
        {
            let h = dispatcher.self_killed_handle();
            let mut g = h.lock().unwrap();
            let stale = Instant::now()
                .checked_sub(SELF_KILL_GRACE * 10)
                .unwrap_or_else(Instant::now);
            g.insert("ancient-sid".to_string(), stale);
        }

        // Run any dispatch; prune happens early.
        let sig = signal("stall", Severity::Stalled, "stall");
        let input = dispatch_input("sess-prune", sig);
        let _ = dispatcher.handle_signal(input).await;

        let h = dispatcher.self_killed_handle();
        let g = h.lock().unwrap();
        assert!(
            !g.contains_key("ancient-sid"),
            "stale self_killed entry should be pruned"
        );
    }
}
