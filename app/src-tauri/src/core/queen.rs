//! Queen execution orchestrator.
//!
//! The queen manages the overall swarm execution lifecycle. It coordinates
//! the scheduler, scouts, workers, guards, and nurse to execute features
//! in dependency order with bounded concurrency and cooperative cancellation.
//
// LOCK ORDER (must be observed everywhere in this module):
//   `features` (RwLock<Vec<Feature>>)  >>  `swarm_state` (RwLock<SwarmState>)
//
// That is: if both locks are needed simultaneously, acquire `features` first
// (or snapshot what you need and drop its guard), then acquire `swarm_state`.
// Holding `swarm_state` while awaiting on `features.read()` / `features.write()`
// is forbidden — it sets up a circular wait against the dominant order used
// by `update_feature_status`, `run_feature`, the scheduler-prep blocks, etc.,
// and risks deadlock.
//
// When both pieces of data are needed, the safe idiom is to **clone the data
// out of `features` first, drop the guard, then take `swarm_state.write()`**:
//
//     let snapshot = {
//         let feats = features.read().await;
//         feats.iter().map(...).collect::<Vec<_>>()  // or just .clone()
//     }; // <-- guard dropped here
//     let mut state = swarm_state.write().await;
//     // ... use `snapshot` ...
//
// Sequential, non-nested locking (each lock in its own `{ ... }` scope that
// fully releases before the next acquires) is fine in either order.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, Notify, RwLock, Semaphore};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

use crate::core::guard::ValidationResult;
use crate::core::handoff::{HandoffParseFailed, SuccessState};
use crate::core::scheduler::Scheduler;
use crate::core::scout::run_scout;
use crate::core::validation::{AssertionStatus, ValidationAssertion, ValidationState};
use crate::core::worker::run_worker;
use crate::domain::swarm::{
    Feature, FeatureStatus, Milestone, ModelSettings, SwarmState, SwarmStatus,
    SwarmUsageAccumulator,
};
use crate::pi::events::PiEvent;
use crate::pi::manager::PiManager;
use crate::pi::rpc::PiRpcError;
use crate::pi::session::PiSession;
use crate::state::channel_drop::DropWarner;
use crate::state::progress::{ProgressEvent, ProgressEventType};
use crate::state::store::SwarmStore;
use crate::state::usage_store::{UsageEntry, UsageStore};

/// Runtime/fix-feature system prompt for the Queen orchestrator.
///
/// This markdown documents the contract Worker/Guard sessions assume when
/// the Queen synthesises fix-features after a Guard failure. The actual
/// fix-feature dispatch path is deterministic Rust (`create_fix_features`
/// below); this prompt is baked into the binary so the Settings → Prompts
/// tab can surface the contract alongside the other bee agents.
const QUEEN_SYSTEM_PROMPT: &str = include_str!("../../prompts/queen_system.md");

/// Returns the runtime system prompt for the Queen role.
pub fn default_system_prompt() -> &'static str {
    QUEEN_SYSTEM_PROMPT
}

/// Sender used to forward per-agent Pi activity to the frontend
/// `swarm-activity` event stream. Wrapped in `Option` at call sites because
/// the simpler `run_swarm` test harness does not provide one.
///
/// Bounded at the construction site (see `commands::swarms::start_swarm`)
/// so a slow frontend can't buffer unbounded swarm activity into memory.
/// Use [`try_send_activity`] to send and drop-with-rate-limited-warn on
/// `Full`, rather than calling `.send().await` (which would block the
/// producer on backpressure — undesirable for live UI streaming).
pub type ActivityTx = mpsc::Sender<serde_json::Value>;

/// Rate-limited counter for activity-channel drops. Shared across every
/// `try_send_activity` call site so the warn line reflects total system-wide
/// backpressure, not per-site.
static ACTIVITY_DROP_WARN: DropWarner = DropWarner::new("swarm_activity");

/// Try to enqueue a swarm-activity payload, dropping (with a rate-limited
/// warn) on `Full`. Returns true if the message was queued.
///
/// Used everywhere that previously did `let _ = tx.send(payload)` on the
/// unbounded variant. Now that the channel is bounded we want to
/// observe + record drops rather than silently lose them. Channel-closed
/// drops are intentionally not warned about — that path fires every time a
/// swarm completes (consumer task exits).
pub fn try_send_activity(tx: &ActivityTx, payload: serde_json::Value) -> bool {
    match tx.try_send(payload) {
        Ok(()) => true,
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
            ACTIVITY_DROP_WARN.note_drop();
            false
        }
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => false,
    }
}

/// Auto-inject per-milestone validator features into `features`.
///
/// For every milestone, append a synthetic feature with:
/// - `id`: `validate-<milestone_id>`
/// - `name`: `Validate <milestone.name>`
/// - `description`: stub describing the assertions to run
/// - `dependencies`: every impl feature listed in `milestone.features`
/// - `milestone`: `Some(milestone_id)`
/// - `fulfills`: every `VAL-*` ID belonging to this milestone
///
/// Additionally, for each milestone after the first, mark the **first impl
/// feature** of that milestone (as ordered in the input `features` slice)
/// as depending on the previous milestone's validator. This gives us
/// "milestone sealing" for free via the dependency graph: features in
/// milestone M+1 cannot start until the validator for milestone M has
/// completed.
///
/// Idempotent: if a `validate-<milestone_id>` feature is already present,
/// it is not re-injected. Existing impl-feature dependencies are preserved
/// (we only add, never remove).
pub fn inject_milestone_validators(
    features: &mut Vec<Feature>,
    milestones: &[Milestone],
    assertions: &[ValidationAssertion],
) {
    if milestones.is_empty() {
        return;
    }

    // Bucket assertion IDs by milestone for fast lookup.
    let mut assertions_by_milestone: std::collections::HashMap<&str, Vec<String>> =
        std::collections::HashMap::new();
    for a in assertions {
        assertions_by_milestone
            .entry(a.milestone_id.as_str())
            .or_default()
            .push(a.id.clone());
    }

    // Find the first impl feature for each milestone, preserving input order.
    // We honour the order milestones appear in the input slice and the order
    // features appear in the input list. Collected as owned strings to
    // satisfy the borrow checker when we mutate `features` below.
    let mut first_feature_of_milestone: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for (idx, f) in features.iter().enumerate() {
        if f.is_validator() {
            continue;
        }
        if let Some(mid) = f.milestone.as_deref() {
            first_feature_of_milestone
                .entry(mid.to_string())
                .or_insert(idx);
        }
    }

    // Add inter-milestone gating: each milestone's first impl feature
    // depends on the previous milestone's validator. We use the order in
    // `milestones` (not feature appearance) so the gating chain matches the
    // user's intended sequencing.
    for window in milestones.windows(2) {
        let prev = &window[0];
        let next = &window[1];
        let validator_id = format!("validate-{}", prev.id);
        if let Some(&fidx) = first_feature_of_milestone.get(next.id.as_str()) {
            let feature = &mut features[fidx];
            if !feature.dependencies.contains(&validator_id) {
                feature.dependencies.push(validator_id);
            }
        }
    }

    // Append validator features. Skip any that already exist (idempotent).
    let existing_ids: std::collections::HashSet<String> =
        features.iter().map(|f| f.id.clone()).collect();

    for milestone in milestones {
        let validator_id = format!("validate-{}", milestone.id);
        if existing_ids.contains(&validator_id) {
            continue;
        }

        let fulfills: Vec<String> = assertions_by_milestone
            .get(milestone.id.as_str())
            .cloned()
            .unwrap_or_default();

        // Only inject a validator when there is actually something to check.
        // A milestone with no assertions has no contract worth verifying;
        // letting an empty validator run only adds noise.
        if fulfills.is_empty() {
            continue;
        }

        let dependencies = milestone.features.clone();
        let description = format!(
            "Run Guard validation for milestone '{}' against assertions: {}",
            milestone.name,
            fulfills.join(", ")
        );

        let mut validator = Feature::new(
            validator_id,
            format!("Validate {}", milestone.name),
            description,
        );
        validator.dependencies = dependencies;
        validator.milestone = Some(milestone.id.clone());
        validator.fulfills = fulfills;
        features.push(validator);
    }
}

/// Pause control plumbed in from the `SwarmRegistry`. The queen blocks on
/// the `notify` whenever `paused` is true, so the user's Pause click takes
/// effect at the next safe yield point (between feature batches).
#[derive(Clone)]
pub struct PauseHandles {
    pub paused: Arc<AtomicBool>,
    pub notify: Arc<Notify>,
}

/// Block while `pause.paused` is true. Wakes on `pause.notify` (Resume) or
/// `cancel_token` (Stop). Returns immediately when the swarm isn't paused.
async fn wait_while_paused(pause: Option<&PauseHandles>, cancel_token: &CancellationToken) {
    let Some(p) = pause else {
        return;
    };
    while p.paused.load(Ordering::Relaxed) {
        if cancel_token.is_cancelled() {
            return;
        }
        tokio::select! {
            _ = p.notify.notified() => {},
            _ = cancel_token.cancelled() => return,
        }
    }
}

/// Build an `agent_start` activity payload for a fresh agent session.
fn agent_start_payload(
    swarm_id: &str,
    feature_id: &str,
    agent: &str,
    session_id: &str,
    model: &str,
) -> serde_json::Value {
    serde_json::json!({
        "swarm_id": swarm_id,
        "feature_id": feature_id,
        "agent": agent,
        "session_id": session_id,
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "kind": "agent_start",
        "model": model,
    })
}

/// Build an `agent_end` activity payload, marking success / failure.
fn agent_end_payload(
    swarm_id: &str,
    feature_id: &str,
    agent: &str,
    session_id: &str,
    success: bool,
) -> serde_json::Value {
    serde_json::json!({
        "swarm_id": swarm_id,
        "feature_id": feature_id,
        "agent": agent,
        "session_id": session_id,
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "kind": "agent_end",
        "success": success,
    })
}

/// Spawn a background task that subscribes to a Pi session's event broadcast
/// and forwards each interesting `PiEvent` to the swarm activity channel,
/// tagged with the agent's context. The task exits when the broadcast closes
/// (i.e. the session is killed) or `RecvError::Closed` is observed.
///
/// Runs in parallel with the agent's normal `collect_response()` consumer —
/// the broadcast supports multiple receivers.
/// Public wrapper around the private `spawn_agent_forwarder` so sibling
/// modules (e.g. `core::scout_review`) can plug their own Pi sessions into
/// the swarm activity stream without duplicating the broadcast bridge.
pub fn spawn_agent_forwarder_public(
    session: &Arc<PiSession>,
    swarm_id: String,
    feature_id: String,
    agent: String,
    session_id: String,
    activity_tx: ActivityTx,
) {
    spawn_agent_forwarder(
        session,
        swarm_id,
        feature_id,
        agent,
        session_id,
        activity_tx,
    )
}

