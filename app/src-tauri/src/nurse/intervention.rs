//! Intervention dispatcher — the Tier-1/2/3 action surface that talks to
//! `PiManager`, `SwarmRegistry`, and the running-Hivemind cancellation map.
//!
//! Today this module hosts the minimum surface needed to drive
//! [`NurseEngine`](crate::nurse::engine::NurseEngine)'s dark-mode and
//! Step-6 synthesized paths: an `InterventionContext` carrying the
//! refs the dispatcher needs, plus the [`dispatch_synthesized`] entry
//! used by both `report_synthesized` and the deterministic-fallback
//! arm of `report_error`. The full three-tier dispatch with mandatory
//! kill verification lands in Step 5.
//!
//! Per the rewrite plan, kill verification is non-negotiable: after every
//! Cancel or Restart the dispatcher MUST poll `session.is_alive()` for up
//! to 10 s and escalate to `pi_manager.kill_session(...)` (force kill)
//! before declaring success.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use chrono::Utc;
use tauri::AppHandle;
use tauri::Emitter;
use uuid::Uuid;

use crate::nurse::config::NurseProfile;
use crate::nurse::dispatcher::EventSeq;
use crate::nurse::observability::decision_log::{events as dec_events, DecisionLogger};
use crate::nurse::snapshot::{
    NurseDispatchTier, NurseEvent, NurseInterventionRecord, NurseLifecyclePayload,
    NurseLifecycleStatus, NurseSessionAction, SessionOwnerDto,
};
use crate::nurse::synthesized::{
    dedup_key as synthesized_dedup_key, describe_synthesized, severity_for, InterventionOwner,
    SynthesizedKind,
};
use crate::pi::manager::{PiManager, PiManagerError};
use crate::pi::rpc::PiRpcError;
use crate::pi::session::PiSession;

/// Holds the long-lived refs the dispatcher needs.
pub struct InterventionContext {
    pub app: AppHandle,
    pub pi_manager: Arc<PiManager>,
    /// Per-session in-flight guard. Cleared by the dispatcher.
    pub in_flight: Arc<std::sync::Mutex<std::collections::HashMap<String, String>>>,
}

impl InterventionContext {
    pub fn new(app: AppHandle, pi_manager: Arc<PiManager>) -> Self {
        Self {
            app,
            pi_manager,
            in_flight: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        }
    }
}

/// Synthesized-path dispatch. Emits Started + Completed pair on
/// `nurse-event`. No live session: no Pi action, no kill verification.
/// Returns the completed payload so callers can also broadcast it on
/// other channels (e.g. `swarm-event`).
///
/// When `decision_logger` is `Some`, also writes a 3-row decision chain
/// (`decision_started` → `intervention_dispatched` → `decision_finalised`)
/// keyed by the same id as the `intervention_id`, so post-hoc
/// reconstruction works the same as any Tier 1/2/3 dispatch. The
/// `decision_id` (== `intervention_id`) is generated once and reused.
///
/// The decision-log envelope `owner` field is `None` here: the
/// `InterventionOwner` shape doesn't map cleanly to `SessionOwnerDto`
/// (which carries Pi-session role fields the synthesized path doesn't
/// have). Decision-log readers should reconstruct the owner from the
/// emitted `nurse-event` lifecycle payloads where needed.
pub fn dispatch_synthesized(
    ctx: &InterventionContext,
    owner: InterventionOwner,
    kind: SynthesizedKind,
    decision_logger: Option<&Arc<DecisionLogger>>,
) -> NurseLifecyclePayload {
    let (level, observation, action) = describe_synthesized(&kind);
    // Re-use the same id for the lifecycle `intervention_id` and the
    // decision-log `decision_id` so consumers can join the two streams
    // without an extra mapping table.
    let intervention_id = Uuid::new_v4().simple().to_string();
    let session_id = owner.session_id.clone().unwrap_or_default();
    let now = Utc::now();

    // Decision-log row 0: decision_started.
    if let Some(logger) = decision_logger {
        let severity = severity_for(&kind);
        let dedup = synthesized_dedup_key(&kind);
        let tier_at_birth = format!("{:?}", severity.tier()).to_lowercase();
        logger.write(dec_events::decision_started(
            &intervention_id,
            &Some(session_id.clone()),
            &None,
            &None,
            0,
            "report_synthesized",
            &tier_at_birth,
            "synthesized",
            severity,
            &dedup,
        ));
    }

    let started = NurseLifecyclePayload {
        intervention_id: intervention_id.clone(),
        status: NurseLifecycleStatus::Started,
        level,
        session_id: session_id.clone(),
        task_id: owner.task_id.clone(),
        swarm_id: owner.swarm_id.clone(),
        feature_id: owner.feature_id.clone(),
        review_id: owner.review_id.clone(),
        observation: observation.clone(),
        action: action.clone(),
        reasoning_delta: None,
        full_reasoning: None,
        error: None,
        timestamp: now,
    };
    let _ = ctx
        .app
        .emit("nurse-event", NurseEvent::Lifecycle(started.clone()));

    let completed = NurseLifecyclePayload {
        intervention_id: intervention_id.clone(),
        status: NurseLifecycleStatus::Completed,
        level,
        session_id: session_id.clone(),
        task_id: owner.task_id.clone(),
        swarm_id: owner.swarm_id.clone(),
        feature_id: owner.feature_id.clone(),
        review_id: owner.review_id.clone(),
        observation,
        action,
        reasoning_delta: None,
        full_reasoning: None,
        error: None,
        timestamp: Utc::now(),
    };
    let _ = ctx
        .app
        .emit("nurse-event", NurseEvent::Lifecycle(completed.clone()));

    // Decision-log rows 1 + 2: intervention_dispatched + decision_finalised.
    if let Some(logger) = decision_logger {
        let kind_str = format!("{:?}", level).to_lowercase();
        logger.write(dec_events::intervention_dispatched(
            &intervention_id,
            &Some(session_id.clone()),
            &None,
            &None,
            1,
            "synthesized",
            &kind_str,
            &completed.action,
        ));
        logger.write(dec_events::decision_finalised(
            &intervention_id,
            &Some(session_id.clone()),
            &None,
            &None,
            2,
            "dispatched_synthesized",
            0,
            3,
            serde_json::Value::Null,
        ));
    }

    completed
}

/// Build a legacy-shaped `NurseInterventionRecord` from a completed
/// lifecycle payload — used for the in-memory ring buffer.
pub fn record_from_payload(payload: &NurseLifecyclePayload) -> NurseInterventionRecord {
    NurseInterventionRecord {
        id: payload.intervention_id.clone(),
        session_id: payload.session_id.clone(),
        timestamp: payload.timestamp,
        level: payload.level,
        analysis: payload.observation.clone(),
        action_taken: NurseSessionAction {
            level: payload.level,
            session_id: payload.session_id.clone(),
            message: payload.action.clone(),
            timestamp: payload.timestamp,
        },
        outcome: payload.error.clone(),
    }
}

/// Audit-mandated kill-verification grace constants. Step 5 wires these
/// into the full Cancel/Restart paths.
pub const POST_ABORT_LIVENESS_GRACE: Duration = Duration::from_secs(3);
pub const POST_ABORT_LIVENESS_POLL: Duration = Duration::from_millis(200);
pub const KILL_VERIFICATION_DEADLINE: Duration = Duration::from_secs(10);

/// Grace period inserted between `abort()` and the follow-up `send_prompt`
/// so Pi has time to finish unwinding the in-flight turn (and emit its
/// TurnComplete) before the new prompt arrives on stdin. Too low and the
/// prompt races the agent's tear-down; too high and the nurse intervention
/// feels sluggish. 200ms matches the existing `POST_ABORT_LIVENESS_POLL`
/// cadence used by `kill_with_verification`.
pub const STEER_ABORT_GRACE_MS: u64 = 200;

/// Stub: marker for the new dispatcher's tier label so callers can pass
/// it through.
pub fn tier_for_classifier_decision() -> NurseDispatchTier {
    NurseDispatchTier::Llm
}

