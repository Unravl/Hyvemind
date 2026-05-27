//! Nurse IPC surface — status snapshot, configuration, and per-session
//! evaluation commands for the self-healing watchdog system.
//!
//! The Nurse is the heartbeat/recovery agent (§4 of PRODUCT.md). This module
//! exposes its live status and configuration to the frontend, plus the
//! on-demand `check_chat_session` endpoint that the frontend watchdog timer
//! calls at the configured interval.
//!
//! All commands return `Result<T, String>` — errors are surfaced as
//! user-visible strings in the frontend.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tauri::{Emitter, State};

use crate::commands::util::validate_session_id;
use crate::nurse::config::{clamp_stall_threshold, NurseProfile};
use crate::nurse::health::{Severity, Signal, Tier};
use crate::nurse::observability::decision_log::DecisionLogRow;
use crate::nurse::observability::signal_stream::SignalStreamRow;
use crate::nurse::snapshot::{
    NurseActionKind, NurseDecision, NurseDispatchTier, NurseEvent as LegacyNurseEvent, NurseEvent,
    NurseInterventionRecord, NurseLifecyclePayload, NurseLifecycleStatus, NurseSessionAction,
    NurseStatusSnapshot, SessionOwnerDto, TunableDef,
};
use crate::state::app_state::AppState;
use crate::state::ipc_error::IpcError;

/// Live snapshot of the Nurse's current status.
///
/// Returns a [`NurseStatusSnapshot`] carrying `enabled`, `is_running`,
/// `stall_threshold_secs`, `nurse_model`, `tick_interval_secs`, active
/// session counts, and recent event history.
///
/// The frontend polls this on mount and after every `nurse-event`
/// emission to keep the Settings panel and dropdown in sync.
///
/// Returns `Ok(NurseStatusSnapshot)`. This command never fails — the
/// snapshot is built entirely from in-memory atomics.
#[tauri::command]
pub async fn get_nurse_status(state: State<'_, AppState>) -> Result<NurseStatusSnapshot, IpcError> {
    if let Some(engine) = state.nurse_engine() {
        Ok(engine.snapshot_status())
    } else {
        // Engine not yet attached — return a default snapshot rather than
        // an error so the frontend's early poll doesn't surface a
        // confusing error during startup.
        Ok(NurseStatusSnapshot::default())
    }
}

/// Update the Nurse's runtime configuration and persist it to disk.
///
/// All parameters are `Option` — only the supplied values are applied;
/// omitted keys keep their current setting.
///
/// # Parameters
///
/// * `enabled` — turn the Nurse on or off. When flipped to `false` the
///   watchdog loop stops; when flipped to `true` it starts or resumes.
/// * `stall_threshold_secs` — seconds of Pi inactivity before the Nurse
///   considers a session stalled. Clamped via
///   [`clamp_stall_threshold`] so the frontend can pass an arbitrary
///   slider value without breaking invariants.
/// * `nurse_model` — model ID for LLM-driven Nurse evaluation. Pass
///   `"none"` or `""` to clear the override (falls back to the
///   deterministic rules engine).
/// * `allow_destructive` — **deprecated**. Accepted for backward
///   compatibility with stale frontends; the value is logged and
///   ignored. Nurse is always autonomous in the batched architecture.
/// * `tick_interval_secs` — how often the Nurse watchdog loop wakes
///   to scan all sessions for stalls.
/// * `nurse_provider` — explicit provider override for Nurse LLM
///   calls. Pass `""` to clear (falls back to the default model's
///   provider).
///
/// # Side effects
///
/// After updating in-memory state and writing the config to disk, this
/// command emits a `"nurse-event"` Tauri event with a full
/// [`NurseStatusSnapshot`] so all frontend listeners (Settings panel,
/// dropdown) reflect the change immediately.
///
/// # Errors
///
/// Returns `Err` only when the disk config file cannot be written
/// (disk full, permissions, etc.).
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn set_nurse_config(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
    enabled: Option<bool>,
    stall_threshold_secs: Option<u64>,
    nurse_model: Option<String>,
    // Deprecated — Nurse is always autonomous in the batched architecture.
    // We still accept this from stale frontends to avoid IPC deserialization
    // failures; the value is logged and ignored.
    allow_destructive: Option<bool>,
    tick_interval_secs: Option<u64>,
    nurse_provider: Option<String>,
    nurse_batch_interval_secs: Option<u64>,
    swarms_only: Option<bool>,
) -> Result<(), IpcError> {
    let engine = state
        .nurse_engine()
        .cloned()
        .ok_or_else(|| IpcError::internal("nurse engine not attached"))?;

    // Snapshot the current runtime config first so we can apply edits to
    // a local copy and then publish it once at the end. Apply per-Option
    // edits both to the runtime config (for hot-reload) and the disk
    // config (for persistence).
    let mut runtime_cfg = engine.config.read().await.clone();

    let (data_dir, bytes) = {
        let mut disk_config = state.config.write().await;

        if let Some(e) = enabled {
            runtime_cfg.enabled = e;
            disk_config.nurse_enabled = e;
        }
        if let Some(t) = stall_threshold_secs {
            let clamped = clamp_stall_threshold(t);
            disk_config.nurse_stall_threshold_secs = clamped;
            // The v2 engine carries stall thresholds per-profile-per-detector.
            // Apply to every profile's stall detector so the slider continues
            // to behave like a master "stall threshold" control.
            for cfg in runtime_cfg.profiles.values_mut() {
                cfg.stall.stalled_secs = clamped;
            }
        }
        if let Some(m) = nurse_model {
            if m == "none" || m.is_empty() {
                tracing::info!("nurse model cleared (set to none)");
                runtime_cfg.nurse_model = None;
                disk_config.nurse_model = None;
            } else {
                runtime_cfg.nurse_model = Some(m.clone());
                disk_config.nurse_model = Some(m);
            }
        }
        if let Some(val) = allow_destructive {
            tracing::debug!(
                value = val,
                "set_nurse_config: allow_destructive is deprecated and ignored"
            );
        }
        if let Some(t) = tick_interval_secs {
            // The v2 engine reads `HYVEMIND_NURSE_TICK_INTERVAL_SECS` at
            // startup; we persist the user-facing setting but it only
            // takes effect on the next launch (no runtime field today).
            disk_config.nurse_tick_interval_secs = t;
        }
        if let Some(p) = nurse_provider {
            // Treat empty string as "clear the explicit provider override".
            let opt = if p.trim().is_empty() { None } else { Some(p) };
            runtime_cfg.nurse_provider = opt.clone();
            disk_config.nurse_provider = opt;
        }
        if let Some(v) = swarms_only {
            runtime_cfg.swarms_only = v;
            disk_config.nurse_swarms_only = v;
        }
        if let Some(v) = nurse_batch_interval_secs {
            // 0 means "clear override and fall back to the env-var tunable".
            let opt = if v == 0 {
                None
            } else {
                Some(v.clamp(
                    crate::state::config::NURSE_BATCH_INTERVAL_MIN_SECS,
                    crate::state::config::NURSE_BATCH_INTERVAL_MAX_SECS,
                ))
            };
            runtime_cfg.nurse_batch_interval_secs = opt;
            disk_config.nurse_batch_interval_secs = opt;
        }

        let bytes = disk_config.snapshot_to_bytes().map_err(IpcError::from)?;
        (disk_config.data_dir.clone(), bytes)
    };

    crate::state::config::Config::write_bytes(data_dir, bytes)
        .await
        .map_err(IpcError::from)?;

    // Snapshot the booleans the synthesized path mirrors before moving
    // `runtime_cfg` into the publish step. Order: config write first,
    // then atomic update of the sync mirrors. The dispatcher (primary
    // path) always sees the latest config; the synthesized path
    // (secondary/fallback) may lag the window of these two sequential
    // lines — the direction of drift is safe.
    let enabled_after = runtime_cfg.enabled;
    let swarms_only_after = runtime_cfg.swarms_only;

    // Publish the runtime config so the next tick picks it up.
    *engine.config.write().await = runtime_cfg;

    // Update the sync-readable mirrors used by `report_synthesized` so
    // the master "Nurse off" / "swarms only" toggles silence the
    // synthesized path the moment they're committed.
    engine.set_master_enabled(enabled_after);
    engine.set_master_swarms_only(swarms_only_after);

    // Emit a full snapshot so all frontends (dropdown + settings) reflect
    // the change immediately.
    let snapshot = engine.snapshot_status();
    if let Err(e) = app.emit("nurse-event", LegacyNurseEvent::StatusUpdate(snapshot)) {
        tracing::warn!(error = %e, "set_nurse_config: failed to emit StatusUpdate");
    }

    Ok(())
}