fn spawn_agent_forwarder(
    session: &Arc<PiSession>,
    swarm_id: String,
    feature_id: String,
    agent: String,
    session_id: String,
    activity_tx: ActivityTx,
) {
    let mut rx = session.subscribe_events();
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    let mapped: Option<(&'static str, serde_json::Value)> = match event {
                        PiEvent::TextDelta(s) => Some(("text", serde_json::json!({ "text": s }))),
                        PiEvent::ThinkingDelta(s) => {
                            Some(("thinking", serde_json::json!({ "text": s })))
                        }
                        PiEvent::ToolExecutionStart {
                            tool_call_id,
                            name,
                            args,
                        } => {
                            // submit_context is the Hivemind context-gather
                            // submission tool. Its `summary` arg IS the body
                            // the user sees in the Hivemind-context activity
                            // card, so surface it as a synthetic text event
                            // (same shape as a TextDelta) instead of a
                            // tool_start card. Without this the card would
                            // render empty whenever Pi takes the tool path.
                            if name == "submit_context" {
                                args.get("summary")
                                    .and_then(|s| s.as_str())
                                    .map(|s| ("text", serde_json::json!({ "text": s.to_string() })))
                            } else if agent == "hivemind-merge" && name == "submit_plan" {
                                // For the hivemind merge agent, the
                                // `submit_plan` tool's `plan_markdown` arg is
                                // the merge body the user wants to read —
                                // surface it as a synthetic text event so the
                                // swarm-activity merge bubble renders the
                                // plan inline instead of a bare tool card.
                                args.get("plan_markdown")
                                    .and_then(|s| s.as_str())
                                    .map(|s| ("text", serde_json::json!({ "text": s.to_string() })))
                            } else {
                                Some((
                                    "tool_start",
                                    serde_json::json!({
                                        "tool_call_id": tool_call_id,
                                        "tool_name": name,
                                    }),
                                ))
                            }
                        }
                        PiEvent::ToolExecutionUpdate {
                            tool_call_id,
                            output,
                        } => Some((
                            "tool_update",
                            serde_json::json!({
                                "tool_call_id": tool_call_id,
                                "tool_output": output,
                            }),
                        )),
                        PiEvent::ToolExecutionEnd {
                            tool_call_id,
                            result,
                        } => Some((
                            "tool_end",
                            serde_json::json!({
                                "tool_call_id": tool_call_id,
                                "tool_result": result,
                            }),
                        )),
                        PiEvent::Error(msg) => Some(("error", serde_json::json!({ "error": msg }))),
                        _ => None,
                    };
                    if let Some((kind, mut payload)) = mapped {
                        if let serde_json::Value::Object(ref mut m) = payload {
                            m.insert(
                                "swarm_id".to_string(),
                                serde_json::Value::String(swarm_id.clone()),
                            );
                            m.insert(
                                "feature_id".to_string(),
                                serde_json::Value::String(feature_id.clone()),
                            );
                            m.insert(
                                "agent".to_string(),
                                serde_json::Value::String(agent.clone()),
                            );
                            m.insert(
                                "session_id".to_string(),
                                serde_json::Value::String(session_id.clone()),
                            );
                            m.insert(
                                "timestamp".to_string(),
                                serde_json::Value::String(chrono::Utc::now().to_rfc3339()),
                            );
                            m.insert(
                                "kind".to_string(),
                                serde_json::Value::String(kind.to_string()),
                            );
                        }
                        // try_send_activity drops on Full (with rate-limited
                        // warn) but returns false on either Full or Closed.
                        // We only want to break the loop on Closed, since
                        // Full is transient backpressure that may resolve.
                        match activity_tx.try_send(payload) {
                            Ok(()) => {}
                            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                                ACTIVITY_DROP_WARN.note_drop();
                            }
                            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                                break;
                            }
                        }
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(
                        swarm_id = %swarm_id,
                        agent = %agent,
                        feature_id = %feature_id,
                        dropped = n,
                        "swarm activity forwarder broadcast lagged"
                    );
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

/// Configuration for the queen orchestrator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueenConfig {
    /// Maximum number of features to execute concurrently.
    pub max_concurrent_features: usize,
    /// Maximum number of fix attempts per feature.
    pub max_fix_attempts: u32,
    /// Phase 5A: hard cap on this swarm's lifetime spend in USD. `None`
    /// means unlimited. The queen consults this between feature batches
    /// against the live `SwarmUsageAccumulator` total.
    #[serde(default)]
    pub swarm_budget_usd: Option<f64>,
    /// Phase 5A: hard cap on aggregate spend across all swarms / hivemind /
    /// chat usage today (UTC). `None` means unlimited. The queen sums
    /// today's `usage_log` rows via `UsageStore::daily_total_cost`.
    #[serde(default)]
    pub daily_budget_usd: Option<f64>,
}

impl Default for QueenConfig {
    fn default() -> Self {
        Self {
            // Sequential by default. The bee-colony mental model implies
            // one feature at a time; concurrency is opt-in via NewSwarm's
            // Advanced panel, plumbed through ModelSettings.
            max_concurrent_features: 1,
            max_fix_attempts: 3,
            swarm_budget_usd: None,
            daily_budget_usd: None,
        }
    }
}

/// Full-featured swarm execution with all subsystem handles.
///
/// This is the richer entry point used when all subsystems (PiManager,
/// SwarmStore, ProgressWriter) are available. The simpler `run_swarm`
/// delegates to this when the caller provides the full set of handles.
///
/// The optional `accumulator` provides real-time token/cost tracking that
/// is merged into `get_swarm_usage` responses. If `None`, a fresh one is
/// created internally but is inaccessible externally (the caller should
/// register an accumulator via `SwarmRegistry` and pass it here).
#[allow(clippy::too_many_arguments)]
#[tracing::instrument(skip_all, fields(swarm_id = tracing::field::Empty, agent = "queen"))]
pub async fn run_swarm_full(
    config: QueenConfig,
    swarm_state: Arc<RwLock<SwarmState>>,
    features: Arc<RwLock<Vec<Feature>>>,
    milestones: Vec<Milestone>,
    validation_assertions: Vec<ValidationAssertion>,
    pi_manager: Arc<PiManager>,
    swarm_store: Arc<SwarmStore>,
    event_tx: broadcast::Sender<ProgressEvent>,
    cancel_token: CancellationToken,
    usage_store: Option<Arc<UsageStore>>,
    activity_tx: Option<ActivityTx>,
    accumulator: Option<SwarmUsageAccumulator>,
    pause: Option<PauseHandles>,
    scout_review_ctx: Option<crate::core::scout_review::ScoutReviewContext>,
) -> Result<()> {
    let milestone_map: HashMap<String, Milestone> = milestones
        .iter()
        .map(|m| (m.id.clone(), m.clone()))
        .collect();
    // Build an `Arc<HashMap<assertion_id, ValidationAssertion>>` so each
    // spawned feature task can look up VAL-* assertions by ID without
    // cloning the whole registry.
    let assertion_registry: Arc<HashMap<String, ValidationAssertion>> = Arc::new(
        validation_assertions
            .into_iter()
            .map(|a| (a.id.clone(), a))
            .collect(),
    );

    let scheduler = {
        let feats = features.read().await;
        Arc::new(RwLock::new(Scheduler::new(feats.clone())?))
    };

    // Update swarm status to Implementing
    {
        let mut state = swarm_state.write().await;
        state.set_status(SwarmStatus::Implementing);
        state.current_phase = "implementing".to_string();
    }

    let swarm_id = {
        let state = swarm_state.read().await;
        state.id.clone()
    };
    tracing::Span::current().record("swarm_id", swarm_id.as_str());

    // Persist initial state
    {
        let state = swarm_state.read().await;
        swarm_store
            .write_state(&swarm_id, &state)
            .await
            .context("failed to persist initial swarm state")?;
    }

    emit_progress(
        &event_tx,
        &swarm_id,
        ProgressEventType::SwarmStarted,
        "Swarm execution started",
    );

    let (working_dir, model_settings) = {
        let state = swarm_state.read().await;
        (
            PathBuf::from(&state.working_directory),
            state.model_settings.clone(),
        )
    };

    let semaphore = Arc::new(Semaphore::new(config.max_concurrent_features));

    // Phase 5A budget enforcement state. Caps are read once at swarm
    // start; `last_check` throttles the per-iteration DB query so we
    // don't hammer SQLite for the daily total on a tight scheduler
    // loop. First iteration always checks.
    let budget_caps = crate::core::budget::BudgetCaps {
        swarm_budget_usd: config.swarm_budget_usd,
        daily_budget_usd: config.daily_budget_usd,
    };
    let mut last_budget_check: Option<std::time::Instant> = None;
    const BUDGET_CHECK_THROTTLE: std::time::Duration = std::time::Duration::from_secs(10);

    loop {
        if cancel_token.is_cancelled() {
            tracing::info!("swarm cancelled by user");
            let mut state = swarm_state.write().await;
            state.set_status(SwarmStatus::Cancelled);
            let _ = swarm_store.write_state(&swarm_id, &state).await;
            emit_progress(
                &event_tx,
                &swarm_id,
                ProgressEventType::SwarmPaused,
                "Swarm cancelled",
            );
            return Ok(());
        }

        // Honour Pause between batches. Currently-running features finish;
        // no new batch dispatches until Resume or Stop.
        if let Some(p) = &pause {
            if p.paused.load(Ordering::Relaxed) {
                tracing::info!(swarm_id = %swarm_id, "swarm paused; waiting for resume");
                emit_progress(
                    &event_tx,
                    &swarm_id,
                    ProgressEventType::SwarmPaused,
                    "Swarm paused",
                );
                wait_while_paused(Some(p), &cancel_token).await;
                if cancel_token.is_cancelled() {
                    continue;
                }
                tracing::info!(swarm_id = %swarm_id, "swarm resumed");
                emit_progress(
                    &event_tx,
                    &swarm_id,
                    ProgressEventType::SwarmStarted,
                    "Swarm resumed",
                );
            }
        }

        let statuses = {
            let feats = features.read().await;
            feats
                .iter()
                .map(|f| (f.id.clone(), f.status.clone()))
                .collect::<HashMap<String, FeatureStatus>>()
        };

        // Phase 5A: budget check between feature batches. Throttled to at
        // most once per `BUDGET_CHECK_THROTTLE` so the daily-total query
        // doesn't dominate the loop. Skipped entirely when both caps are
        // None. Failures (e.g. SQLite hiccup) are logged at WARN and
        // ignored — the check must never kill the swarm itself.
        let needs_budget_check = (budget_caps.swarm_budget_usd.is_some()
            || budget_caps.daily_budget_usd.is_some())
            && last_budget_check
                .map(|t| t.elapsed() >= BUDGET_CHECK_THROTTLE)
                .unwrap_or(true);
        if needs_budget_check {
            last_budget_check = Some(std::time::Instant::now());

            let swarm_spend = accumulator
                .as_ref()
                .map(|a| a.snapshot().cost)
                .unwrap_or(0.0);

            // The DB-aggregated daily total only matters when the daily
            // cap is set. Skip the query otherwise. Use 0.0 on error so
            // the helper still considers the swarm cap; emit a WARN.
            let daily_spend = if budget_caps.daily_budget_usd.is_some() {
                match &usage_store {
                    Some(store) => match store.daily_total_cost().await {
                        Ok(v) => v,
                        Err(e) => {
                            tracing::warn!(
                                swarm_id = %swarm_id,
                                error = %e,
                                "budget check: daily_total_cost query failed; continuing without daily cap enforcement"
                            );
                            0.0
                        }
                    },
                    None => 0.0,
                }
            } else {
                0.0
            };

            if let Some(breach) =
                crate::core::budget::evaluate_budget(budget_caps, swarm_spend, daily_spend)
            {
                let cap = match breach {
                    crate::core::budget::BudgetBreach::Swarm => budget_caps.swarm_budget_usd,
                    crate::core::budget::BudgetBreach::Daily => budget_caps.daily_budget_usd,
                };
                let scope = match breach {
                    crate::core::budget::BudgetBreach::Swarm => "swarm",
                    crate::core::budget::BudgetBreach::Daily => "daily",
                };
                tracing::warn!(
                    swarm_id = %swarm_id,
                    scope,
                    swarm_spend,
                    daily_spend,
                    cap = ?cap,
                    "{}",
                    breach.reason()
                );

                let metadata = serde_json::json!({
                    "reason": breach.reason(),
                    "scope": scope,
                    "swarm_spend": swarm_spend,
                    "daily_spend": daily_spend,
                    "cap": cap,
                });
                let event = ProgressEvent::new(
                    ProgressEventType::BudgetExceeded,
                    swarm_id.clone(),
                    breach.reason().to_string(),
                )
                .with_metadata(metadata);
                let _ = event_tx.send(event);

                // Set the paused flag on the swarm. If the registry's
                // pause handles were threaded in we use them so the
                // user's later Resume click can wake the same Notify;
                // either way we transition status to Paused and persist.
                if let Some(p) = &pause {
                    p.paused.store(true, Ordering::Relaxed);
                }
                {
                    let mut state = swarm_state.write().await;
                    state.set_status(SwarmStatus::Paused);
                    let _ = swarm_store.write_state(&swarm_id, &state).await;
                }

                emit_progress(
                    &event_tx,
                    &swarm_id,
                    ProgressEventType::SwarmPaused,
                    &format!("Swarm paused: {}", breach.reason()),
                );

                // Fall through to the existing pause-handling block at
                // the top of the loop on the next iteration so the same
                // wait_while_paused() path runs (which also handles Stop
                // cleanly). `continue` restarts the loop.
                continue;
            }
        }

        {
            let sched = scheduler.read().await;
            if sched.all_complete(&statuses) {
                tracing::info!("all features complete");
                break;
            }
        }

        let ready = {
            let sched = scheduler.read().await;
            sched.next_ready_batch(&statuses)
        };

        tracing::debug!(
            swarm_id = %swarm_id,
            ready_count = ready.len(),
            ready_ids = ?ready,
            in_progress = statuses.values().filter(|s| matches!(s, FeatureStatus::Scouting | FeatureStatus::Implementing | FeatureStatus::Reviewing | FeatureStatus::Validating)).count(),
            completed = statuses.values().filter(|s| matches!(s, FeatureStatus::Completed)).count(),
            "scheduler batch (full)"
        );

        if ready.is_empty() {
            let has_in_progress = statuses.values().any(|s| {
                matches!(
                    s,
                    FeatureStatus::Scouting
                        | FeatureStatus::Implementing
                        | FeatureStatus::Reviewing
                        | FeatureStatus::Validating
                )
            });

            if !has_in_progress {
                let mut state = swarm_state.write().await;
                state.set_error(
                    "deadlock detected: no features are ready and none are in progress".to_string(),
                );
                let _ = swarm_store.write_state(&swarm_id, &state).await;
                return Err(anyhow!("deadlock: no features ready and none in progress"));
            }

            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            continue;
        }

        let mut join_set: JoinSet<Result<(String, FeatureStatus)>> = JoinSet::new();

        for feature_id in ready {
            let sem = semaphore.clone();
            let features_ref = features.clone();
            let pi_mgr = pi_manager.clone();
            let milestones_ref = milestone_map.clone();
            let event_tx_ref = event_tx.clone();
            let swarm_id_ref = swarm_id.clone();
            let cancel = cancel_token.clone();
            let config_ref = config.clone();
            let scheduler_ref = scheduler.clone();
            let working_dir_ref = working_dir.clone();
            let model_settings_ref = model_settings.clone();
            let usage_ref = usage_store.clone();
            let store_ref = swarm_store.clone();
            let activity_ref = activity_tx.clone();
            let acc_ref = accumulator.clone();
            let assertion_registry_ref = assertion_registry.clone();
            let scout_review_ctx_ref = scout_review_ctx.clone();

            let feature_span = tracing::info_span!(
                "feature_run",
                swarm_id = %swarm_id_ref,
                feature_id = %feature_id,
            );
            join_set.spawn(
                async move {
                    let _permit = sem
                        .acquire()
                        .await
                        .map_err(|e| anyhow!("semaphore closed: {}", e))?;

                    let result = run_feature_full(
                        &feature_id,
                        &features_ref,
                        &pi_mgr,
                        &milestones_ref,
                        &assertion_registry_ref,
                        &event_tx_ref,
                        &swarm_id_ref,
                        &cancel,
                        &config_ref,
                        &scheduler_ref,
                        &working_dir_ref,
                        &model_settings_ref,
                        usage_ref.as_deref(),
                        Some(&store_ref),
                        activity_ref.as_ref(),
                        acc_ref,
                        scout_review_ctx_ref.as_ref(),
                    )
                    .await;

                    match result {
                        Ok(status) => Ok((feature_id, status)),
                        Err(e) => {
                            tracing::error!(error = %e, "feature execution failed");
                            Ok((feature_id, FeatureStatus::Failed))
                        }
                    }
                }
                .instrument(feature_span),
            );
        }

        while let Some(result) = tokio::select! {
            result = join_set.join_next() => result,
            _ = cancel_token.cancelled() => {
                tracing::info!("cancellation requested during feature execution");
                join_set.shutdown().await;
                let mut state = swarm_state.write().await;
                state.set_status(SwarmStatus::Cancelled);
                let _ = swarm_store.write_state(&swarm_id, &state).await;
                emit_progress(
                    &event_tx,
                    &swarm_id,
                    ProgressEventType::SwarmPaused,
                    "Swarm cancelled during execution",
                );
                return Ok(());
            }
        } {
            match result {
                Ok(Ok((feature_id, status))) => {
                    {
                        let mut feats = features.write().await;
                        if let Some(feat) = feats.iter_mut().find(|f| f.id == feature_id) {
                            feat.status = status.clone();
                        }
                    }

                    {
                        let mut state = swarm_state.write().await;
                        state.updated_at = chrono::Utc::now();
                        let _ = swarm_store.write_state(&swarm_id, &state).await;
                    }

                    {
                        let feats = features.read().await;
                        let _ = swarm_store.write_features(&swarm_id, &feats).await;
                    }

                    let event_type = match &status {
                        FeatureStatus::Completed => ProgressEventType::FeatureValidated,
                        FeatureStatus::Failed => ProgressEventType::FeatureFailed,
                        FeatureStatus::Skipped => ProgressEventType::FeatureSkipped,
                        _ => ProgressEventType::FeatureImplemented,
                    };

                    emit_feature_progress(
                        &event_tx,
                        &swarm_id,
                        &feature_id,
                        event_type,
                        &format!("Feature '{}' status: {}", feature_id, status),
                    );

                    tracing::info!(
                        feature_id = %feature_id,
                        status = %status,
                        "feature execution result recorded"
                    );
                }
                Ok(Err(e)) => {
                    tracing::error!(
                        swarm_id = %swarm_id,
                        error = %e,
                        "feature task returned error",
                    );
                }
                Err(e) => {
                    tracing::error!(
                        swarm_id = %swarm_id,
                        error = %e,
                        "feature task panicked",
                    );
                }
            }
        }
    }

    // Write final state
    {
        let mut state = swarm_state.write().await;
        let feats = features.read().await;
        let all_completed = feats.iter().all(|f| f.status == FeatureStatus::Completed);
        let any_failed = feats.iter().any(|f| f.status == FeatureStatus::Failed);

        if all_completed {
            state.set_status(SwarmStatus::Completed);
        } else if any_failed {
            state.set_status(SwarmStatus::Failed);
        } else {
            state.set_status(SwarmStatus::Completed);
        }

        state.current_phase = "finished".to_string();
        let _ = swarm_store.write_state(&swarm_id, &state).await;
        let _ = swarm_store.write_features(&swarm_id, &feats).await;
    }

    emit_progress(
        &event_tx,
        &swarm_id,
        ProgressEventType::SwarmCompleted,
        "Swarm execution complete",
    );

    tracing::info!(swarm_id = %swarm_id, "swarm execution finished");

    Ok(())
}