// ── Trait abstractions for the kill-verification path ─────────────────
//
// The dispatcher's `kill_with_verification` helper drives both Cancel and
// Restart through the same code path: send `session.abort()`, poll
// `session.is_alive()` for up to `POST_ABORT_LIVENESS_GRACE`, then escalate
// to `pi_manager.kill_session()` and poll again until either dead or
// `KILL_VERIFICATION_DEADLINE` exceeded.
//
// These two traits abstract the only real Pi touchpoints in that helper
// so dispatcher tests can inject mocks (`MockKillableSession` with a
// configurable `is_alive` schedule, `MockSessionKiller` returning
// pre-registered sessions or `SessionNotFound`) without spinning up a
// real `PiManager`. Production code uses the blanket impls below
// (`Arc<PiSession>` → `dyn KillableSession`, `Arc<PiManager>` →
// `dyn SessionKiller`).
//
// `#[async_trait]` is required because native async fn in traits is
// not yet object-safe — without it the `Arc<dyn KillableSession>`
// returned by `SessionKiller::get_session` cannot exist.

/// Minimal Pi-session surface the dispatcher's kill path needs. Production
/// impl on `PiSession` delegates to the existing `abort()` + `is_alive()`
/// methods.
#[async_trait]
pub trait KillableSession: Send + Sync {
    /// Fire-and-forget abort — equivalent to Pi sending the user's
    /// implicit "Ctrl+C". Success does not prove the process exited; the
    /// dispatcher polls `is_alive` to confirm.
    async fn abort(&self) -> Result<(), PiRpcError>;

    /// Cheap, sync liveness probe. Implementations MUST be panic-safe
    /// because the dispatcher wraps each poll in `catch_unwind` — a
    /// panicking probe is treated as `false` (dead).
    fn is_alive(&self) -> bool;

    /// Deliver the nurse's redirect message to the session. Production
    /// `PiSession` impl calls `send_prompt` (not `PiSession::steer`) because
    /// the dispatcher calls this *after* `abort()` — the in-flight turn is
    /// over, so Pi's queue-based `steer` (designed for mid-turn injection)
    /// would just sit in the queue without triggering a new agent reply.
    /// `send_prompt` definitively starts a new turn that produces a visible
    /// agent response. Default impl returns `StdinClosed` so test mocks
    /// don't have to override.
    async fn steer(&self, _message: &str) -> Result<(), PiRpcError> {
        Err(PiRpcError::StdinClosed)
    }
}

#[async_trait]
impl KillableSession for PiSession {
    async fn abort(&self) -> Result<(), PiRpcError> {
        PiSession::abort(self).await
    }
    fn is_alive(&self) -> bool {
        PiSession::is_alive(self)
    }
    async fn steer(&self, message: &str) -> Result<(), PiRpcError> {
        // Use `send_prompt` (not `PiSession::steer`) — see trait doc above.
        PiSession::send_prompt(self, message, None).await
    }
}

/// Manager-side surface the dispatcher needs to escalate from abort to
/// force-kill. The `get_session` return type is `Arc<dyn KillableSession>`
/// (not `Arc<PiSession>`) so test impls can return mock sessions without
/// constructing a real `PiSession`.
#[async_trait]
pub trait SessionKiller: Send + Sync {
    /// Look up a live session by id. Returns `None` if the session was
    /// already torn down (e.g. self-killed earlier in the same decision
    /// chain, then re-checked).
    async fn get_session(&self, session_id: &str) -> Option<Arc<dyn KillableSession>>;

    /// Force-kill via the manager. `PiManagerError::SessionNotFound` is
    /// treated by callers as success-equivalent ("already killed"); other
    /// errors propagate.
    async fn kill_session(&self, session_id: &str) -> Result<(), PiManagerError>;
}

#[async_trait]
impl SessionKiller for PiManager {
    async fn get_session(&self, session_id: &str) -> Option<Arc<dyn KillableSession>> {
        PiManager::get_session(self, session_id)
            .await
            .map(|s| s as Arc<dyn KillableSession>)
    }
    async fn kill_session(&self, session_id: &str) -> Result<(), PiManagerError> {
        PiManager::kill_session(self, session_id).await
    }
}

// ── Self-kill grace map helper ───────────────────────────────────────
//
// The dispatcher's Step 3 (self-kill grace) consults this map to suppress
// re-entrant `process_dead` / `synthesized:process_crashed` raises for
// `SELF_KILL_GRACE` after a Cancel/Restart fires. The dispatcher's
// `Drop`-style pruning keeps the map bounded. This helper exists so the
// `DefaultApplier` and any future intervention caller can insert without
// reimplementing the poison-safe lock dance.

/// Mark `session_id` as just-killed by us so the dispatcher's next-pass
/// self-kill grace check suppresses the re-entrant ProcessCrashed signal.
/// Poison-safe per the project-wide convention.
pub fn mark_self_killed(
    self_killed: &Arc<std::sync::Mutex<HashMap<String, Instant>>>,
    session_id: &str,
) {
    let mut g = self_killed.lock().unwrap_or_else(|e| e.into_inner());
    g.insert(session_id.to_string(), Instant::now());
}

// ── Kill verification ────────────────────────────────────────────────
//
// Drives both Cancel and Restart through the same audit-mandated
// sequence: `abort` → grace poll → escalate to `kill_session` →
// post-kill poll → either `dead_at` or `double_fail_giving_up`. No
// retry after `double_fail_giving_up` — the safety circuit
// (`KILL_VERIFICATION_DEADLINE`) terminates retry so the budget is no
// longer eaten by repeated Cancel attempts on a runaway session.

/// Outcome of [`kill_with_verification`]. The dispatcher's Step-11 outcome
/// accounting maps these onto `(BudgetCharge, NurseLifecycleStatus)` —
/// successful kills (whether via `abort`, `kill_session`, or
/// `already_killed`) charge the budget; failures (`KillError`,
/// `DoubleFail`) do NOT, so a runaway session can't drain the user's
/// per-detector budget through repeated re-attempts.
#[derive(Debug)]
pub enum KillOutcome {
    /// `abort()` returned Ok and `is_alive()` flipped false within the
    /// grace window. Cleanest path.
    Aborted,
    /// `abort()` timed out or errored, but `kill_session` succeeded and
    /// the session died within the deadline.
    ForceKilled,
    /// `kill_session` returned `SessionNotFound` — the session was
    /// already torn down (e.g. the eviction loop reaped it, or a prior
    /// decision on the same session killed it).
    AlreadyKilled,
    /// `kill_session` returned any error other than `SessionNotFound`.
    /// Treated as a failure outcome: budget is NOT charged.
    KillError(String),
    /// Both `abort` AND `kill_session` succeeded but the process is
    /// still alive past `KILL_VERIFICATION_DEADLINE`. Safety circuit;
    /// the dispatcher does NOT retry — the session is leaked but the
    /// budget is preserved for genuine future interventions.
    DoubleFail,
}

impl KillOutcome {
    pub fn outcome_string(&self) -> String {
        match self {
            KillOutcome::Aborted => "session aborted".to_string(),
            KillOutcome::ForceKilled => "force-killed".to_string(),
            KillOutcome::AlreadyKilled => "session not found".to_string(),
            KillOutcome::KillError(e) => format!("kill error: {}", e),
            KillOutcome::DoubleFail => {
                "kill verification deadline exceeded; session may be a runaway".to_string()
            }
        }
    }
}