/// Frontend-facing tagged DTO for [`NurseDecision`].
///
/// Serialized with a `"kind"` discriminant (`"leave_it"`, `"steer"`,
/// `"restart"`, `"cancel"`, `"noop"`) for ergonomic TypeScript narrowing
/// via discriminated unions.
///
/// # Variants
///
/// * `LeaveIt` — the session is healthy; check back after
///   `check_back_secs` (clamped to `[1, 1800]`).
/// * `Steer` — the session is stuck or looping; the Nurse injects a
///   course-correction message. Steer decisions are applied server-side
///   before the DTO is returned, so the frontend only needs to rearm
///   its watchdog.
/// * `Restart` — the session is unrecoverable; the frontend should kill
///   and respawn it.
/// * `Cancel` — the session should be terminated (fatal error,
///   exhausted retries). The frontend tears it down and shows an error
///   banner.
/// * `Noop` — synthetic variant returned when the watchdog fires
///   against a `session_id` that no longer exists (the session ended
///   cleanly between the timer and the IPC call). The frontend silently
///   clears its timer.
#[derive(Debug, Clone, Serialize)]
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
    /// Watchdog fired after the session was already torn down. Frontend
    /// should clear its timer silently — no error UI, no stopChat call.
    Noop {
        reasoning: String,
    },
}

impl From<NurseDecision> for NurseDecisionDto {
    fn from(d: NurseDecision) -> Self {
        match d {
            NurseDecision::LeaveIt {
                reasoning,
                check_back_secs,
                ..
            } => NurseDecisionDto::LeaveIt {
                reasoning,
                check_back_secs: check_back_secs.clamp(1, 1800),
            },
            NurseDecision::Steer {
                reasoning, message, ..
            } => NurseDecisionDto::Steer { reasoning, message },
            NurseDecision::Restart { reasoning, .. } => NurseDecisionDto::Restart { reasoning },
            NurseDecision::Cancel {
                reasoning, message, ..
            } => NurseDecisionDto::Cancel { reasoning, message },
        }
    }
}