/// Execute a single feature through the full pipeline with a PiManager.
///
/// Pipeline: Scout -> Worker -> Guard (if milestone complete)
///
/// Model selection is driven by `model_settings`:
/// - `scout_model` is used for the scout Pi session.
/// - `primary_model` is used for worker and guard Pi sessions.
///
/// The optional `accumulator` receives live token/cost deltas from each
/// Pi session as they execute. After `record_session_usage` persists the
/// final stats to the DB, the accumulator entry is subtracted to prevent
/// double-counting in `get_swarm_usage`.
#[allow(clippy::too_many_arguments)]
#[tracing::instrument(
    skip_all,
    fields(
        feature_id = %feature_id,
        swarm_id = %swarm_id,
        // Audit 2.3: filled in below once we mint a per-attempt run_id.
        run_id = tracing::field::Empty,
    )
)]
async fn run_feature_full(
    feature_id: &str,
    features: &Arc<RwLock<Vec<Feature>>>,
    pi_manager: &Arc<PiManager>,
    milestones: &HashMap<String, Milestone>,
    assertion_registry: &Arc<HashMap<String, ValidationAssertion>>,
    event_tx: &broadcast::Sender<ProgressEvent>,
    swarm_id: &str,
    cancel_token: &CancellationToken,
    config: &QueenConfig,
    scheduler: &Arc<RwLock<Scheduler>>,
    working_dir: &PathBuf,
    model_settings: &ModelSettings,
    usage_store: Option<&UsageStore>,
    swarm_store: Option<&Arc<SwarmStore>>,
    activity_tx: Option<&ActivityTx>,
    accumulator: Option<SwarmUsageAccumulator>,
    scout_review_ctx: Option<&crate::core::scout_review::ScoutReviewContext>,
) -> Result<FeatureStatus> {
    let feature = {
        let feats = features.read().await;
        feats
            .iter()
            .find(|f| f.id == feature_id)
            .cloned()
            .ok_or_else(|| anyhow!("feature '{}' not found", feature_id))?
    };

    // Audit 2.3: mint a per-attempt run_id so retries of the same feature
    // are distinguishable in the JSONL progress log. The Scout, Worker, and
    // (if it runs inline) Guard for this attempt all share this id.
    let run_id = format!(
        "run-{}-{}-{}",
        feature_id,
        feature.fix_attempt_count,
        uuid::Uuid::new_v4().simple()
    );
    tracing::Span::current().record("run_id", run_id.as_str());

    // Auto-injected validator features skip Scout/Worker entirely and run
    // Guard directly against the assertions listed in their `fulfills`.
    // This is Phase 2: Guard is no longer a magic phase inside this
    // function; it's a regular scheduled feature.
    if feature.is_validator() {
        return run_validator_feature_full(
            &feature,
            features,
            pi_manager,
            milestones,
            assertion_registry,
            event_tx,
            swarm_id,
            cancel_token,
            config,
            scheduler,
            working_dir,
            model_settings,
            usage_store,
            swarm_store,
            activity_tx,
            accumulator,
            scout_review_ctx.and_then(|c| c.nurse_engine.as_ref()),
            scout_review_ctx.map(|c| &c.app_handle),
        )
        .await;
    }

    // ---- Phase 1: Scout ----
    if cancel_token.is_cancelled() {
        return Ok(FeatureStatus::Skipped);
    }

    update_feature_status(
        features,
        swarm_store,
        swarm_id,
        feature_id,
        FeatureStatus::Scouting,
    )
    .await;
    emit_feature_progress_with_run(
        event_tx,
        swarm_id,
        feature_id,
        &run_id,
        ProgressEventType::FeatureStarted,
        &format!("Scouting feature '{}'", feature.name),
    );

    // Namespace by swarm_id so concurrent swarms running the same feature
    // ids (e.g. two clones of the same plan) don't collide in PiManager's
    // session map. A bare `scout-{feature_id}` key trips SessionExists and
    // fails the feature with no LLM call.
    let scout_session_id = format!("scout-{}-{}", swarm_id, feature_id);
    let scout_options = crate::pi::rpc::PiSessionOptions::for_scout(
        &model_settings.scout_model,
        crate::core::scout::default_system_prompt(),
    )
    .with_thinking_level(
        model_settings
            .scout_thinking_level
            .parse()
            .unwrap_or(crate::pi::rpc::ThinkingLevel::High),
    );
    let scout_session = pi_manager
        .spawn_session_with_options(&scout_session_id, &scout_options, working_dir.as_path())
        .await
        .map_err(|e| anyhow::anyhow!(e))
        .context("failed to spawn scout session")?;
    scout_session.set_owner(crate::pi::session::SessionOwner::Swarm {
        swarm_id: swarm_id.to_string(),
        role: "scout".to_string(),
    });
    // Audit 2.3: emit PiSessionSpawned now that the Pi subprocess is up.
    emit_pi_session_spawned(
        event_tx,
        swarm_id,
        Some(feature_id),
        Some(&run_id),
        &scout_session_id,
        "scout",
        scout_session.pid(),
    );

    if let Some(tx) = activity_tx {
        try_send_activity(
            tx,
            agent_start_payload(
                swarm_id,
                feature_id,
                "scout",
                &scout_session_id,
                &model_settings.scout_model,
            ),
        );
        spawn_agent_forwarder(
            &scout_session,
            swarm_id.to_string(),
            feature_id.to_string(),
            "scout".to_string(),
            scout_session_id.clone(),
            tx.clone(),
        );
    }

    let scout_start = std::time::Instant::now();
    let scout_result_outcome = run_scout(&scout_session, &feature, working_dir.as_path(), "")
        .await
        .context(format!("scout failed for feature '{}'", feature_id));
    if let Some(tx) = activity_tx {
        try_send_activity(
            tx,
            agent_end_payload(
                swarm_id,
                feature_id,
                "scout",
                &scout_session_id,
                scout_result_outcome.is_ok(),
            ),
        );
    }
    let scout_result = scout_result_outcome?;

    record_session_usage(
        usage_store,
        &scout_session,
        swarm_id,
        feature_id,
        "scout",
        &model_settings.scout_model,
        scout_start.elapsed().as_millis() as i64,
        accumulator.as_ref(),
    )
    .await;

    // Clean up scout session
    let _ = pi_manager.kill_session(&scout_session_id).await;
    emit_pi_session_killed(
        event_tx,
        swarm_id,
        Some(feature_id),
        Some(&run_id),
        &scout_session_id,
        "scout_complete",
    );

    tracing::info!(
        feature_id = %feature_id,
        complexity = %scout_result.estimated_complexity,
        "scout complete"
    );

    emit_feature_progress_with_run(
        event_tx,
        swarm_id,
        feature_id,
        &run_id,
        ProgressEventType::FeatureScouted,
        &format!(
            "Scout complete for '{}': complexity={}",
            feature.name, scout_result.estimated_complexity
        ),
    );

    // ---- Phase 1.5: Optional Hivemind review of Scout's plan ----
    //
    // If the swarm's ModelSettings enabled Scout-plan review and the bundle
    // of Hivemind subsystem handles was plumbed through, run a multi-model
    // review and use the refined plan as the Worker's input. Any failure
    // (missing hivemind, parse error, all-model failure, context-Pi crash)
    // falls back to the original Scout plan and emits a
    // `HivemindReviewSkipped` event so the user sees what happened.
    let worker_plan: String = if model_settings.use_hivemind_on_scout {
        match (scout_review_ctx, model_settings.hivemind_id.as_deref()) {
            (Some(ctx), Some(hivemind_id)) if !hivemind_id.is_empty() => {
                match crate::core::scout_review::run_scout_hivemind_review(
                    ctx,
                    pi_manager,
                    swarm_id,
                    feature_id,
                    &feature,
                    &scout_result.plan,
                    model_settings,
                    hivemind_id,
                    working_dir.as_path(),
                    cancel_token,
                    event_tx,
                    activity_tx,
                )
                .await
                {
                    Ok(outcome) => {
                        tracing::info!(
                            feature_id = %feature_id,
                            job_id = %outcome.job_id,
                            refined_len = outcome.refined_plan.len(),
                            "scout-plan hivemind review complete; using refined plan for worker"
                        );
                        outcome.refined_plan
                    }
                    Err(e) => {
                        tracing::warn!(
                            feature_id = %feature_id,
                            hivemind_id = %hivemind_id,
                            error = %e,
                            "scout-plan hivemind review failed; falling back to scout plan"
                        );
                        emit_feature_progress_with_meta(
                            event_tx,
                            swarm_id,
                            feature_id,
                            ProgressEventType::HivemindReviewSkipped,
                            &format!("Hivemind review skipped: {}", e),
                            Some(serde_json::json!({
                                "reason": e.to_string(),
                                "hivemind_id": hivemind_id,
                            })),
                        );
                        scout_result.plan.clone()
                    }
                }
            }
            _ => {
                tracing::warn!(
                    feature_id = %feature_id,
                    use_hivemind_on_scout = true,
                    has_ctx = scout_review_ctx.is_some(),
                    has_hivemind_id = model_settings.hivemind_id.is_some(),
                    "scout-plan review requested but context or hivemind_id missing; falling back to scout plan"
                );
                emit_feature_progress_with_meta(
                    event_tx,
                    swarm_id,
                    feature_id,
                    ProgressEventType::HivemindReviewSkipped,
                    "Hivemind review skipped: missing subsystem context or hivemind_id",
                    Some(serde_json::json!({
                        "reason": "missing_context_or_hivemind_id",
                    })),
                );
                scout_result.plan.clone()
            }
        }
    } else {
        scout_result.plan.clone()
    };

    // ---- Phase 2: Worker ----
    if cancel_token.is_cancelled() {
        return Ok(FeatureStatus::Skipped);
    }

    update_feature_status(
        features,
        swarm_store,
        swarm_id,
        feature_id,
        FeatureStatus::Implementing,
    )
    .await;
    emit_feature_progress_with_run(
        event_tx,
        swarm_id,
        feature_id,
        &run_id,
        ProgressEventType::FeatureStarted,
        &format!("Implementing feature '{}'", feature.name),
    );

    let worker_session_id = format!("worker-{}-{}", swarm_id, feature_id);
    let worker_options = crate::pi::rpc::PiSessionOptions::for_worker(
        &model_settings.primary_model,
        crate::core::worker::default_system_prompt(),
    )
    .with_thinking_level(
        model_settings
            .worker_thinking_level
            .parse()
            .unwrap_or(crate::pi::rpc::ThinkingLevel::Medium),
    );
    let worker_session = pi_manager
        .spawn_session_with_options(&worker_session_id, &worker_options, working_dir.as_path())
        .await
        .map_err(|e| anyhow::anyhow!(e))
        .context("failed to spawn worker session")?;
    worker_session.set_owner(crate::pi::session::SessionOwner::Swarm {
        swarm_id: swarm_id.to_string(),
        role: "worker".to_string(),
    });
    emit_pi_session_spawned(
        event_tx,
        swarm_id,
        Some(feature_id),
        Some(&run_id),
        &worker_session_id,
        "worker",
        worker_session.pid(),
    );

    if let Some(tx) = activity_tx {
        try_send_activity(
            tx,
            agent_start_payload(
                swarm_id,
                feature_id,
                "worker",
                &worker_session_id,
                &model_settings.primary_model,
            ),
        );
        spawn_agent_forwarder(
            &worker_session,
            swarm_id.to_string(),
            feature_id.to_string(),
            "worker".to_string(),
            worker_session_id.clone(),
            tx.clone(),
        );
    }

    let worker_start = std::time::Instant::now();
    // Phase 3: load optional per-swarm context (AGENTS.md / notes.md /
    // services.yaml) and inject into the worker prompt. Failures here are
    // logged-and-swallowed — context is a best-effort enhancement, never a
    // hard requirement for execution.
    let swarm_context = match swarm_store {
        Some(store) => {
            match crate::core::swarm_context::SwarmContext::load_for(store, swarm_id).await {
                Ok(ctx) => Some(ctx),
                Err(e) => {
                    tracing::warn!(
                        swarm_id = %swarm_id,
                        error = %e,
                        "failed to load swarm context; proceeding without it"
                    );
                    None
                }
            }
        }
        None => None,
    };
    let worker_result_outcome = run_worker(
        &worker_session,
        &feature,
        &worker_plan,
        working_dir.as_path(),
        "",
        swarm_context.as_ref(),
    )
    .await
    .context(format!("worker failed for feature '{}'", feature_id));
    if let Some(tx) = activity_tx {
        try_send_activity(
            tx,
            agent_end_payload(
                swarm_id,
                feature_id,
                "worker",
                &worker_session_id,
                worker_result_outcome.is_ok(),
            ),
        );
    }
    let worker_result = match worker_result_outcome {
        Ok(r) => r,
        Err(err) => {
            // Surface Nurse-recoverable failures (handoff parse, Pi RPC
            // timeout, subprocess crash) as a visible nurse intervention
            // before the feature is recorded as Failed. The LLM-driven path
            // emits its own Started lifecycle immediately; if Nurse is
            // disabled / degraded / fails, it falls back to the deterministic
            // synthesized event. Existing fix-feature retry logic in the
            // Guard validation path still drives recovery.
            synthesize_nurse_for_error(
                event_tx,
                &err,
                swarm_id,
                &feature,
                &worker_session_id,
                scout_review_ctx.and_then(|c| c.nurse_engine.as_ref()),
                Some(pi_manager),
                scout_review_ctx.map(|c| &c.app_handle),
            );
            return Err(err);
        }
    };

    record_session_usage(
        usage_store,
        &worker_session,
        swarm_id,
        feature_id,
        "worker",
        &model_settings.primary_model,
        worker_start.elapsed().as_millis() as i64,
        accumulator.as_ref(),
    )
    .await;

    // Clean up worker session
    let _ = pi_manager.kill_session(&worker_session_id).await;
    emit_pi_session_killed(
        event_tx,
        swarm_id,
        Some(feature_id),
        Some(&run_id),
        &worker_session_id,
        "worker_complete",
    );

    // Audit 2.3: emit a WorkerHandoff progress event so the JSONL log
    // carries the parsed Worker contract for crash recovery / replay.
    emit_worker_handoff(
        event_tx,
        swarm_id,
        feature_id,
        &run_id,
        &worker_result.handoff.success_state.to_string(),
    );

    // Check worker success state
    match worker_result.handoff.success_state {
        SuccessState::Failure => {
            tracing::warn!(feature_id = %feature_id, "worker reported failure");
            emit_feature_progress_with_run(
                event_tx,
                swarm_id,
                feature_id,
                &run_id,
                ProgressEventType::FeatureFailed,
                &format!("Worker reported failure for '{}'", feature.name),
            );
            return Ok(FeatureStatus::Failed);
        }
        SuccessState::Success | SuccessState::Partial => {
            tracing::info!(
                feature_id = %feature_id,
                success_state = %worker_result.handoff.success_state,
                "worker completed"
            );
        }
    }

    // ---- Phase 5C: Surface worker-reported discovered issues ----
    //
    // The Worker may have emitted `discovered_issues` in its handoff for
    // problems it noticed but which are NOT a hard failure of the feature.
    // We emit one progress event per issue (broadcast to the frontend),
    // and persist each to a per-swarm append-only JSONL audit log.
    //
    // CRITICAL: this is fire-and-forget. Failures to persist are logged at
    // WARN and never block worker completion — the swarm runs on.
    for issue in &worker_result.handoff.discovered_issues {
        let metadata = serde_json::json!({
            "severity": issue.severity.to_string(),
            "description": issue.description,
            "suggested_fix": issue.suggested_fix,
            "feature_name": feature.name,
        });
        let event = ProgressEvent::new(
            ProgressEventType::DiscoveredIssue,
            swarm_id.to_string(),
            format!(
                "Worker discovered {} issue: {}",
                issue.severity,
                // Truncate description in the human message; the full text
                // is in metadata for the UI.
                if issue.description.len() > 120 {
                    format!("{}…", &issue.description[..120])
                } else {
                    issue.description.clone()
                }
            ),
        )
        .with_feature(feature_id.to_string())
        .with_metadata(metadata);
        let _ = event_tx.send(event);

        if let Some(store) = swarm_store {
            if let Err(e) = store
                .append_discovered_issue(swarm_id, feature_id, issue)
                .await
            {
                tracing::warn!(
                    swarm_id = %swarm_id,
                    feature_id = %feature_id,
                    error = %e,
                    "failed to append DiscoveredIssue to JSONL log; continuing",
                );
            }
        }
    }

    emit_feature_progress_with_run(
        event_tx,
        swarm_id,
        feature_id,
        &run_id,
        ProgressEventType::FeatureImplemented,
        &format!("Worker complete for '{}'", feature.name),
    );

    // ---- Phase 3: Guard ----
    //
    // Phase 2 of the Factory adoption roadmap removed the inline Guard
    // block from this function. Validation now runs as a scheduled
    // synthetic feature (`validate-<milestone_id>`) auto-injected by
    // `inject_milestone_validators`, which calls `run_validator_feature_full`
    // when scheduled. Impl features no longer carry an inline Guard hop —
    // the dep graph alone sequences validators after their milestone's
    // impl features.

    Ok(FeatureStatus::Completed)
}