/// Safe wrapper around the (sync) `KillableSession::is_alive()` probe.
/// Per plan §C, `tokio::time::timeout` cannot catch a panic from a sync
/// call. We wrap each poll in `catch_unwind` and treat a panic as `false`
/// (dead) — the bounded duration guarantee comes from the outer deadline,
/// not from the timeout future.
fn is_alive_panic_safe(session: &Arc<dyn KillableSession>) -> bool {
    let session_for_probe = Arc::clone(session);
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        session_for_probe.is_alive()
    }))
    .unwrap_or(false)
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Audit-mandated kill verification — Cancel + Restart both flow through
/// this exact sequence. See plan §C for the verbatim spec; the comments
/// below mark each numbered step.
///
/// Decision-log rows are emitted via the supplied `decision_logger` +
/// `event_seq` so the dispatcher's chain stays gapless. `owner_dto` +
/// `profile` populate the row envelope.
///
/// Bounded by `KILL_VERIFICATION_DEADLINE` (10 s). No retry after
/// `DoubleFail`.
#[allow(clippy::too_many_arguments)]
pub async fn kill_with_verification(
    decision_id: &str,
    session_id: &str,
    owner_dto: &SessionOwnerDto,
    profile: NurseProfile,
    decision_logger: &Arc<DecisionLogger>,
    event_seq: &EventSeq,
    pi_manager: &Arc<dyn SessionKiller>,
    session: &Arc<dyn KillableSession>,
) -> KillOutcome {
    let env_session = Some(session_id.to_string());
    let env_owner = Some(owner_dto.clone());
    let env_profile = Some(format!("{:?}", profile).to_lowercase());

    // ── 1. abort_sent ──────────────────────────────────────────────
    decision_logger.write(dec_events::kill_verification(
        decision_id,
        &env_session,
        &env_owner,
        &env_profile,
        event_seq.next(),
        "abort_sent",
        now_unix_ms(),
        None,
        None,
    ));

    // ── 2. abort with grace ────────────────────────────────────────
    let abort_result = tokio::time::timeout(POST_ABORT_LIVENESS_GRACE, session.abort()).await;

    let abort_succeeded = matches!(abort_result, Ok(Ok(())));

    if abort_succeeded {
        // ── 3. Poll is_alive every POST_ABORT_LIVENESS_POLL until grace deadline ──
        let deadline = Instant::now() + POST_ABORT_LIVENESS_GRACE;
        loop {
            if !is_alive_panic_safe(session) {
                decision_logger.write(dec_events::kill_verification(
                    decision_id,
                    &env_session,
                    &env_owner,
                    &env_profile,
                    event_seq.next(),
                    "dead_at",
                    now_unix_ms(),
                    Some(false),
                    None,
                ));
                return KillOutcome::Aborted;
            }
            if Instant::now() >= deadline {
                decision_logger.write(dec_events::kill_verification(
                    decision_id,
                    &env_session,
                    &env_owner,
                    &env_profile,
                    event_seq.next(),
                    "liveness_check_at_t3s",
                    now_unix_ms(),
                    Some(true),
                    None,
                ));
                break;
            }
            tokio::time::sleep(POST_ABORT_LIVENESS_POLL).await;
        }
    }

    // ── 4. force_kill_sent ─────────────────────────────────────────
    decision_logger.write(dec_events::kill_verification(
        decision_id,
        &env_session,
        &env_owner,
        &env_profile,
        event_seq.next(),
        "force_kill_sent",
        now_unix_ms(),
        None,
        None,
    ));

    match pi_manager.kill_session(session_id).await {
        Err(PiManagerError::SessionNotFound { .. }) => {
            decision_logger.write(dec_events::kill_verification(
                decision_id,
                &env_session,
                &env_owner,
                &env_profile,
                event_seq.next(),
                "dead_at",
                now_unix_ms(),
                None,
                Some("already_killed"),
            ));
            KillOutcome::AlreadyKilled
        }
        Err(e) => {
            let msg = e.to_string();
            decision_logger.write(dec_events::intervention_outcome(
                decision_id,
                &env_session,
                &env_owner,
                &env_profile,
                event_seq.next(),
                "kill_error",
                Some(&msg),
            ));
            KillOutcome::KillError(msg)
        }
        Ok(()) => {
            // ── 5. Poll until dead OR deadline ─────────────────────
            let post_kill_budget = KILL_VERIFICATION_DEADLINE - POST_ABORT_LIVENESS_GRACE;
            let deadline = Instant::now() + post_kill_budget;
            loop {
                if !is_alive_panic_safe(session) {
                    decision_logger.write(dec_events::kill_verification(
                        decision_id,
                        &env_session,
                        &env_owner,
                        &env_profile,
                        event_seq.next(),
                        "dead_at",
                        now_unix_ms(),
                        Some(false),
                        None,
                    ));
                    return KillOutcome::ForceKilled;
                }
                if Instant::now() >= deadline {
                    decision_logger.write(dec_events::kill_verification(
                        decision_id,
                        &env_session,
                        &env_owner,
                        &env_profile,
                        event_seq.next(),
                        "double_fail_giving_up",
                        now_unix_ms(),
                        Some(true),
                        None,
                    ));
                    return KillOutcome::DoubleFail;
                }
                tokio::time::sleep(POST_ABORT_LIVENESS_POLL).await;
            }
        }
    }
}

// ── cancel_hivemind_review ───────────────────────────────────────────
//
// Moved verbatim from v1 `core/nurse_service.rs` so the DefaultApplier's
// Cancel branch can route Review / Merge owners through it without
// reaching into the dying v1 module.

/// Cancel a running Hivemind review by signalling its CancellationToken
/// and emitting a `hivemind-progress` event so the UI clears its
/// spinner. Returns a human-readable outcome string for the lifecycle
/// payload.
pub async fn cancel_hivemind_review<R: tauri::Runtime>(
    app_handle: &tauri::AppHandle<R>,
    job_id: &str,
    review_id: &str,
) -> String {
    use crate::hivemind::events::HivemindProgressEvent;
    use crate::state::app_state::AppState;
    use tauri::Manager as _;

    let Some(state) = app_handle.try_state::<AppState>() else {
        tracing::warn!(
            job_id,
            review_id,
            "nurse: AppState unavailable, skipping hivemind cancel"
        );
        return "review cancel skipped (app state unavailable)".to_string();
    };

    let token_opt = {
        let reviews = state.running_reviews.read().await;
        reviews.get(job_id).cloned()
    };

    let outcome_msg = if let Some(token) = token_opt {
        token.cancel();
        if let Err(e) = state
            .hivemind_store
            .update_job_status(job_id, "cancelled")
            .await
        {
            tracing::warn!(
                error = %e,
                job_id,
                "nurse: failed to mark hivemind job cancelled"
            );
        }
        tracing::info!(job_id, review_id, "nurse: signalled hivemind cancellation");
        "review cancelled".to_string()
    } else {
        // Review's cleanup guard already dropped the token — engine
        // either completed or was cancelled by another path. Treat as
        // a soft success (the user-facing notice still surfaces).
        "review already completed".to_string()
    };

    let _ = app_handle.emit(
        "hivemind-progress",
        HivemindProgressEvent {
            job_id: job_id.to_string(),
            review_id: Some(review_id.to_string()),
            event_type: "cancelled".to_string(),
            round: 0,
            model_id: String::new(),
            message: "Review cancelled by Nurse".to_string(),
            phase: Some("cancelled".to_string()),
            ..Default::default()
        },
    );

    outcome_msg
}

// ── DefaultApplier ───────────────────────────────────────────────────
//
// Production [`ActionApplier`](crate::nurse::dispatcher::ActionApplier)
// implementation. The applier itself owns nothing — every reference it
// needs at call time (pi_manager, app, self_killed, decision_logger,
// event_seq) is supplied via [`ApplyActionCtx`].
//
// One applier call executes one `NurseDecision` end-to-end:
//
//   1. Emit `Started` lifecycle on `nurse-event` (Tauri).
//   2. Perform the side effect (Steer / Cancel / Restart / LeaveIt).
//   3. Emit `Completed` or `Failed` lifecycle on `nurse-event`.
//   4. Return [`ActionOutcome`] so the dispatcher's Step 11 can:
//      - record the budget charge (`Charge` for successful non-LeaveIt,
//        `Free` otherwise),
//      - emit `intervention_dispatched` decision-log row,
//      - call `record_from_payload(..)` → `intervention_writer.send`,
//      - emit `decision_finalised{status: "dispatched"}`.
//
// **Ground rule 10**: every lifecycle payload sets
// `intervention_id = decision_id` so the `nurse-event` IPC stream
// cross-references the on-disk decision log.

use crate::nurse::dispatcher::{ActionApplier, ActionOutcome, ApplyActionCtx, BudgetCharge};
use crate::nurse::snapshot::NurseActionKind;
use crate::pi::session::SessionOwner;

/// Production [`ActionApplier`] impl. Stateless — every reference comes
/// from [`ApplyActionCtx`].
#[derive(Debug, Default)]
pub struct DefaultApplier;

impl DefaultApplier {
    pub fn new() -> Self {
        Self
    }

    /// Convenience: wrap in `Arc<dyn ActionApplier>` for the dispatcher.
    pub fn new_arc() -> Arc<dyn ActionApplier> {
        Arc::new(Self)
    }
}

/// Owner routing surface — pulls task_id / swarm_id / feature_id /
/// review_id from the live `SessionOwner` for the lifecycle payload.
fn route_from_owner(owner: &SessionOwner) -> RoutingFields {
    match owner {
        SessionOwner::Task { task_id } => RoutingFields {
            task_id: Some(task_id.clone()),
            ..Default::default()
        },
        SessionOwner::Review { job_id } => RoutingFields {
            // Without a separate `review_id`, callers treat job_id as
            // both. The hivemind IPC consumer accepts either field
            // interchangeably for the Review/Merge branches.
            review_id: Some(job_id.clone()),
            ..Default::default()
        },
        SessionOwner::Merge {
            job_id, swarm_id, ..
        } => RoutingFields {
            review_id: Some(job_id.clone()),
            swarm_id: swarm_id.clone(),
            ..Default::default()
        },
        SessionOwner::Swarm { swarm_id, role: _ } => RoutingFields {
            swarm_id: Some(swarm_id.clone()),
            ..Default::default()
        },
        SessionOwner::Unknown => RoutingFields::default(),
    }
}