/// Force a one-shot Nurse evaluation of one specific Pi session.
///
/// Called by the frontend watchdog at the configured `chat_check_in_secs`
/// interval. Returns the decision (cancel / leave_it / steer / restart /
/// noop) — the frontend then acts on it (rearm watchdog / kill session /
/// show error / silently clear).
///
/// `Steer` decisions are applied here in the backend before returning, so
/// the frontend just needs to rearm the watchdog and surface the reasoning.
/// `Restart` and `Cancel` are surfaced as-is — the frontend kills the
/// session via its existing stop_chat path so it can drive its own UI state
/// (error banner, phase rollback, etc.) consistently. `Noop` is returned
/// when the session no longer exists (the common race where the session
/// ended cleanly between watchdog fire and IPC arrival); the frontend
/// silently clears its timer with no user-facing error.
///
/// `caller` is a free-form tag (typically "chat" | "context" | "merge")
/// included in the trace span so we can tell which of the three frontend
/// watchdogs fired this evaluation. It has no effect on the decision.
#[tracing::instrument(skip(state), fields(caller = caller.as_deref().unwrap_or("unknown")))]
#[tauri::command]
pub async fn check_chat_session(
    state: State<'_, AppState>,
    session_id: String,
    caller: Option<String>,
) -> Result<NurseDecisionDto, IpcError> {
    use crate::nurse::dispatcher::{DispatchInput, DispatchOrigin, DispatchResultKind};
    use crate::nurse::health::Signal;
    use chrono::Utc;
    use std::time::Duration;

    validate_session_id(&session_id).map_err(IpcError::validation)?;
    tracing::info!(
        session_id = %session_id,
        caller = caller.as_deref().unwrap_or("unknown"),
        "check_chat_session invoked"
    );

    let engine = state
        .nurse_engine()
        .cloned()
        .ok_or_else(|| IpcError::internal("nurse engine not attached"))?;
    let check_back_secs = {
        let cfg = state.config.read().await;
        cfg.chat_check_in_secs.clamp(1, 1800)
    };
    // Snapshot swarms-only before grabbing `engine.sessions` so we never
    // hold the sessions guard across the engine.config read. Preserves
    // the lock-ordering convention (config → sessions).
    let swarms_only = engine.config.read().await.swarms_only;

    // Synthesize a watchdog signal so the dispatcher's classifier prompt
    // sees something explicit when Tier 3 runs.
    let synthetic = Signal {
        detector: "watchdog",
        severity: Severity::Stalled,
        dedup_key: "watchdog:check_in".into(),
        summary: "frontend watchdog check".into(),
        raised_at: Utc::now(),
        evidence: serde_json::Value::Null,
    };

    // Gate before dispatch. The frontend timer can fire repeatedly while
    // a session is idle, after a kill is settling, or while only host-side
    // polling/heartbeats are flowing. Those states do not justify a Nurse
    // LLM call; only dispatch when Pi has produced fresh Nurse-relevant
    // activity since the previous admitted watchdog check.
    {
        let mut sessions = engine.sessions.write().unwrap_or_else(|p| p.into_inner());
        let Some(st) = sessions.get_mut(&session_id) else {
            tracing::debug!(
                session_id = %session_id,
                "check_chat_session: session not tracked by engine — returning Noop"
            );
            return Ok(NurseDecisionDto::Noop {
                reasoning: "session no longer exists".to_string(),
            });
        };
        if swarms_only && !matches!(st.owner, crate::pi::session::SessionOwner::Swarm { .. }) {
            tracing::debug!(
                session_id = %session_id,
                "check_chat_session: nurse is in swarms-only mode — skipping non-swarm session"
            );
            return Ok(NurseDecisionDto::Noop {
                reasoning: "nurse is in swarms-only mode".to_string(),
            });
        }
        let Some(session) = st.session.upgrade() else {
            tracing::debug!(
                session_id = %session_id,
                "check_chat_session: session handle gone — returning Noop"
            );
            return Ok(NurseDecisionDto::Noop {
                reasoning: "session no longer exists".to_string(),
            });
        };
        if !session.is_busy() {
            tracing::debug!(
                session_id = %session_id,
                "check_chat_session: session is not busy — returning Noop"
            );
            return Ok(NurseDecisionDto::Noop {
                reasoning: "session is not running".to_string(),
            });
        }
        let activity_count = session.nurse_activity_count();
        if activity_count == 0 {
            tracing::debug!(
                session_id = %session_id,
                "check_chat_session: no Nurse-relevant activity yet — skipping dispatch"
            );
            return Ok(NurseDecisionDto::LeaveIt {
                reasoning: "no agent activity since prompt yet".to_string(),
                check_back_secs,
            });
        }
        if activity_count <= st.last_watchdog_checked_activity_count {
            tracing::debug!(
                session_id = %session_id,
                activity_count,
                last_checked = st.last_watchdog_checked_activity_count,
                "check_chat_session: no new Nurse-relevant activity — skipping dispatch"
            );
            return Ok(NurseDecisionDto::LeaveIt {
                reasoning: "no new agent activity since previous nurse check".to_string(),
                check_back_secs,
            });
        }
        st.last_watchdog_checked_activity_count = activity_count;
        // Push the signal onto the session's health BEFORE dispatch so the
        // Tier 3 prompt observes it (and so storm-guard / dedup accounting is
        // consistent with detector-driven raises).
        st.health.push_signal(synthetic.clone());
    }

    let dispatcher = engine
        .dispatcher
        .get()
        .ok_or_else(|| IpcError::internal("nurse dispatcher not attached"))?
        .clone();

    let decision_id = uuid::Uuid::new_v4().simple().to_string();
    let input = DispatchInput {
        decision_id,
        session_id: session_id.clone(),
        trigger_signal: synthetic,
        origin: DispatchOrigin::Watchdog,
    };

    let result = match tokio::time::timeout(
        Duration::from_secs(95),
        dispatcher.handle_signal(input),
    )
    .await
    {
        Ok(r) => r,
        Err(_) => {
            tracing::warn!(session_id = %session_id, "check_chat_session: dispatcher timed out — short leave_it");
            return Ok(NurseDecisionDto::LeaveIt {
                reasoning: "nurse evaluation timed out".to_string(),
                check_back_secs: 60,
            });
        }
    };

    let dto = match result.kind {
        DispatchResultKind::Dispatched(decision, outcome) => {
            // If the dispatched action was Steer AND the applier reported
            // a Failed lifecycle status, downgrade to Cancel so the
            // frontend tears the session down rather than thinking the
            // steer succeeded.
            let is_steer = matches!(decision, NurseDecision::Steer { .. });
            if is_steer && outcome.completed_status == NurseLifecycleStatus::Failed {
                NurseDecisionDto::Cancel {
                    reasoning: format!("steer failed: {}", outcome.outcome_string),
                    message: "Failed to apply nurse steering — session cancelled.".to_string(),
                }
            } else {
                NurseDecisionDto::from(decision)
            }
        }
        DispatchResultKind::FastPathLeaveIt(d) => NurseDecisionDto::from(d),
        DispatchResultKind::GatedInFlight { .. } => NurseDecisionDto::LeaveIt {
            reasoning: "decision already in-flight in background".to_string(),
            check_back_secs: 15,
        },
        DispatchResultKind::NoSession => NurseDecisionDto::Noop {
            reasoning: "session no longer exists".to_string(),
        },
        DispatchResultKind::ClassifierFailed(e) => NurseDecisionDto::LeaveIt {
            reasoning: format!("nurse returned no decision: {}", e),
            check_back_secs: 60,
        },
        DispatchResultKind::ClassifierSkippedNoModel => NurseDecisionDto::Noop {
            reasoning: "nurse model not configured".to_string(),
        },
        DispatchResultKind::GatedDisabled => NurseDecisionDto::Noop {
            reasoning: "nurse is disabled".to_string(),
        },
        DispatchResultKind::GatedSeverity => NurseDecisionDto::Noop {
            reasoning: "signal below escalation threshold".to_string(),
        },
        DispatchResultKind::GatedPostLag => NurseDecisionDto::Noop {
            reasoning: "post-lag suppression active".to_string(),
        },
        DispatchResultKind::GatedStormGuard => NurseDecisionDto::Noop {
            reasoning: "storm guard suppressed re-dispatch".to_string(),
        },
        DispatchResultKind::GatedBudget(reason) => NurseDecisionDto::Noop {
            reasoning: format!("intervention budget exhausted: {:?}", reason),
        },
        DispatchResultKind::GatedSelfKillGrace => NurseDecisionDto::Noop {
            reasoning: "session is in self-kill grace window".to_string(),
        },
        DispatchResultKind::GatedSwarmsOnly => NurseDecisionDto::Noop {
            reasoning: "nurse is in swarms-only mode".to_string(),
        },
        DispatchResultKind::EngineGone => NurseDecisionDto::Noop {
            reasoning: "engine is shutting down".to_string(),
        },
        DispatchResultKind::Panic(msg) => {
            tracing::error!(session_id = %session_id, panic = %msg, "check_chat_session: dispatcher panicked");
            return Err(
                IpcError::internal(format!("nurse dispatcher panicked: {}", msg))
                    .with_id(session_id),
            );
        }
    };
    Ok(dto)
}

// ---------------------------------------------------------------------------
// New push-mode Nurse engine IPC surface (additive — legacy commands above
// continue to power the existing UI).
// ---------------------------------------------------------------------------

/// Helper: fetch the engine or return a friendly error.
fn engine_or_err(
    state: &AppState,
) -> Result<std::sync::Arc<crate::nurse::engine::NurseEngine>, IpcError> {
    state
        .nurse_engine()
        .cloned()
        .ok_or_else(|| IpcError::internal("nurse engine not yet attached"))
}

/// Snapshot from the NEW push-mode engine (`crate::nurse::engine::NurseEngine`).
///
/// Distinct from [`get_nurse_status`], which continues to surface the legacy
/// `nurse_service` snapshot. When the engine has not yet been attached
/// returns an `internal` IpcError so the frontend can fall back gracefully.
#[tauri::command]
pub async fn get_nurse_engine_status(
    state: State<'_, AppState>,
) -> Result<crate::nurse::snapshot::NurseStatusSnapshot, IpcError> {
    let engine = engine_or_err(&state)?;
    Ok(engine.snapshot_status())
}

