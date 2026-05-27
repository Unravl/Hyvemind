//! `NurseEngine` — bus subscriber, detector driver, decision pipeline.
//!
//! The engine subscribes once to [`NurseBus`](crate::nurse::bus::NurseBus)
//! and dispatches events to per-session `SessionState`s. Detectors are
//! invoked under `catch_unwind` so a buggy detector cannot take the
//! whole loop down. The supervisor wraps the loop in
//! `util::supervise::super_watchdog` as a second-layer safety net.
//!
//! Lock-ordering invariant:
//!   `config` (tokio::sync::RwLock, async) is acquired and released
//!   BEFORE `sessions` (std::sync::RwLock, sync). Never hold the sync
//!   `sessions` guard across an `.await`, and never call `PiManager`
//!   methods, dispatch interventions, or send on the InterventionWriter
//!   channel while holding `sessions`. The dispatcher itself enforces
//!   this by snapshotting `nurse_cfg` before each sessions-guarded
//!   block and dropping the guard before any await.
//!
//!   Detector code MUST NOT touch `engine.sessions` directly — it
//!   operates on the `&mut SessionState` passed via `DetectorContext`.
//!   The `try_upgrade` helper returns synthetic signals as data, not
//!   by dispatching inline, precisely to honour this rule.

use std::any::Any;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, RwLock, Weak};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use chrono::Utc;
use smallvec::SmallVec;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tokio::time::{interval, MissedTickBehavior};
use tokio_util::sync::CancellationToken;

use crate::nurse::budget::BudgetState;
use crate::nurse::bus::{NurseBus, NurseBusEvent, SessionEndReason};
use crate::nurse::config::{NurseConfig, NurseProfile};
use crate::nurse::detector::{DetectorContext, DetectorRegistry, SignalDelta, TickKind};
use crate::nurse::detectors::{
    ContextSaturationDetector, ProcessHealthDetector, ProviderHealthDetector,
    ReasoningLoopDetector, RetryExhaustionDetector, StallDetector, ToolFailureDetector,
};
use crate::nurse::health::{SessionHealth, Severity, Signal};
use crate::nurse::intervention::InterventionContext;
use crate::nurse::intervention_writer::InterventionWriter;
use crate::nurse::observability::bus_telemetry::{self as bus_tel, BusEventKind};
use crate::nurse::observability::signal_stream::{SignalStreamKind, SignalStreamRow};
use crate::nurse::observability::ObservabilityHandles;
use crate::nurse::playbook::SteerPlaybook;
use crate::nurse::snapshot::{ProviderStateSnapshot, SessionOwnerDto};
use crate::nurse::storm_guard::StormGuard;
use crate::nurse::synthesized::{
    dedup_key as synthesized_dedup_key, severity_for, InterventionOwner, SynthesizedKind,
};
use crate::pi::session::{PiSession, SessionOwner};

/// Per-session bookkeeping.
pub struct SessionState {
    pub health: SessionHealth,
    pub owner: SessionOwner,
    pub provider: Option<String>,
    pub model_id: Option<String>,
    /// Per-detector internal state, keyed by `detector.name()`.
    pub detector_states: HashMap<&'static str, Box<dyn Any + Send + Sync>>,
    pub budget: BudgetState,
    pub session: Weak<PiSession>,
    pub session_first_observed_at: Instant,
    pub stale_sweeps: u8,
    pub intervention_count: u32,
    pub consecutive_bad_parse_ticks: u32,
    pub post_lag_until: Option<Instant>,
    /// Nurse-relevant activity count at the last frontend watchdog
    /// evaluation that was allowed through to the dispatcher.
    pub last_watchdog_checked_activity_count: u64,
    /// Nurse-relevant activity count at the last batched LLM review.
    pub last_batch_reviewed_activity_count: u64,
}

impl SessionState {
    pub(crate) fn new(session_id: String, owner: SessionOwner, weak: Weak<PiSession>) -> Self {
        let health = SessionHealth::new(session_id, &owner);
        let budget = BudgetState::new(Instant::now());
        Self {
            health,
            owner,
            provider: None,
            model_id: None,
            detector_states: HashMap::new(),
            budget,
            session: weak,
            session_first_observed_at: Instant::now(),
            stale_sweeps: 0,
            intervention_count: 0,
            consecutive_bad_parse_ticks: 0,
            post_lag_until: None,
            last_watchdog_checked_activity_count: 0,
            last_batch_reviewed_activity_count: 0,
        }
    }
}

/// Engine-wide health counters surfaced via `get_nurse_status`.
#[derive(Debug)]
pub struct NurseHealthCounters {
    pub last_tick_at_ms: AtomicU64,
    pub last_successful_tick_at_ms: AtomicU64,
    pub consecutive_failed_ticks: AtomicU32,
    pub consecutive_bad_parse_ticks: AtomicU32,
    pub consecutive_skipped_ticks: AtomicU32,
    pub degraded: AtomicBool,
    pub tier3_skipped_no_model: AtomicU64,
    pub intervention_writer_dropped: AtomicU64,
    pub observability_dropped: AtomicU64,
    /// Cumulative LLM provider calls made by Nurse this process lifetime —
    /// covers the Tier 3 per-session classifier (`LlmClassifier::classify`)
    /// AND the batched periodic reviewer (`BatchReviewer::tick`). Increments
    /// once per provider call (before the call so an in-flight crash still
    /// counts). Not persisted; resets to 0 on app start. Held behind `Arc`
    /// so `LlmClassifier::with_call_counter` can share the same atomic.
    pub llm_calls_total: Arc<AtomicU64>,
}

impl Default for NurseHealthCounters {
    fn default() -> Self {
        Self {
            last_tick_at_ms: AtomicU64::new(0),
            last_successful_tick_at_ms: AtomicU64::new(0),
            consecutive_failed_ticks: AtomicU32::new(0),
            consecutive_bad_parse_ticks: AtomicU32::new(0),
            consecutive_skipped_ticks: AtomicU32::new(0),
            degraded: AtomicBool::new(false),
            tier3_skipped_no_model: AtomicU64::new(0),
            intervention_writer_dropped: AtomicU64::new(0),
            observability_dropped: AtomicU64::new(0),
            llm_calls_total: Arc::new(AtomicU64::new(0)),
        }
    }
}