#[derive(Default, Clone)]
struct RoutingFields {
    task_id: Option<String>,
    swarm_id: Option<String>,
    feature_id: Option<String>,
    review_id: Option<String>,
}

/// Pull (observation, action) preview from the decision's reasoning
/// fields, with a friendly fallback when the LLM omitted them. This
/// keeps the inline-card UX identical to v1 `describe_decision`.
fn preview_from_decision(decision: &crate::nurse::snapshot::NurseDecision) -> (String, String) {
    use crate::nurse::snapshot::NurseDecision as D;
    match decision {
        D::LeaveIt {
            reasoning,
            observation,
            action,
            ..
        } => {
            let snippet: String = reasoning.chars().take(160).collect();
            (
                observation.clone().unwrap_or_else(|| {
                    if snippet.is_empty() {
                        "This session looks like it's still doing legitimate work.".to_string()
                    } else {
                        format!("I'm watching this one — {}", snippet)
                    }
                }),
                action.clone().unwrap_or_else(|| {
                    "I'll leave it alone for now and check back shortly.".to_string()
                }),
            )
        }
        D::Steer {
            reasoning,
            message,
            observation,
            action,
        } => {
            let why: String = reasoning.chars().take(160).collect();
            let msg: String = message.chars().take(160).collect();
            (
                observation.clone().unwrap_or_else(|| {
                    if why.is_empty() {
                        "I spotted the session going in circles.".to_string()
                    } else {
                        format!("I spotted a stuck pattern — {}", why)
                    }
                }),
                action.clone().unwrap_or_else(|| {
                    if msg.is_empty() {
                        "I'll send a steer to nudge it back on track.".to_string()
                    } else {
                        format!("I'll steer the session with: \"{}\"", msg)
                    }
                }),
            )
        }
        D::Restart {
            reasoning,
            observation,
            action,
        } => {
            let snippet: String = reasoning.chars().take(160).collect();
            (
                observation.clone().unwrap_or_else(|| {
                    if snippet.is_empty() {
                        "This session looks fundamentally wedged.".to_string()
                    } else {
                        format!("This session looks fundamentally wedged — {}", snippet)
                    }
                }),
                action.clone().unwrap_or_else(|| {
                    "I'll close the session so a fresh attempt can take over.".to_string()
                }),
            )
        }
        D::Cancel {
            reasoning,
            message,
            observation,
            action,
        } => {
            let why: String = reasoning.chars().take(160).collect();
            let msg: String = message.chars().take(160).collect();
            (
                observation.clone().unwrap_or_else(|| {
                    if why.is_empty() {
                        "Something critical is wrong with this session.".to_string()
                    } else {
                        format!("Something critical is wrong — {}", why)
                    }
                }),
                action.clone().unwrap_or_else(|| {
                    if msg.is_empty() {
                        "I'll cancel it and surface this to you.".to_string()
                    } else {
                        format!("I'll cancel it and tell you: \"{}\"", msg)
                    }
                }),
            )
        }
    }
}

#[async_trait]
impl ActionApplier for DefaultApplier {
    async fn apply(&self, ctx: ApplyActionCtx<'_>) -> ActionOutcome {
        let level = match &ctx.decision {
            crate::nurse::snapshot::NurseDecision::LeaveIt { .. } => NurseActionKind::LeaveIt,
            crate::nurse::snapshot::NurseDecision::Steer { .. } => NurseActionKind::Steer,
            crate::nurse::snapshot::NurseDecision::Restart { .. } => NurseActionKind::Restart,
            crate::nurse::snapshot::NurseDecision::Cancel { .. } => NurseActionKind::Cancel,
        };
        // LeaveIt is the no-op "nothing to do, check back later" decision.
        // It still flows through the dispatcher (decision-chain logging,
        // intervention log on the Nurse screen, budget bookkeeping) but
        // emitting a Lifecycle on `nurse-event` would render a noisy pink
        // Nurse card in the Tasks UI every time the batched reviewer
        // ticks against a healthy session. Suppress the IPC emit for
        // LeaveIt and let the audit trail handle the rest.
        let emit_lifecycle = !matches!(
            &ctx.decision,
            crate::nurse::snapshot::NurseDecision::LeaveIt { .. }
        );
        let routing = route_from_owner(&ctx.owner);
        let (observation, action) = preview_from_decision(&ctx.decision);
        let intervention_id = ctx.decision_id.to_string();
        let now = Utc::now();

        // ── 1. Emit Started lifecycle on `nurse-event` (non-LeaveIt only).
        let started = NurseLifecyclePayload {
            intervention_id: intervention_id.clone(),
            status: NurseLifecycleStatus::Started,
            level,
            session_id: ctx.session_id.to_string(),
            task_id: routing.task_id.clone(),
            swarm_id: routing.swarm_id.clone(),
            feature_id: routing.feature_id.clone(),
            review_id: routing.review_id.clone(),
            observation: observation.clone(),
            action: action.clone(),
            reasoning_delta: None,
            full_reasoning: None,
            error: None,
            timestamp: now,
        };
        if emit_lifecycle {
            if let Some(app) = ctx.app {
                if let Err(e) = app.emit("nurse-event", NurseEvent::Lifecycle(started.clone())) {
                    tracing::warn!(error = %e, "nurse: failed to emit Lifecycle::Started");
                }
            } else {
                tracing::debug!(
                    decision_id = %ctx.decision_id,
                    "nurse: app handle not attached; skipping Lifecycle::Started emit"
                );
            }
        }

        // ── 2 + 3. Perform side effect + build Completed payload.
        let (completed_status, outcome_string, budget_charge, error_for_payload) =
            self.perform_action(&ctx, &started, level).await;

        let completed = NurseLifecyclePayload {
            intervention_id: intervention_id.clone(),
            status: completed_status,
            level,
            session_id: ctx.session_id.to_string(),
            task_id: routing.task_id,
            swarm_id: routing.swarm_id,
            feature_id: routing.feature_id,
            review_id: routing.review_id,
            observation,
            action,
            reasoning_delta: None,
            full_reasoning: None,
            error: error_for_payload,
            timestamp: Utc::now(),
        };
        if emit_lifecycle {
            if let Some(app) = ctx.app {
                if let Err(e) = app.emit("nurse-event", NurseEvent::Lifecycle(completed.clone())) {
                    tracing::warn!(error = %e, "nurse: failed to emit Lifecycle::Completed/Failed");
                }
            }
        }

        ActionOutcome {
            budget_charge,
            completed_status,
            outcome_string,
            lifecycle_payload: completed,
        }
    }
}