/// Query envelope for [`get_nurse_intervention_log`]. Mirrors the
/// frontend `InterventionLogQuery` (`app/src/lib/nurseTypes.ts`). Every
/// field defaults to `None` so an empty `{}` from the frontend decodes
/// cleanly (the React hook always builds the object but optional fields
/// are omitted/`null`).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct InterventionLogQuery {
    #[serde(default)]
    pub before_ts: Option<DateTime<Utc>>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub profile: Option<NurseProfile>,
    #[serde(default)]
    pub action: Option<NurseActionKind>,
    #[serde(default)]
    pub tier: Option<NurseDispatchTier>,
    #[serde(default)]
    pub severity: Option<Severity>,
    #[serde(default)]
    pub owner_kind: Option<String>,
    #[serde(default)]
    pub success: Option<bool>,
}

/// Page envelope returned by [`get_nurse_intervention_log`]. Mirrors
/// the frontend `InterventionLogPage`. `total` is omitted today (the
/// type marks it optional on the frontend); the writer ring is bounded
/// at 100 records so a cheap total would not aid pagination.
#[derive(Debug, Clone, Serialize)]
pub struct InterventionLogPage {
    pub rows: Vec<NurseInterventionRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_before_ts: Option<DateTime<Utc>>,
}

/// Pure filter + paginate over an already-collected slice of
/// intervention records. Split out as a free function so the unit
/// tests can exercise the pagination math without standing up the
/// engine / `AppState`.
fn filter_and_paginate(
    snapshot: Vec<NurseInterventionRecord>,
    query: &InterventionLogQuery,
) -> InterventionLogPage {
    let limit = query.limit.unwrap_or(100).min(500);
    // `severity`, `profile`, and `tier` are reserved for future
    // detector-severity / profile / dispatch-tier columns on
    // NurseInterventionRecord; today the writer ring carries action
    // level only, so we silently accept-but-skip filtering on them
    // (matches the legacy pattern).
    let _ = query.severity;
    let _ = query.profile;
    let _ = query.tier;

    let mut filtered: Vec<NurseInterventionRecord> = snapshot
        .into_iter()
        .filter(|rec| {
            query
                .before_ts
                .map(|cut| rec.timestamp < cut)
                .unwrap_or(true)
        })
        .filter(|rec| query.action.map(|a| rec.level == a).unwrap_or(true))
        .filter(|rec| {
            query
                .owner_kind
                .as_deref()
                .map(|kind| {
                    // Best-effort owner_kind filter — we only have
                    // session_id on the record; owner discrimination by
                    // string prefix matches the convention used by the
                    // legacy synthesized owners (e.g. `hm-`, `task-`).
                    match kind {
                        "task" => {
                            !rec.session_id.starts_with("hm-")
                                && !rec.session_id.starts_with("swarm-")
                                && !rec.session_id.starts_with("merge-")
                        }
                        "swarm" => rec.session_id.starts_with("swarm-"),
                        "hivemind" | "review" => rec.session_id.starts_with("hm-"),
                        "merge" => rec.session_id.starts_with("merge-"),
                        _ => true,
                    }
                })
                .unwrap_or(true)
        })
        .filter(|rec| {
            query
                .success
                .map(|s| rec.outcome.is_some() == s)
                .unwrap_or(true)
        })
        .collect();
    // Newest first.
    filtered.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));

    // Cursor: if we have more rows than the limit, return the
    // timestamp of the last row we're about to emit so a subsequent
    // call with `before_ts = cursor` continues exclusively.
    let next_before_ts = if filtered.len() > limit && limit > 0 {
        Some(filtered[limit - 1].timestamp)
    } else {
        None
    };
    filtered.truncate(limit);
    InterventionLogPage {
        rows: filtered,
        next_before_ts,
    }
}

/// Clear the in-memory intervention ring.
///
/// This empties the bounded `VecDeque` maintained by the engine's
/// `InterventionWriter`. It is **not** persisted to disk — the data is
/// best-effort observability, not correctness-critical. A
/// `window.confirm()` guard on the frontend prevents accidental clears.
#[tauri::command]
pub async fn clear_nurse_intervention_log(state: State<'_, AppState>) -> Result<(), IpcError> {
    let engine = engine_or_err(&state)?;
    engine.intervention_writer.clear();
    Ok(())
}

/// Filtered slice of recent interventions surfaced by the engine.
///
/// All filters are AND-combined and applied in Rust (the writer's in-memory
/// ring is small — `RING_CAPACITY = 100`). `limit` defaults to 100 and is
/// capped at 500. Returns an [`InterventionLogPage`] envelope with a
/// `next_before_ts` cursor when more rows match than the limit allows.
#[tauri::command]
pub async fn get_nurse_intervention_log(
    state: State<'_, AppState>,
    query: InterventionLogQuery,
) -> Result<InterventionLogPage, IpcError> {
    let engine = engine_or_err(&state)?;
    let snapshot = engine.intervention_writer.recent_snapshot();
    Ok(filter_and_paginate(snapshot, &query))
}

/// Aggregated detector-level counters surfaced for the Detector Activity tab.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NurseDetectorStat {
    pub detector: String,
    pub description: String,
    pub total_signals: u64,
    pub by_severity: HashMap<Severity, u64>,
}

/// Walk the engine's detector registry and aggregate currently-raised
/// signals across all monitored sessions into per-detector counters.
///
/// `since_ts` is accepted for forward compatibility but ignored today —
/// the writer doesn't expose a persistent signal history. The Detector
/// Activity panel uses this as a snapshot, not a time series.
#[tauri::command]
pub async fn get_nurse_detector_stats(
    state: State<'_, AppState>,
    since_ts: Option<DateTime<Utc>>,
) -> Result<Vec<NurseDetectorStat>, IpcError> {
    let _ = since_ts;
    let engine = engine_or_err(&state)?;
    // Seed each registered detector with zero counters so the UI can show
    // "0 raises" rather than missing entries entirely.
    let mut stats: HashMap<&'static str, NurseDetectorStat> = HashMap::new();
    for detector in engine.detectors.iter() {
        stats.insert(
            detector.name(),
            NurseDetectorStat {
                detector: detector.name().to_string(),
                description: detector.description().to_string(),
                total_signals: 0,
                by_severity: HashMap::new(),
            },
        );
    }
    {
        // Short-held read lock — no .await across this guard.
        let sessions = engine.sessions.read().unwrap_or_else(|p| p.into_inner());
        for state in sessions.values() {
            for sig in &state.health.signals {
                let entry = stats
                    .entry(sig.detector)
                    .or_insert_with(|| NurseDetectorStat {
                        detector: sig.detector.to_string(),
                        description: String::new(),
                        total_signals: 0,
                        by_severity: HashMap::new(),
                    });
                entry.total_signals = entry.total_signals.saturating_add(1);
                *entry.by_severity.entry(sig.severity).or_insert(0) += 1;
            }
        }
    }
    let mut out: Vec<NurseDetectorStat> = stats.into_values().collect();
    out.sort_by(|a, b| a.detector.cmp(&b.detector));
    Ok(out)
}