/// Execute a synthetic validator feature: skip Scout/Worker and run Guard
/// directly against the assertions in `feature.fulfills`.
///
/// On failure, applies the existing fix-feature machinery (capped by
/// `feature.max_fix_attempts`) but tags each fix with only the failed
/// `VAL-*` ids. On success, marks the milestone `sealed: true` on disk
/// and updates `validation-state.json`.
#[allow(clippy::too_many_arguments)]
#[tracing::instrument(skip_all, fields(feature_id = %feature.id, swarm_id = %swarm_id, validator = true))]
async fn run_validator_feature_full(
    feature: &Feature,
    features: &Arc<RwLock<Vec<Feature>>>,
    pi_manager: &Arc<PiManager>,
    milestones: &HashMap<String, Milestone>,
    assertion_registry: &Arc<HashMap<String, ValidationAssertion>>,
    event_tx: &broadcast::Sender<ProgressEvent>,
    swarm_id: &str,
    cancel_token: &CancellationToken,
    config: &QueenConfig,
    scheduler: &Arc<RwLock<Scheduler>>,
    working_dir: &PathBuf,
    model_settings: &ModelSettings,
    usage_store: Option<&UsageStore>,
    swarm_store: Option<&Arc<SwarmStore>>,
    activity_tx: Option<&ActivityTx>,
    accumulator: Option<SwarmUsageAccumulator>,
    nurse_engine: Option<&Arc<crate::nurse::engine::NurseEngine>>,
    app_handle: Option<&tauri::AppHandle>,
) -> Result<FeatureStatus> {
    let feature_id = &feature.id;

    // Audit 2.3: mint a per-attempt run_id for the validator's Guard run.
    let run_id = format!(
        "run-{}-{}-{}",
        feature_id,
        feature.fix_attempt_count,
        uuid::Uuid::new_v4().simple()
    );

    if cancel_token.is_cancelled() {
        return Ok(FeatureStatus::Skipped);
    }

    // Resolve the milestone this validator covers. Without a matching
    // milestone we have nothing to validate against.
    let milestone_id = match feature.milestone.as_deref() {
        Some(m) => m,
        None => {
            tracing::warn!(
                feature_id = %feature_id,
                "validator feature has no milestone; skipping"
            );
            return Ok(FeatureStatus::Skipped);
        }
    };
    let milestone = match milestones.get(milestone_id) {
        Some(m) => m.clone(),
        None => {
            tracing::warn!(
                feature_id = %feature_id,
                milestone_id = %milestone_id,
                "validator feature references unknown milestone; skipping"
            );
            return Ok(FeatureStatus::Skipped);
        }
    };

    // Resolve the VAL-* IDs in `fulfills` against the registry.
    let to_check: Vec<ValidationAssertion> = feature
        .fulfills
        .iter()
        .filter_map(|id| assertion_registry.get(id).cloned())
        .collect();

    if to_check.is_empty() {
        tracing::info!(
            feature_id = %feature_id,
            milestone_id = %milestone_id,
            "validator feature has no resolvable assertions; auto-passing"
        );
        // No work means auto-success — also seal the milestone so the
        // dep graph unblocks downstream features.
        seal_milestone_on_disk(swarm_store, swarm_id, milestone_id).await;
        return Ok(FeatureStatus::Completed);
    }

    update_feature_status(
        features,
        swarm_store,
        swarm_id,
        feature_id,
        FeatureStatus::Validating,
    )
    .await;
    emit_feature_progress_with_run(
        event_tx,
        swarm_id,
        feature_id,
        &run_id,
        ProgressEventType::GuardValidation,
        &format!("Validating milestone '{}'", milestone.name),
    );

    let guard_session_id = format!("guard-{}-{}", swarm_id, feature_id);
    let guard_options = crate::pi::rpc::PiSessionOptions::for_guard(
        model_settings.effective_guard_model(),
        crate::core::guard::default_system_prompt(),
    )
    .with_thinking_level(
        model_settings
            .guard_thinking_level
            .parse()
            .unwrap_or(crate::pi::rpc::ThinkingLevel::Medium),
    );
    let guard_session = pi_manager
        .spawn_session_with_options(&guard_session_id, &guard_options, working_dir.as_path())
        .await
        .map_err(|e| anyhow::anyhow!(e))
        .context("failed to spawn guard session for validator feature")?;
    guard_session.set_owner(crate::pi::session::SessionOwner::Swarm {
        swarm_id: swarm_id.to_string(),
        role: "guard".to_string(),
    });
    emit_pi_session_spawned(
        event_tx,
        swarm_id,
        Some(feature_id),
        Some(&run_id),
        &guard_session_id,
        "guard",
        guard_session.pid(),
    );
    // Audit 2.3: emit GuardAttempt at the top of each guard run iteration.
    // `attempt` is 1-indexed for human readability (fix_attempt_count is
    // 0-indexed and only incremented on failure, so attempt 1 corresponds
    // to fix_attempt_count == 0, attempt 2 to == 1, etc).
    emit_guard_attempt(
        event_tx,
        swarm_id,
        feature_id,
        Some(&run_id),
        feature.fix_attempt_count.saturating_add(1),
        milestone_id,
    );

    if let Some(tx) = activity_tx {
        try_send_activity(
            tx,
            agent_start_payload(
                swarm_id,
                feature_id,
                "guard",
                &guard_session_id,
                model_settings.effective_guard_model(),
            ),
        );
        spawn_agent_forwarder(
            &guard_session,
            swarm_id.to_string(),
            feature_id.to_string(),
            "guard".to_string(),
            guard_session_id.clone(),
            tx.clone(),
        );
    }

    let guard_start = std::time::Instant::now();
    let to_check_refs: Vec<&ValidationAssertion> = to_check.iter().collect();
    let guard_result_outcome = crate::core::guard::run_guard_with_assertions(
        &guard_session,
        feature,
        &milestone,
        &to_check_refs,
        working_dir.as_path(),
        "",
    )
    .await
    .context(format!(
        "guard validation failed for validator feature '{}'",
        feature_id
    ));
    if let Some(tx) = activity_tx {
        try_send_activity(
            tx,
            agent_end_payload(
                swarm_id,
                feature_id,
                "guard",
                &guard_session_id,
                guard_result_outcome
                    .as_ref()
                    .map(|r| r.passed)
                    .unwrap_or(false),
            ),
        );
    }
    let guard_result = match guard_result_outcome {
        Ok(r) => r,
        Err(err) => {
            synthesize_nurse_for_error(
                event_tx,
                &err,
                swarm_id,
                feature,
                &guard_session_id,
                nurse_engine,
                Some(pi_manager),
                app_handle,
            );
            return Err(err);
        }
    };

    record_session_usage(
        usage_store,
        &guard_session,
        swarm_id,
        feature_id,
        "guard",
        model_settings.effective_guard_model(),
        guard_start.elapsed().as_millis() as i64,
        accumulator.as_ref(),
    )
    .await;

    let _ = pi_manager.kill_session(&guard_session_id).await;
    emit_pi_session_killed(
        event_tx,
        swarm_id,
        Some(feature_id),
        Some(&run_id),
        &guard_session_id,
        "guard_complete",
    );

    // Update on-disk validation-state.json with per-assertion outcomes.
    update_validation_state_on_disk(swarm_store, swarm_id, &guard_result, &to_check).await;

    if !guard_result.passed {
        tracing::warn!(
            feature_id = %feature_id,
            failures = guard_result.failure_count(),
            "validator guard reported failures"
        );

        emit_feature_progress_with_run(
            event_tx,
            swarm_id,
            feature_id,
            &run_id,
            ProgressEventType::GuardValidation,
            &format!(
                "Guard validation failed for '{}': {} assertions failed",
                feature.name,
                guard_result.failure_count()
            ),
        );

        // Retry budget is tracked on the validator feature itself.
        let current_attempts = {
            let feats = features.read().await;
            feats
                .iter()
                .find(|f| f.id == *feature_id)
                .map(|f| f.fix_attempt_count)
                .unwrap_or(0)
        };

        if current_attempts < config.max_fix_attempts {
            // Generate fix features, but override their dependencies so they
            // wait on the validator's own dependencies (the milestone's impl
            // features, already Completed) rather than on the validator
            // itself. The default `create_fix_features` pattern of "depend
            // on the parent feature" works for impl features (Worker
            // already finished Completed when Guard ran), but **breaks for
            // validators**: the validator is about to be reset to Pending,
            // so any fix that depended on the validator would form a cycle
            // — or, if the validator stayed Failed, would never schedule.
            let mut fix_features = create_fix_features(&guard_result, feature);
            for fix in fix_features.iter_mut() {
                fix.dependencies = feature.dependencies.clone();
            }

            if !fix_features.is_empty() {
                // Phase 5B: enforce milestone sealing. If the milestone has
                // already been sealed (e.g. concurrent validator pass on a
                // related path), refuse to inject the fix-feature rather
                // than silently un-sealing the milestone. Mark the parent
                // Failed and let the user decide whether to start a fresh
                // swarm to address the issue.
                let milestones_vec: Vec<Milestone> = milestones.values().cloned().collect();
                let fix_ids: Vec<String> = fix_features.iter().map(|f| f.id.clone()).collect();

                let mut sched = scheduler.write().await;
                if let Err(e) =
                    sched.add_features_respecting_seals(fix_features.clone(), &milestones_vec)
                {
                    tracing::warn!(
                        feature_id = %feature_id,
                        milestone_id = %milestone_id,
                        reason = %e,
                        "refusing to inject fix-features into sealed milestone; marking parent Failed"
                    );
                    drop(sched);
                    return Ok(FeatureStatus::Failed);
                }

                // Extend the validator's dependencies (in the scheduler) to
                // include every new fix-feature id. The validator will be
                // reset to `Pending` below; the scheduler then waits for
                // the fixes to complete before re-firing the validator.
                let mut new_deps = feature.dependencies.clone();
                for fix_id in &fix_ids {
                    if !new_deps.contains(fix_id) {
                        new_deps.push(fix_id.clone());
                    }
                }
                if let Err(e) = sched.update_feature_deps(feature_id, new_deps.clone()) {
                    tracing::error!(
                        feature_id = %feature_id,
                        error = %e,
                        "failed to extend validator deps with fix-feature ids; marking Failed"
                    );
                    drop(sched);
                    return Ok(FeatureStatus::Failed);
                }
                drop(sched);

                {
                    let mut feats = features.write().await;
                    for fix in &fix_features {
                        feats.push(fix.clone());
                    }
                    if let Some(feat) = feats.iter_mut().find(|f| f.id == *feature_id) {
                        feat.increment_fix_attempts();
                        feat.dependencies = new_deps;
                    }
                }

                emit_progress(
                    event_tx,
                    swarm_id,
                    ProgressEventType::GuardValidation,
                    &format!(
                        "Created {} fix features for milestone '{}' — validator re-queued for retry",
                        fix_features.len(),
                        milestone.name
                    ),
                );

                // Reset the validator to Pending so the queen scheduler
                // re-picks it after the fix-features complete. Returning
                // Failed here would leave the validator terminal and block
                // every downstream feature that depends on it (the next
                // milestone, plus the fixes themselves before this
                // dependency override).
                return Ok(FeatureStatus::Pending);
            }

            return Ok(FeatureStatus::Failed);
        } else {
            tracing::error!(
                feature_id = %feature_id,
                attempts = current_attempts,
                "max fix attempts exceeded on validator feature"
            );
            return Ok(FeatureStatus::Failed);
        }
    }

    tracing::info!(
        feature_id = %feature_id,
        milestone_id = %milestone_id,
        "validator guard passed; sealing milestone"
    );

    // Seal the milestone on disk so future invocations can see it.
    seal_milestone_on_disk(swarm_store, swarm_id, milestone_id).await;

    Ok(FeatureStatus::Completed)
}