impl DefaultApplier {
    /// Returns (completed_status, outcome_string, budget_charge,
    /// optional error string for the lifecycle payload).
    async fn perform_action<'a>(
        &self,
        ctx: &ApplyActionCtx<'a>,
        started: &NurseLifecyclePayload,
        level: NurseActionKind,
    ) -> (NurseLifecycleStatus, String, BudgetCharge, Option<String>) {
        use crate::nurse::snapshot::NurseDecision as D;
        match &ctx.decision {
            // ── B.1 LeaveIt — no Pi call.
            D::LeaveIt { .. } => {
                tracing::debug!(
                    session_id = %ctx.session_id,
                    decision_id = %ctx.decision_id,
                    "nurse: applier leaving session alone"
                );
                (
                    NurseLifecycleStatus::Completed,
                    "left alone".to_string(),
                    BudgetCharge::Free,
                    None,
                )
            }

            // ── B.2 Steer { message }.
            D::Steer { message, .. } => {
                let session = ctx.pi_manager.get_session(ctx.session_id).await;
                match session {
                    None => {
                        tracing::info!(
                            session_id = %ctx.session_id,
                            decision_id = %ctx.decision_id,
                            "nurse: session terminated before steer could be applied"
                        );
                        (
                            NurseLifecycleStatus::Failed,
                            "session not found".to_string(),
                            BudgetCharge::Free,
                            Some("session not found".to_string()),
                        )
                    }
                    Some(session) => {
                        tracing::info!(
                            session_id = %ctx.session_id,
                            decision_id = %ctx.decision_id,
                            message_len = message.len(),
                            "nurse: steering session"
                        );
                        // Abort the in-flight turn first so the agent stops
                        // its current monologue immediately. Pi's native `steer`
                        // is mid-turn queueing — if we used it here, the message
                        // would sit in the queue without triggering a new agent
                        // reply because the turn just ended. Instead, the trait's
                        // production `steer` impl uses `send_prompt` to start a
                        // *fresh* turn carrying the redirect message; the user
                        // sees a normal agent response that addresses the steer.
                        if let Err(e) = session.abort().await {
                            tracing::warn!(
                                session_id = %ctx.session_id,
                                decision_id = %ctx.decision_id,
                                error = %e,
                                "nurse: abort before steer failed (continuing)"
                            );
                        }
                        // Brief grace so Pi can settle the abort (emit
                        // TurnComplete + flip `busy = false`) before the
                        // follow-up prompt arrives on the same stdin pipe.
                        tokio::time::sleep(Duration::from_millis(STEER_ABORT_GRACE_MS)).await;
                        match session.steer(message).await {
                            Ok(()) => (
                                NurseLifecycleStatus::Completed,
                                "success".to_string(),
                                BudgetCharge::Charge,
                                None,
                            ),
                            Err(e) => {
                                let msg = format!("steer failed: {}", e);
                                tracing::warn!(
                                    session_id = %ctx.session_id,
                                    decision_id = %ctx.decision_id,
                                    error = %e,
                                    "nurse: steer failed"
                                );
                                (
                                    NurseLifecycleStatus::Failed,
                                    msg.clone(),
                                    BudgetCharge::Free,
                                    Some(msg),
                                )
                            }
                        }
                    }
                }
            }

            // ── B.3 Cancel { message? }.
            D::Cancel {
                message, reasoning, ..
            } => {
                let user_message = if message.is_empty() {
                    reasoning.clone()
                } else {
                    message.clone()
                };
                let outcome = self.do_cancel(ctx, &user_message).await;
                self.emit_user_notice(ctx, level, &user_message, started.timestamp);
                outcome
            }

            // ── B.4 Restart per SessionOwner.
            D::Restart { reasoning, .. } => self.do_restart(ctx, reasoning).await,
        }
    }

    /// Cancel implementation — owner-driven routing.
    async fn do_cancel<'a>(
        &self,
        ctx: &ApplyActionCtx<'a>,
        _user_message: &str,
    ) -> (NurseLifecycleStatus, String, BudgetCharge, Option<String>) {
        match &ctx.owner {
            SessionOwner::Review { job_id } | SessionOwner::Merge { job_id, .. } => {
                tracing::warn!(
                    session_id = %ctx.session_id,
                    decision_id = %ctx.decision_id,
                    job_id = %job_id,
                    "nurse: cancelling hivemind review"
                );
                // TODO: SessionOwner::Review doesn't carry a separate
                // review_id; pass job_id for both. The hivemind IPC
                // consumer treats them as interchangeable on this branch.
                let outcome_msg = if let Some(app) = ctx.app {
                    cancel_hivemind_review(app, job_id, job_id).await
                } else {
                    tracing::warn!(
                        decision_id = %ctx.decision_id,
                        "nurse: app handle not attached; skipping cancel_hivemind_review"
                    );
                    "review cancel skipped (app handle unavailable)".to_string()
                };

                // Best-effort cleanup: if a live Pi session exists for the
                // same session_id, kill it. Errors are swallowed — the
                // hivemind cancel is the primary signal.
                if let Some(session) = ctx.pi_manager.get_session(ctx.session_id).await {
                    mark_self_killed(ctx.self_killed, ctx.session_id);
                    let _ = kill_with_verification(
                        ctx.decision_id,
                        ctx.session_id,
                        &ctx.owner_dto,
                        ctx.profile,
                        ctx.decision_logger,
                        ctx.event_seq,
                        ctx.pi_manager,
                        &session,
                    )
                    .await;
                }

                (
                    NurseLifecycleStatus::Completed,
                    outcome_msg,
                    BudgetCharge::Charge,
                    None,
                )
            }
            _ => {
                let session = ctx.pi_manager.get_session(ctx.session_id).await;
                match session {
                    None => {
                        tracing::info!(
                            session_id = %ctx.session_id,
                            decision_id = %ctx.decision_id,
                            "nurse: session not found for cancel"
                        );
                        (
                            NurseLifecycleStatus::Failed,
                            "session not found".to_string(),
                            BudgetCharge::Free,
                            Some("session not found".to_string()),
                        )
                    }
                    Some(session) => {
                        mark_self_killed(ctx.self_killed, ctx.session_id);
                        let outcome = kill_with_verification(
                            ctx.decision_id,
                            ctx.session_id,
                            &ctx.owner_dto,
                            ctx.profile,
                            ctx.decision_logger,
                            ctx.event_seq,
                            ctx.pi_manager,
                            &session,
                        )
                        .await;
                        map_kill_outcome(outcome)
                    }
                }
            }
        }
    }

    /// Restart implementation — non-Hivemind owners only.
    async fn do_restart<'a>(
        &self,
        ctx: &ApplyActionCtx<'a>,
        _reasoning: &str,
    ) -> (NurseLifecycleStatus, String, BudgetCharge, Option<String>) {
        // Hivemind owners should have been downgraded to Cancel upstream
        // (Tier 1 / Tier 3 owner-aware downgrade). Defensive: assert in
        // debug builds, fall through to the kill path otherwise.
        if matches!(
            &ctx.owner,
            SessionOwner::Review { .. } | SessionOwner::Merge { .. }
        ) {
            debug_assert!(
                false,
                "Restart on Hivemind owner should have been downgraded to Cancel upstream"
            );
            tracing::warn!(
                session_id = %ctx.session_id,
                decision_id = %ctx.decision_id,
                "nurse: Restart on Hivemind owner — defensive Cancel fallback"
            );
            return self.do_cancel(ctx, "").await;
        }

        if matches!(&ctx.owner, SessionOwner::Unknown) {
            tracing::warn!(
                session_id = %ctx.session_id,
                decision_id = %ctx.decision_id,
                "nurse: Restart on Unknown owner — kill has no follow-up effect"
            );
        }

        let session = ctx.pi_manager.get_session(ctx.session_id).await;
        match session {
            None => {
                tracing::info!(
                    session_id = %ctx.session_id,
                    decision_id = %ctx.decision_id,
                    "nurse: session not found for restart"
                );
                (
                    NurseLifecycleStatus::Failed,
                    "session not found".to_string(),
                    BudgetCharge::Free,
                    Some("session not found".to_string()),
                )
            }
            Some(session) => {
                mark_self_killed(ctx.self_killed, ctx.session_id);
                let outcome = kill_with_verification(
                    ctx.decision_id,
                    ctx.session_id,
                    &ctx.owner_dto,
                    ctx.profile,
                    ctx.decision_logger,
                    ctx.event_seq,
                    ctx.pi_manager,
                    &session,
                )
                .await;
                map_kill_outcome(outcome)
            }
        }
    }

    /// Emit the user-facing `UserNotice` mirror that v1 sent for
    /// Cancel/Restart so the agent's inline-card stream sees a separate
    /// "Nurse cancelled ..." / "Nurse restarted ..." text bubble.
    fn emit_user_notice<'a>(
        &self,
        ctx: &ApplyActionCtx<'a>,
        level: NurseActionKind,
        user_message: &str,
        timestamp: chrono::DateTime<Utc>,
    ) {
        if let Some(app) = ctx.app {
            if let Err(e) = app.emit(
                "nurse-event",
                NurseEvent::UserNotice {
                    session_id: ctx.session_id.to_string(),
                    level,
                    message: user_message.to_string(),
                    timestamp,
                },
            ) {
                tracing::warn!(error = %e, "nurse: failed to emit UserNotice");
            }
        }
    }
}

/// Map [`KillOutcome`] → `(NurseLifecycleStatus, outcome_string,
/// BudgetCharge, error_for_payload)`. Successful kills (Aborted /
/// ForceKilled / AlreadyKilled) charge the budget; failures
/// (KillError / DoubleFail) do NOT — runaway sessions can't drain the
/// per-detector budget through repeated re-attempts.
fn map_kill_outcome(
    outcome: KillOutcome,
) -> (NurseLifecycleStatus, String, BudgetCharge, Option<String>) {
    match outcome {
        KillOutcome::Aborted => (
            NurseLifecycleStatus::Completed,
            "session aborted".to_string(),
            BudgetCharge::Charge,
            None,
        ),
        KillOutcome::ForceKilled => (
            NurseLifecycleStatus::Completed,
            "force-killed".to_string(),
            BudgetCharge::Charge,
            None,
        ),
        KillOutcome::AlreadyKilled => (
            NurseLifecycleStatus::Completed,
            "session not found".to_string(),
            BudgetCharge::Charge,
            None,
        ),
        KillOutcome::KillError(e) => {
            let msg = format!("kill error: {}", e);
            (
                NurseLifecycleStatus::Failed,
                msg.clone(),
                BudgetCharge::Free,
                Some(msg),
            )
        }
        KillOutcome::DoubleFail => {
            let msg = "kill verification deadline exceeded; session may be a runaway".to_string();
            (
                NurseLifecycleStatus::Failed,
                msg.clone(),
                BudgetCharge::Free,
                Some(msg),
            )
        }
    }
}