/// Per-session detail snapshot used by the Session Drill-Down panel.
#[derive(Debug, Clone, Serialize)]
pub struct NurseSessionDetail {
    pub session_id: String,
    pub owner: SessionOwnerDto,
    pub tier: Tier,
    pub signals: Vec<Signal>,
    pub intervention_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
}

#[tauri::command]
pub async fn get_nurse_session_detail(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<NurseSessionDetail, IpcError> {
    validate_session_id(&session_id).map_err(IpcError::validation)?;
    let engine = engine_or_err(&state)?;
    // Brief read lock; clone everything we need out before dropping it.
    let sessions = engine.sessions.read().unwrap_or_else(|p| p.into_inner());
    let st = sessions
        .get(&session_id)
        .ok_or_else(|| IpcError::not_found("nurse_session", session_id.clone()))?;
    Ok(NurseSessionDetail {
        session_id: session_id.clone(),
        owner: SessionOwnerDto::from(&st.owner),
        tier: st.health.tier,
        signals: st.health.signals.clone(),
        intervention_count: st.intervention_count,
        provider: st.provider.clone(),
        model_id: st.model_id.clone(),
    })
}

/// Record user feedback (thumbs-up / thumbs-down + optional note) for a
/// specific intervention. V1 logs to tracing only — SQLite persistence is
/// post-MVP per the migration plan.
#[tauri::command]
pub async fn record_nurse_intervention_feedback(
    intervention_id: String,
    rating: i8,
    note: Option<String>,
) -> Result<(), IpcError> {
    if rating != -1 && rating != 1 {
        return Err(IpcError::validation("rating must be -1 or +1"));
    }
    tracing::info!(
        intervention_id = %intervention_id,
        rating,
        note = %note.as_deref().unwrap_or(""),
        "nurse intervention feedback recorded"
    );
    Ok(())
}

/// Manual operator action surface — bypasses the classifier but still emits
/// a Manual-tier Lifecycle payload so the UI updates.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ManualAction {
    Steer {
        message: String,
    },
    Cancel {
        #[serde(default)]
        message: Option<String>,
    },
    ForceRestart,
}

/// Take a manual action on a live Pi session. Sanity check: session must
/// exist in the Pi pool. Emits a `nurse-event` Lifecycle payload tagged
/// `Manual` so the operator sees their intervention in the timeline.
#[tauri::command]
pub async fn nurse_manual_action(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
    session_id: String,
    action: ManualAction,
) -> Result<(), IpcError> {
    validate_session_id(&session_id).map_err(IpcError::validation)?;
    let session = state
        .pi_manager
        .get_session(&session_id)
        .await
        .ok_or_else(|| IpcError::not_found("pi_session", session_id.clone()))?;

    let (level, observation, action_text) = match &action {
        ManualAction::Steer { message } => {
            session
                .steer(message, None)
                .await
                .map_err(|e| IpcError::internal(format!("steer failed: {}", e)))?;
            (
                NurseActionKind::Steer,
                "operator-issued manual steer".to_string(),
                format!("steered with: {}", truncate_for_observation(message, 240)),
            )
        }
        ManualAction::Cancel { message } => {
            state
                .pi_manager
                .kill_session(&session_id)
                .await
                .map_err(|e| IpcError::internal(format!("kill failed: {}", e)))?;
            (
                NurseActionKind::Cancel,
                "operator-issued manual cancel".to_string(),
                message
                    .clone()
                    .unwrap_or_else(|| "session cancelled by operator".to_string()),
            )
        }
        ManualAction::ForceRestart => {
            state
                .pi_manager
                .kill_session(&session_id)
                .await
                .map_err(|e| IpcError::internal(format!("kill failed: {}", e)))?;
            (
                NurseActionKind::Restart,
                "operator-issued manual force-restart".to_string(),
                "session killed; client may respawn".to_string(),
            )
        }
    };

    let intervention_id = uuid::Uuid::new_v4().simple().to_string();
    let now = Utc::now();
    let payload = NurseLifecyclePayload {
        intervention_id: intervention_id.clone(),
        status: NurseLifecycleStatus::Completed,
        level,
        session_id: session_id.clone(),
        task_id: None,
        swarm_id: None,
        feature_id: None,
        review_id: None,
        observation: observation.clone(),
        action: action_text.clone(),
        reasoning_delta: None,
        full_reasoning: None,
        error: None,
        timestamp: now,
    };
    // The snapshot module's NurseEvent is wire-identical to the legacy
    // shape, so listeners on `nurse-event` decode the same envelope.
    if let Err(e) = app.emit("nurse-event", NurseEvent::Lifecycle(payload.clone())) {
        tracing::warn!(error = %e, "nurse_manual_action: emit failed");
    }

    // Record the action in the engine's writer so it shows up in the
    // intervention log alongside automatic interventions. Tier is Manual.
    if let Some(engine) = state.nurse_engine() {
        let rec = NurseInterventionRecord {
            id: intervention_id,
            session_id: session_id.clone(),
            timestamp: now,
            level,
            analysis: observation,
            action_taken: NurseSessionAction {
                level,
                session_id,
                message: action_text,
                timestamp: now,
            },
            outcome: Some("manual".to_string()),
        };
        engine.intervention_writer.send(rec);
        // Suppress unused-variable warning in the (unlikely) cfg path
        // where NurseDispatchTier is unused.
        let _ = NurseDispatchTier::Manual;
    }
    Ok(())
}

fn truncate_for_observation(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut end = max;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &s[..end])
    }
}

/// Detector tunable schema for the Profiles tab. Each detector exposes its
/// own `Vec<TunableDef>` via `Detector::config_schema()`.
#[derive(Debug, Clone, Serialize)]
pub struct NurseDetectorSchema {
    pub name: String,
    pub description: String,
    pub tunables: Vec<TunableDef>,
}