/// Mark a milestone as sealed on disk. Best-effort: log-and-swallow any
/// I/O error so an in-flight swarm isn't aborted by a single failed write.
async fn seal_milestone_on_disk(
    swarm_store: Option<&Arc<SwarmStore>>,
    swarm_id: &str,
    milestone_id: &str,
) {
    let Some(store) = swarm_store else { return };
    let mut milestones = match store.read_milestones(swarm_id).await {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(
                swarm_id = %swarm_id,
                milestone_id = %milestone_id,
                error = %e,
                "failed to read milestones for sealing; skipping"
            );
            return;
        }
    };

    let mut changed = false;
    for m in milestones.iter_mut() {
        if m.id == milestone_id && !m.sealed {
            m.sealed = true;
            changed = true;
        }
    }

    if !changed {
        return;
    }

    if let Err(e) = store.write_milestones(swarm_id, &milestones).await {
        tracing::warn!(
            swarm_id = %swarm_id,
            milestone_id = %milestone_id,
            error = %e,
            "failed to persist sealed milestone"
        );
    }
}

/// Update `validation-state.json` with each assertion's pass/fail outcome.
async fn update_validation_state_on_disk(
    swarm_store: Option<&Arc<SwarmStore>>,
    swarm_id: &str,
    result: &ValidationResult,
    assertions: &[ValidationAssertion],
) {
    let Some(store) = swarm_store else { return };
    let mut state = store
        .read_validation_state(swarm_id)
        .await
        .unwrap_or_else(|_| ValidationState::default());

    // The Guard returns one `AssertionResult` per input assertion in the
    // same order. Zip them with our `assertions` slice to get a stable
    // mapping from VAL-* id -> pass/fail.
    for (a, r) in assertions.iter().zip(result.assertion_results.iter()) {
        let status = if r.passed {
            AssertionStatus::Passed
        } else {
            AssertionStatus::Failed
        };
        state.record(&a.id, status, r.error.clone());
    }

    if let Err(e) = store.write_validation_state(swarm_id, &state).await {
        tracing::warn!(
            swarm_id = %swarm_id,
            error = %e,
            "failed to persist validation state"
        );
    }
}