#[cfg(test)]
mod kill_tests {
    use super::*;
    use crate::nurse::observability::ObservabilityHandles;
    use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

    /// Test KillableSession that returns a configurable `is_alive`
    /// schedule + recorded abort calls.
    struct MockKillableSession {
        is_alive: AtomicU32, // 0=false, >=1=true; decrement on each call for the "dies after N polls" pattern
        abort_calls: AtomicU32,
        abort_err: bool,
    }

    impl MockKillableSession {
        fn new_always_alive() -> Arc<dyn KillableSession> {
            Arc::new(Self {
                is_alive: AtomicU32::new(u32::MAX),
                abort_calls: AtomicU32::new(0),
                abort_err: false,
            })
        }
        fn _dies_after(n: u32) -> Arc<dyn KillableSession> {
            Arc::new(Self {
                is_alive: AtomicU32::new(n),
                abort_calls: AtomicU32::new(0),
                abort_err: false,
            })
        }
    }

    #[async_trait]
    impl KillableSession for MockKillableSession {
        async fn abort(&self) -> Result<(), PiRpcError> {
            self.abort_calls.fetch_add(1, Ordering::SeqCst);
            if self.abort_err {
                Err(PiRpcError::StdinClosed)
            } else {
                Ok(())
            }
        }
        fn is_alive(&self) -> bool {
            let cur = self.is_alive.load(Ordering::SeqCst);
            if cur == 0 {
                false
            } else if cur == u32::MAX {
                true
            } else {
                // Decrement and report still-alive until we hit 0.
                self.is_alive.fetch_sub(1, Ordering::SeqCst);
                true
            }
        }
    }

    /// Test SessionKiller that always returns the given session and a
    /// configurable `kill_session` outcome.
    struct MockSessionKiller {
        session: Option<Arc<dyn KillableSession>>,
        kill_err: Option<PiManagerError>,
    }

    #[async_trait]
    impl SessionKiller for MockSessionKiller {
        async fn get_session(&self, _session_id: &str) -> Option<Arc<dyn KillableSession>> {
            self.session.as_ref().map(Arc::clone)
        }
        async fn kill_session(&self, session_id: &str) -> Result<(), PiManagerError> {
            if let Some(err) = &self.kill_err {
                // Clone the error to return — PiManagerError doesn't impl Clone,
                // so we reconstruct on the variants we use in tests.
                return Err(match err {
                    PiManagerError::SessionNotFound { session_id } => {
                        PiManagerError::SessionNotFound {
                            session_id: session_id.clone(),
                        }
                    }
                    other => PiManagerError::SessionNotFound {
                        session_id: format!("test:{}:{}", session_id, other),
                    },
                });
            }
            Ok(())
        }
    }

    fn test_logger() -> (Arc<DecisionLogger>, tempfile::TempDir) {
        let tmp = tempfile::TempDir::new().unwrap();
        let dropped = Arc::new(AtomicU64::new(0));
        let logger = Arc::new(DecisionLogger::new(tmp.path().to_path_buf(), dropped));
        (logger, tmp)
    }

    async fn read_chain(tmp: &tempfile::TempDir) -> Vec<serde_json::Value> {
        use crate::nurse::observability::writer::today_yyyy_mm_dd;
        let path = tmp
            .path()
            .join(format!("decisions.jsonl.{}", today_yyyy_mm_dd()));
        // Poll for up to 2s — JsonlWriter writes via tokio task.
        for _ in 0..40 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            if let Ok(text) = std::fs::read_to_string(&path) {
                let rows: Vec<serde_json::Value> = text
                    .lines()
                    .filter_map(|l| serde_json::from_str(l).ok())
                    .collect();
                if !rows.is_empty() {
                    return rows;
                }
            }
        }
        Vec::new()
    }

    // Test 13: Kill verification deadline path. MockKillableSession returns
    // true forever for is_alive → assert double_fail_giving_up row +
    // DoubleFail outcome + no retry loop (a single kill_session call).
    #[tokio::test(start_paused = true)]
    async fn test_13_kill_verification_deadline_double_fail() {
        let _ = ObservabilityHandles::new(); // not needed; logger is standalone
        let (logger, tmp) = test_logger();
        let event_seq = EventSeq::new();

        let session = MockKillableSession::new_always_alive();
        let killer: Arc<dyn SessionKiller> = Arc::new(MockSessionKiller {
            session: Some(Arc::clone(&session)),
            kill_err: None, // kill_session succeeds, but is_alive stays true
        });

        let owner_dto = SessionOwnerDto::Task {
            task_id: "t1".into(),
        };
        let outcome = kill_with_verification(
            "test-dec-13",
            "sess-13",
            &owner_dto,
            NurseProfile::Tasks,
            &logger,
            &event_seq,
            &killer,
            &session,
        )
        .await;

        assert!(
            matches!(outcome, KillOutcome::DoubleFail),
            "expected DoubleFail, got {:?}",
            outcome
        );

        let rows = read_chain(&tmp).await;
        let stages: Vec<String> = rows
            .iter()
            .filter_map(|r| r.get("data")?.get("stage")?.as_str().map(|s| s.to_string()))
            .collect();
        // Expected stages: abort_sent → liveness_check_at_t3s →
        // force_kill_sent → double_fail_giving_up. No second
        // force_kill_sent (no retry).
        let force_kill_count = stages.iter().filter(|s| *s == "force_kill_sent").count();
        assert_eq!(
            force_kill_count, 1,
            "kill verification must NOT retry after double_fail_giving_up — got force_kill_sent count {}",
            force_kill_count
        );
        assert!(
            stages.iter().any(|s| s == "double_fail_giving_up"),
            "missing double_fail_giving_up row, got stages: {:?}",
            stages
        );
    }

    // Test extra: AlreadyKilled path — kill_session returns SessionNotFound.
    #[tokio::test(start_paused = true)]
    async fn kill_verification_already_killed_marker() {
        let (logger, tmp) = test_logger();
        let event_seq = EventSeq::new();

        let session = MockKillableSession::new_always_alive();
        let killer: Arc<dyn SessionKiller> = Arc::new(MockSessionKiller {
            session: Some(Arc::clone(&session)),
            kill_err: Some(PiManagerError::SessionNotFound {
                session_id: "sess-x".into(),
            }),
        });

        let outcome = kill_with_verification(
            "test-dec-already",
            "sess-x",
            &SessionOwnerDto::Unknown,
            NurseProfile::Default,
            &logger,
            &event_seq,
            &killer,
            &session,
        )
        .await;

        assert!(matches!(outcome, KillOutcome::AlreadyKilled));

        let rows = read_chain(&tmp).await;
        // The dead_at row at the end carries reason="already_killed".
        let dead_at = rows
            .iter()
            .rev()
            .find(|r| {
                r.get("data")
                    .and_then(|d| d.get("stage"))
                    .and_then(|s| s.as_str())
                    == Some("dead_at")
            })
            .expect("missing terminal dead_at row");
        let reason = dead_at
            .get("data")
            .and_then(|d| d.get("reason"))
            .and_then(|r| r.as_str());
        assert_eq!(reason, Some("already_killed"));
    }

    // Test: mark_self_killed is poison-safe and idempotent.
    #[test]
    fn mark_self_killed_inserts_and_is_poison_safe() {
        let map: Arc<std::sync::Mutex<HashMap<String, Instant>>> =
            Arc::new(std::sync::Mutex::new(HashMap::new()));
        mark_self_killed(&map, "s1");
        assert!(map.lock().unwrap().contains_key("s1"));

        // Poison the lock from a thread, then ensure mark_self_killed still works.
        let map_for_thread = Arc::clone(&map);
        let h = std::thread::spawn(move || {
            let _g = map_for_thread.lock().unwrap();
            panic!("poisoning");
        });
        let _ = h.join();
        assert!(map.is_poisoned());

        mark_self_killed(&map, "s2");
        let g = map.lock().unwrap_or_else(|e| e.into_inner());
        assert!(g.contains_key("s1"));
        assert!(g.contains_key("s2"));
    }
}