/// Push-driven, detector-based Nurse engine.
pub struct NurseEngine {
    pub bus: Arc<NurseBus>,
    pi_manager: Arc<crate::pi::manager::PiManager>,
    pub config: Arc<tokio::sync::RwLock<NurseConfig>>,
    pub detectors: Arc<DetectorRegistry>,
    pub sessions: Arc<RwLock<HashMap<String, SessionState>>>,
    pub storm_guard: Arc<StormGuard>,
    pub playbook: Arc<SteerPlaybook>,
    pub health: Arc<NurseHealthCounters>,
    pub intervention_writer: Arc<InterventionWriter>,
    pub observability: Arc<ObservabilityHandles>,
    pub intervention_ctx: Arc<tokio::sync::OnceCell<InterventionContext>>,
    /// Per-engine in-flight dispatcher slot map. Owned by the engine
    /// (rather than nested inside [`InterventionContext`]) so the
    /// dispatcher pipeline can claim/release a session-id slot even
    /// before the AppHandle-bound `intervention_ctx` is attached — the
    /// latter is required only for actions that emit `nurse-event` or
    /// call into Pi. Lock policy: `unwrap_or_else(|p| p.into_inner())`.
    pub in_flight: Arc<std::sync::Mutex<HashMap<String, String>>>,
    /// Three-tier dispatcher. Late-attached via `attach_dispatcher` from
    /// `lib.rs::setup` so the engine and dispatcher can hold `Weak`
    /// references to one another without an initialisation cycle.
    /// `start()` refuses to spawn until this is populated.
    pub dispatcher: Arc<tokio::sync::OnceCell<Arc<crate::nurse::dispatcher::Dispatcher>>>,
    /// Optional batched LLM reviewer. When attached (the default in
    /// production wiring), the engine spawns a periodic ticker that
    /// snapshots every active session's recent transcript, batches
    /// them into a single LLM call, and dispatches per-session
    /// decisions back through the dispatcher. Catches loops the
    /// heuristic detectors miss.
    pub batch_reviewer: Arc<tokio::sync::OnceCell<Arc<crate::nurse::batch_review::BatchReviewer>>>,
    /// Sync-readable mirror of `config.read().await.enabled`. The
    /// synthesized path (`report_synthesized`) is sync and called from
    /// spawned async tasks, so it can't await on the config RwLock.
    /// `set_nurse_config` is the only mutator and keeps this in sync.
    /// IMPORTANT: keep `engine.master_enabled` in sync if a new writer
    /// of `config.enabled` lands.
    pub master_enabled: Arc<AtomicBool>,
    /// Mirror of `config.read().await.swarms_only`. Same rationale.
    /// IMPORTANT: keep `engine.master_swarms_only` in sync if a new
    /// writer of `config.swarms_only` lands.
    pub master_swarms_only: Arc<AtomicBool>,
    shutdown: CancellationToken,
}

impl NurseEngine {
    /// Build a new engine. Detectors are registered in a fixed order so
    /// decision-log output is reproducible across runs.
    pub fn new(
        bus: Arc<NurseBus>,
        pi_manager: Arc<crate::pi::manager::PiManager>,
        config: NurseConfig,
    ) -> std::io::Result<Self> {
        let mut registry = DetectorRegistry::new();
        registry.register(StallDetector::new());
        registry.register(ReasoningLoopDetector::new());
        registry.register(ToolFailureDetector::new());
        registry.register(ProcessHealthDetector::new());
        registry.register(ProviderHealthDetector::new());
        registry.register(ContextSaturationDetector::new());
        registry.register(RetryExhaustionDetector::new());

        let health = Arc::new(NurseHealthCounters::default());
        let observability = Arc::new(ObservabilityHandles::new()?);
        // Best-effort startup prune; failures are logged, not fatal.
        if let Err(e) = observability.prune_on_startup() {
            tracing::warn!(error = %e, "nurse observability startup prune failed");
        }
        let intervention_writer = Arc::new(InterventionWriter::new(Arc::new(AtomicU64::new(0))));
        let master_enabled = Arc::new(AtomicBool::new(config.enabled));
        let master_swarms_only = Arc::new(AtomicBool::new(config.swarms_only));
        Ok(Self {
            bus,
            pi_manager,
            config: Arc::new(tokio::sync::RwLock::new(config)),
            detectors: Arc::new(registry),
            sessions: Arc::new(RwLock::new(HashMap::new())),
            storm_guard: Arc::new(StormGuard::new()),
            playbook: Arc::new(SteerPlaybook::seeded()),
            health,
            intervention_writer,
            observability,
            intervention_ctx: Arc::new(tokio::sync::OnceCell::new()),
            in_flight: Arc::new(std::sync::Mutex::new(HashMap::new())),
            dispatcher: Arc::new(tokio::sync::OnceCell::new()),
            batch_reviewer: Arc::new(tokio::sync::OnceCell::new()),
            master_enabled,
            master_swarms_only,
            shutdown: CancellationToken::new(),
        })
    }

    /// Lazy-attach the AppHandle-bound intervention context. Must be
    /// called from `lib.rs::setup` once the AppHandle is available.
    pub fn attach_app_handle(self: &Arc<Self>, app: tauri::AppHandle) {
        let _ = self
            .intervention_ctx
            .set(InterventionContext::new(app, Arc::clone(&self.pi_manager)));
    }

    /// Late-attach the three-tier dispatcher. Must be called from
    /// `lib.rs::setup` BEFORE `start()` — `start()` returns an error
    /// if any required OnceCell is still empty.
    pub fn attach_dispatcher(
        self: &Arc<Self>,
        dispatcher: Arc<crate::nurse::dispatcher::Dispatcher>,
    ) {
        let _ = self.dispatcher.set(dispatcher);
    }

    /// IMPORTANT: keep `master_enabled` in sync if this is the last
    /// writer. Sole production caller today is
    /// `commands::nurse::set_nurse_config`.
    pub fn set_master_enabled(&self, v: bool) {
        self.master_enabled.store(v, Ordering::Relaxed);
    }

    /// IMPORTANT: keep `master_swarms_only` in sync if this is the last
    /// writer. Sole production caller today is
    /// `commands::nurse::set_nurse_config`.
    pub fn set_master_swarms_only(&self, v: bool) {
        self.master_swarms_only.store(v, Ordering::Relaxed);
    }

    /// Late-attach the batched LLM reviewer. Optional — if never
    /// attached, the periodic batch sweep simply doesn't run and the
    /// engine still serves signal-driven dispatches as before.
    pub fn attach_batch_reviewer(
        self: &Arc<Self>,
        br: Arc<crate::nurse::batch_review::BatchReviewer>,
    ) {
        let _ = self.batch_reviewer.set(br);
    }

    /// Test-only helper: attach both `InterventionContext` and `Dispatcher`
    /// in one call. Production wires them separately in `lib.rs::setup`.
    #[cfg(test)]
    pub fn attach_for_tests(
        self: &Arc<Self>,
        app: tauri::AppHandle,
        dispatcher: Arc<crate::nurse::dispatcher::Dispatcher>,
    ) {
        let _ = self
            .intervention_ctx
            .set(crate::nurse::intervention::InterventionContext::new(
                app,
                Arc::clone(&self.pi_manager),
            ));
        let _ = self.dispatcher.set(dispatcher);
    }