/// Update a feature's status in the shared feature list and persist the
/// resulting feature snapshot to disk when a `SwarmStore` is available.
///
/// Persisting on every transition is what lets the frontend reflect
/// Scouting / Implementing / Validating in real time instead of only
/// seeing terminal statuses after a feature task returns.
async fn update_feature_status(
    features: &Arc<RwLock<Vec<Feature>>>,
    swarm_store: Option<&Arc<SwarmStore>>,
    swarm_id: &str,
    feature_id: &str,
    status: FeatureStatus,
) {
    let snapshot = {
        let mut feats = features.write().await;
        if let Some(feat) = feats.iter_mut().find(|f| f.id == feature_id) {
            feat.status = status;
        }
        feats.clone()
    };
    if let Some(store) = swarm_store {
        if let Err(e) = store.write_features(swarm_id, &snapshot).await {
            tracing::warn!(
                swarm_id = %swarm_id,
                feature_id = %feature_id,
                error = %e,
                "failed to persist feature status transition"
            );
        }
    }
}

/// Emit a progress event over the broadcast channel.
fn emit_progress(
    event_tx: &broadcast::Sender<ProgressEvent>,
    swarm_id: &str,
    event_type: ProgressEventType,
    message: &str,
) {
    let event = ProgressEvent::new(event_type, swarm_id.to_string(), message.to_string());
    // Broadcast send only fails when there are zero receivers; that is not
    // an error worth propagating.
    let _ = event_tx.send(event);
}

/// Emit a feature-specific progress event over the broadcast channel.
fn emit_feature_progress(
    event_tx: &broadcast::Sender<ProgressEvent>,
    swarm_id: &str,
    feature_id: &str,
    event_type: ProgressEventType,
    message: &str,
) {
    let event = ProgressEvent::new(event_type, swarm_id.to_string(), message.to_string())
        .with_feature(feature_id.to_string());
    let _ = event_tx.send(event);
}

/// Audit 2.3: emit a feature-scoped event tagged with the per-attempt
/// `run_id` so retries on the same feature are distinguishable in the
/// JSONL progress log. Used for the in-flight feature lifecycle events
/// (`FeatureStarted`, `FeatureScouted`, `FeatureImplemented`,
/// `FeatureValidated`, `FeatureFailed`) inside `run_feature_full`.
fn emit_feature_progress_with_run(
    event_tx: &broadcast::Sender<ProgressEvent>,
    swarm_id: &str,
    feature_id: &str,
    run_id: &str,
    event_type: ProgressEventType,
    message: &str,
) {
    let event = ProgressEvent::new(event_type, swarm_id.to_string(), message.to_string())
        .with_feature(feature_id.to_string())
        .with_run_id(run_id.to_string());
    let _ = event_tx.send(event);
}

/// Inspect a swarm error and, if it matches a Nurse-recoverable failure mode
/// (handoff parse failure, Pi RPC timeout, Pi subprocess crash), invoke the
/// Nurse LLM (fire-and-forget) and *also* emit the existing synthesized
/// `ProgressEvent::NurseIntervention` so the Tasks view and SwarmControl
/// screens have something to render even if the LLM hasn't responded yet.
///
/// The original error is still propagated upstream — Nurse here is
/// announcement-only; the existing feature-failed / fix-feature retry paths
/// continue to drive recovery.
///
/// `nurse_engine` is optional so test call sites can still construct the
/// helper without the full AppState. When it's `None`, a tracing warn fires
/// and no intervention is dispatched.
///
/// `event_tx` / `pi_manager` / `app_handle` are kept in the signature so
/// existing callers don't have to be touched; they're unused after the v2
/// cutover.
fn synthesize_nurse_for_error(
    _event_tx: &broadcast::Sender<ProgressEvent>,
    err: &anyhow::Error,
    swarm_id: &str,
    feature: &Feature,
    session_id: &str,
    nurse_engine: Option<&Arc<crate::nurse::engine::NurseEngine>>,
    _pi_manager: Option<&Arc<PiManager>>,
    _app_handle: Option<&tauri::AppHandle>,
) {
    use crate::nurse::synthesized::{InterventionOwner, SynthesizedKind};

    let kind = if let Some(parse_err) = err.downcast_ref::<HandoffParseFailed>() {
        tracing::warn!(
            feature_id = %feature.id,
            reason = %parse_err.reason,
            "worker handoff parse failed — synthesizing nurse intervention"
        );
        Some(SynthesizedKind::HandoffParseFailure {
            feature_name: feature.name.clone(),
        })
    } else if let Some(rpc_err) = err.downcast_ref::<PiRpcError>() {
        match rpc_err {
            PiRpcError::Timeout => Some(SynthesizedKind::RpcTimeout { idle_secs: 0 }),
            PiRpcError::ProcessCrashed { exit_code, stderr } => {
                Some(SynthesizedKind::ProcessCrashed {
                    exit_code: *exit_code,
                    stderr: stderr.clone(),
                })
            }
            PiRpcError::StdinClosed => Some(SynthesizedKind::ProcessCrashed {
                exit_code: None,
                stderr: "stdin closed".to_string(),
            }),
            PiRpcError::ProcessUnavailable { stderr } => Some(SynthesizedKind::ProcessCrashed {
                exit_code: None,
                stderr: stderr.clone(),
            }),
            _ => None,
        }
    } else {
        None
    };

    if let Some(kind) = kind {
        let owner = InterventionOwner {
            session_id: Some(session_id.to_string()),
            swarm_id: Some(swarm_id.to_string()),
            feature_id: Some(feature.id.clone()),
            ..InterventionOwner::default()
        };

        if let Some(engine) = nurse_engine {
            engine.report_error(kind, session_id.to_string(), owner);
        } else {
            tracing::warn!(
                feature_id = %feature.id,
                "synthesize_nurse_for_error called without nurse_engine; intervention dropped"
            );
        }
    }
}

/// Variant of `emit_feature_progress` that also attaches a JSON metadata blob.
fn emit_feature_progress_with_meta(
    event_tx: &broadcast::Sender<ProgressEvent>,
    swarm_id: &str,
    feature_id: &str,
    event_type: ProgressEventType,
    message: &str,
    metadata: Option<serde_json::Value>,
) {
    let mut event = ProgressEvent::new(event_type, swarm_id.to_string(), message.to_string())
        .with_feature(feature_id.to_string());
    if let Some(m) = metadata {
        event = event.with_metadata(m);
    }
    let _ = event_tx.send(event);
}

// ---------------------------------------------------------------------------
// Audit 2.3: emit helpers for the new progress event variants.
// ---------------------------------------------------------------------------

/// Emit a `PiSessionSpawned` progress event with the standard metadata
/// payload from [`crate::state::progress::pi_session_spawned_metadata`].
/// `pid` is best-effort — test-only Pi clients can report `None`.
fn emit_pi_session_spawned(
    event_tx: &broadcast::Sender<ProgressEvent>,
    swarm_id: &str,
    feature_id: Option<&str>,
    run_id: Option<&str>,
    session_id: &str,
    role: &str,
    pid: Option<u32>,
) {
    let mut event = ProgressEvent::new(
        ProgressEventType::PiSessionSpawned,
        swarm_id.to_string(),
        format!("spawned pi session {} ({})", session_id, role),
    )
    .with_metadata(crate::state::progress::pi_session_spawned_metadata(
        session_id,
        role,
        feature_id,
        pid.unwrap_or(0),
    ));
    if let Some(fid) = feature_id {
        event = event.with_feature(fid.to_string());
    }
    if let Some(rid) = run_id {
        event = event.with_run_id(rid.to_string());
    }
    let _ = event_tx.send(event);
}

/// Emit a `PiSessionKilled` progress event with the standard metadata
/// payload from [`crate::state::progress::pi_session_killed_metadata`].
fn emit_pi_session_killed(
    event_tx: &broadcast::Sender<ProgressEvent>,
    swarm_id: &str,
    feature_id: Option<&str>,
    run_id: Option<&str>,
    session_id: &str,
    reason: &str,
) {
    let mut event = ProgressEvent::new(
        ProgressEventType::PiSessionKilled,
        swarm_id.to_string(),
        format!("killed pi session {} ({})", session_id, reason),
    )
    .with_metadata(crate::state::progress::pi_session_killed_metadata(
        session_id, reason,
    ));
    if let Some(fid) = feature_id {
        event = event.with_feature(fid.to_string());
    }
    if let Some(rid) = run_id {
        event = event.with_run_id(rid.to_string());
    }
    let _ = event_tx.send(event);
}

/// Emit a `WorkerHandoff` progress event after parsing a Worker's handoff
/// JSON. Records the per-attempt run_id and the success_state so replay
/// can fold the Worker contract back onto the feature.
fn emit_worker_handoff(
    event_tx: &broadcast::Sender<ProgressEvent>,
    swarm_id: &str,
    feature_id: &str,
    run_id: &str,
    success_state: &str,
) {
    let event = ProgressEvent::new(
        ProgressEventType::WorkerHandoff,
        swarm_id.to_string(),
        format!("worker handoff: {} ({})", feature_id, success_state),
    )
    .with_feature(feature_id.to_string())
    .with_run_id(run_id.to_string())
    .with_metadata(crate::state::progress::worker_handoff_metadata(
        feature_id,
        run_id,
        success_state,
    ));
    let _ = event_tx.send(event);
}

/// Emit a `GuardAttempt` progress event at the top of each Guard run.
/// `attempt` is keyed off `Feature::fix_attempt_count` so retries are
/// distinguishable in the log.
fn emit_guard_attempt(
    event_tx: &broadcast::Sender<ProgressEvent>,
    swarm_id: &str,
    feature_id: &str,
    run_id: Option<&str>,
    attempt: u32,
    milestone_id: &str,
) {
    let mut event = ProgressEvent::new(
        ProgressEventType::GuardAttempt,
        swarm_id.to_string(),
        format!(
            "guard attempt {} for {} (milestone {})",
            attempt, feature_id, milestone_id
        ),
    )
    .with_feature(feature_id.to_string())
    .with_metadata(crate::state::progress::guard_attempt_metadata(
        feature_id,
        attempt,
        milestone_id,
    ));
    if let Some(rid) = run_id {
        event = event.with_run_id(rid.to_string());
    }
    let _ = event_tx.send(event);
}