#[tauri::command]
pub async fn get_nurse_detector_schemas(
    state: State<'_, AppState>,
) -> Result<Vec<NurseDetectorSchema>, IpcError> {
    let engine = engine_or_err(&state)?;
    let mut out: Vec<NurseDetectorSchema> = engine
        .detectors
        .iter()
        .map(|d| NurseDetectorSchema {
            name: d.name().to_string(),
            description: d.description().to_string(),
            tunables: d.config_schema(),
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Read `~/.hyvemind/debug/nurse/decisions.jsonl.{today,yesterday}` and
/// return every row where `decision_id == arg`, sorted by `event_seq`.
/// Cap at 200 rows.
#[tauri::command]
pub async fn get_nurse_decision_chain(
    state: State<'_, AppState>,
    decision_id: String,
) -> Result<Vec<DecisionLogRow>, IpcError> {
    if decision_id.trim().is_empty() {
        return Err(IpcError::validation("decision_id must be non-empty"));
    }
    let engine = engine_or_err(&state)?;
    let root = engine.observability.decisions.root().to_path_buf();
    let mut rows = read_decision_jsonl(&root, |row| row.decision_id == decision_id);
    rows.sort_by(|a, b| a.event_seq.cmp(&b.event_seq));
    rows.truncate(200);
    Ok(rows)
}

/// Compact summary of a single decision chain, used by the Session
/// Drill-Down panel to render the timeline.
#[derive(Debug, Clone, Serialize)]
pub struct NurseDecisionSummary {
    pub decision_id: String,
    pub started_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finalised_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tier_used: Option<NurseDispatchTier>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<NurseActionKind>,
}

#[tauri::command]
pub async fn get_nurse_decisions_for_session(
    state: State<'_, AppState>,
    session_id: String,
    since_ts: Option<DateTime<Utc>>,
    limit: Option<usize>,
) -> Result<Vec<NurseDecisionSummary>, IpcError> {
    validate_session_id(&session_id).map_err(IpcError::validation)?;
    let limit = limit.unwrap_or(50).min(200);
    let engine = engine_or_err(&state)?;
    let root = engine.observability.decisions.root().to_path_buf();
    let rows = read_decision_jsonl(&root, |row| {
        row.session_id.as_deref() == Some(session_id.as_str())
            && since_ts.map(|cut| row.ts >= cut).unwrap_or(true)
    });

    // Group by decision_id, then derive (started_at, finalised_at, status).
    let mut grouped: HashMap<String, Vec<DecisionLogRow>> = HashMap::new();
    for row in rows {
        grouped
            .entry(row.decision_id.clone())
            .or_default()
            .push(row);
    }
    let mut summaries: Vec<NurseDecisionSummary> = grouped
        .into_iter()
        .map(|(decision_id, mut events)| {
            events.sort_by(|a, b| a.event_seq.cmp(&b.event_seq));
            let started_at = events.first().map(|e| e.ts).unwrap_or_else(Utc::now);
            let finalised_event = events
                .iter()
                .rev()
                .find(|e| e.event == "decision_finalised")
                .cloned();
            let finalised_at = finalised_event.as_ref().map(|e| e.ts);
            let status = finalised_event.as_ref().and_then(|e| {
                e.data
                    .get("status")
                    .and_then(|v| v.as_str())
                    .map(String::from)
            });
            let tier_used = finalised_event
                .as_ref()
                .and_then(|e| e.data.get("tier_used"))
                .and_then(|v| serde_json::from_value(v.clone()).ok());
            let action = finalised_event
                .as_ref()
                .and_then(|e| e.data.get("action"))
                .and_then(|v| serde_json::from_value(v.clone()).ok());
            NurseDecisionSummary {
                decision_id,
                started_at,
                finalised_at,
                status,
                tier_used,
                action,
            }
        })
        .collect();
    summaries.sort_by(|a, b| b.started_at.cmp(&a.started_at));
    summaries.truncate(limit);
    Ok(summaries)
}

/// Read `~/.hyvemind/debug/nurse/signals/{session_id}.jsonl` line-by-line.
/// Cap at 500 rows. Missing file returns an empty Vec.
#[tauri::command]
pub async fn get_nurse_signal_stream(
    state: State<'_, AppState>,
    session_id: String,
    since_ts: Option<DateTime<Utc>>,
    limit: Option<usize>,
) -> Result<Vec<SignalStreamRow>, IpcError> {
    validate_session_id(&session_id).map_err(IpcError::validation)?;
    let limit = limit.unwrap_or(200).min(500);
    let engine = engine_or_err(&state)?;
    let root = engine.observability.signals.root().to_path_buf();
    let safe_sid: String = session_id
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let path = root.join("signals").join(format!("{}.jsonl", safe_sid));
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|e| IpcError::internal(format!("read signal stream: {}", e)))?;
    let mut rows: Vec<SignalStreamRow> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<SignalStreamRow>(l).ok())
        .filter(|r| since_ts.map(|cut| r.ts >= cut).unwrap_or(true))
        .collect();
    // Newest first, then trim.
    rows.sort_by(|a, b| b.ts.cmp(&a.ts));
    rows.truncate(limit);
    Ok(rows)
}

/// Result wrapper for a classifier prompt / response capture.
#[derive(Debug, Clone, Serialize)]
pub struct NurseCaptureResult {
    pub contents: String,
    pub truncated: bool,
}

const CAPTURE_MAX_BYTES: usize = 256 * 1024;

/// Read `~/.hyvemind/debug/nurse/captures/{decision_id}-{kind}.txt`.
/// Cap at 256 KB; reports `truncated = true` when the file is larger.
#[tauri::command]
pub async fn get_nurse_capture(
    state: State<'_, AppState>,
    decision_id: String,
    kind: String,
) -> Result<NurseCaptureResult, IpcError> {
    if decision_id.trim().is_empty() {
        return Err(IpcError::validation("decision_id must be non-empty"));
    }
    if kind != "prompt" && kind != "response" {
        return Err(IpcError::validation("kind must be 'prompt' or 'response'"));
    }
    let engine = engine_or_err(&state)?;
    let path = engine
        .observability
        .captures
        .root()
        .join("captures")
        .join(format!("{}-{}.txt", decision_id, kind));
    if !path.exists() {
        return Err(IpcError::not_found("nurse_capture", decision_id));
    }
    let mut contents = std::fs::read_to_string(&path)
        .map_err(|e| IpcError::internal(format!("read capture: {}", e)))?;
    let truncated = contents.len() > CAPTURE_MAX_BYTES;
    if truncated {
        let mut end = CAPTURE_MAX_BYTES;
        while end > 0 && !contents.is_char_boundary(end) {
            end -= 1;
        }
        contents.truncate(end);
    }
    Ok(NurseCaptureResult {
        contents,
        truncated,
    })
}

/// Result of `export_nurse_diagnostic_bundle`. `format = "manifest"` means
/// no zip/tar crates were available; the bundle is a JSON file listing the
/// relevant paths the operator should attach manually.
#[derive(Debug, Clone, Serialize)]
pub struct NurseDiagnosticBundle {
    pub bundle_path: String,
    pub format: String,
}

/// Export a slice of `~/.hyvemind/debug/nurse/` as a diagnostic bundle.
///
/// `decision_id` / `session_id` scope the slice; `window_secs` (default 300)
/// is reserved for time-windowed selection (not yet enforced — the manifest
/// fallback ships the full per-session signal stream and the full decision
/// log files).
#[tauri::command]
pub async fn export_nurse_diagnostic_bundle(
    state: State<'_, AppState>,
    decision_id: Option<String>,
    session_id: Option<String>,
    window_secs: Option<u32>,
) -> Result<NurseDiagnosticBundle, IpcError> {
    let _ = window_secs;
    let engine = engine_or_err(&state)?;
    let cache_dir = dirs::cache_dir()
        .ok_or_else(|| IpcError::internal("cache_dir unavailable on this platform"))?;
    std::fs::create_dir_all(&cache_dir)
        .map_err(|e| IpcError::internal(format!("create cache dir: {}", e)))?;

    let nurse_root = engine.observability.decisions.root().to_path_buf();
    let mut entries: Vec<String> = Vec::new();

    if let Some(sid) = session_id.as_deref() {
        let safe: String = sid
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let signals_file = nurse_root.join("signals").join(format!("{}.jsonl", safe));
        if signals_file.exists() {
            entries.push(signals_file.display().to_string());
        }
    }
    if let Some(did) = decision_id.as_deref() {
        for kind in ["prompt", "response"] {
            let cap = nurse_root
                .join("captures")
                .join(format!("{}-{}.txt", did, kind));
            if cap.exists() {
                entries.push(cap.display().to_string());
            }
        }
    }
    // Always include today's decision + bus log for context.
    if nurse_root.exists() {
        if let Ok(rd) = std::fs::read_dir(&nurse_root) {
            for entry in rd.flatten() {
                let p = entry.path();
                if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                    if name.starts_with("decisions.jsonl.") || name.starts_with("bus.jsonl.") {
                        entries.push(p.display().to_string());
                    }
                }
            }
        }
    }

    let manifest = serde_json::json!({
        "generated_at": Utc::now(),
        "decision_id": decision_id,
        "session_id": session_id,
        "files": entries,
    });
    let stamp = Utc::now().timestamp_millis();
    let bundle_path = cache_dir.join(format!("hyvemind-nurse-bundle-{}.json", stamp));
    std::fs::write(
        &bundle_path,
        serde_json::to_vec_pretty(&manifest).unwrap_or_default(),
    )
    .map_err(|e| IpcError::internal(format!("write manifest: {}", e)))?;

    Ok(NurseDiagnosticBundle {
        bundle_path: bundle_path.display().to_string(),
        format: "manifest".to_string(),
    })
}