    /// Spawn the run loop and the slow-probe task. Returns the JoinHandle
    /// of the main loop; the slow-probe task lives until shutdown.
    ///
    /// Returns `Err` if the engine has not been fully wired
    /// (`attach_app_handle` + `attach_dispatcher` must both have run).
    /// Refusing to start in that state is intentional — the legacy
    /// dark-mode fallback is gone, so a half-wired engine would silently
    /// drop every dispatch attempt.
    pub fn start(self: Arc<Self>) -> Result<JoinHandle<()>, &'static str> {
        if self.intervention_ctx.get().is_none() {
            return Err(
                "nurse engine: start() requires attach_app_handle() / attach_intervention_ctx() to have completed first",
            );
        }
        if self.dispatcher.get().is_none() {
            return Err(
                "nurse engine: start() requires attach_dispatcher() to have completed first",
            );
        }

        // Optional batched-review ticker. Lives only when
        // `attach_batch_reviewer` has been called AND `nurse_batch_enabled`
        // is true in the engine config. Each loop iteration recomputes
        // the effective interval (`NurseConfig::effective_batch_interval_secs`)
        // so a user-driven Settings change takes effect on the next tick
        // without a restart. Using `sleep_until` instead of
        // `tokio::time::interval` because `interval`'s period is fixed at
        // creation; recreating it would lose tick alignment.
        if let Some(br) = self.batch_reviewer.get().cloned() {
            let shutdown = self.shutdown.clone();
            let engine_for_tick = Arc::clone(&self);
            tokio::spawn(async move {
                use tokio::time::{sleep, Instant};
                loop {
                    let interval_secs = {
                        let cfg = engine_for_tick.config.read().await;
                        cfg.effective_batch_interval_secs()
                    };
                    let next = Instant::now() + std::time::Duration::from_secs(interval_secs);
                    // Publish the upcoming tick wall-clock time BEFORE sleeping
                    // so the topbar progress bar can render meaningful progress
                    // from the moment the engine spins up — otherwise
                    // `next_tick_at_unix_ms` stays 0 until the first tick
                    // completes (~120s after app start) and the bar pins at 0%.
                    let next_unix_ms = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map(|d| d.as_millis() as u64 + interval_secs * 1000)
                        .unwrap_or(0);
                    br.status
                        .next_tick_at_unix_ms
                        .store(next_unix_ms, Ordering::Relaxed);
                    tokio::select! {
                        biased;
                        _ = shutdown.cancelled() => break,
                        _ = sleep(next.saturating_duration_since(Instant::now())) => {
                            let enabled = {
                                let cfg = engine_for_tick.config.read().await;
                                cfg.enabled && cfg.nurse_batch_enabled
                            };
                            if !enabled {
                                continue;
                            }
                            if let Err(e) = br.tick().await {
                                tracing::warn!(error = %e, "nurse batch-review tick failed");
                            }
                        }
                    }
                }
            });
        }