/// Snapshot a Pi session's cumulative token usage and persist it to the
/// usage log so the per-swarm cost/token aggregator picks it up.
///
/// `source_id` is `{swarm_id}:{feature_id}:{role}` so the aggregator can
/// match by exact swarm_id or by `{swarm_id}:%` LIKE prefix. Failures are
/// downgraded to a `warn` log — usage tracking must never block execution.
///
/// When an `accumulator` is provided, the session's final stats are added
/// to the in-memory accumulator before the DB write, then subtracted after
/// the write completes. This ensures the accumulator tracks only stats not
/// yet persisted to the DB (preventing double-counting in `get_swarm_usage`
/// which merges DB total + accumulator snapshot).
async fn record_session_usage(
    usage_store: Option<&UsageStore>,
    session: &Arc<PiSession>,
    swarm_id: &str,
    feature_id: &str,
    role: &str,
    model_id: &str,
    duration_ms: i64,
    accumulator: Option<&SwarmUsageAccumulator>,
) {
    let Some(store) = usage_store else {
        // Even without a usage store, feed stats into the accumulator
        // so live totals are visible during execution.
        if let Some(acc) = accumulator {
            if let Ok(stats) = session.get_session_stats().await {
                acc.add(
                    stats.input as i64,
                    stats.output as i64,
                    stats.cache_read as i64,
                    stats.cache_write as i64,
                    stats.cost,
                    duration_ms,
                );
            }
        }
        return;
    };

    let stats = match session.get_session_stats().await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(
                swarm_id = %swarm_id,
                feature_id = %feature_id,
                role = %role,
                error = %e,
                "failed to read session stats — swarm usage will be undercounted"
            );
            return;
        }
    };

    // Add session stats to the live accumulator BEFORE the DB write
    // (so the accumulator briefly has the data before it's persisted).
    if let Some(acc) = accumulator {
        acc.add(
            stats.input as i64,
            stats.output as i64,
            stats.cache_read as i64,
            stats.cache_write as i64,
            stats.cost,
            duration_ms,
        );
    }

    // Match the chat.rs convention: if the model id contains a provider
    // prefix, split it; otherwise default to anthropic.
    let (provider, model_id) = if let Some((p, m)) = model_id.split_once('/') {
        (p.to_string(), m.to_string())
    } else {
        ("anthropic".to_string(), model_id.to_string())
    };

    let entry = UsageEntry {
        source: "swarm".to_string(),
        source_id: Some(format!("{}:{}:{}", swarm_id, feature_id, role)),
        model_id,
        provider,
        input_tokens: stats.input as i64,
        output_tokens: stats.output as i64,
        cache_read_tokens: stats.cache_read as i64,
        cache_write_tokens: stats.cache_write as i64,
        cost: stats.cost,
        duration_ms,
    };
    if let Err(e) = store.record_usage(entry).await {
        tracing::warn!(
            swarm_id = %swarm_id,
            feature_id = %feature_id,
            role = %role,
            error = %e,
            "failed to record swarm usage"
        );
    }

    // Subtract from accumulator AFTER the DB write to prevent
    // double-counting: what is now in the DB should not remain in
    // the in-memory accumulator.
    if let Some(acc) = accumulator {
        acc.subtract(
            stats.input as i64,
            stats.output as i64,
            stats.cache_read as i64,
            stats.cache_write as i64,
            stats.cost,
            duration_ms,
        );
    }
}