#[cfg(test)]
mod applier_tests {
    //! DefaultApplier unit tests. These mock the `SessionKiller` +
    //! `KillableSession` traits and pass `app: None` so the Tauri emit
    //! sites silently skip — the applier still drives kill_with_verification
    //! through to a KillOutcome and returns the same ActionOutcome shape
    //! production would.
    //!
    //! cancel_hivemind_review's full path (with a running review token)
    //! requires a real AppState; the Test 19 Review-owner test below
    //! exercises only the no-AppHandle fallback. Manual acceptance in
    //! Step 20 covers the live cancel path end-to-end.

    use super::*;
    use crate::nurse::config::NurseProfile;
    use crate::nurse::dispatcher::{ActionApplier, ApplyActionCtx, BudgetCharge, EventSeq};
    use crate::nurse::observability::decision_log::DecisionLogger;
    use crate::nurse::snapshot::{NurseDecision, NurseLifecycleStatus, SessionOwnerDto};
    use crate::pi::session::SessionOwner;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::time::Instant;

    /// Test KillableSession with configurable abort + steer + is_alive.
    struct TestSession {
        abort_ok: AtomicBool,
        alive: AtomicBool,
        steer_ok: AtomicBool,
        steer_calls: std::sync::Mutex<Vec<String>>,
    }

    impl TestSession {
        fn dies_immediately() -> Arc<dyn KillableSession> {
            Arc::new(Self {
                abort_ok: AtomicBool::new(true),
                alive: AtomicBool::new(false),
                steer_ok: AtomicBool::new(true),
                steer_calls: std::sync::Mutex::new(Vec::new()),
            })
        }
        fn steer_ok() -> Arc<dyn KillableSession> {
            Arc::new(Self {
                abort_ok: AtomicBool::new(true),
                alive: AtomicBool::new(true),
                steer_ok: AtomicBool::new(true),
                steer_calls: std::sync::Mutex::new(Vec::new()),
            })
        }
        fn steer_fails() -> Arc<dyn KillableSession> {
            Arc::new(Self {
                abort_ok: AtomicBool::new(true),
                alive: AtomicBool::new(true),
                steer_ok: AtomicBool::new(false),
                steer_calls: std::sync::Mutex::new(Vec::new()),
            })
        }
    }

    #[async_trait]
    impl KillableSession for TestSession {
        async fn abort(&self) -> Result<(), PiRpcError> {
            if self.abort_ok.load(Ordering::SeqCst) {
                self.alive.store(false, Ordering::SeqCst);
                Ok(())
            } else {
                Err(PiRpcError::StdinClosed)
            }
        }
        fn is_alive(&self) -> bool {
            self.alive.load(Ordering::SeqCst)
        }
        async fn steer(&self, message: &str) -> Result<(), PiRpcError> {
            self.steer_calls.lock().unwrap().push(message.to_string());
            if self.steer_ok.load(Ordering::SeqCst) {
                Ok(())
            } else {
                Err(PiRpcError::StdinClosed)
            }
        }
    }

    /// Test SessionKiller — Option<session>; kill_session succeeds.
    struct TestKiller {
        session: std::sync::Mutex<Option<Arc<dyn KillableSession>>>,
    }

    impl TestKiller {
        fn with(session: Option<Arc<dyn KillableSession>>) -> Arc<dyn SessionKiller> {
            Arc::new(Self {
                session: std::sync::Mutex::new(session),
            })
        }
    }

    #[async_trait]
    impl SessionKiller for TestKiller {
        async fn get_session(&self, _session_id: &str) -> Option<Arc<dyn KillableSession>> {
            self.session.lock().unwrap().clone()
        }
        async fn kill_session(&self, _session_id: &str) -> Result<(), PiManagerError> {
            // Clear the test session so subsequent get_session calls return None.
            *self.session.lock().unwrap() = None;
            Ok(())
        }
    }

    fn test_logger() -> (Arc<DecisionLogger>, tempfile::TempDir) {
        let tmp = tempfile::TempDir::new().unwrap();
        let dropped = Arc::new(AtomicU64::new(0));
        let logger = Arc::new(DecisionLogger::new(tmp.path().to_path_buf(), dropped));
        (logger, tmp)
    }