/// Read every `decisions.jsonl.YYYY-MM-DD` file under `root` and return
/// every row where `predicate` returns true. Tolerates truncated tails and
/// malformed lines (best-effort observability read path).
fn read_decision_jsonl<P>(root: &std::path::Path, predicate: P) -> Vec<DecisionLogRow>
where
    P: Fn(&DecisionLogRow) -> bool,
{
    let mut out: Vec<DecisionLogRow> = Vec::new();
    let Ok(rd) = std::fs::read_dir(root) else {
        return out;
    };
    let mut paths: Vec<std::path::PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with("decisions.jsonl."))
                .unwrap_or(false)
        })
        .collect();
    // Read today + yesterday — but be tolerant of timezone drift and just
    // sort newest-first by filename suffix and take the two most recent.
    paths.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
    paths.truncate(2);
    for path in paths {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(row) = serde_json::from_str::<DecisionLogRow>(line) {
                if predicate(&row) {
                    out.push(row);
                }
            }
        }
    }
    out
}

// ── Profile CRUD ─────────────────────────────────────────────────────────
//
// The frontend `ProfileConfig` is shaped as `{ enabled, intervention_mode,
// escalation_min_severity, budget: BudgetConfigDto, detectors: HashMap<name,
// {enabled, config}> }`. Internally the backend uses typed per-detector
// fields and `BudgetConfig` with the longer `_lifetime_` suffixes. The two
// DTOs below translate between them; the conversion preserves any
// internal-only fields not exposed in the schema (e.g. `warn_secs`) by
// reading from the current value, so partial DTOs round-trip cleanly.

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BudgetConfigDto {
    initial_cap: u32,
    decay_per_hour: u32,
    max_cap: u32,
    per_detector_cap: u32,
    per_key_cooldown_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProfileDetectorConfigDto {
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default)]
    config: HashMap<String, serde_json::Value>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileConfigDto {
    enabled: bool,
    intervention_mode: crate::nurse::config::InterventionMode,
    escalation_min_severity: Severity,
    budget: BudgetConfigDto,
    detectors: HashMap<String, ProfileDetectorConfigDto>,
}

impl From<&crate::nurse::config::BudgetConfig> for BudgetConfigDto {
    fn from(b: &crate::nurse::config::BudgetConfig) -> Self {
        Self {
            initial_cap: b.initial_lifetime_cap,
            decay_per_hour: b.decay_per_hour,
            max_cap: b.max_lifetime_cap,
            per_detector_cap: b.per_detector_cap,
            per_key_cooldown_secs: b.per_key_cooldown_secs,
        }
    }
}

impl From<BudgetConfigDto> for crate::nurse::config::BudgetConfig {
    fn from(d: BudgetConfigDto) -> Self {
        Self {
            initial_lifetime_cap: d.initial_cap,
            decay_per_hour: d.decay_per_hour,
            max_lifetime_cap: d.max_cap,
            per_detector_cap: d.per_detector_cap,
            per_key_cooldown_secs: d.per_key_cooldown_secs,
        }
    }
}

/// Serialise one typed detector struct into the generic `{enabled, config}`
/// shape the frontend expects. We round-trip through JSON to avoid hand-rolling
/// per-detector field lists; the `enabled` field is hoisted out so the UI's
/// detector-level toggle binds to a single value.
fn detector_to_dto<T: Serialize>(typed: &T) -> ProfileDetectorConfigDto {
    let mut map: HashMap<String, serde_json::Value> = serde_json::to_value(typed)
        .ok()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();
    let enabled = map
        .remove("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    ProfileDetectorConfigDto {
        enabled,
        config: map,
    }
}

/// Merge a DTO into a typed detector struct: start from `current` so any
/// internal-only field the schema doesn't surface (e.g. `awaiting_model_hard_limit_secs`)
/// survives unchanged. Unknown keys in `dto.config` are silently dropped.
fn detector_from_dto<T>(current: &T, dto: &ProfileDetectorConfigDto) -> T
where
    T: Serialize + for<'de> Deserialize<'de> + Clone,
{
    let Ok(serde_json::Value::Object(mut current_map)) = serde_json::to_value(current) else {
        return current.clone();
    };
    for (k, v) in &dto.config {
        if current_map.contains_key(k) {
            current_map.insert(k.clone(), v.clone());
        }
    }
    current_map.insert("enabled".to_string(), serde_json::Value::Bool(dto.enabled));
    serde_json::from_value(serde_json::Value::Object(current_map))
        .unwrap_or_else(|_| current.clone())
}

impl From<&crate::nurse::config::ProfileConfig> for ProfileConfigDto {
    fn from(c: &crate::nurse::config::ProfileConfig) -> Self {
        let mut detectors: HashMap<String, ProfileDetectorConfigDto> = HashMap::new();
        detectors.insert("stall".to_string(), detector_to_dto(&c.stall));
        detectors.insert(
            "reasoning_loop".to_string(),
            detector_to_dto(&c.reasoning_loop),
        );
        detectors.insert("tool_failure".to_string(), detector_to_dto(&c.tool_failure));
        detectors.insert(
            "provider_health".to_string(),
            detector_to_dto(&c.provider_health),
        );
        detectors.insert(
            "context_saturation".to_string(),
            detector_to_dto(&c.context_saturation),
        );
        detectors.insert(
            "retry_exhaustion".to_string(),
            detector_to_dto(&c.retry_exhaustion),
        );
        Self {
            enabled: c.enabled,
            intervention_mode: c.intervention_mode,
            escalation_min_severity: c.escalation_min_severity,
            budget: BudgetConfigDto::from(&c.budget),
            detectors,
        }
    }
}