/// Generate fix features for failed guard assertions.
///
/// Each failed assertion becomes a new feature that depends on the original
/// feature and attempts to fix the specific failure.
///
/// When the parent is a validator feature (`validate-*`), the synthesized
/// fix ids deliberately **do not** start with `validate-`, so the queen
/// routes them through the normal Scout → Worker pipeline instead of
/// straight back into Guard. The retry counter is embedded in the id to
/// avoid collisions across successive failed validator runs.
pub fn create_fix_features(guard_result: &ValidationResult, feature: &Feature) -> Vec<Feature> {
    let retry = feature.fix_attempt_count + 1;
    let id_stem: String = if feature.is_validator() {
        let suffix = feature.id.strip_prefix("validate-").unwrap_or(&feature.id);
        format!("fix-{}-r{}", suffix, retry)
    } else {
        format!("{}-fix", feature.id)
    };

    guard_result
        .failures()
        .iter()
        .enumerate()
        .map(|(i, failure)| {
            let fix_id = format!("{}-{}", id_stem, i + 1);
            let fix_description = format!(
                "Fix failed assertion for feature '{}': {}\nError: {}",
                feature.name,
                failure.assertion,
                failure.error.as_deref().unwrap_or("unknown error"),
            );

            // Fix features fulfil only the failed assertion's VAL-ID when
            // available (Phase 2). Legacy guard runs without an
            // `assertion_id` get an empty fulfills list — they still
            // depend on the original feature for retry sequencing.
            let fulfills = failure
                .assertion_id
                .as_ref()
                .map(|id| vec![id.clone()])
                .unwrap_or_default();

            Feature {
                id: fix_id.clone(),
                name: format!("Fix: {} (assertion {})", feature.name, i + 1),
                description: fix_description,
                status: FeatureStatus::Pending,
                dependencies: vec![feature.id.clone()],
                milestone: feature.milestone.clone(),
                fix_attempt_count: feature.fix_attempt_count + 1,
                max_fix_attempts: feature.max_fix_attempts,
                fulfills,
                interrupted: false,
                resumable: false,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::guard::AssertionResult;

    #[test]
    fn test_queen_config_default() {
        let config = QueenConfig::default();
        assert_eq!(config.max_concurrent_features, 1);
        assert_eq!(config.max_fix_attempts, 3);
    }

    #[test]
    fn test_create_fix_features_empty_on_pass() {
        let result = ValidationResult {
            passed: true,
            assertion_results: vec![AssertionResult {
                assertion: "test".to_string(),
                passed: true,
                output: "ok".to_string(),
                error: None,
                assertion_id: None,
            }],
            feature_id: "f1".to_string(),
        };

        let feature = Feature::new(
            "f1".to_string(),
            "Feature 1".to_string(),
            "desc".to_string(),
        );
        let fixes = create_fix_features(&result, &feature);
        assert!(fixes.is_empty());
    }

    #[test]
    fn test_create_fix_features_for_failures() {
        let result = ValidationResult {
            passed: false,
            assertion_results: vec![
                AssertionResult {
                    assertion: "app compiles".to_string(),
                    passed: true,
                    output: "ok".to_string(),
                    error: None,
                    assertion_id: None,
                },
                AssertionResult {
                    assertion: "login works".to_string(),
                    passed: false,
                    output: "not found".to_string(),
                    error: Some("LoginForm component missing".to_string()),
                    assertion_id: None,
                },
                AssertionResult {
                    assertion: "validation works".to_string(),
                    passed: false,
                    output: "error".to_string(),
                    error: Some("validation schema undefined".to_string()),
                    assertion_id: None,
                },
            ],
            feature_id: "f1".to_string(),
        };

        let feature = Feature::new(
            "f1".to_string(),
            "Login".to_string(),
            "login page".to_string(),
        );
        let fixes = create_fix_features(&result, &feature);

        assert_eq!(fixes.len(), 2);
        assert_eq!(fixes[0].id, "f1-fix-1");
        assert_eq!(fixes[1].id, "f1-fix-2");
        assert!(fixes[0].dependencies.contains(&"f1".to_string()));
        assert!(fixes[0].description.contains("LoginForm component missing"));
        assert_eq!(fixes[0].status, FeatureStatus::Pending);
    }

    #[test]
    fn test_fix_features_inherit_milestone() {
        let result = ValidationResult {
            passed: false,
            assertion_results: vec![AssertionResult {
                assertion: "test".to_string(),
                passed: false,
                output: "fail".to_string(),
                error: Some("err".to_string()),
                assertion_id: None,
            }],
            feature_id: "f1".to_string(),
        };

        let mut feature = Feature::new(
            "f1".to_string(),
            "Feature 1".to_string(),
            "desc".to_string(),
        );
        feature.milestone = Some("m1".to_string());
        feature.fix_attempt_count = 1;

        let fixes = create_fix_features(&result, &feature);
        assert_eq!(fixes.len(), 1);
        assert_eq!(fixes[0].milestone, Some("m1".to_string()));
        assert_eq!(fixes[0].fix_attempt_count, 2);
    }

    #[test]
    fn test_create_fix_features_carries_val_id_into_fulfills() {
        // Phase 2: when a Guard result carries an `assertion_id`, the
        // generated fix feature's `fulfills` should reference that VAL-* id.
        let result = ValidationResult {
            passed: false,
            assertion_results: vec![AssertionResult {
                assertion: "cargo test passes".to_string(),
                passed: false,
                output: "out".to_string(),
                error: Some("2 tests failed".to_string()),
                assertion_id: Some("VAL-FND-007".to_string()),
            }],
            feature_id: "validate-m1".to_string(),
        };
        let mut feature = Feature::new(
            "validate-m1".to_string(),
            "Validate M1".to_string(),
            "desc".to_string(),
        );
        feature.milestone = Some("m1".to_string());
        feature.fulfills = vec!["VAL-FND-007".to_string()];

        let fixes = create_fix_features(&result, &feature);
        assert_eq!(fixes.len(), 1);
        assert_eq!(fixes[0].fulfills, vec!["VAL-FND-007".to_string()]);
    }

    #[test]
    fn test_validator_fix_features_route_through_scout_worker() {
        // When Guard fails on a validator feature, the synthesized fix-feature
        // ids must NOT start with `validate-`. Otherwise the queen treats them
        // as validators (skipping Scout/Worker) and re-runs Guard against an
        // unchanged working dir, producing an infinite "fix" loop until
        // max_fix_attempts trips.
        let result = ValidationResult {
            passed: false,
            assertion_results: vec![
                AssertionResult {
                    assertion: "librespot dep declared".to_string(),
                    passed: false,
                    output: "".to_string(),
                    error: Some("librespot is commented out".to_string()),
                    assertion_id: Some("VAL-MFM-006".to_string()),
                },
                AssertionResult {
                    assertion: "cargo check passes".to_string(),
                    passed: false,
                    output: "".to_string(),
                    error: Some("E0432: unresolved import".to_string()),
                    assertion_id: Some("VAL-MFM-001".to_string()),
                },
            ],
            feature_id: "validate-m1-foundations".to_string(),
        };
        let mut feature = Feature::new(
            "validate-m1-foundations".to_string(),
            "Validate Project skeleton".to_string(),
            "desc".to_string(),
        );
        feature.milestone = Some("m1-foundations".to_string());
        assert!(feature.is_validator());

        let fixes = create_fix_features(&result, &feature);
        assert_eq!(fixes.len(), 2);
        for fix in &fixes {
            assert!(
                !fix.is_validator(),
                "fix-feature {} must not be a validator",
                fix.id
            );
            assert!(fix.id.starts_with("fix-m1-foundations-r1-"));
            assert_eq!(
                fix.dependencies,
                vec!["validate-m1-foundations".to_string()]
            );
            assert_eq!(fix.milestone, Some("m1-foundations".to_string()));
        }
        assert_eq!(fixes[0].id, "fix-m1-foundations-r1-1");
        assert_eq!(fixes[1].id, "fix-m1-foundations-r1-2");
    }

    #[test]
    fn test_validator_fix_features_retry_counter_avoids_collisions() {
        // On the second validator-Guard failure, the parent validator's
        // fix_attempt_count is 1, so the new fix-features must embed `r2`
        // in their ids to avoid colliding with the r1 batch already in the
        // scheduler.
        let result = ValidationResult {
            passed: false,
            assertion_results: vec![AssertionResult {
                assertion: "cargo check passes".to_string(),
                passed: false,
                output: "".to_string(),
                error: Some("still broken".to_string()),
                assertion_id: Some("VAL-MFM-001".to_string()),
            }],
            feature_id: "validate-m1-foundations".to_string(),
        };
        let mut feature = Feature::new(
            "validate-m1-foundations".to_string(),
            "Validate Project skeleton".to_string(),
            "desc".to_string(),
        );
        feature.milestone = Some("m1-foundations".to_string());
        feature.fix_attempt_count = 1;

        let fixes = create_fix_features(&result, &feature);
        assert_eq!(fixes.len(), 1);
        assert_eq!(fixes[0].id, "fix-m1-foundations-r2-1");
        assert!(!fixes[0].is_validator());
    }

    // ----------------------------------------------------------------
    // inject_milestone_validators
    // ----------------------------------------------------------------

    fn impl_feature(id: &str, milestone: &str, deps: Vec<&str>) -> Feature {
        let mut f = Feature::new(
            id.to_string(),
            id.to_string(),
            format!("{} description", id),
        );
        f.milestone = Some(milestone.to_string());
        f.dependencies = deps.into_iter().map(String::from).collect();
        f
    }

    fn milestone_with(
        id: &str,
        name: &str,
        features: Vec<&str>,
        assertions: Vec<&str>,
    ) -> Milestone {
        Milestone {
            id: id.to_string(),
            name: name.to_string(),
            features: features.into_iter().map(String::from).collect(),
            assertions: assertions.into_iter().map(String::from).collect(),
            sealed: false,
        }
    }

    #[test]
    fn test_inject_validators_appends_one_per_milestone() {
        let mut features = vec![
            impl_feature("f1", "m1", vec![]),
            impl_feature("f2", "m1", vec!["f1"]),
            impl_feature("f3", "m2", vec![]),
        ];
        let milestones = vec![
            milestone_with("m1", "M1", vec!["f1", "f2"], vec!["assert-1"]),
            milestone_with("m2", "M2", vec!["f3"], vec!["assert-2"]),
        ];
        let assertions = crate::core::validation::assign_assertion_ids(&milestones);

        inject_milestone_validators(&mut features, &milestones, &assertions);

        // Two validator features should have been appended.
        let validators: Vec<&Feature> = features.iter().filter(|f| f.is_validator()).collect();
        assert_eq!(validators.len(), 2);

        let v_m1 = features
            .iter()
            .find(|f| f.id == "validate-m1")
            .expect("validate-m1 present");
        assert_eq!(v_m1.name, "Validate M1");
        assert_eq!(v_m1.milestone.as_deref(), Some("m1"));
        // Validator depends on every impl feature in its milestone
        assert!(v_m1.dependencies.contains(&"f1".to_string()));
        assert!(v_m1.dependencies.contains(&"f2".to_string()));
        // Validator fulfills the milestone's VAL-* assertions
        assert_eq!(v_m1.fulfills.len(), 1);
        assert!(v_m1.fulfills[0].starts_with("VAL-"));

        let v_m2 = features
            .iter()
            .find(|f| f.id == "validate-m2")
            .expect("validate-m2 present");
        assert_eq!(v_m2.dependencies, vec!["f3"]);
    }

    #[test]
    fn test_inject_validators_sequences_milestones_via_dep_graph() {
        // First impl feature of M2 must depend on validate-m1 so the
        // scheduler refuses to run anything in M2 until M1 is validated.
        let mut features = vec![
            impl_feature("f1", "m1", vec![]),
            impl_feature("f2", "m1", vec!["f1"]),
            impl_feature("f3", "m2", vec![]),
            impl_feature("f4", "m2", vec!["f3"]),
        ];
        let milestones = vec![
            milestone_with("m1", "M1", vec!["f1", "f2"], vec!["a"]),
            milestone_with("m2", "M2", vec!["f3", "f4"], vec!["b"]),
        ];
        let assertions = crate::core::validation::assign_assertion_ids(&milestones);
        inject_milestone_validators(&mut features, &milestones, &assertions);

        // f3 (first impl feature of M2) must now depend on validate-m1.
        let f3 = features.iter().find(|f| f.id == "f3").expect("f3 present");
        assert!(
            f3.dependencies.contains(&"validate-m1".to_string()),
            "f3 should depend on validate-m1; got {:?}",
            f3.dependencies
        );
        // f4 is NOT the first impl feature of M2; it keeps its original deps.
        let f4 = features.iter().find(|f| f.id == "f4").expect("f4 present");
        assert!(!f4.dependencies.contains(&"validate-m1".to_string()));
    }

    #[test]
    fn test_inject_validators_skips_milestone_with_no_assertions() {
        // A milestone with zero assertions has no contract worth verifying;
        // we don't inject a validator (and don't bridge to the next).
        let mut features = vec![
            impl_feature("f1", "m1", vec![]),
            impl_feature("f2", "m2", vec![]),
        ];
        let milestones = vec![
            milestone_with("m1", "M1", vec!["f1"], vec![]),
            milestone_with("m2", "M2", vec!["f2"], vec!["a"]),
        ];
        let assertions = crate::core::validation::assign_assertion_ids(&milestones);
        inject_milestone_validators(&mut features, &milestones, &assertions);

        // Only one validator: validate-m2
        let validators: Vec<&Feature> = features.iter().filter(|f| f.is_validator()).collect();
        assert_eq!(validators.len(), 1);
        assert_eq!(validators[0].id, "validate-m2");
    }

    #[test]
    fn test_inject_validators_idempotent() {
        let mut features = vec![impl_feature("f1", "m1", vec![])];
        let milestones = vec![milestone_with("m1", "M1", vec!["f1"], vec!["a"])];
        let assertions = crate::core::validation::assign_assertion_ids(&milestones);

        inject_milestone_validators(&mut features, &milestones, &assertions);
        let after_first = features.len();

        // Run again — should not duplicate the validator.
        inject_milestone_validators(&mut features, &milestones, &assertions);
        assert_eq!(features.len(), after_first);
    }

    #[test]
    fn test_inject_validators_empty_milestones_is_noop() {
        let mut features = vec![Feature::new("f1".into(), "f1".into(), "".into())];
        let len_before = features.len();
        inject_milestone_validators(&mut features, &[], &[]);
        assert_eq!(features.len(), len_before);
    }

    // -- Audit 7.2: Queen E2E against MockRpcClient ------------------------
    //
    // These tests exercise the Scout → Worker path through a real
    // `PiManager` whose transport factory is rewired to return mock
    // transports. They don't drive the full `run_swarm_full` loop (which
    // needs SwarmStore / UsageStore / activity channel / nurse — out of
    // scope for 7.2) but they do prove that:
    //
    // 1. `PiManager::with_transport_factory` plumbs the mock down through
    //    `spawn_session_with_options` to a real `PiSession`.
    // 2. The Pi-using agent functions (`run_scout`, `run_worker`) operate
    //    correctly against that session.
    // 3. Failure paths (transport crash, malformed handoff) propagate
    //    through to the agent's error chain.

    use crate::pi::manager::{TransportFactory, TransportSpawnRequest};
    use crate::pi::mock::MockRpcClient;
    use crate::pi::transport::PiTransport;

    /// Build a `PiManager` whose transport factory returns mocks scripted
    /// per session id. The `script` closure runs once per spawn and may
    /// emit events / fail synchronously.
    fn manager_with_scripted_mocks<F>(script: F) -> Arc<PiManager>
    where
        F: Fn(&str, &Arc<MockRpcClient>) + Send + Sync + 'static,
    {
        let script: Arc<dyn Fn(&str, &Arc<MockRpcClient>) + Send + Sync> = Arc::new(script);
        let factory: TransportFactory = Arc::new(move |req: TransportSpawnRequest<'_>| {
            let script = script.clone();
            // The session id is derivable from the spawn request only via
            // the system prompt / model id — neither is unique. We use
            // the model id as a tag so the script can decide what to emit.
            let model = req.options.model.clone();
            // The OwnedSemaphorePermit must be kept alive for the lifetime
            // of the transport (audit 2.9). MockRpcClient doesn't hold it
            // natively, so we leak it via a closure-captured field. In
            // tests this is fine — the manager outlives the permit pool.
            let _permit = req.process_permit;
            Box::pin(async move {
                let mock = MockRpcClient::new();
                script(&model, &mock);
                let transport: Arc<dyn PiTransport> = mock;
                // Hold the permit alive until shutdown by leaking into a
                // tokio task that waits forever on the transport's
                // liveness — pragmatic for a test. We just drop it; the
                // semaphore pool has 30 slots which is more than enough.
                drop(_permit);
                Ok::<Arc<dyn PiTransport>, PiRpcError>(transport)
            })
        });
        Arc::new(PiManager::new_with_mock_factory(factory))
    }

    /// Success path: queue Scout's response on the mock, then drive the
    /// scout function through a manager-spawned session. The PiManager
    /// must hand back a session whose underlying transport is the mock.
    #[tokio::test]
    async fn queen_scout_phase_e2e_against_mock_pi_manager() {
        // Pre-seed events keyed by the model identifier so multiple
        // spawns in the same test can each get their own script.
        let scripted: Arc<std::sync::Mutex<HashMap<String, Vec<PiEvent>>>> =
            Arc::new(std::sync::Mutex::new(HashMap::new()));
        scripted.lock().unwrap().insert(
            "scout-model".to_string(),
            vec![
                PiEvent::TextDelta("Analyzing the feature...\n".into()),
                PiEvent::ToolExecutionStart {
                    tool_call_id: "tc-1".to_string(),
                    name: "submit_scout_result".to_string(),
                    args: serde_json::json!({
                        "plan": "Do X",
                        "estimated_complexity": "medium",
                        "risks": [],
                    }),
                },
                PiEvent::AgentEnd,
            ],
        );
        let scripted_for_factory = scripted.clone();

        let manager = manager_with_scripted_mocks(move |model_id, mock| {
            if let Some(events) = scripted_for_factory.lock().unwrap().get(model_id).cloned() {
                for e in events {
                    mock.emit(e);
                }
            }
        });

        let session = manager
            .spawn_session_with_options(
                "scout-feat-1",
                &crate::pi::rpc::PiSessionOptions::for_scout("scout-model", "sys"),
                std::path::Path::new("/tmp"),
            )
            .await
            .expect("manager should spawn mock session");

        let feature = Feature::new("feat-1".into(), "F1".into(), "desc".into());
        let result =
            crate::core::scout::run_scout(&session, &feature, std::path::Path::new("/tmp"), "")
                .await
                .expect("scout should succeed");
        assert_eq!(result.feature_id, "feat-1");
        assert_eq!(result.estimated_complexity, "medium");
    }

    /// Failure path: the mock transport crashes before sending any text.
    /// The scout function surfaces a `PiRpcError::ProcessCrashed`-flavoured
    /// error chain — the Queen loop downcasts on this to decide whether
    /// to retry / give up.
    #[tokio::test]
    async fn queen_scout_phase_e2e_handles_transport_crash() {
        let manager = manager_with_scripted_mocks(|_model, mock| {
            mock.crash("boom: provider 500");
        });

        let session = manager
            .spawn_session_with_options(
                "scout-crash",
                &crate::pi::rpc::PiSessionOptions::for_scout("crash-model", "sys"),
                std::path::Path::new("/tmp"),
            )
            .await
            .expect("manager should spawn mock session");

        let feature = Feature::new("feat-1".into(), "F1".into(), "desc".into());
        let err =
            crate::core::scout::run_scout(&session, &feature, std::path::Path::new("/tmp"), "")
                .await
                .expect_err("scout should propagate the transport crash");
        let msg = format!("{:#}", err);
        assert!(msg.contains("scout collect_response failed"), "{msg}");
        assert!(msg.contains("boom: provider 500"), "{msg}");
    }

    #[test]
    fn test_inject_validators_produces_schedulable_graph() {
        // The combined feature list (impl + validators) must remain
        // acyclic so the scheduler accepts it without an error.
        let mut features = vec![
            impl_feature("f1", "m1", vec![]),
            impl_feature("f2", "m1", vec!["f1"]),
            impl_feature("f3", "m2", vec![]),
        ];
        let milestones = vec![
            milestone_with("m1", "M1", vec!["f1", "f2"], vec!["a"]),
            milestone_with("m2", "M2", vec!["f3"], vec!["b"]),
        ];
        let assertions = crate::core::validation::assign_assertion_ids(&milestones);
        inject_milestone_validators(&mut features, &milestones, &assertions);

        crate::core::scheduler::Scheduler::new(features)
            .expect("scheduler must accept injected graph");
    }
}