    fn make_ctx<'a>(
        decision: &'a NurseDecision,
        session_id: &'a str,
        owner: SessionOwner,
        owner_dto: SessionOwnerDto,
        decision_id: &'a str,
        decision_logger: &'a Arc<DecisionLogger>,
        event_seq: &'a EventSeq,
        pi_manager: &'a Arc<dyn SessionKiller>,
        self_killed: &'a Arc<std::sync::Mutex<HashMap<String, Instant>>>,
    ) -> ApplyActionCtx<'a> {
        ApplyActionCtx {
            decision: decision.clone(),
            session_id,
            owner,
            decision_id,
            tier_used: NurseDispatchTier::Llm,
            decision_logger,
            event_seq,
            owner_dto,
            profile: NurseProfile::Tasks,
            app: None,
            pi_manager,
            self_killed,
        }
    }

    fn self_killed_map() -> Arc<std::sync::Mutex<HashMap<String, Instant>>> {
        Arc::new(std::sync::Mutex::new(HashMap::new()))
    }

    // LeaveIt path — no Pi calls, Completed + Free.
    #[tokio::test]
    async fn leave_it_returns_completed_free_no_pi_call() {
        let (logger, _tmp) = test_logger();
        let seq = EventSeq::new();
        // The killer should NEVER be consulted for LeaveIt; use empty.
        let killer = TestKiller::with(None);
        let sk = self_killed_map();
        let decision = NurseDecision::LeaveIt {
            reasoning: "still working".into(),
            check_back_secs: 60,
            observation: None,
            action: None,
        };
        let ctx = make_ctx(
            &decision,
            "sess-leaveit",
            SessionOwner::Task {
                task_id: "t-1".into(),
            },
            SessionOwnerDto::Task {
                task_id: "t-1".into(),
            },
            "dec-leaveit",
            &logger,
            &seq,
            &killer,
            &sk,
        );
        let applier = DefaultApplier::new();
        let out = applier.apply(ctx).await;
        assert_eq!(out.budget_charge, BudgetCharge::Free);
        assert_eq!(out.completed_status, NurseLifecycleStatus::Completed);
        assert_eq!(out.outcome_string, "left alone");
        assert_eq!(out.lifecycle_payload.intervention_id, "dec-leaveit");
        // self_killed must NOT be marked for LeaveIt.
        assert!(sk.lock().unwrap().is_empty());
    }

    // Steer success on Task owner → Completed/Charge with "success".
    #[tokio::test]
    async fn steer_success_on_task_returns_completed_charge() {
        let (logger, _tmp) = test_logger();
        let seq = EventSeq::new();
        let session = TestSession::steer_ok();
        let killer = TestKiller::with(Some(Arc::clone(&session)));
        let sk = self_killed_map();
        let decision = NurseDecision::Steer {
            reasoning: "looping".into(),
            message: "try a different file".into(),
            observation: None,
            action: None,
        };
        let ctx = make_ctx(
            &decision,
            "sess-steer-ok",
            SessionOwner::Task {
                task_id: "t-2".into(),
            },
            SessionOwnerDto::Task {
                task_id: "t-2".into(),
            },
            "dec-steer-ok",
            &logger,
            &seq,
            &killer,
            &sk,
        );
        let applier = DefaultApplier::new();
        let out = applier.apply(ctx).await;
        assert_eq!(out.budget_charge, BudgetCharge::Charge);
        assert_eq!(out.completed_status, NurseLifecycleStatus::Completed);
        assert_eq!(out.outcome_string, "success");
        assert!(out.lifecycle_payload.error.is_none());
    }

    // Steer failure on Task owner → Failed/Free with "steer failed: ...".
    #[tokio::test]
    async fn steer_failure_on_task_returns_failed_free() {
        let (logger, _tmp) = test_logger();
        let seq = EventSeq::new();
        let session = TestSession::steer_fails();
        let killer = TestKiller::with(Some(Arc::clone(&session)));
        let sk = self_killed_map();
        let decision = NurseDecision::Steer {
            reasoning: "looping".into(),
            message: "try a different file".into(),
            observation: None,
            action: None,
        };
        let ctx = make_ctx(
            &decision,
            "sess-steer-fail",
            SessionOwner::Task {
                task_id: "t-3".into(),
            },
            SessionOwnerDto::Task {
                task_id: "t-3".into(),
            },
            "dec-steer-fail",
            &logger,
            &seq,
            &killer,
            &sk,
        );
        let applier = DefaultApplier::new();
        let out = applier.apply(ctx).await;
        assert_eq!(out.budget_charge, BudgetCharge::Free);
        assert_eq!(out.completed_status, NurseLifecycleStatus::Failed);
        assert!(
            out.outcome_string.starts_with("steer failed"),
            "outcome_string was: {}",
            out.outcome_string
        );
        assert!(out.lifecycle_payload.error.is_some());
    }

    // Steer with no session → Failed/Free + "session not found".
    #[tokio::test]
    async fn steer_no_session_returns_failed_free() {
        let (logger, _tmp) = test_logger();
        let seq = EventSeq::new();
        let killer = TestKiller::with(None);
        let sk = self_killed_map();
        let decision = NurseDecision::Steer {
            reasoning: "looping".into(),
            message: "nudge".into(),
            observation: None,
            action: None,
        };
        let ctx = make_ctx(
            &decision,
            "sess-steer-none",
            SessionOwner::Task {
                task_id: "t-4".into(),
            },
            SessionOwnerDto::Task {
                task_id: "t-4".into(),
            },
            "dec-steer-none",
            &logger,
            &seq,
            &killer,
            &sk,
        );
        let applier = DefaultApplier::new();
        let out = applier.apply(ctx).await;
        assert_eq!(out.budget_charge, BudgetCharge::Free);
        assert_eq!(out.completed_status, NurseLifecycleStatus::Failed);
        assert_eq!(out.outcome_string, "session not found");
    }

    // Cancel on Task with live session that dies on abort → Completed/Charge.
    #[tokio::test]
    async fn cancel_on_task_with_live_session_returns_completed_charge() {
        let (logger, _tmp) = test_logger();
        let seq = EventSeq::new();
        let session = TestSession::dies_immediately();
        let killer = TestKiller::with(Some(Arc::clone(&session)));
        let sk = self_killed_map();
        let decision = NurseDecision::Cancel {
            reasoning: "irrecoverable".into(),
            message: "I gave up on this one".into(),
            observation: None,
            action: None,
        };
        let ctx = make_ctx(
            &decision,
            "sess-cancel-task",
            SessionOwner::Task {
                task_id: "t-5".into(),
            },
            SessionOwnerDto::Task {
                task_id: "t-5".into(),
            },
            "dec-cancel-task",
            &logger,
            &seq,
            &killer,
            &sk,
        );
        let applier = DefaultApplier::new();
        let out = applier.apply(ctx).await;
        assert_eq!(out.budget_charge, BudgetCharge::Charge);
        assert_eq!(out.completed_status, NurseLifecycleStatus::Completed);
        // KillOutcome::Aborted → "session aborted".
        assert_eq!(out.outcome_string, "session aborted");
        // self_killed map MUST be marked.
        assert!(sk.lock().unwrap().contains_key("sess-cancel-task"));
    }

    // Cancel on Task with no live session → Failed/Free + "session not found".
    #[tokio::test]
    async fn cancel_on_task_no_session_returns_failed_free() {
        let (logger, _tmp) = test_logger();
        let seq = EventSeq::new();
        let killer = TestKiller::with(None);
        let sk = self_killed_map();
        let decision = NurseDecision::Cancel {
            reasoning: "bad state".into(),
            message: "".into(),
            observation: None,
            action: None,
        };
        let ctx = make_ctx(
            &decision,
            "sess-cancel-none",
            SessionOwner::Task {
                task_id: "t-6".into(),
            },
            SessionOwnerDto::Task {
                task_id: "t-6".into(),
            },
            "dec-cancel-none",
            &logger,
            &seq,
            &killer,
            &sk,
        );
        let applier = DefaultApplier::new();
        let out = applier.apply(ctx).await;
        assert_eq!(out.budget_charge, BudgetCharge::Free);
        assert_eq!(out.completed_status, NurseLifecycleStatus::Failed);
        assert_eq!(out.outcome_string, "session not found");
    }

    // Test 19: Cancel on Review owner with no AppHandle attached + no
    // live Pi session for cleanup → Completed/Charge with the
    // "app handle unavailable" sentinel string. The full
    // cancel_hivemind_review path (which needs a running review
    // CancellationToken in AppState) is exercised by manual acceptance
    // in Step 20; this verifies the routing decision is correct.
    #[tokio::test]
    async fn test_19_cancel_review_owner_routes_through_cancel_hivemind_review() {
        let (logger, _tmp) = test_logger();
        let seq = EventSeq::new();
        // No live Pi session — Review owners do their cleanup best-effort.
        let killer = TestKiller::with(None);
        let sk = self_killed_map();
        let decision = NurseDecision::Cancel {
            reasoning: "review wedged".into(),
            message: "cancelling".into(),
            observation: None,
            action: None,
        };
        let ctx = make_ctx(
            &decision,
            "sess-review",
            SessionOwner::Review {
                job_id: "job-xyz".into(),
            },
            SessionOwnerDto::Review {
                job_id: "job-xyz".into(),
            },
            "dec-cancel-review",
            &logger,
            &seq,
            &killer,
            &sk,
        );
        let applier = DefaultApplier::new();
        let out = applier.apply(ctx).await;
        assert_eq!(out.budget_charge, BudgetCharge::Charge);
        assert_eq!(out.completed_status, NurseLifecycleStatus::Completed);
        // Without an AppHandle, the applier emits the sentinel routing string.
        assert!(
            out.outcome_string.contains("review"),
            "outcome_string did not mention review: {}",
            out.outcome_string
        );
        // intervention_id == decision_id per ground rule 10.
        assert_eq!(out.lifecycle_payload.intervention_id, "dec-cancel-review");
        // Routing fields filled from the Review owner.
        assert_eq!(out.lifecycle_payload.review_id.as_deref(), Some("job-xyz"));
    }

    // Restart on Task owner with successful kill → Completed/Charge.
    #[tokio::test]
    async fn restart_on_task_with_successful_kill_returns_completed_charge() {
        let (logger, _tmp) = test_logger();
        let seq = EventSeq::new();
        let session = TestSession::dies_immediately();
        let killer = TestKiller::with(Some(Arc::clone(&session)));
        let sk = self_killed_map();
        let decision = NurseDecision::Restart {
            reasoning: "fundamentally wedged".into(),
            observation: None,
            action: None,
        };
        let ctx = make_ctx(
            &decision,
            "sess-restart-task",
            SessionOwner::Task {
                task_id: "t-7".into(),
            },
            SessionOwnerDto::Task {
                task_id: "t-7".into(),
            },
            "dec-restart-task",
            &logger,
            &seq,
            &killer,
            &sk,
        );
        let applier = DefaultApplier::new();
        let out = applier.apply(ctx).await;
        assert_eq!(out.budget_charge, BudgetCharge::Charge);
        assert_eq!(out.completed_status, NurseLifecycleStatus::Completed);
        assert_eq!(out.outcome_string, "session aborted");
        // self_killed must be marked.
        assert!(sk.lock().unwrap().contains_key("sess-restart-task"));
        // intervention_id == decision_id.
        assert_eq!(out.lifecycle_payload.intervention_id, "dec-restart-task");
        // Level is Restart even though the kill mechanics match Cancel.
        assert_eq!(out.lifecycle_payload.level, NurseActionKind::Restart);
    }

    // Restart on Hivemind owner triggers debug_assert. In debug builds
    // this panics; in release the applier falls through to Cancel. We
    // guard the assertion behind cfg(debug_assertions) so the test fails
    // the right way for each profile.
    #[cfg(debug_assertions)]
    #[tokio::test]
    #[should_panic(expected = "Restart on Hivemind owner should have been downgraded")]
    async fn restart_on_hivemind_owner_debug_panics() {
        let (logger, _tmp) = test_logger();
        let seq = EventSeq::new();
        let killer = TestKiller::with(None);
        let sk = self_killed_map();
        let decision = NurseDecision::Restart {
            reasoning: "should not happen — caller should downgrade".into(),
            observation: None,
            action: None,
        };
        let ctx = make_ctx(
            &decision,
            "sess-restart-review",
            SessionOwner::Review {
                job_id: "job-bad".into(),
            },
            SessionOwnerDto::Review {
                job_id: "job-bad".into(),
            },
            "dec-restart-review",
            &logger,
            &seq,
            &killer,
            &sk,
        );
        let applier = DefaultApplier::new();
        let _ = applier.apply(ctx).await;
    }
}