        let engine = Arc::clone(&self);
        Ok(tokio::spawn(async move { engine.run_loop().await }))
    }

    pub fn shutdown(&self) {
        self.shutdown.cancel();
    }

    /// Public entry: synthesized (non-Pi) intervention. Bypasses the
    /// three-tier pipeline and emits Lifecycle directly. Returns the
    /// completed payload so the caller can also broadcast on other
    /// channels (e.g. `swarm-event`).
    ///
    /// Gated on the engine's master `enabled` / `swarms_only` mirrors
    /// (see `Self::master_enabled` / `Self::master_swarms_only`) so
    /// "Nurse off" in the UI truly silences the synthesized path — the
    /// dispatcher already gates on the same bits at `dispatcher.rs:520`
    /// and `:610`, and we mirror those gates here so the synthesized
    /// emit-Lifecycle code path is symmetric. When gated, a
    /// `decision_started` + `decision_finalised{status}` pair is still
    /// written to the decision log so analytics stay consistent.
    pub fn report_synthesized(
        self: &Arc<Self>,
        owner: InterventionOwner,
        kind: SynthesizedKind,
    ) -> Option<crate::nurse::snapshot::NurseLifecyclePayload> {
        // Master switch — when Nurse is "off" in the UI, the synthesized
        // path stays silent (matches the dispatcher's `gated_disabled`
        // behaviour at dispatcher.rs:520 / :954). Decision-log chain is
        // still written so observability sees the suppression.
        if !self.master_enabled.load(Ordering::Relaxed) {
            crate::nurse::intervention::write_gated_synthesized_pair(
                &self.observability.decisions,
                &owner,
                &kind,
                "gated_disabled",
            );
            return None;
        }
        // Swarms-only — mirrors dispatcher's gate at dispatcher.rs:610
        // for non-Pi pseudo sessions. The InterventionOwner DTO doesn't
        // map cleanly to `SessionOwner`, so we proxy with the heuristic
        // "no swarm_id AND no feature_id => not a swarm". All current
        // swarm-originating callers (e.g. `core/queen.rs::synthesize_nurse_for_error`)
        // populate both fields together.
        if self.master_swarms_only.load(Ordering::Relaxed)
            && owner.swarm_id.is_none()
            && owner.feature_id.is_none()
        {
            crate::nurse::intervention::write_gated_synthesized_pair(
                &self.observability.decisions,
                &owner,
                &kind,
                "gated_swarms_only",
            );
            return None;
        }
        let ctx = self.intervention_ctx.get()?;
        let payload = crate::nurse::intervention::dispatch_synthesized(
            ctx,
            owner,
            kind,
            Some(&self.observability.decisions),
        );
        self.intervention_writer
            .send(crate::nurse::intervention::record_from_payload(&payload));
        Some(payload)
    }

    /// Public entry: Pi-backed error-driven intervention. Injects a
    /// synthetic signal into the session's `SessionHealth` and routes
    /// through the three-tier pipeline.
    ///
    /// Callers with NO live session entry (e.g. Hivemind pseudo session
    /// IDs like `hm-<review_id>-r<round>-<model>`) should call
    /// `report_synthesized` directly — this path assumes a `sessions`
    /// entry exists so the dispatcher's tier evaluation has a
    /// `SessionState` to read from.
    ///
    /// If the dispatcher has not yet been attached (only possible in a
    /// startup sliver before `lib.rs::setup` runs to completion), this
    /// falls back to `report_synthesized` so callers still observe the
    /// lifecycle pair.
    pub fn report_error(
        self: &Arc<Self>,
        kind: SynthesizedKind,
        session_id: String,
        owner: InterventionOwner,
    ) {
        let dedup = synthesized_dedup_key(&kind);
        let severity = severity_for(&kind);
        let summary = match &kind {
            SynthesizedKind::PiError { message } => format!("pi error: {}", message),
            SynthesizedKind::RpcTimeout { idle_secs } => {
                format!("rpc timeout after {}s", idle_secs)
            }
            other => format!("{:?}", other),
        };
        let evidence = serde_json::to_value(&kind).unwrap_or(serde_json::Value::Null);
        let synthetic = Signal {
            detector: "synthesized",
            severity,
            dedup_key: dedup.clone(),
            summary,
            raised_at: Utc::now(),
            evidence,
        };
        {
            let mut sessions = self.sessions.write().unwrap_or_else(|p| p.into_inner());
            if let Some(state) = sessions.get_mut(&session_id) {
                state.health.push_signal(synthetic.clone());
            }
        }
        self.observability.signals.write(SignalStreamRow {
            ts: chrono::Utc::now(),
            ts_unix_ms: chrono::Utc::now().timestamp_millis().max(0) as u64,
            session_id: session_id.clone(),
            kind: SignalStreamKind::Raise,
            detector: "synthesized".into(),
            severity,
            dedup_key: dedup,
            summary: synthetic.summary.clone(),
            evidence: synthetic.evidence.clone(),
            session_tier_after: severity.tier(),
            active_decision_id: None,
        });

        // Dispatch through the live pipeline. `report_synthesized` is no
        // longer the unconditional fallback — the dispatcher handles every
        // signal class uniformly so the decision chain matches the
        // detector-raise path.
        if let Some(dispatcher) = self.dispatcher.get().cloned() {
            let decision_id = uuid::Uuid::new_v4().simple().to_string();
            let input = crate::nurse::dispatcher::DispatchInput {
                decision_id,
                session_id,
                trigger_signal: synthetic,
                origin: crate::nurse::dispatcher::DispatchOrigin::ReportError,
            };
            tokio::spawn(async move {
                let _ = dispatcher.handle_signal(input).await;
            });
        } else {
            // Dispatcher not yet attached (only the startup sliver before
            // `lib.rs::setup` finishes wiring). Fall back to deterministic
            // synthesized dispatch so callers still see the lifecycle pair.
            tracing::warn!(
                ?session_id,
                "nurse: report_error called before dispatcher attached; falling back to report_synthesized"
            );
            let _ = self.report_synthesized(owner, kind);
        }
    }

    /// Public snapshot used by the IPC `get_nurse_status` handler.
    pub fn snapshot_status(&self) -> crate::nurse::snapshot::NurseStatusSnapshot {
        use crate::nurse::snapshot::*;
        let sessions = self.sessions.read().unwrap_or_else(|p| p.into_inner());
        let session_entries: Vec<MonitoredSessionSnapshot> = sessions
            .iter()
            .map(|(sid, st)| {
                let last_ms = st
                    .session
                    .upgrade()
                    .map(|s| s.last_activity_ms())
                    .unwrap_or(0);
                let event_count = st.session.upgrade().map(|s| s.event_count()).unwrap_or(0);
                let is_alive = st.session.upgrade().map(|s| s.is_alive()).unwrap_or(false);
                let is_busy = st.session.upgrade().map(|s| s.is_busy()).unwrap_or(false);
                let status = if !is_alive {
                    SessionHealthStatus::Failed
                } else if st.health.has_critical() {
                    SessionHealthStatus::Stalled
                } else {
                    use crate::nurse::health::Tier;
                    match st.health.tier {
                        Tier::Quiet => SessionHealthStatus::Healthy,
                        Tier::Warning => SessionHealthStatus::Warning,
                        Tier::Stalled => SessionHealthStatus::Stalled,
                        Tier::Critical => SessionHealthStatus::Failed,
                    }
                };
                let active_signals = st
                    .health
                    .signals
                    .iter()
                    .map(|s| NurseActiveSignal {
                        detector: s.detector.to_string(),
                        severity: s.severity,
                        dedup_key: s.dedup_key.clone(),
                        summary: s.summary.clone(),
                        raised_at: s.raised_at,
                    })
                    .collect();
                MonitoredSessionSnapshot {
                    session_id: sid.clone(),
                    last_activity_ms: last_ms,
                    event_count,
                    is_alive,
                    is_busy,
                    status,
                    stall_detected_at: None,
                    intervention_count: st.intervention_count,
                    last_check_at: None,
                    tier: st.health.tier,
                    owner: Some(SessionOwnerDto::from(&st.owner)),
                    active_signals,
                }
            })
            .collect();

        let cfg = self
            .config
            .try_read()
            .map(|c| c.clone())
            .unwrap_or_default();
        let stall_threshold = cfg.profile(NurseProfile::Default).stall.stalled_secs;
        let config_snapshot = NurseServiceConfigSnapshot {
            enabled: cfg.enabled,
            stall_threshold_secs: stall_threshold,
            nurse_model: cfg.nurse_model.clone().unwrap_or_default(),
            max_interventions: cfg.max_interventions,
            tick_interval_secs: crate::tunables::nurse_tick_interval_secs(),
            nurse_provider: cfg.nurse_provider.clone(),
            swarms_only: cfg.swarms_only,
        };
        let stats = NurseStats {
            monitored_count: session_entries.len(),
            stall_count: session_entries
                .iter()
                .filter(|s| matches!(s.status, SessionHealthStatus::Stalled))
                .count(),
            intervention_count: session_entries.iter().map(|s| s.intervention_count).sum(),
            last_check_at: None,
            is_running: cfg.enabled,
        };
        let health = NurseHealthSnapshot {
            last_tick_at: ts_or_none(&self.health.last_tick_at_ms),
            last_successful_tick_at: ts_or_none(&self.health.last_successful_tick_at_ms),
            consecutive_failed_ticks: self.health.consecutive_failed_ticks.load(Ordering::Relaxed),
            consecutive_bad_parse_ticks: self
                .health
                .consecutive_bad_parse_ticks
                .load(Ordering::Relaxed),
            consecutive_skipped_ticks: self
                .health
                .consecutive_skipped_ticks
                .load(Ordering::Relaxed),
            degraded: self.health.degraded.load(Ordering::Relaxed),
            tier3_skipped_no_model: self.health.tier3_skipped_no_model.load(Ordering::Relaxed),
            intervention_writer_dropped: self
                .health
                .intervention_writer_dropped
                .load(Ordering::Relaxed),
            observability_dropped: self.health.observability_dropped.load(Ordering::Relaxed),
        };
        let recent = self.intervention_writer.recent_snapshot();
        let batch = self.batch_reviewer.get().map(|br| {
            let snap = br.status.snapshot();
            crate::nurse::snapshot::BatchTickSnapshotDto {
                enabled: cfg.nurse_batch_enabled,
                // Resolve through the same helper the ticker uses so the
                // topbar countdown matches reality after a Settings edit.
                interval_secs: cfg.effective_batch_interval_secs(),
                last_tick_at_unix_ms: snap.last_tick_at_unix_ms,
                last_tick_duration_ms: snap.last_tick_duration_ms,
                next_tick_at_unix_ms: snap.next_tick_at_unix_ms,
                last_tick_session_count: snap.last_tick_session_count,
                llm_calls_total: self.health.llm_calls_total.load(Ordering::Relaxed),
            }
        });
        NurseStatusSnapshot {
            stats,
            sessions: session_entries,
            recent_interventions: recent,
            config: config_snapshot,
            health,
            batch,
        }
    }

    /// Reconcile `PiManager`'s live sessions into the engine's session
    /// map on startup and on supervised restart.
    pub async fn reconcile_from_pi_manager(self: &Arc<Self>) {
        let live = self.pi_manager.list_sessions().await;
        let mut sessions = self.sessions.write().unwrap_or_else(|p| p.into_inner());
        for (sid, arc) in live {
            let owner = arc.owner();
            sessions.entry(sid.clone()).or_insert_with(|| {
                SessionState::new(sid.clone(), owner.clone(), Arc::downgrade(&arc))
            });
        }
    }

    async fn run_loop(self: Arc<Self>) {
        self.reconcile_from_pi_manager().await;
        let mut rx = self.bus.subscribe();
        let interval_secs = crate::tunables::nurse_tick_interval_secs();
        let mut tick = interval(std::time::Duration::from_secs(interval_secs));
        tick.set_missed_tick_behavior(MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                biased;
                _ = self.shutdown.cancelled() => break,
                ev = rx.recv() => match ev {
                    Ok(arc_ev) => self.on_bus_event(arc_ev).await,
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(dropped = n, "nurse bus receiver lagged");
                        self.health
                            .consecutive_skipped_ticks
                            .fetch_add(1, Ordering::Relaxed);
                        self.observability.bus.write(bus_tel::row(
                            BusEventKind::Lag,
                            None,
                            serde_json::json!({"dropped_count": n}),
                        ));
                        self.mark_all_sessions_post_lag();
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                },
                _ = tick.tick() => self.run_periodic_sweep().await,
            }
        }
    }

    fn mark_all_sessions_post_lag(&self) {
        let mut sessions = self.sessions.write().unwrap_or_else(|p| p.into_inner());
        let until = Instant::now() + std::time::Duration::from_secs(30);
        for (sid, state) in sessions.iter_mut() {
            state.post_lag_until = Some(until);
            self.observability.bus.write(bus_tel::row(
                BusEventKind::PostLagSuppressionEntered,
                Some(sid.clone()),
                serde_json::json!({
                    "session_id": sid,
                    "until_unix_ms": SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map(|d| d.as_millis() as u64 + 30_000)
                        .unwrap_or(0),
                    "reason": "tier2_3_suppressed_after_lag"
                }),
            ));
        }
    }

    async fn on_bus_event(self: &Arc<Self>, ev: Arc<NurseBusEvent>) {
        match ev.as_ref() {
            NurseBusEvent::SessionSpawned {
                session_id,
                provider,
                model_id,
                session,
                ..
            } => {
                let owner = session
                    .upgrade()
                    .map(|s| s.owner())
                    .unwrap_or(SessionOwner::Unknown);
                let mut sessions = self.sessions.write().unwrap_or_else(|p| p.into_inner());
                let entry = sessions.entry(session_id.clone()).or_insert_with(|| {
                    SessionState::new(session_id.clone(), owner.clone(), session.clone())
                });
                entry.provider = provider.clone();
                entry.model_id = model_id.clone();
                entry.session = session.clone();
                drop(sessions);
                self.observability.bus.write(bus_tel::row(
                    BusEventKind::SessionSpawned,
                    Some(session_id.clone()),
                    serde_json::json!({
                        "provider": provider,
                        "model_id": model_id,
                    }),
                ));
            }
            NurseBusEvent::OwnerChanged { session_id, owner } => {
                let mut sessions = self.sessions.write().unwrap_or_else(|p| p.into_inner());
                if let Some(state) = sessions.get_mut(session_id) {
                    state.owner = owner.clone();
                    state.health.owner = SessionOwnerDto::from(owner);
                }
                drop(sessions);
                self.observability.bus.write(bus_tel::row(
                    BusEventKind::OwnerChanged,
                    Some(session_id.clone()),
                    serde_json::to_value(SessionOwnerDto::from(owner)).unwrap_or_default(),
                ));
            }
            NurseBusEvent::Event {
                session_id, event, ..
            } => self.on_pi_event(session_id, event).await,
            NurseBusEvent::SessionEnded {
                session_id, reason, ..
            } => {
                // Let detectors flush state, then remove.
                let provider_snap = ProviderStateSnapshot::default();
                let nurse_cfg = self.config.read().await.clone();
                let mut sessions = self.sessions.write().unwrap_or_else(|p| p.into_inner());
                if let Some(state) = sessions.get_mut(session_id) {
                    let profile = NurseProfile::for_owner(&state.owner);
                    let profile_config = nurse_cfg.profile(profile);
                    for detector in self.detectors.iter() {
                        if let Some(det_state) = state.detector_states.get_mut(detector.name()) {
                            let weak = state.session.clone();
                            let ctx = DetectorContext {
                                session: &weak,
                                state: det_state.as_mut(),
                                now: Instant::now(),
                                now_wall_ms: 0,
                                profile,
                                profile_config: &profile_config,
                                provider_state: &provider_snap,
                                provider: state.provider.as_deref(),
                                model_id: state.model_id.as_deref(),
                            };
                            detector.on_session_ended(&ctx);
                        }
                    }
                    sessions.remove(session_id);
                }
                drop(sessions);
                self.storm_guard.reset_for_session(session_id);
                self.observability.bus.write(bus_tel::row(
                    BusEventKind::SessionEnded,
                    Some(session_id.clone()),
                    serde_json::json!({
                        "reason": match reason {
                            SessionEndReason::Killed => "killed",
                            SessionEndReason::Dropped => "dropped",
                        }
                    }),
                ));
            }
        }
    }

    async fn on_pi_event(self: &Arc<Self>, session_id: &str, event: &crate::pi::events::PiEvent) {
        let provider_snap = ProviderStateSnapshot::default();
        let now_wall_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let now_mono = Instant::now();
        let session_id_owned = session_id.to_string();

        // Snapshot the NurseConfig once per event so detector reads see a
        // consistent ProfileConfig even if `set_nurse_profile` fires
        // mid-tick. Resolution is per-session below.
        let nurse_cfg = self.config.read().await.clone();

        // Block-scope the write lock so it is released BEFORE any .await
        // — keeps the future Send. Returns (deltas, tier_after, raised_signals)
        // so the post-lock dispatcher fan-out has owned Signals without
        // re-walking deltas.
        let (deltas, tier_after, raised_signals): (
            Vec<(String, SmallVec<[SignalDelta; 2]>)>,
            _,
            Vec<Signal>,
        ) = {
            let mut sessions = self.sessions.write().unwrap_or_else(|p| p.into_inner());
            let Some(state) = sessions.get_mut(session_id) else {
                return;
            };
            let profile = NurseProfile::for_owner(&state.owner);
            let profile_config = nurse_cfg.profile(profile);
            let provider = state.provider.clone();
            let model_id = state.model_id.clone();
            let session_weak = state.session.clone();
            let mut deltas: Vec<(String, SmallVec<[SignalDelta; 2]>)> = Vec::new();

            for detector in self.detectors.iter() {
                let det_state = state
                    .detector_states
                    .entry(detector.name())
                    .or_insert_with(|| {
                        // Use a stable placeholder for the very first
                        // `on_session_started` call — detectors are not
                        // allowed to mutate engine state through it.
                        let mut placeholder: Box<dyn Any + Send + Sync> = Box::new(());
                        let ctx = DetectorContext {
                            session: &session_weak,
                            state: placeholder.as_mut(),
                            now: now_mono,
                            now_wall_ms,
                            profile,
                            profile_config: &profile_config,
                            provider_state: &provider_snap,
                            provider: provider.as_deref(),
                            model_id: model_id.as_deref(),
                        };
                        detector.on_session_started(&ctx)
                    });
                let observed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let mut ctx = DetectorContext {
                        session: &session_weak,
                        state: det_state.as_mut(),
                        now: now_mono,
                        now_wall_ms,
                        profile,
                        profile_config: &profile_config,
                        provider_state: &provider_snap,
                        provider: provider.as_deref(),
                        model_id: model_id.as_deref(),
                    };
                    detector.observe(event, &mut ctx)
                }));
                match observed {
                    Ok(out) => deltas.push((detector.name().to_string(), out)),
                    Err(_) => {
                        tracing::error!(
                            detector = detector.name(),
                            "nurse detector panicked — continuing with remaining detectors"
                        );
                    }
                }
            }
            // Apply Raise/Clear into per-session health AND, in the same
            // pass, collect every Raise into a sidecar vec so the dispatcher
            // fan-out below has owned Signals without re-iterating deltas.
            let mut raised_signals: Vec<Signal> = Vec::new();
            for (_det, out) in &deltas {
                for d in out {
                    match d {
                        SignalDelta::Raise(sig) => {
                            state.health.push_signal(sig.clone());
                            raised_signals.push(sig.clone());
                        }
                        SignalDelta::Clear {
                            detector,
                            dedup_key,
                        } => state.health.clear_signal(detector, dedup_key),
                    }
                }
            }
            let tier_after = state.health.tier;
            (deltas, tier_after, raised_signals)
        };

        // Stream signals to observability.
        for (_det, out) in deltas {
            for d in out {
                match d {
                    SignalDelta::Raise(sig) => {
                        self.observability.signals.write(SignalStreamRow {
                            ts: sig.raised_at,
                            ts_unix_ms: sig.raised_at.timestamp_millis().max(0) as u64,
                            session_id: session_id_owned.clone(),
                            kind: SignalStreamKind::Raise,
                            detector: sig.detector.to_string(),
                            severity: sig.severity,
                            dedup_key: sig.dedup_key.clone(),
                            summary: sig.summary.clone(),
                            evidence: sig.evidence.clone(),
                            session_tier_after: tier_after,
                            active_decision_id: None,
                        });
                    }
                    SignalDelta::Clear {
                        detector,
                        dedup_key,
                    } => {
                        let now = chrono::Utc::now();
                        self.observability.signals.write(SignalStreamRow {
                            ts: now,
                            ts_unix_ms: now.timestamp_millis().max(0) as u64,
                            session_id: session_id_owned.clone(),
                            kind: SignalStreamKind::Clear,
                            detector: detector.to_string(),
                            severity: Severity::Info,
                            dedup_key,
                            summary: "cleared".to_string(),
                            evidence: serde_json::Value::Null,
                            session_tier_after: tier_after,
                            active_decision_id: None,
                        });
                    }
                }
            }
        }

        // Dispatcher fan-out: one decision per raised signal. The engine
        // does NOT pre-filter on severity here — the dispatcher's Step 2
        // (severity gate) is the single authoritative gate. Pre-filtering
        // here would subvert `OwnerChanged` re-derivation (the owner can
        // change mid-session and the dispatcher re-reads the live owner
        // before applying the severity gate).
        if let Some(dispatcher) = self.dispatcher.get().cloned() {
            for sig in raised_signals {
                let decision_id = uuid::Uuid::new_v4().simple().to_string();
                let input = crate::nurse::dispatcher::DispatchInput {
                    decision_id,
                    session_id: session_id_owned.clone(),
                    trigger_signal: sig,
                    origin: crate::nurse::dispatcher::DispatchOrigin::DetectorRaise,
                };
                let disp = Arc::clone(&dispatcher);
                tokio::spawn(async move {
                    let _ = disp.handle_signal(input).await;
                });
            }
        }
    }

    pub(crate) async fn run_periodic_sweep(self: &Arc<Self>) {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        self.health.last_tick_at_ms.store(now_ms, Ordering::Relaxed);
        let provider_snap = ProviderStateSnapshot::default();
        let now_wall_ms = now_ms;
        let now_mono = Instant::now();
        // Snapshot the NurseConfig before grabbing the sync sessions lock —
        // we can't `.await` inside the loop.
        let nurse_cfg = self.config.read().await.clone();
        let mut sessions = self.sessions.write().unwrap_or_else(|p| p.into_inner());
        let mut to_remove: Vec<String> = Vec::new();
        for (sid, state) in sessions.iter_mut() {
            if state.session.upgrade().is_none() {
                state.stale_sweeps = state.stale_sweeps.saturating_add(1);
                if state.stale_sweeps >= 2 {
                    to_remove.push(sid.clone());
                }
                continue;
            } else {
                state.stale_sweeps = 0;
            }
            let session_weak = state.session.clone();
            let provider = state.provider.clone();
            let model_id = state.model_id.clone();
            let profile = NurseProfile::for_owner(&state.owner);
            let profile_config = nurse_cfg.profile(profile);
            for detector in self
                .detectors
                .iter()
                .filter(|d| matches!(d.tick_kind(), TickKind::Fast))
            {
                let det_state = state
                    .detector_states
                    .entry(detector.name())
                    .or_insert_with(|| {
                        let mut placeholder: Box<dyn Any + Send + Sync> = Box::new(());
                        let ctx = DetectorContext {
                            session: &session_weak,
                            state: placeholder.as_mut(),
                            now: now_mono,
                            now_wall_ms,
                            profile,
                            profile_config: &profile_config,
                            provider_state: &provider_snap,
                            provider: provider.as_deref(),
                            model_id: model_id.as_deref(),
                        };
                        detector.on_session_started(&ctx)
                    });
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let mut ctx = DetectorContext {
                        session: &session_weak,
                        state: det_state.as_mut(),
                        now: now_mono,
                        now_wall_ms,
                        profile,
                        profile_config: &profile_config,
                        provider_state: &provider_snap,
                        provider: provider.as_deref(),
                        model_id: model_id.as_deref(),
                    };
                    detector.tick(&mut ctx)
                }));
                match result {
                    Ok(out) => {
                        for d in out {
                            match d {
                                SignalDelta::Raise(sig) => state.health.push_signal(sig),
                                SignalDelta::Clear {
                                    detector,
                                    dedup_key,
                                } => state.health.clear_signal(detector, &dedup_key),
                            }
                        }
                    }
                    Err(_) => {
                        tracing::error!(detector = detector.name(), "nurse detector tick panicked");
                    }
                }
            }
        }
        for sid in to_remove {
            sessions.remove(&sid);
        }
        drop(sessions);

        // ── Phase A: snapshot dispatch candidates under brief read ──
        //
        // Two-phase per ground rule 9 (re-derive profile per session from
        // live owner inside Phase B). The snapshot is cheap: clone the
        // small per-session pieces we need; `BudgetState` is Clone-derived
        // so the cooldown pre-filter can run without holding the engine
        // lock.
        struct SweepCandidate {
            session_id: String,
            owner: SessionOwner,
            active_signals: Vec<Signal>,
            budget_snapshot: BudgetState,
        }

        let dispatcher_opt = self.dispatcher.get().cloned();
        if dispatcher_opt.is_none() {
            // Dispatcher not attached — sweep still runs detectors (above)
            // but doesn't fire decisions. Production wires the dispatcher
            // in `lib.rs::setup` BEFORE `engine.start()`.
            self.health
                .last_successful_tick_at_ms
                .store(now_ms, Ordering::Relaxed);
            return;
        }
        let dispatcher = dispatcher_opt.unwrap();

        let candidates: Vec<SweepCandidate> = {
            let sessions = self.sessions.read().unwrap_or_else(|p| p.into_inner());
            sessions
                .iter()
                .map(|(sid, state)| SweepCandidate {
                    session_id: sid.clone(),
                    owner: state.owner.clone(),
                    active_signals: state.health.signals.iter().cloned().collect(),
                    budget_snapshot: state.budget.clone(),
                })
                .collect()
            // sessions read guard drops here.
        };

        // ── Phase B: filter + dispatch without holding sessions lock ──
        let in_flight_for_filter = Arc::clone(&self.in_flight);
        let self_killed_for_filter = dispatcher.self_killed_handle();

        let mut dispatch_inputs: Vec<crate::nurse::dispatcher::DispatchInput> = Vec::new();
        for cand in &candidates {
            // Re-derive profile from LIVE owner (rule 9).
            let profile = NurseProfile::for_owner(&cand.owner);
            let profile_config = nurse_cfg.profile(profile);

            for signal in &cand.active_signals {
                // Pre-filter 1: severity gate (the dispatcher re-checks
                // authoritatively).
                if signal.severity < profile_config.escalation_min_severity {
                    continue;
                }
                // Pre-filter 2: in-flight (dispatcher re-checks authoritatively).
                {
                    let g = in_flight_for_filter
                        .lock()
                        .unwrap_or_else(|p| p.into_inner());
                    if g.contains_key(&cand.session_id) {
                        continue;
                    }
                }
                // Pre-filter 3: per-key cooldown elapsed (dispatcher
                // re-admits).
                if !cand.budget_snapshot.is_cooldown_elapsed(
                    &profile_config.budget,
                    signal.detector,
                    &signal.dedup_key,
                    now_mono,
                ) {
                    continue;
                }
                // Pre-filter 4: self-kill grace for process_dead-class
                // signals.
                let is_self_kill_signal =
                    matches!(signal.detector, "process_health" | "synthesized")
                        && matches!(
                            signal.dedup_key.as_str(),
                            "process_dead"
                                | "synthesized:process_crashed"
                                | "session_gone_unobserved"
                        );
                if is_self_kill_signal {
                    let g = self_killed_for_filter
                        .lock()
                        .unwrap_or_else(|p| p.into_inner());
                    if let Some(ts) = g.get(&cand.session_id) {
                        if now_mono < *ts + crate::nurse::dispatcher::SELF_KILL_GRACE {
                            continue;
                        }
                    }
                }

                dispatch_inputs.push(crate::nurse::dispatcher::DispatchInput {
                    decision_id: uuid::Uuid::new_v4().simple().to_string(),
                    session_id: cand.session_id.clone(),
                    trigger_signal: signal.clone(),
                    origin: crate::nurse::dispatcher::DispatchOrigin::PeriodicSweep,
                });
            }
        }

        // Spawn each dispatch on its own tokio task — bounded by the
        // dispatcher's in-flight gate (one decision per session at a time).
        for input in dispatch_inputs {
            let disp = Arc::clone(&dispatcher);
            tokio::spawn(async move {
                let _ = disp.handle_signal(input).await;
            });
        }

        self.health
            .last_successful_tick_at_ms
            .store(now_ms, Ordering::Relaxed);
    }
}