fn profile_from_dto(
    base: &crate::nurse::config::ProfileConfig,
    dto: ProfileConfigDto,
) -> crate::nurse::config::ProfileConfig {
    let mut out = base.clone();
    out.enabled = dto.enabled;
    out.intervention_mode = dto.intervention_mode;
    out.escalation_min_severity = dto.escalation_min_severity;
    out.budget = dto.budget.into();
    if let Some(d) = dto.detectors.get("stall") {
        out.stall = detector_from_dto(&base.stall, d);
    }
    if let Some(d) = dto.detectors.get("reasoning_loop") {
        out.reasoning_loop = detector_from_dto(&base.reasoning_loop, d);
    }
    if let Some(d) = dto.detectors.get("tool_failure") {
        out.tool_failure = detector_from_dto(&base.tool_failure, d);
    }
    if let Some(d) = dto.detectors.get("provider_health") {
        out.provider_health = detector_from_dto(&base.provider_health, d);
    }
    if let Some(d) = dto.detectors.get("context_saturation") {
        out.context_saturation = detector_from_dto(&base.context_saturation, d);
    }
    if let Some(d) = dto.detectors.get("retry_exhaustion") {
        out.retry_exhaustion = detector_from_dto(&base.retry_exhaustion, d);
    }
    out
}

/// Return the persisted [`ProfileConfig`] for `profile`, falling back to
/// [`ProfileConfig::default_for`] when the user hasn't overridden anything.
#[tauri::command]
pub async fn get_nurse_profile(
    state: State<'_, AppState>,
    profile: NurseProfile,
) -> Result<ProfileConfigDto, IpcError> {
    let cfg = state.config.read().await;
    let internal = cfg
        .nurse_profiles
        .get(&profile)
        .cloned()
        .unwrap_or_else(|| crate::nurse::config::ProfileConfig::default_for(profile));
    Ok(ProfileConfigDto::from(&internal))
}

/// Persist `dto` to `config.json::nurse_profiles[profile]` AND write through
/// to the running engine's `NurseConfig` so the next detector tick observes
/// the new values without a restart.
#[tauri::command]
pub async fn set_nurse_profile(
    state: State<'_, AppState>,
    profile: NurseProfile,
    config: ProfileConfigDto,
) -> Result<ProfileConfigDto, IpcError> {
    let (response, data_dir, bytes, persisted) = {
        let mut cfg = state.config.write().await;
        let base = cfg
            .nurse_profiles
            .get(&profile)
            .cloned()
            .unwrap_or_else(|| crate::nurse::config::ProfileConfig::default_for(profile));
        let merged = profile_from_dto(&base, config);
        cfg.nurse_profiles.insert(profile, merged.clone());
        let bytes = cfg
            .snapshot_to_bytes()
            .map_err(|e| IpcError::internal(format!("serialize config failed: {}", e)))?;
        let response = ProfileConfigDto::from(&merged);
        (response, cfg.data_dir.clone(), bytes, merged)
    };
    crate::state::config::Config::write_bytes(data_dir, bytes)
        .await
        .map_err(|e| IpcError::internal(format!("save config failed: {}", e)))?;
    // Hot-reload the running engine so the next tick reads the new value.
    if let Some(engine) = state.nurse_engine() {
        let mut engine_cfg = engine.config.write().await;
        engine_cfg.profiles.insert(profile, persisted);
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nurse::snapshot::{NurseActionKind, NurseSessionAction};
    use chrono::Duration;

    fn fake_record(id: &str, ts: DateTime<Utc>) -> NurseInterventionRecord {
        NurseInterventionRecord {
            id: id.to_string(),
            session_id: "sid".to_string(),
            timestamp: ts,
            level: NurseActionKind::Steer,
            analysis: "".to_string(),
            action_taken: NurseSessionAction {
                level: NurseActionKind::Steer,
                session_id: "sid".to_string(),
                message: "m".to_string(),
                timestamp: ts,
            },
            outcome: None,
        }
    }

    #[test]
    fn filter_and_paginate_returns_all_when_under_limit() {
        let now = Utc::now();
        let records = vec![
            fake_record("a", now),
            fake_record("b", now - Duration::seconds(10)),
            fake_record("c", now - Duration::seconds(20)),
        ];
        let page = filter_and_paginate(records, &InterventionLogQuery::default());
        assert_eq!(page.rows.len(), 3);
        assert!(page.next_before_ts.is_none());
        // Newest first.
        assert_eq!(page.rows[0].id, "a");
        assert_eq!(page.rows[2].id, "c");
    }

    #[test]
    fn filter_and_paginate_cursor_at_last_returned_row_when_truncating() {
        let now = Utc::now();
        let records: Vec<NurseInterventionRecord> = (0..5)
            .map(|i| fake_record(&format!("r{i}"), now - Duration::seconds(i as i64 * 10)))
            .collect();
        let query = InterventionLogQuery {
            limit: Some(2),
            ..InterventionLogQuery::default()
        };
        let page = filter_and_paginate(records, &query);
        assert_eq!(page.rows.len(), 2);
        // Cursor is the timestamp of the last (i.e. 2nd) returned row.
        let cursor = page.next_before_ts.expect("cursor should be set");
        assert_eq!(cursor, page.rows[1].timestamp);
        // And the boundary is exclusive: re-applying the cursor with
        // before_ts = cursor must drop the boundary row from the next
        // page.
        let follow_records: Vec<NurseInterventionRecord> = (0..5)
            .map(|i| fake_record(&format!("r{i}"), now - Duration::seconds(i as i64 * 10)))
            .collect();
        let follow_query = InterventionLogQuery {
            limit: Some(2),
            before_ts: Some(cursor),
            ..InterventionLogQuery::default()
        };
        let follow = filter_and_paginate(follow_records, &follow_query);
        // The boundary row (whose timestamp equals the cursor) is
        // excluded by `rec.timestamp < cut`.
        assert!(
            follow.rows.iter().all(|r| r.timestamp < cursor),
            "cursor must be exclusive"
        );
    }

    #[test]
    fn noop_dto_serializes_with_kind_discriminant() {
        let dto = NurseDecisionDto::Noop {
            reasoning: "session no longer exists".to_string(),
        };
        let json = serde_json::to_value(&dto).expect("serialize");
        assert_eq!(json["kind"], "noop");
        assert_eq!(json["reasoning"], "session no longer exists");
    }

    #[test]
    fn leave_it_dto_clamps_check_back_secs() {
        // The From impl clamps check_back_secs into [1, 1800].
        let too_long = NurseDecisionDto::from(NurseDecision::LeaveIt {
            reasoning: "ok".to_string(),
            check_back_secs: 99_999,
            observation: None,
            action: None,
        });
        match too_long {
            NurseDecisionDto::LeaveIt {
                check_back_secs, ..
            } => {
                assert_eq!(check_back_secs, 1800);
            }
            _ => panic!("expected LeaveIt"),
        }
    }
}