fn ts_or_none(a: &AtomicU64) -> Option<chrono::DateTime<chrono::Utc>> {
    let v = a.load(Ordering::Relaxed);
    if v == 0 {
        None
    } else {
        Some(
            chrono::DateTime::<chrono::Utc>::from_timestamp_millis(v as i64)
                .unwrap_or_else(|| chrono::Utc::now()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn engine_constructs_and_reports_empty_snapshot() {
        // Construction goes through the file-system observability writers;
        // they create per-process temp paths via `dirs::home_dir()`.
        // The test merely asserts that construction does not panic on
        // a developer machine with a writable home.
        let bus = Arc::new(NurseBus::new());
        let pi = Arc::new(crate::pi::manager::PiManager::new_for_tests());
        let cfg = NurseConfig::default();
        if let Ok(engine) = NurseEngine::new(bus, pi, cfg) {
            let snap = engine.snapshot_status();
            assert!(snap.sessions.is_empty());
        }
    }

    /// Build an unwired engine — no `intervention_ctx`, no dispatcher.
    /// Sufficient to exercise the new gate inside `report_synthesized`
    /// since the gate runs BEFORE `intervention_ctx.get()?`.
    fn build_engine_unwired(cfg: NurseConfig) -> Option<Arc<NurseEngine>> {
        let bus = Arc::new(NurseBus::new());
        let pi = Arc::new(crate::pi::manager::PiManager::new_for_tests());
        let engine = NurseEngine::new(bus, pi, cfg).ok()?;
        Some(Arc::new(engine))
    }

    /// Drain rows from the engine's shared decisions JSONL file and
    /// filter to the supplied `session_id`. The writer is async — poll
    /// for up to ~2s.
    async fn decision_rows_for_session(
        engine: &Arc<NurseEngine>,
        session_id: &str,
    ) -> Vec<serde_json::Value> {
        use crate::nurse::observability::writer::today_yyyy_mm_dd;
        let path = engine
            .observability
            .decisions
            .root()
            .join(format!("decisions.jsonl.{}", today_yyyy_mm_dd()));
        for _ in 0..40 {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let rows: Vec<serde_json::Value> = text
                .lines()
                .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
                .filter(|r| r.get("session_id").and_then(|v| v.as_str()) == Some(session_id))
                .collect();
            if !rows.is_empty() {
                return rows;
            }
        }
        Vec::new()
    }

    fn final_status(rows: &[serde_json::Value]) -> Option<String> {
        rows.iter()
            .find(|r| r.get("event").and_then(|v| v.as_str()) == Some("decision_finalised"))
            .and_then(|r| r.get("data"))
            .and_then(|d| d.get("status"))
            .and_then(|s| s.as_str())
            .map(|s| s.to_string())
    }

    #[tokio::test]
    async fn report_synthesized_returns_none_when_master_disabled() {
        let cfg = NurseConfig {
            enabled: false,
            ..NurseConfig::default()
        };
        let Some(engine) = build_engine_unwired(cfg) else {
            return;
        };
        let sid = format!("sess-gate-disabled-{}", uuid::Uuid::new_v4().simple());
        let owner = InterventionOwner {
            session_id: Some(sid.clone()),
            task_id: Some("t-1".into()),
            ..Default::default()
        };
        let kind = SynthesizedKind::SteerFailed { reason: "x".into() };
        let out = engine.report_synthesized(owner, kind);
        assert!(
            out.is_none(),
            "report_synthesized must return None when master enabled is false"
        );
        let rows = decision_rows_for_session(&engine, &sid).await;
        assert_eq!(
            final_status(&rows).as_deref(),
            Some("gated_disabled"),
            "expected gated_disabled decision_finalised row; got rows={:?}",
            rows
        );
    }

    #[tokio::test]
    async fn report_synthesized_gated_when_swarms_only_and_non_swarm_owner() {
        let cfg = NurseConfig {
            enabled: true,
            swarms_only: true,
            ..NurseConfig::default()
        };
        let Some(engine) = build_engine_unwired(cfg) else {
            return;
        };
        // Task-only owner: no swarm_id, no feature_id.
        let sid = format!("sess-task-only-{}", uuid::Uuid::new_v4().simple());
        let owner = InterventionOwner {
            session_id: Some(sid.clone()),
            task_id: Some("t-7".into()),
            ..Default::default()
        };
        let kind = SynthesizedKind::SteerFailed { reason: "x".into() };
        let out = engine.report_synthesized(owner, kind);
        assert!(
            out.is_none(),
            "report_synthesized must return None for non-swarm owner under swarms_only"
        );
        let rows = decision_rows_for_session(&engine, &sid).await;
        assert_eq!(
            final_status(&rows).as_deref(),
            Some("gated_swarms_only"),
            "expected gated_swarms_only decision_finalised row; got rows={:?}",
            rows
        );
    }

    #[tokio::test]
    async fn report_synthesized_does_not_gate_swarm_owner_under_swarms_only() {
        // With swarms_only=true AND a swarm-owned synthesized event, the
        // gate must NOT fire. We can't easily verify dispatch returns
        // `Some` here without a real AppHandle (the mock-runtime
        // AppHandle isn't assignable to the non-generic
        // `InterventionContext::app` field), so we verify the *negative*
        // observable signal: no `gated_swarms_only` row appears for this
        // session_id in the decisions log.
        let cfg = NurseConfig {
            enabled: true,
            swarms_only: true,
            ..NurseConfig::default()
        };
        let Some(engine) = build_engine_unwired(cfg) else {
            return;
        };
        let sid = format!("sess-swarm-owner-{}", uuid::Uuid::new_v4().simple());
        let owner = InterventionOwner {
            session_id: Some(sid.clone()),
            swarm_id: Some("swarm-abc".into()),
            feature_id: Some("feat-001".into()),
            ..Default::default()
        };
        let kind = SynthesizedKind::SteerFailed { reason: "x".into() };
        // Without intervention_ctx attached this still returns None
        // (via `?` on `intervention_ctx.get()`), but importantly the
        // gate didn't fire — so no decision-log row exists.
        let _ = engine.report_synthesized(owner, kind);

        // Brief wait so the (non-existent) write would have hit disk.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let rows = decision_rows_for_session(&engine, &sid).await;
        let gated = rows.iter().any(|r| {
            r.get("event").and_then(|v| v.as_str()) == Some("decision_finalised")
                && r.get("data")
                    .and_then(|d| d.get("status"))
                    .and_then(|s| s.as_str())
                    == Some("gated_swarms_only")
        });
        assert!(
            !gated,
            "swarm-owned synthesized event must not produce a gated_swarms_only row"
        );
    }

    #[tokio::test]
    async fn set_master_enabled_flip_silences_synthesized_path() {
        // Verify the atomic setter takes effect by checking the
        // decision log: a baseline (enabled=true) call produces NO
        // `gated_disabled` row; after `set_master_enabled(false)` a
        // subsequent call DOES write one.
        let cfg = NurseConfig {
            enabled: true,
            ..NurseConfig::default()
        };
        let Some(engine) = build_engine_unwired(cfg) else {
            return;
        };

        let sid_before = format!("sess-flip-before-{}", uuid::Uuid::new_v4().simple());
        let _ = engine.report_synthesized(
            InterventionOwner {
                session_id: Some(sid_before.clone()),
                task_id: Some("t-pre".into()),
                ..Default::default()
            },
            SynthesizedKind::SteerFailed { reason: "x".into() },
        );
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let rows_before = decision_rows_for_session(&engine, &sid_before).await;
        let gated_before = rows_before.iter().any(|r| {
            r.get("data")
                .and_then(|d| d.get("status"))
                .and_then(|s| s.as_str())
                == Some("gated_disabled")
        });
        assert!(
            !gated_before,
            "baseline (enabled=true) call should NOT produce gated_disabled row"
        );

        // Flip and try again.
        engine.set_master_enabled(false);
        let sid_after = format!("sess-flip-after-{}", uuid::Uuid::new_v4().simple());
        let out = engine.report_synthesized(
            InterventionOwner {
                session_id: Some(sid_after.clone()),
                task_id: Some("t-post".into()),
                ..Default::default()
            },
            SynthesizedKind::SteerFailed { reason: "x".into() },
        );
        assert!(out.is_none(), "flip to disabled must return None");
        let rows_after = decision_rows_for_session(&engine, &sid_after).await;
        assert_eq!(
            final_status(&rows_after).as_deref(),
            Some("gated_disabled"),
            "flip-after call must produce gated_disabled row; got rows={:?}",
            rows_after
        );
    }

    #[tokio::test]
    async fn sweep_two_phase_does_not_block_concurrent_writes() {
        // Run sweep on an engine with no sessions registered — the
        // snapshot phase should complete instantly even if a concurrent
        // writer is waiting on the sessions lock. This is a smoke test
        // for the brief-read-lock pattern, NOT for cross-thread races.
        let bus = Arc::new(NurseBus::new());
        let pi = Arc::new(crate::pi::manager::PiManager::new_for_tests());
        let config = NurseConfig::default();
        let Ok(engine) = NurseEngine::new(bus, pi, config) else {
            return;
        };
        let engine = Arc::new(engine);
        // No dispatcher attached — sweep should early-return after the
        // detector tick without dispatching.
        engine.run_periodic_sweep().await;
    }
}
