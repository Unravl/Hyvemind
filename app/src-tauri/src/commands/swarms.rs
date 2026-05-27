use std::sync::Arc;
use tauri::Emitter;
use tokio::sync::{broadcast, mpsc, RwLock};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn, Instrument};

use crate::commands::util::{check_payload_size, validate_id};
use crate::core::queen::{inject_milestone_validators, run_swarm_full, QueenConfig};
use crate::core::validation::{assign_assertion_ids, ValidationAssertion};
use crate::domain::swarm::{
    Feature, FeatureStatus, Milestone, ModelSettings, SwarmConfig, SwarmState, SwarmStatus,
    SwarmUsageSummary,
};
use crate::state::activity_log::{ActivityReader, ActivityWriter, SwarmActivityLogPage};
use crate::state::app_state::AppState;
use crate::state::ipc_error::IpcError;
use crate::state::progress::{ProgressEvent, ProgressReader, ProgressWriter};
use crate::tunables;

/// Default page size for `get_swarm_activity_log` when the caller doesn't
/// supply one.
const ACTIVITY_LOG_DEFAULT_LIMIT: u32 = 500;
/// Hard cap on the page size — even an explicit `limit` from the frontend
/// is clamped to this so a single IPC call can't materialise an unbounded
/// chunk of activity into memory.
const ACTIVITY_LOG_MAX_LIMIT: u32 = 2000;

/// Maximum length (bytes) of a swarm goal / description from the frontend.
const MAX_GOAL_LEN: usize = 64 * 1024;
/// Maximum length (bytes) of a single feature description from the frontend.
const MAX_FEATURE_DESC_LEN: usize = 64 * 1024;
/// Maximum length (chars) of a swarm name from the frontend.
const MAX_NAME_LEN: usize = 200;

/// Validate a working-directory string from the frontend WITHOUT the
/// approved-dirs allowlist check. Used internally by `apply_update` (the
/// pure function exercised by unit tests with no `AppState` fixture); the
/// allowlist check is layered on by `validate_working_dir_with_allowlist`
/// which the live IPC handlers call. See audit item 1.11.
fn validate_working_dir(p: &str) -> Result<std::path::PathBuf, String> {
    crate::commands::util::canonicalize_working_dir(p)
}

/// Allowlist-enforcing wrapper around `validate_working_dir` (audit 1.11).
/// Every `#[tauri::command]` in this module that accepts a `working_dir`
/// from the frontend funnels through this helper so a single bug or
/// missing call site can't bypass the check.
async fn validate_working_dir_with_allowlist(
    state: &tauri::State<'_, AppState>,
    p: &str,
) -> Result<std::path::PathBuf, String> {
    let approved = state.config.read().await.approved_working_dirs.clone();
    crate::commands::util::validate_approved_working_dir(p, &approved)
}

/// Create a new swarm with the given configuration.
///
/// Generates a swarm ID, initializes file-based storage, and returns the
/// initial swarm state. The swarm is not started until `start_swarm` is called.
#[tracing::instrument(skip(state))]
#[tauri::command]
pub async fn create_swarm(
    state: tauri::State<'_, AppState>,
    name: String,
    description: String,
    working_directory: String,
    model_settings: serde_json::Value,
) -> Result<SwarmState, IpcError> {
    info!(
        name = %name,
        working_dir = %working_directory,
        "create_swarm invoked"
    );

    // Bound the inbound JSON payload before any work — a 100 MiB
    // model_settings blob would otherwise be deserialized into memory
    // before the caller hits any other validation.
    check_payload_size(&model_settings).map_err(IpcError::validation)?;

    // Length-cap user-supplied free text. Reject (don't truncate) so the
    // frontend can display a meaningful error.
    if description.len() > MAX_GOAL_LEN {
        return Err(IpcError::validation(format!(
            "description too long: {} bytes (max {})",
            description.len(),
            MAX_GOAL_LEN
        )));
    }

    // Validate the working directory: trim whitespace, expand "~", canonicalize,
    // require an existing directory, AND enforce the approved-dirs allowlist
    // (audit item 1.11). Store the canonical path string so downstream
    // consumers (Pi sessions, agents) get a stable path.
    let canonical_working_dir = validate_working_dir_with_allowlist(&state, &working_directory)
        .await
        .map_err(IpcError::not_approved)?;
    let working_directory = canonical_working_dir.display().to_string();

    // Parse model settings from JSON
    let settings: ModelSettings = serde_json::from_value(model_settings).unwrap_or_default();

    // Build config
    let config = SwarmConfig {
        name,
        description,
        working_directory,
        model_settings: settings,
        features: Vec::new(),
        milestones: Vec::new(),
    };

    // Create SwarmState from config
    let swarm_state = SwarmState::from_config(&config);
    let swarm_id = swarm_state.id.clone();

    // Initialize file-based persistence for this swarm
    state.swarm_store.init_swarm(&swarm_id).await.map_err(|e| {
        IpcError::internal(format!("failed to init swarm directory: {}", e))
            .with_id(swarm_id.clone())
    })?;

    // Write initial state to disk
    state
        .swarm_store
        .write_state(&swarm_id, &swarm_state)
        .await
        .map_err(|e| {
            IpcError::internal(format!("failed to write initial swarm state: {}", e))
                .with_id(swarm_id.clone())
        })?;

    // Register in the in-memory registry as idle
    let cancel_token = CancellationToken::new();
    state
        .swarm_registry
        .register(swarm_id.clone(), swarm_state.clone(), cancel_token)
        .await;

    info!(swarm_id = %swarm_id, "swarm created");
    Ok(swarm_state)
}

/// Start a previously created swarm with a list of features to implement.
///
/// Parses the feature specifications and (optionally) milestone definitions,
/// spawns the queen orchestrator as a detached task, and registers the swarm
/// in the registry. Validates that `hivemind_id` is set if any hivemind
/// integration flags are enabled.
///
/// Milestones are optional from the IPC caller. If absent and a
/// `milestones.json` already exists on disk for this swarm (e.g. from a
/// previous attempt), we fall back to that. Empty milestones means Guard
/// validation is disabled for the whole swarm.
#[tracing::instrument(skip(app, state))]
#[tauri::command]
pub async fn start_swarm(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    swarm_id: String,
    features: Vec<serde_json::Value>,
    milestones: Option<Vec<serde_json::Value>>,
) -> Result<(), IpcError> {
    validate_id(&swarm_id).map_err(IpcError::validation)?;
    info!(
        swarm_id = %swarm_id,
        feature_count = features.len(),
        milestone_count = milestones.as_ref().map(|m| m.len()).unwrap_or(0),
        "start_swarm invoked"
    );
    tracing::debug!(features = ?features, milestones = ?milestones, "start_swarm details");

    // Bound the inbound JSON payload before any work — each feature /
    // milestone is itself an untrusted `Value`. Check the aggregate, since
    // a malicious caller could split bloat across many entries.
    let features_value = serde_json::Value::Array(features.clone());
    check_payload_size(&features_value)?;
    if let Some(ref ms_vec) = milestones {
        let milestones_value = serde_json::Value::Array(ms_vec.clone());
        check_payload_size(&milestones_value)?;
    }

    // --- Per-swarm concurrency lock ---
    let start_lock = state.swarm_registry.get_start_lock(&swarm_id).await;
    let _start_guard = start_lock.lock().await;

    // Reject double-launch: if the swarm already has a queen task running,
    // spawning a second one would orphan the first one's CancellationToken
    // (registry.register replaces the entry), making Stop unreachable.
    // Use `is_active` not `is_running` — `create_swarm` pre-registers every
    // swarm in `Planning`, so registry presence alone is not enough.
    if state.swarm_registry.is_active(&swarm_id).await {
        info!(swarm_id = %swarm_id, "start_swarm: swarm already running, ignoring");
        return Err(
            IpcError::validation(format!("swarm '{}' is already running", swarm_id))
                .with_id(swarm_id.clone()),
        );
    }

    // Get the current swarm state — try in-memory registry first, then disk.
    let mut swarm_state = match state.swarm_registry.get_state(&swarm_id).await {
        Some(s) => s,
        None => {
            // Fall back to disk (swarm created in a previous session).
            match state.swarm_store.read_state(&swarm_id).await {
                Ok(Some(s)) => {
                    tracing::info!(swarm_id = %swarm_id, "loaded swarm state from disk");
                    // Reset stale status. After restart, disk may hold
                    // Implementing / Completed / Failed / Cancelled from a
                    // previous session. Setting to Planning ensures the user
                    // can re-launch after restart.
                    let mut s = s;
                    s.set_status(SwarmStatus::Planning);
                    s
                }
                Ok(None) => {
                    return Err(IpcError::not_found("swarm", swarm_id.clone()));
                }
                Err(e) => {
                    return Err(IpcError::internal(format!(
                        "failed to read swarm '{}' state from disk: {}",
                        swarm_id, e
                    ))
                    .with_id(swarm_id.clone()));
                }
            }
        }
    };

    // Validate hivemind_id if use_hivemind flags are set
    let ms = &swarm_state.model_settings;
    if (ms.use_hivemind_on_scout || ms.use_hivemind_on_queen) && ms.hivemind_id.is_none() {
        return Err(IpcError::validation(
            "hivemind_id must be set when use_hivemind_on_scout or use_hivemind_on_queen is true",
        ));
    }

    // Re-validate working_directory at start time. The state may have been
    // loaded from disk after a restart; the directory could have been moved
    // or deleted in the meantime, OR removed from the approved-dirs allowlist
    // (audit 1.11) — in which case the user needs to re-approve before we
    // hand the path to Pi.
    let canonical_working_dir =
        validate_working_dir_with_allowlist(&state, &swarm_state.working_directory).await?;
    let canonical_working_dir_str = canonical_working_dir.display().to_string();
    if canonical_working_dir_str != swarm_state.working_directory {
        swarm_state.working_directory = canonical_working_dir_str;
    }

    // Parse feature specifications, rejecting any that exceed the description
    // length cap so a buggy frontend can't push huge blobs into Pi prompts.
    let parsed_features: Vec<Feature> = features
        .into_iter()
        .enumerate()
        .map(|(i, val)| {
            let id = val["id"]
                .as_str()
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("feature-{}", i));
            let name = val["name"]
                .as_str()
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("Feature {}", i));
            let description = val["description"]
                .as_str()
                .map(|s| s.to_string())
                .unwrap_or_default();

            if description.len() > MAX_FEATURE_DESC_LEN {
                return Err(format!(
                    "feature {} description too long: {} bytes (max {})",
                    id,
                    description.len(),
                    MAX_FEATURE_DESC_LEN
                ));
            }

            let mut feature = Feature::new(id, name, description);

            if let Some(deps) = val["dependencies"].as_array() {
                feature.dependencies = deps
                    .iter()
                    .filter_map(|d| d.as_str().map(|s| s.to_string()))
                    .collect();
            }

            if let Some(ms) = val["milestone"].as_str() {
                feature.milestone = Some(ms.to_string());
            }

            if let Some(arr) = val["fulfills"].as_array() {
                feature.fulfills = arr
                    .iter()
                    .filter_map(|d| d.as_str().map(|s| s.to_string()))
                    .collect();
            }

            Ok(feature)
        })
        .collect::<Result<Vec<Feature>, String>>()?;

    // Parse milestone specifications. Milestones from the IPC payload
    // override any persisted milestones.json. If the caller omitted them,
    // try disk so an in-flight relaunch picks up the previous attempt's
    // contract.
    let parsed_milestones: Vec<Milestone> = match milestones {
        Some(values) => values
            .into_iter()
            .enumerate()
            .map(|(i, val)| {
                let id = val["id"]
                    .as_str()
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| format!("milestone-{}", i));
                let name = val["name"]
                    .as_str()
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| id.clone());
                let features: Vec<String> = val["features"]
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|d| d.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
                let assertions: Vec<String> = val["assertions"]
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|d| d.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
                Ok::<Milestone, String>(Milestone {
                    id,
                    name,
                    features,
                    assertions,
                    sealed: false,
                })
            })
            .collect::<Result<Vec<_>, String>>()?,
        None => state
            .swarm_store
            .read_milestones(&swarm_id)
            .await
            .map_err(|e| format!("failed to read existing milestones: {}", e))?,
    };

    // Cross-check: every feature.milestone reference resolves. We don't
    // hard-fail (the warn in queen.rs handles per-feature absence) but log
    // it loudly here so the user sees it at launch time.
    let milestone_ids: std::collections::HashSet<&str> =
        parsed_milestones.iter().map(|m| m.id.as_str()).collect();
    for feat in &parsed_features {
        if let Some(mid) = feat.milestone.as_deref() {
            if !milestone_ids.contains(mid) {
                tracing::warn!(
                    swarm_id = %swarm_id,
                    feature_id = %feat.id,
                    milestone_id = %mid,
                    "feature references milestone with no matching definition; \
                     Guard will not run for this feature"
                );
            }
        }
    }

    // Phase 2: assign stable VAL-* IDs to every milestone assertion, then
    // auto-inject one synthetic validator feature per milestone. The
    // validator depends on every impl feature in its milestone, so
    // sequencing falls out of the dep graph naturally; the next milestone's
    // first impl feature then depends on the previous milestone's
    // validator (milestone sealing for free).
    let validation_assertions = assign_assertion_ids(&parsed_milestones);
    let mut parsed_features = parsed_features;
    inject_milestone_validators(
        &mut parsed_features,
        &parsed_milestones,
        &validation_assertions,
    );

    // Persist features to disk (including the synthetic validators).
    state
        .swarm_store
        .write_features(&swarm_id, &parsed_features)
        .await
        .map_err(|e| format!("failed to write features: {}", e))?;

    // Persist milestones to disk for resume/recovery.
    state
        .swarm_store
        .write_milestones(&swarm_id, &parsed_milestones)
        .await
        .map_err(|e| format!("failed to write milestones: {}", e))?;

    // Persist the human-readable validation contract markdown alongside
    // the machine-readable assertion list. Best-effort: a failed write
    // here shouldn't block the swarm from launching.
    if !validation_assertions.is_empty() {
        if let Err(e) = state
            .swarm_store
            .write_validation_contract(&swarm_id, &parsed_milestones, &validation_assertions)
            .await
        {
            tracing::warn!(
                swarm_id = %swarm_id,
                error = %e,
                "failed to write validation-contract.md; continuing"
            );
        }
    }

    // Update status to Implementing on the state object and persist to disk.
    // This write happens *before* registration so a failure aborts cleanly
    // without a running queen task.
    swarm_state.set_status(SwarmStatus::Implementing);
    state
        .swarm_store
        .write_state(&swarm_id, &swarm_state)
        .await
        .map_err(|e| format!("failed to persist swarm state before start: {}", e))?;

    spawn_queen_task(
        &app,
        &state,
        swarm_state,
        parsed_features,
        parsed_milestones,
        validation_assertions,
    )
    .await
    .map_err(IpcError::internal)
}

/// Reset feature statuses that aren't safe to leave mid-flight after a
/// queen task has been killed. The scheduler will pick them up again as
/// soon as their dependencies are still met. Terminal statuses
/// (Completed / Failed / Skipped) and Pending are preserved.
///
/// Audit 2.2: features the crash reconciler marked `Failed` with
/// `interrupted = true` are also re-queued — they were forcibly failed
/// only because the host died mid-execution, and the user-initiated
/// resume should re-try them. The `interrupted` / `resumable` markers
/// are cleared on reset so the next reconciliation cycle starts clean.
pub(crate) fn reset_in_flight_features(features: &mut [Feature]) -> usize {
    let mut n = 0;
    for f in features.iter_mut() {
        let in_flight = matches!(
            f.status,
            FeatureStatus::Scouting
                | FeatureStatus::Implementing
                | FeatureStatus::Reviewing
                | FeatureStatus::Validating
        );
        let interrupted_fail = f.status == FeatureStatus::Failed && f.interrupted;
        if in_flight || interrupted_fail {
            f.status = FeatureStatus::Pending;
            f.interrupted = false;
            f.resumable = false;
            n += 1;
        }
    }
    n
}

/// Reset features for a full resume from a terminal failure/cancellation.
///
/// Unlike `reset_in_flight_features` (which preserves terminal-failed features
/// that represent genuine fix-exhaustion failures), this function resets:
/// 1. In-flight features (Scouting / Implementing / Reviewing / Validating) → Pending
/// 2. Features marked `Failed { interrupted: true }` (crash victims) → Pending
/// 3. Features marked `Failed` (non-interrupted, fix-exhaustion failures) → Pending
///
/// In all three cases `interrupted` and `resumable` flags are cleared, and
/// `fix_attempt_count` is reset to 0 so every retried feature gets a fresh
/// retry budget.
///
/// Completed, Skipped, and Pending features are preserved unchanged.
pub(crate) fn reset_features_for_full_resume(features: &mut [Feature]) -> usize {
    let mut n = 0;
    for f in features.iter_mut() {
        let in_flight = matches!(
            f.status,
            FeatureStatus::Scouting
                | FeatureStatus::Implementing
                | FeatureStatus::Reviewing
                | FeatureStatus::Validating
        );
        let is_failed = f.status == FeatureStatus::Failed;
        // Both interrupted-failed (crash victims) and terminal-failed
        // (fix-exhaustion) features are reset. The `interrupted_fail`
        // case is a subset of `is_failed`, so we just check `is_failed`.
        if in_flight || is_failed {
            f.status = FeatureStatus::Pending;
            f.interrupted = false;
            f.resumable = false;
            f.fix_attempt_count = 0; // fresh retry budget
            n += 1;
        }
    }
    n
}

/// Build all queen-task plumbing (event channels, frontend emitter,
/// progress-log writer, activity coalescer, usage accumulator, registry
/// registration, pause handles) and spawn `run_swarm_full`.
///
/// `swarm_state` MUST already be persisted to disk with `Implementing`
/// status before this is called — the helper does not write swarm state
/// itself, only registers it in the in-memory registry.
async fn spawn_queen_task(
    app: &tauri::AppHandle,
    state: &AppState,
    swarm_state: SwarmState,
    features: Vec<Feature>,
    milestones: Vec<Milestone>,
    validation_assertions: Vec<ValidationAssertion>,
) -> Result<(), String> {
    let swarm_id = swarm_state.id.clone();

    // Create a cancellation token for the queen
    let cancel_token = CancellationToken::new();
    let registry = state.swarm_registry.clone();
    let store_for_queen = state.swarm_store.clone();
    let pi_manager = state.pi_manager.clone();
    let usage_store = state.usage_store.clone();

    // Create broadcast channel for progress events. We open multiple receivers
    // off this channel: one forwards to the Tauri frontend, one writes to the
    // on-disk JSONL progress log so `get_swarm_progress` can replay history.
    let (event_tx, mut event_rx_emit) = broadcast::channel::<ProgressEvent>(256);
    let mut event_rx_persist = event_tx.subscribe();

    // Forward ProgressEvents to the Tauri frontend. The swarm-event payload
    // includes the optional `metadata` blob so the Swarm UI can render rich
    // Nurse interventions (observation/action/reasoning) without a second
    // round-trip. NurseIntervention events are *also* mirrored to the
    // `nurse-event` channel so the Tasks-view conversation (if the swarm was
    // launched from a Task) renders the same inline card.
    let app_emitter = app.clone();
    let swarm_id_fwd = swarm_id.clone();
    // Audit 2.12: panic-safe wrapper. If this fire-and-forget forwarder
    // panics, emit a synthetic `failed` swarm-event so the UI does not
    // spin forever, and flip the on-disk swarm to `Failed`.
    let panic_app = app.clone();
    let panic_swarm_id = swarm_id.clone();
    let panic_store = store_for_queen.clone();
    let panic_registry = registry.clone();
    let swarm_id_fwd_panic_marker = swarm_id_fwd.clone();
    tokio::spawn(
        crate::supervise!(
            context = format!("swarm={} component=event_forwarder", swarm_id_fwd_panic_marker),
            on_panic = move |panic_msg: String| {
                let _ = panic_app.emit(
                    "swarm-event",
                    serde_json::json!({
                        "swarm_id": &panic_swarm_id,
                        "event_type": "failed",
                        "message": format!("internal task panicked (event_forwarder): {panic_msg}"),
                    }),
                );
                // Best-effort: mark the swarm Failed in memory + on disk.
                let pid = panic_swarm_id.clone();
                let preg = panic_registry.clone();
                let pstore = panic_store.clone();
                tokio::spawn(async move {
                    preg.update_status(&pid, SwarmStatus::Failed).await;
                    if let Some(s) = preg.get_state(&pid).await {
                        let _ = pstore.write_state(&pid, &s).await;
                    }
                });
            },
            async move {
                #[cfg(test)]
                crate::util::supervise::maybe_panic_for_test("swarms_event_forwarder");
                loop {
                    let evt = match event_rx_emit.recv().await {
                        Ok(evt) => evt,
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            warn!(
                                swarm_id = %swarm_id_fwd,
                                dropped = n,
                                "swarm-event broadcast receiver lagged \u{2014} skipping dropped events"
                            );
                            continue;
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    };
                    let event_type = format!("{:?}", evt.event_type).to_lowercase();
                    let _ = app_emitter.emit(
                        "swarm-event",
                        serde_json::json!({
                            "swarm_id": &swarm_id_fwd,
                            "event_type": event_type,
                            "feature_id": evt.feature_id,
                            "message": evt.message,
                            "metadata": evt.metadata,
                        }),
                    );
                    // Mirror nurse interventions to the `nurse-event` channel so
                    // they render inline in the originating Tasks-view conversation
                    // (when present in the metadata). For pure-swarm interventions
                    // with no `task_id`, the frontend filter simply drops them.
                    if matches!(
                        evt.event_type,
                        crate::state::progress::ProgressEventType::NurseIntervention
                    ) {
                        if let Some(metadata) = evt.metadata.as_ref() {
                            let mut envelope = serde_json::Map::new();
                            envelope.insert(
                                "event_type".to_string(),
                                serde_json::Value::String("Lifecycle".to_string()),
                            );
                            if let serde_json::Value::Object(fields) = metadata {
                                for (k, v) in fields {
                                    envelope.insert(k.clone(), v.clone());
                                }
                            }
                            let _ =
                                app_emitter.emit("nurse-event", serde_json::Value::Object(envelope));
                        }
                    }
                }
            }
        )
        .instrument(tracing::Span::current()),
    );

    // Per-agent live activity channel. Each running agent (Scout/Worker/Guard)
    // forwards its Pi events through this channel; one task drains it and
    // emits each payload to the frontend as a `swarm-activity` event so the
    // Swarms page can render Task-view-style streams.
    //
    // Text and thinking deltas are coalesced per (session_id, kind) with a
    // 50ms / 256-byte cap so the webview isn't flooded with one IPC per
    // token. Mirrors the chat.rs DeltaCoalescer pattern; the swarm variant
    // is multi-session because multiple agents stream concurrently.
    //
    // Bounded at 4096 so a slow frontend (or a webview that's frozen because
    // the user backgrounded the app) can't buffer 10s of MB of swarm activity
    // payloads in memory. Producers use `try_send_activity` (see
    // `core::queen`) which drops with a rate-limited warn on `Full`.
    let (activity_tx, mut activity_rx) = mpsc::channel::<serde_json::Value>(4096);
    let app_activity = app.clone();
    // Per-swarm append-only activity transcript. Constructed here (sync I/O,
    // cheap on swarm start) and moved into the forwarder task so each
    // about-to-be-emitted payload is persisted with its assigned `seq`.
    // A construction failure does NOT abort the swarm — the live stream
    // still works without persistence, the frontend just can't replay
    // history. The forwarder logs the disabled state once and continues.
    let activity_log_path = state.swarm_store.activity_log_path(&swarm_id);
    let activity_writer = match ActivityWriter::new(&activity_log_path) {
        Ok(w) => Some(w),
        Err(e) => {
            tracing::warn!(
                swarm_id = %swarm_id,
                error = %e,
                path = %activity_log_path.display(),
                "failed to open activity log writer; activity will not be persisted"
            );
            None
        }
    };
    // Audit 2.12: panic-safe wrapper. The activity forwarder coalesces
    // text/thinking deltas — a panic here silently drops live token streams.
    // On panic, emit a synthetic `failed` swarm-event and mark the swarm
    // Failed.
    let panic_app_act = app.clone();
    let panic_swarm_id_act = swarm_id.clone();
    let panic_store_act = store_for_queen.clone();
    let panic_registry_act = registry.clone();
    tokio::spawn(crate::supervise!(
        context = format!("swarm={} component=activity_forwarder", panic_swarm_id_act),
        on_panic = move |panic_msg: String| {
            let _ = panic_app_act.emit(
                "swarm-event",
                serde_json::json!({
                    "swarm_id": &panic_swarm_id_act,
                    "event_type": "failed",
                    "message": format!("internal task panicked (activity_forwarder): {panic_msg}"),
                }),
            );
            let pid = panic_swarm_id_act.clone();
            let preg = panic_registry_act.clone();
            let pstore = panic_store_act.clone();
            tokio::spawn(async move {
                preg.update_status(&pid, SwarmStatus::Failed).await;
                if let Some(s) = preg.get_state(&pid).await {
                    let _ = pstore.write_state(&pid, &s).await;
                }
            });
        },
        async move {
            #[cfg(test)]
            crate::util::supervise::maybe_panic_for_test("swarms_activity_forwarder");
        use std::collections::HashMap;
        use std::time::{Duration, Instant};

        const FLUSH_BYTES: usize = 256;
        const FLUSH_INTERVAL: Duration = Duration::from_millis(50);
        const TICK_INTERVAL: Duration = Duration::from_millis(25);

        // (session_id, kind) -> (buffered text, batch start time, frozen
        // metadata cloned from the first delta; the `text` field is
        // re-inserted at flush time with the accumulated buffer).
        type Key = (String, &'static str);
        let mut buffers: HashMap<
            Key,
            (String, Instant, serde_json::Map<String, serde_json::Value>),
        > = HashMap::new();

        // Persist + emit one fully-formed payload (object map). The writer
        // injects `"seq": N` so the persisted line and the IPC-emitted
        // payload carry the same monotonic sequence number. A persistence
        // failure is warn-logged and we still emit — the live stream
        // outranks crash-recovery if the disk is unhappy.
        // Returns whether we wrote anything (callers track this to decide
        // if a drain-edge fsync is warranted).
        fn persist_and_emit(
            writer: &mut Option<ActivityWriter>,
            app: &tauri::AppHandle,
            map: serde_json::Map<String, serde_json::Value>,
        ) -> bool {
            let mut value = serde_json::Value::Object(map);
            let mut did_write = false;
            if let Some(w) = writer.as_mut() {
                match w.append(&mut value) {
                    Ok(_seq) => {
                        did_write = true;
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "failed to persist activity event; emitting without seq"
                        );
                    }
                }
            }
            let _ = app.emit("swarm-activity", value);
            did_write
        }

        let mut writer = activity_writer;
        // Tracks whether we've appended anything since the last fsync. We
        // fsync once per channel drain (recv returned an event AND the
        // queue is now empty) rather than per write — keeps the crash-loss
        // window at the same ~50ms cadence as the coalescer without
        // paying ~1-3ms fsync cost per delta.
        let mut writes_pending_sync = false;

        let mut ticker = tokio::time::interval(TICK_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                recv = activity_rx.recv() => {
                    let Some(payload) = recv else {
                        // Channel closed; flush everything and exit.
                        for (_k, entry) in buffers.drain() {
                            let (buf, _started, mut template) = entry;
                            template.insert("text".to_string(), serde_json::Value::String(buf));
                            if persist_and_emit(&mut writer, &app_activity, template) {
                                writes_pending_sync = true;
                            }
                        }
                        if writes_pending_sync {
                            if let Some(w) = writer.as_mut() {
                                if let Err(e) = w.flush_and_sync() {
                                    tracing::warn!(
                                        error = %e,
                                        "final fsync of activity log failed"
                                    );
                                }
                            }
                        }
                        break;
                    };
                    let Some(obj) = payload.as_object() else {
                        // Non-object payload — wrap-and-persist via persist_and_emit
                        // so the seq still gets attached.
                        let mut wrap = serde_json::Map::new();
                        wrap.insert("payload".to_string(), payload);
                        if persist_and_emit(&mut writer, &app_activity, wrap) {
                            writes_pending_sync = true;
                        }
                        continue;
                    };
                    let kind = obj.get("kind").and_then(|v| v.as_str()).unwrap_or("");
                    let session_id = obj
                        .get("session_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();

                    if (kind == "text" || kind == "thinking") && !session_id.is_empty() {
                        let static_kind: &'static str =
                            if kind == "text" { "text" } else { "thinking" };
                        let other_kind: &'static str =
                            if static_kind == "text" { "thinking" } else { "text" };
                        // Preserve in-session ordering between text and
                        // thinking by flushing the OTHER kind first.
                        let other_key: Key = (session_id.clone(), other_kind);
                        if let Some(entry) = buffers.remove(&other_key) {
                            let (buf, _started, mut template) = entry;
                            template.insert("text".to_string(), serde_json::Value::String(buf));
                            if persist_and_emit(&mut writer, &app_activity, template) {
                                writes_pending_sync = true;
                            }
                        }

                        let text = obj
                            .get("text")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let key: Key = (session_id.clone(), static_kind);
                        let should_flush = match buffers.get_mut(&key) {
                            Some(entry) => {
                                entry.0.push_str(&text);
                                entry.0.len() >= FLUSH_BYTES
                            }
                            None => {
                                let mut template = obj.clone();
                                template.remove("text");
                                let len = text.len();
                                buffers.insert(key.clone(), (text, Instant::now(), template));
                                len >= FLUSH_BYTES
                            }
                        };
                        if should_flush {
                            if let Some(entry) = buffers.remove(&key) {
                                let (buf, _started, mut template) = entry;
                                template.insert("text".to_string(), serde_json::Value::String(buf));
                                if persist_and_emit(&mut writer, &app_activity, template) {
                                    writes_pending_sync = true;
                                }
                            }
                        }
                    } else {
                        // Non-coalesced event (agent_start/end, tool_*, error).
                        // Flush both text and thinking buffers for this
                        // session first so ordering is preserved.
                        if !session_id.is_empty() {
                            for k in ["text", "thinking"] {
                                let key: Key = (session_id.clone(), k);
                                if let Some(entry) = buffers.remove(&key) {
                                    let (buf, _started, mut template) = entry;
                                    template.insert(
                                        "text".to_string(),
                                        serde_json::Value::String(buf),
                                    );
                                    if persist_and_emit(&mut writer, &app_activity, template) {
                                        writes_pending_sync = true;
                                    }
                                }
                            }
                        }
                        let owned = obj.clone();
                        if persist_and_emit(&mut writer, &app_activity, owned) {
                            writes_pending_sync = true;
                        }
                    }

                    // Drain edge: if the channel is now empty AND we've
                    // accumulated writes, fsync. Bounds the crash-loss
                    // window at the coalescer cadence without per-write
                    // sync overhead.
                    if writes_pending_sync && activity_rx.is_empty() {
                        if let Some(w) = writer.as_mut() {
                            if let Err(e) = w.flush_and_sync() {
                                tracing::warn!(
                                    error = %e,
                                    "fsync of activity log failed; continuing"
                                );
                            }
                        }
                        writes_pending_sync = false;
                    }
                }
                _ = ticker.tick() => {
                    let now = Instant::now();
                    let to_flush: Vec<Key> = buffers
                        .iter()
                        .filter_map(|(k, (buf, started, _))| {
                            if buf.len() >= FLUSH_BYTES
                                || now.duration_since(*started) >= FLUSH_INTERVAL
                            {
                                Some(k.clone())
                            } else {
                                None
                            }
                        })
                        .collect();
                    for k in to_flush {
                        if let Some(entry) = buffers.remove(&k) {
                            let (buf, _started, mut template) = entry;
                            template.insert("text".to_string(), serde_json::Value::String(buf));
                            if persist_and_emit(&mut writer, &app_activity, template) {
                                writes_pending_sync = true;
                            }
                        }
                    }
                    // Tick-driven flushes still warrant a drain-edge fsync
                    // when the receiver is idle — otherwise a slow-but-
                    // steady trickle of writes could sit in the kernel
                    // page cache indefinitely.
                    if writes_pending_sync && activity_rx.is_empty() {
                        if let Some(w) = writer.as_mut() {
                            if let Err(e) = w.flush_and_sync() {
                                tracing::warn!(
                                    error = %e,
                                    "fsync of activity log failed; continuing"
                                );
                            }
                        }
                        writes_pending_sync = false;
                    }
                }
            }
        }
        }
    ).instrument(tracing::Span::current()));

    // Persist ProgressEvents to disk so `get_swarm_progress` (and crash-
    // recovery in the future) can replay them. Opens the JSONL log in
    // append mode and writes one line per event.
    let progress_log_path = state
        .swarm_store
        .swarm_dir(&swarm_id)
        .join("progress_log.jsonl");
    let progress_writer = match ProgressWriter::new(&progress_log_path).await {
        Ok(w) => Some(Arc::new(w)),
        Err(e) => {
            tracing::warn!(
                swarm_id = %swarm_id,
                error = %e,
                "failed to open progress log writer; progress will not be persisted"
            );
            None
        }
    };
    if let Some(writer) = progress_writer.clone() {
        // Audit 2.12: panic-safe wrapper. A panic in the persistence task
        // silently breaks crash-recovery — log it explicitly and emit a
        // synthetic failure event.
        let panic_app_pw = app.clone();
        let panic_swarm_id_pw = swarm_id.clone();
        let panic_store_pw = store_for_queen.clone();
        let panic_registry_pw = registry.clone();
        let progress_writer_ctx = panic_swarm_id_pw.clone();
        tokio::spawn(
            crate::supervise!(
                context = format!("swarm={} component=progress_writer", progress_writer_ctx),
                on_panic = move |panic_msg: String| {
                    let _ = panic_app_pw.emit(
                        "swarm-event",
                        serde_json::json!({
                            "swarm_id": &panic_swarm_id_pw,
                            "event_type": "failed",
                            "message": format!("internal task panicked (progress_writer): {panic_msg}"),
                        }),
                    );
                    let pid = panic_swarm_id_pw.clone();
                    let preg = panic_registry_pw.clone();
                    let pstore = panic_store_pw.clone();
                    tokio::spawn(async move {
                        preg.update_status(&pid, SwarmStatus::Failed).await;
                        if let Some(s) = preg.get_state(&pid).await {
                            let _ = pstore.write_state(&pid, &s).await;
                        }
                    });
                },
                async move {
                    #[cfg(test)]
                    crate::util::supervise::maybe_panic_for_test("swarms_progress_writer");
                    loop {
                        let evt = match event_rx_persist.recv().await {
                            Ok(evt) => evt,
                            Err(broadcast::error::RecvError::Lagged(n)) => {
                                warn!(
                                    dropped = n,
                                    "progress-log broadcast receiver lagged \u{2014} skipping dropped events"
                                );
                                continue;
                            }
                            Err(broadcast::error::RecvError::Closed) => break,
                        };
                        if let Err(e) = writer.log(&evt).await {
                            tracing::warn!(error = %e, "failed to persist progress event");
                        }
                    }
                    // Final flush on channel close so the tail of the log isn't lost.
                    let _ = writer.flush().await;
                }
            )
            .instrument(tracing::Span::current()),
        );
    }

    // Build the shared state handles that `run_swarm_full` expects.
    // `swarm_state` is cloned because we also pass the original into the
    // registry below; `features` is moved since it's no longer needed here.
    let swarm_state_arc = Arc::new(RwLock::new(swarm_state.clone()));
    let features_arc = Arc::new(RwLock::new(features));
    let queen_config = {
        let mut cfg = QueenConfig::default();
        let n = swarm_state.model_settings.max_concurrent_features.max(1) as usize;
        // Clamp to a sane upper bound matching the Pi pool semaphore. Override
        // via `HYVEMIND_SWARM_FEATURE_PARALLELISM` (default 6).
        cfg.max_concurrent_features = n.min(tunables::swarm_feature_parallelism_max());
        // Phase 5A: per-swarm budget comes from the swarm's persisted
        // ModelSettings. Daily budget is a global setting read from
        // Config. None on either side means unlimited.
        cfg.swarm_budget_usd = swarm_state.model_settings.swarm_budget_usd;
        {
            let config = state.config.read().await;
            cfg.daily_budget_usd = config.daily_budget_usd;
        }
        cfg
    };

    // Register an in-memory usage accumulator for live token tracking.
    // This is shared between the queen task's `run_swarm_full` and the
    // `get_swarm_usage` command so the frontend sees combined DB + live totals.
    let accumulator = state
        .swarm_registry
        .register_usage_accumulator(&swarm_id)
        .await;

    // Register the swarm BEFORE spawning the queen so the queen can fetch
    // its pause handles. The state already carries Implementing status.
    state
        .swarm_registry
        .register(swarm_id.clone(), swarm_state, cancel_token.clone())
        .await;

    let pause_handles = state
        .swarm_registry
        .get_pause_handles(&swarm_id)
        .await
        .map(|(notify, paused)| crate::core::queen::PauseHandles { paused, notify });

    // Spawn the queen orchestrator task.
    let cancel_clone = cancel_token.clone();
    let swarm_id_queen = swarm_id.clone();
    let registry_queen = registry.clone();
    let event_tx_queen = event_tx.clone();
    // Kept outside the `run_swarm_full` move so the swarm-fail join handler
    // can publish a synthetic `SwarmFailed` progress event after the Queen
    // task returns Err — including the early-abort case where
    // `Scheduler::new` (cycle detection) fails before `SwarmStarted` ever
    // fires, which would otherwise leave `progress_log.jsonl` with only the
    // schema header and `state.error` as `null`.
    let event_tx_failed = event_tx.clone();
    let activity_tx_queen = activity_tx.clone();
    let acc_queen = accumulator.clone();
    let milestones_for_queen = milestones.clone();
    let validation_for_queen = validation_assertions.clone();
    let app_for_final = app.clone();
    // Bundle subsystem handles the Queen needs to run an in-swarm Hivemind
    // review of each Scout's plan when `use_hivemind_on_scout` is set. Only
    // populated when an API key for at least one provider is configured —
    // otherwise the engine would no-op anyway, and the run-time check in
    // `run_feature_full` falls back to the Scout plan with a Skipped event.
    let scout_review_ctx = Some(crate::core::scout_review::ScoutReviewContext {
        hivemind_store: state.hivemind_store.clone(),
        provider_registry: state.provider_registry.clone(),
        usage_store: state.usage_store.clone(),
        app_handle: app.clone(),
        pi_manager: state.pi_manager.clone(),
        merge_capture_registry: state.merge_capture.clone(),
        reviews_dir: state.reviews_dir.clone(),
        nurse_engine: state.nurse_engine().cloned(),
        config: state.config.clone(),
        response_cache: std::sync::Arc::clone(&state.response_cache),
    });
    // Audit 2.12: panic-safe wrapper. The Queen orchestrator owns the
    // outer swarm loop — a panic in here is the worst case for the audit
    // (silent half-finished swarm). We use `run_supervised` directly
    // (rather than the `supervise!` macro) so the spawned task can still
    // return `anyhow::Result<()>` — `set_handle` needs that exact type so
    // `stop().await` can log a precise error when the queen exits Err.
    // On panic we synthesise an Err so `stop()` sees it the same way.
    let panic_app_q = app.clone();
    let panic_swarm_id_q = swarm_id.clone();
    let panic_store_q = store_for_queen.clone();
    let panic_registry_q = registry.clone();
    let queen_panic_ctx = panic_swarm_id_q.clone();
    let event_tx_failed_outer = event_tx_failed.clone();
    let queen_handle: JoinHandle<anyhow::Result<()>> = tokio::spawn(async move {
        let supervised_result = crate::util::supervise::run_supervised(async move {
            #[cfg(test)]
            crate::util::supervise::maybe_panic_for_test("swarms_queen_orchestrator");
            let result = run_swarm_full(
                queen_config,
                swarm_state_arc,
                features_arc,
                milestones_for_queen,
                validation_for_queen,
                pi_manager,
                store_for_queen.clone(),
                event_tx_queen,
                cancel_clone,
                Some(usage_store),
                Some(activity_tx_queen),
                Some(acc_queen),
                pause_handles,
                scout_review_ctx,
            )
            .await;

            // Determine final status from the result. When the Queen exits Err
            // we must do three things the original branch skipped: persist the
            // error string into `state.error` (so the UI knows *why*), emit a
            // synthetic `SwarmFailed` progress event (so `progress_log.jsonl`
            // has a failure marker even when we aborted before `SwarmStarted`,
            // e.g. on a `Scheduler::new` cycle), and carry the message into the
            // `swarm-event` IPC payload so SwarmControl can render a banner.
            let err_str = result.as_ref().err().map(|e| format!("{e:#}"));
            let final_status = match &result {
                Ok(()) => {
                    // run_swarm sets Completed/Failed/Cancelled internally;
                    // read persisted state or default to Completed
                    SwarmStatus::Completed
                }
                Err(_) => {
                    let err = err_str.clone().unwrap_or_default();
                    tracing::error!(
                        swarm_id = %swarm_id_queen,
                        error = %err,
                        "swarm execution failed",
                    );
                    registry_queen.set_error(&swarm_id_queen, err.clone()).await;
                    let evt = ProgressEvent::new(
                        crate::state::progress::ProgressEventType::SwarmFailed,
                        swarm_id_queen.clone(),
                        err,
                    );
                    let _ = event_tx_failed.send(evt);
                    SwarmStatus::Failed
                }
            };

            // On the success path the in-memory state was already mutated by
            // `run_swarm_full` (see queen.rs ~L922-L940). On the failure path
            // `set_error` above did the equivalent. Either way we now need to
            // write the in-memory snapshot back to disk so it survives restart.
            if matches!(result, Ok(())) {
                registry_queen
                    .update_status(&swarm_id_queen, final_status.clone())
                    .await;
            }
            if let Some(final_state) = registry_queen.get_state(&swarm_id_queen).await {
                if let Err(e) = store_for_queen
                    .write_state(&swarm_id_queen, &final_state)
                    .await
                {
                    tracing::warn!(
                        swarm_id = %swarm_id_queen,
                        error = %e,
                        "failed to persist final swarm status to disk"
                    );
                }
            }

            // Clean up the usage accumulator now that the queen has finished.
            registry_queen
                .remove_usage_accumulator(&swarm_id_queen)
                .await;

            let _ = app_for_final.emit(
                "swarm-event",
                serde_json::json!({
                    "swarm_id": swarm_id_queen,
                    "event_type": format!("{}", final_status).to_lowercase(),
                    "message": err_str,
                }),
            );
            info!(swarm_id = %swarm_id_queen, status = %final_status, "swarm execution finished");
            // Surface the inner result through the `JoinHandle` so the registry's
            // `stop()` can log a precise error message if the queen exits Err.
            result
        })
        .await;
        match supervised_result {
            Ok(r) => r,
            Err(panic_msg) => {
                tracing::error!(
                    context = %format!("swarm={} component=queen_orchestrator", queen_panic_ctx),
                    panic = %panic_msg,
                    "queen orchestrator PANICKED — surfacing as Err for stop()/UI"
                );
                let err_msg = format!("queen orchestrator panicked: {panic_msg}");
                let _ = panic_app_q.emit(
                    "swarm-event",
                    serde_json::json!({
                        "swarm_id": &panic_swarm_id_q,
                        "event_type": "failed",
                        "message": err_msg.clone(),
                    }),
                );
                // Best-effort: mark the swarm Failed in memory + on disk and
                // emit a synthetic `SwarmFailed` progress event so the on-disk
                // `progress_log.jsonl` carries the panic message instead of
                // ending without a failure marker (and `state.error` is no
                // longer null on a panic exit).
                panic_registry_q
                    .set_error(&panic_swarm_id_q, err_msg.clone())
                    .await;
                let evt = ProgressEvent::new(
                    crate::state::progress::ProgressEventType::SwarmFailed,
                    panic_swarm_id_q.clone(),
                    err_msg,
                );
                let _ = event_tx_failed_outer.send(evt);
                if let Some(s) = panic_registry_q.get_state(&panic_swarm_id_q).await {
                    let _ = panic_store_q.write_state(&panic_swarm_id_q, &s).await;
                }
                Err(anyhow::anyhow!(
                    "queen orchestrator panicked: {}",
                    panic_msg
                ))
            }
        }
    });

    // Wire the queen's `JoinHandle` back into the registry so `stop_swarm`
    // (and `shutdown_all`) can actually await the queen to flush its writes
    // before returning. Without this `stop()` would cancel the token but
    // return to the UI while the queen kept running in the background.
    if let Err(e) = state
        .swarm_registry
        .set_handle(&swarm_id, queen_handle)
        .await
    {
        tracing::warn!(
            swarm_id = %swarm_id,
            error = %e,
            "failed to attach queen JoinHandle to registry; stop_swarm will not be able to await the queen"
        );
    }

    info!(swarm_id = %swarm_id, "swarm started");
    Ok(())
}

/// Apply an edit to a `SwarmState`, validating the new values.
///
/// Extracted from `update_swarm` so unit tests can exercise the
/// validation/merge logic without a `tauri::State<AppState>` fixture.
/// Returns the updated state on success or a user-facing error string.
pub fn apply_update(
    mut current: SwarmState,
    name: String,
    working_directory: String,
    model_settings: serde_json::Value,
) -> Result<SwarmState, String> {
    // Block updates on running swarms — in-flight queen/scout/worker tasks
    // may already be operating with stale settings, and changing the working
    // directory underneath a live run is unsafe.
    if current.status == SwarmStatus::Implementing {
        return Err("cannot edit a running swarm; pause or stop first".into());
    }

    // Validate name: reject empty/whitespace, cap length.
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("name is empty".into());
    }
    if name.len() > MAX_NAME_LEN {
        return Err(format!(
            "name too long: {} bytes (max {})",
            name.len(),
            MAX_NAME_LEN
        ));
    }

    // Validate the working directory the same way create_swarm does.
    let canonical_working_dir = validate_working_dir(&working_directory)?;
    let canonical_working_dir_str = canonical_working_dir.display().to_string();

    // Parse model settings; default if absent so a partial payload still works.
    let settings: ModelSettings = serde_json::from_value(model_settings)
        .map_err(|e| format!("invalid model_settings: {}", e))?;

    // Mirror start_swarm's hivemind invariant.
    if (settings.use_hivemind_on_scout || settings.use_hivemind_on_queen)
        && settings.hivemind_id.is_none()
    {
        return Err(
            "hivemind_id must be set when use_hivemind_on_scout or use_hivemind_on_queen is true"
                .into(),
        );
    }

    current.name = trimmed.to_string();
    current.working_directory = canonical_working_dir_str;
    current.model_settings = settings;
    current.updated_at = chrono::Utc::now();
    Ok(current)
}

/// Update an existing swarm's metadata and model settings.
///
/// Validates the inputs, refuses to edit a swarm that is currently
/// `Implementing`, persists the updated state to disk, and syncs the
/// in-memory registry so `list_swarms` immediately reflects the change.
#[tracing::instrument(skip(state))]
#[tauri::command]
pub async fn update_swarm(
    state: tauri::State<'_, AppState>,
    swarm_id: String,
    name: String,
    working_directory: String,
    model_settings: serde_json::Value,
) -> Result<SwarmState, IpcError> {
    validate_id(&swarm_id).map_err(IpcError::validation)?;
    info!(
        swarm_id = %swarm_id,
        name = %name,
        working_dir = %working_directory,
        "update_swarm invoked"
    );

    // Bound the inbound JSON payload before any work.
    check_payload_size(&model_settings).map_err(IpcError::validation)?;

    // Audit 1.11: enforce the approved-dirs allowlist BEFORE delegating to
    // the pure `apply_update` helper. We canonicalize via the allowlist
    // helper, then hand the canonical path to apply_update so the inner
    // function's own canonicalize-and-validate call stays a no-op.
    let canonical = validate_working_dir_with_allowlist(&state, &working_directory)
        .await
        .map_err(IpcError::not_approved)?;
    let working_directory = canonical.display().to_string();

    // Load current state: registry first, then disk.
    let current = match state.swarm_registry.get_state(&swarm_id).await {
        Some(s) => s,
        None => state
            .swarm_store
            .read_state(&swarm_id)
            .await
            .map_err(|e| {
                IpcError::internal(format!("failed to read swarm '{}' state: {}", swarm_id, e))
                    .with_id(swarm_id.clone())
            })?
            .ok_or_else(|| IpcError::not_found("swarm", swarm_id.clone()))?,
    };

    let updated = apply_update(current, name, working_directory, model_settings)
        .map_err(IpcError::validation)?;

    state
        .swarm_store
        .write_state(&swarm_id, &updated)
        .await
        .map_err(|e| {
            IpcError::internal(format!("failed to persist updated swarm state: {}", e))
                .with_id(swarm_id.clone())
        })?;

    // Sync the in-memory registry. If the swarm isn't registered (e.g. it
    // was created in a previous session and never started), the disk write
    // alone is sufficient — swallow the "not registered" error.
    if let Err(e) = state
        .swarm_registry
        .replace_state(&swarm_id, updated.clone())
        .await
    {
        tracing::debug!(
            swarm_id = %swarm_id,
            error = %e,
            "swarm not in registry; disk write is authoritative"
        );
    }

    info!(swarm_id = %swarm_id, "swarm updated");
    Ok(updated)
}

/// Policy: how an idempotent stop should resolve based on persisted state.
///
/// Used when the registry has no entry for the swarm — the disk state then
/// determines whether the stop is a no-op, requires a status write, or is a
/// genuine "not found" error.
#[derive(Debug, PartialEq, Eq)]
enum StopOutcome {
    /// Disk record exists in a non-terminal state — write it as `Cancelled`.
    UpdatedDisk,
    /// Disk record exists and is already terminal — true no-op.
    NoOp,
    /// No disk record — report "not found".
    NotFound,
}

/// Decide what to do when the registry has no entry for a swarm being
/// stopped, based on its persisted state (if any).
fn resolve_stop_from_disk(state: Option<&SwarmState>) -> StopOutcome {
    match state {
        None => StopOutcome::NotFound,
        Some(s) => match s.status {
            SwarmStatus::Planning
            | SwarmStatus::Implementing
            | SwarmStatus::Paused
            | SwarmStatus::Interrupted => StopOutcome::UpdatedDisk,
            SwarmStatus::Completed | SwarmStatus::Failed | SwarmStatus::Cancelled => {
                StopOutcome::NoOp
            }
        },
    }
}

/// Pause a running swarm.
///
/// Idempotent: if the swarm is not currently in the registry (e.g. it was
/// only ever persisted, or has already finished), this is a no-op as long as
/// the swarm exists on disk. Returns an error only when the swarm is genuinely
/// unknown.
#[tracing::instrument(skip(state))]
#[tauri::command]
pub async fn pause_swarm(
    state: tauri::State<'_, AppState>,
    swarm_id: String,
) -> Result<(), IpcError> {
    validate_id(&swarm_id).map_err(IpcError::validation)?;
    info!(swarm_id = %swarm_id, "pause_swarm invoked");

    if state.swarm_registry.is_running(&swarm_id).await {
        return state
            .swarm_registry
            .pause(&swarm_id)
            .await
            .map_err(|e| IpcError::internal(e.to_string()).with_id(swarm_id.clone()));
    }

    // Not in registry — verify on disk and treat as no-op.
    let disk = state.swarm_store.read_state(&swarm_id).await.map_err(|e| {
        IpcError::internal(format!("failed to read swarm state: {}", e)).with_id(swarm_id.clone())
    })?;
    if disk.is_none() {
        return Err(IpcError::not_found("swarm", swarm_id.clone()));
    }
    info!(swarm_id = %swarm_id, "pause_swarm: swarm not running, no-op");
    Ok(())
}

/// Resume a paused or interrupted swarm.
///
/// Fast path: an in-memory queen task is paused — simply wake it via the
/// registry. Slow path: no queen task exists (process restart killed it),
/// so rehydrate features.json / milestones.json from disk, reset any
/// mid-flight feature statuses (Scouting/Implementing/Reviewing/Validating)
/// to Pending, flip swarm status back to Implementing, and spawn a fresh
/// queen task that picks up from the next ready feature.
#[tracing::instrument(skip(app, state))]
#[tauri::command]
pub async fn resume_swarm(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    swarm_id: String,
) -> Result<(), IpcError> {
    validate_id(&swarm_id).map_err(IpcError::validation)?;
    info!(swarm_id = %swarm_id, "resume_swarm invoked");

    // Fast path: registered + active (Implementing or Paused).
    // For Paused: wakes the existing queen via pause_token.
    // For Implementing: no-op (swarm is already running; resume()
    // warns and returns Ok).
    if state.swarm_registry.is_active(&swarm_id).await {
        return state
            .swarm_registry
            .resume(&swarm_id)
            .await
            .map_err(|e| IpcError::internal(e.to_string()).with_id(swarm_id.clone()));
    }

    // Slow path: rehydrate from disk under the per-swarm start lock so
    // concurrent Resume / Start clicks serialise.
    let start_lock = state.swarm_registry.get_start_lock(&swarm_id).await;
    let _guard = start_lock.lock().await;

    // Re-check after acquiring the lock — another resume may have raced.
    if state.swarm_registry.is_active(&swarm_id).await {
        return Err(
            IpcError::validation(format!("swarm '{}' is already running", swarm_id))
                .with_id(swarm_id.clone()),
        );
    }

    let mut swarm_state = state
        .swarm_store
        .read_state(&swarm_id)
        .await
        .map_err(|e| {
            IpcError::internal(format!("failed to read swarm state: {}", e))
                .with_id(swarm_id.clone())
        })?
        .ok_or_else(|| IpcError::not_found("swarm", swarm_id.clone()))?;

    if !swarm_state.status.is_resumable() {
        return Err(IpcError::validation(format!(
            "swarm '{}' is in state '{}' which cannot be resumed",
            swarm_id, swarm_state.status
        ))
        .with_id(swarm_id.clone()));
    }

    // Re-validate working_directory — could have been moved/deleted while
    // the app was closed, or removed from the approved-dirs allowlist
    // (audit 1.11).
    let canonical =
        validate_working_dir_with_allowlist(&state, &swarm_state.working_directory).await?;
    swarm_state.working_directory = canonical.display().to_string();

    // Load persisted features and milestones. Validators are already
    // injected on disk, so we do NOT call inject_milestone_validators.
    let mut features = state
        .swarm_store
        .read_features(&swarm_id)
        .await
        .map_err(|e| format!("failed to read features: {}", e))?;
    let milestones = state
        .swarm_store
        .read_milestones(&swarm_id)
        .await
        .map_err(|e| format!("failed to read milestones: {}", e))?;

    // Branch on the original (pre-mutation) swarm status: terminal
    // failures get the full retry treatment (terminal-Failed features
    // re-Pended with a fresh fix_attempt_count); paused/interrupted
    // resumes preserve terminal failures and fix_attempt_count.
    let original_status = swarm_state.status.clone();
    let reset_count = if matches!(
        original_status,
        SwarmStatus::Failed | SwarmStatus::Cancelled
    ) {
        // Full reset: in-flight features, crash-interrupted features,
        // and terminal-failed features all get re-Pended with a fresh
        // fix_attempt_count so the swarm can meaningfully restart.
        reset_features_for_full_resume(&mut features)
    } else {
        // Existing behaviour for Paused / Interrupted: only re-Pend
        // in-flight and crash-interrupted features; preserve terminal
        // failures and fix_attempt_count.
        reset_in_flight_features(&mut features)
    };
    tracing::info!(
        swarm_id = %swarm_id,
        reset_count,
        "resume_swarm: rehydrated from disk"
    );

    // Recompute deterministic VAL-* ids from milestones.
    let validation_assertions = assign_assertion_ids(&milestones);

    // Persist reset features back to disk so a subsequent crash leaves a
    // consistent on-disk picture.
    state
        .swarm_store
        .write_features(&swarm_id, &features)
        .await
        .map_err(|e| format!("failed to persist reset features: {}", e))?;

    // Edge case: a Failed/Cancelled swarm with nothing to reset (e.g. all
    // features already terminal because the swarm failed for a
    // non-feature reason like budget exceeded) should transition directly
    // to Completed instead of spawning a queen with nothing to do.
    if reset_count == 0
        && matches!(
            original_status,
            SwarmStatus::Failed | SwarmStatus::Cancelled
        )
    {
        swarm_state.error = None;
        swarm_state.set_status(SwarmStatus::Completed);
        state
            .swarm_store
            .write_state(&swarm_id, &swarm_state)
            .await
            .map_err(|e| format!("failed to persist completed swarm state: {}", e))?;
        let _ = app.emit(
            "swarm-event",
            serde_json::json!({
                "swarm_id": &swarm_id,
                "event_type": "resumed",
                "message": "Swarm resumed \u{2014} all features already completed; transitioned to Completed".to_string(),
            }),
        );
        return Ok(());
    }

    // Flip status to Implementing and clear the interruption error BEFORE
    // spawning so a failure in the spawn path leaves a sane on-disk record.
    swarm_state.error = None;
    swarm_state.set_status(SwarmStatus::Implementing);
    state
        .swarm_store
        .write_state(&swarm_id, &swarm_state)
        .await
        .map_err(|e| format!("failed to persist resumed swarm state: {}", e))?;

    // Best-effort frontend notification that resume rehydrated from disk.
    let message = if matches!(
        original_status,
        SwarmStatus::Failed | SwarmStatus::Cancelled
    ) {
        format!(
            "Swarm resumed; {} feature(s) reset to Pending with fresh retry budget",
            reset_count
        )
    } else {
        format!(
            "Swarm resumed from interrupted state; {} feature(s) reset to Pending",
            reset_count
        )
    };
    let _ = app.emit(
        "swarm-event",
        serde_json::json!({
            "swarm_id": &swarm_id,
            "event_type": "resumed",
            "message": message,
        }),
    );

    spawn_queen_task(
        &app,
        &state,
        swarm_state,
        features,
        milestones,
        validation_assertions,
    )
    .await
    .map_err(IpcError::internal)
}

/// Stop a running swarm and clean up its resources.
///
/// Idempotent: handles three cases for callers who may be operating on
/// stale UI state.
/// 1. Registry has the swarm → delegate to `registry.stop`.
/// 2. Registry does not, but disk shows a non-terminal status → mark
///    `Cancelled` on disk and succeed.
/// 3. Registry does not, and disk is terminal → no-op success.
/// Only returns `Err` when the swarm is unknown to both registry and disk.
#[tracing::instrument(skip(state))]
#[tauri::command]
pub async fn stop_swarm(
    state: tauri::State<'_, AppState>,
    swarm_id: String,
) -> Result<(), IpcError> {
    validate_id(&swarm_id).map_err(IpcError::validation)?;
    info!(swarm_id = %swarm_id, "stop_swarm invoked");

    if state.swarm_registry.is_running(&swarm_id).await {
        // Cancel the queen + abort feature tasks via the registry. The
        // registry will await the queen handle with a bounded timeout and
        // force-kill Pi sessions itself if the wait times out.
        let stop_result = state
            .swarm_registry
            .stop(&swarm_id, Some(&state.pi_manager))
            .await
            .map_err(|e| IpcError::internal(e.to_string()).with_id(swarm_id.clone()));
        // Force-kill any Pi subprocesses still owned by this swarm so the
        // current LLM calls don't continue to completion in the background.
        // Best-effort: even if `registry.stop` errored above, we'd rather
        // clean up the sessions than leak them. This is also a no-op when
        // the registry already killed them on timeout above.
        state.pi_manager.kill_sessions_for_swarm(&swarm_id).await;
        return stop_result;
    }

    // Not in registry — but Pi sessions could still be alive if the queen
    // task died without cleanly removing them. Sweep them too.
    state.pi_manager.kill_sessions_for_swarm(&swarm_id).await;

    // Not in registry — consult disk.
    let disk = state.swarm_store.read_state(&swarm_id).await.map_err(|e| {
        IpcError::internal(format!("failed to read swarm state: {}", e)).with_id(swarm_id.clone())
    })?;
    match resolve_stop_from_disk(disk.as_ref()) {
        StopOutcome::NotFound => Err(IpcError::not_found("swarm", swarm_id.clone())),
        StopOutcome::NoOp => {
            info!(swarm_id = %swarm_id, "stop_swarm: already terminal, no-op");
            Ok(())
        }
        StopOutcome::UpdatedDisk => {
            let mut s = disk.expect("checked Some above");
            let prior = s.status.clone();
            s.set_status(SwarmStatus::Cancelled);
            state
                .swarm_store
                .write_state(&swarm_id, &s)
                .await
                .map_err(|e| {
                    IpcError::internal(format!("failed to mark swarm cancelled: {}", e))
                        .with_id(swarm_id.clone())
                })?;
            info!(
                swarm_id = %swarm_id,
                %prior,
                "stop_swarm: marked stale swarm Cancelled on disk"
            );
            Ok(())
        }
    }
}

/// Get the current state of a swarm.
///
/// First checks the in-memory registry, then falls back to reading
/// persisted state from disk.
#[tracing::instrument(skip(state))]
#[tauri::command]
pub async fn get_swarm(
    state: tauri::State<'_, AppState>,
    swarm_id: String,
) -> Result<SwarmState, IpcError> {
    validate_id(&swarm_id).map_err(IpcError::validation)?;
    info!(swarm_id = %swarm_id, "get_swarm invoked");

    // Try in-memory registry first
    if let Some(swarm_state) = state.swarm_registry.get_state(&swarm_id).await {
        return Ok(swarm_state);
    }

    // Fall back to disk
    state
        .swarm_store
        .read_state(&swarm_id)
        .await
        .map_err(|e| {
            IpcError::internal(format!("failed to read swarm state: {}", e))
                .with_id(swarm_id.clone())
        })?
        .ok_or_else(|| IpcError::not_found("swarm", swarm_id.clone()))
}

// Reconcile / migrate helpers and their sentinel messages have moved to
// `crate::core::recovery`. Only `reconcile_orphaned_swarms` is still
// referenced from `list_swarms` below; new callers should import directly
// from `crate::core::recovery`.
use crate::core::recovery::reconcile_orphaned_swarms;

/// List all known swarms.
///
/// Combines actively running swarms from the registry with persisted
/// swarms from disk storage. Also performs a best-effort reconciliation
/// pass: any on-disk swarm marked `Implementing` that is no longer in the
/// in-memory registry is rewritten to `Failed` with an explanatory error.
#[tracing::instrument(skip(state))]
#[tauri::command]
pub async fn list_swarms(state: tauri::State<'_, AppState>) -> Result<Vec<SwarmState>, IpcError> {
    info!("list_swarms invoked");

    // Get running swarms from registry
    let mut swarms = state.swarm_registry.list_all().await;
    let running_ids: std::collections::HashSet<String> =
        swarms.iter().map(|s| s.id.clone()).collect();

    // Reconcile any disk-only swarms still marked Implementing (carry-over
    // from a previous session). This is idempotent — once rewritten to
    // Failed, subsequent calls are no-ops.
    let _ = reconcile_orphaned_swarms(&state.swarm_store, &running_ids).await;

    // Add persisted swarms that aren't currently running
    let persisted_ids = state
        .swarm_store
        .list_swarms()
        .await
        .map_err(|e| IpcError::internal(format!("failed to list persisted swarms: {}", e)))?;

    for sid in persisted_ids {
        if !running_ids.contains(&sid) {
            if let Ok(Some(persisted_state)) = state.swarm_store.read_state(&sid).await {
                swarms.push(persisted_state);
            }
        }
    }

    Ok(swarms)
}

/// Delete a swarm permanently.
///
/// If the swarm is currently running, it is stopped first. The swarm is
/// unregistered from the in-memory registry and all persisted files are
/// removed from disk. Returns an error only if file deletion fails (a
/// no-op on an already-deleted swarm).
#[tracing::instrument(skip(state))]
#[tauri::command]
pub async fn delete_swarm(
    state: tauri::State<'_, AppState>,
    swarm_id: String,
) -> Result<(), IpcError> {
    // Critical: delete_swarm calls swarm_store.delete_swarm(&swarm_id) which
    // recursively removes the directory ~/.hyvemind/swarms/{swarm_id}/. A
    // traversal payload (e.g. "../..") would otherwise delete arbitrary
    // directories.
    validate_id(&swarm_id).map_err(IpcError::validation)?;
    info!(swarm_id = %swarm_id, "delete_swarm invoked");

    // Defense in depth: reject ids that could resolve to a parent directory
    // and let remove_dir_all wipe data outside the swarms/ tree. Without
    // this, `swarm_id = ".."` would compute swarm_dir = base_dir/.. and
    // delete the entire ~/.hyvemind data directory.
    validate_id(&swarm_id).map_err(IpcError::validation)?;

    // Stop the swarm if it's currently running (this removes it from the registry).
    // If not running, just remove from the registry to be safe.
    if state.swarm_registry.is_running(&swarm_id).await {
        state
            .swarm_registry
            .stop(&swarm_id, Some(&state.pi_manager))
            .await
            .map_err(|e| {
                IpcError::internal(format!("failed to stop swarm before deletion: {}", e))
                    .with_id(swarm_id.clone())
            })?;
    } else {
        state.swarm_registry.remove(&swarm_id).await;
    }

    // Remove all persisted files from disk.
    state
        .swarm_store
        .delete_swarm(&swarm_id)
        .await
        .map_err(|e| {
            IpcError::internal(format!("failed to delete swarm data: {}", e))
                .with_id(swarm_id.clone())
        })?;

    info!(swarm_id = %swarm_id, "swarm deleted");
    Ok(())
}

/// Read the progress log for a swarm.
///
/// Returns the full list of progress events from the JSONL log on disk.
#[tracing::instrument(skip(state))]
#[tauri::command]
pub async fn get_swarm_progress(
    state: tauri::State<'_, AppState>,
    swarm_id: String,
) -> Result<Vec<ProgressEvent>, IpcError> {
    validate_id(&swarm_id).map_err(IpcError::validation)?;
    info!(swarm_id = %swarm_id, "get_swarm_progress invoked");

    let log_path = state
        .swarm_store
        .swarm_dir(&swarm_id)
        .join("progress_log.jsonl");

    ProgressReader::read_all(&log_path).map_err(|e| {
        IpcError::internal(format!("failed to read progress log: {}", e)).with_id(swarm_id.clone())
    })
}

/// Page through the per-swarm `activity_log.jsonl` written by the swarm-
/// activity forwarder. Used by SwarmControl on mount to replay history so
/// the panel isn't blank when the user opens it after a Scout/Worker has
/// already streamed events.
///
/// - `after_seq = None` starts from the beginning.
/// - `limit` clamps to [`ACTIVITY_LOG_MAX_LIMIT`] (default
///   [`ACTIVITY_LOG_DEFAULT_LIMIT`]).
/// - Returns an empty page (no error) when the log is missing or empty —
///   a freshly-created swarm hasn't streamed anything yet.
/// - Returns `IpcError::not_found { resource: "swarm" }` when the swarm
///   id itself isn't registered with `SwarmStore`.
#[tracing::instrument(skip(state))]
#[tauri::command]
pub async fn get_swarm_activity_log(
    state: tauri::State<'_, AppState>,
    swarm_id: String,
    after_seq: Option<u64>,
    limit: Option<u32>,
) -> Result<SwarmActivityLogPage, IpcError> {
    validate_id(&swarm_id).map_err(IpcError::validation)?;
    info!(
        swarm_id = %swarm_id,
        after_seq = ?after_seq,
        limit = ?limit,
        "get_swarm_activity_log invoked"
    );

    // Confirm the swarm exists in SwarmStore before doing anything else.
    // The activity log may legitimately be absent (no activity yet), but
    // the swarm-id itself must resolve — otherwise we'd silently return
    // an empty page for a typo'd id.
    let known_ids = state.swarm_store.list_swarms().await.map_err(|e| {
        IpcError::internal(format!("failed to enumerate swarms: {}", e)).with_id(swarm_id.clone())
    })?;
    if !known_ids.iter().any(|id| id == &swarm_id) {
        return Err(IpcError::not_found("swarm", swarm_id));
    }

    let log_path = state.swarm_store.activity_log_path(&swarm_id);
    let effective_limit = limit
        .unwrap_or(ACTIVITY_LOG_DEFAULT_LIMIT)
        .clamp(1, ACTIVITY_LOG_MAX_LIMIT);

    // The read is sync (BufReader over the file) — wrap in spawn_blocking
    // so a multi-megabyte log doesn't block a tokio worker.
    let swarm_id_for_err = swarm_id.clone();
    tokio::task::spawn_blocking(move || ActivityReader::page(&log_path, after_seq, effective_limit))
        .await
        .map_err(|e| {
            IpcError::internal(format!("activity log read task join error: {}", e))
                .with_id(swarm_id_for_err.clone())
        })?
        .map_err(|e| {
            IpcError::internal(format!("failed to read activity log: {}", e))
                .with_id(swarm_id_for_err)
        })
}

/// Read the persisted feature list for a swarm.
///
/// Reads `features.json` from the swarm's directory. Returns an empty list
/// if the swarm exists but has not been started yet (no features written).
#[tracing::instrument(skip(state))]
#[tauri::command]
pub async fn get_swarm_features(
    state: tauri::State<'_, AppState>,
    swarm_id: String,
) -> Result<Vec<Feature>, IpcError> {
    // Guards an arbitrary-file-read: read_features joins swarm_id into
    // ~/.hyvemind/swarms/{swarm_id}/features.json.
    validate_id(&swarm_id).map_err(IpcError::validation)?;
    state
        .swarm_store
        .read_features(&swarm_id)
        .await
        .map_err(|e| {
            IpcError::internal(format!("failed to read features: {}", e)).with_id(swarm_id.clone())
        })
}

/// Read the persisted milestone list for a swarm. Returns an empty vec if
/// the swarm has no milestones (created before milestones were wired in, or
/// the plan didn't define any).
#[tracing::instrument(skip(state))]
#[tauri::command]
pub async fn get_swarm_milestones(
    state: tauri::State<'_, AppState>,
    swarm_id: String,
) -> Result<Vec<Milestone>, IpcError> {
    // Same shape as get_swarm_features — guards arbitrary-file read.
    validate_id(&swarm_id).map_err(IpcError::validation)?;
    state
        .swarm_store
        .read_milestones(&swarm_id)
        .await
        .map_err(|e| {
            IpcError::internal(format!("failed to read milestones: {}", e))
                .with_id(swarm_id.clone())
        })
}

/// Sum usage rows whose `source_id` is either `swarm_id` exactly or starts
/// with `swarm_id:` — used by scout/worker/guard sessions (tagged
/// `swarm_id:feature_id:role`) AND by Hivemind context/round/merge phases
/// run on behalf of a swarm (tagged `swarm_id:hivemind-{phase}:{job_id}`).
///
/// The filter is intentionally `source_id`-only (no `source = 'swarm'`
/// guard) so Hivemind rows (`source = 'hivemind'`) with a swarm-prefixed
/// `source_id` are included. This is safe because every other row in
/// `usage_log` has a `source_id` that is either NULL, a bare UUID (no
/// colon), or a bare job_id (no colon) — none of those collide with the
/// `swarm_id:%` prefix.
#[tracing::instrument(skip(state))]
#[tauri::command]
pub async fn get_swarm_usage(
    state: tauri::State<'_, AppState>,
    swarm_id: String,
) -> Result<SwarmUsageSummary, IpcError> {
    validate_id(&swarm_id).map_err(IpcError::validation)?;
    use sqlx::Row;
    let pool = state.usage_store.pool();
    let prefix = format!("{}:%", swarm_id);
    let row = sqlx::query(
        "SELECT \
           COALESCE(SUM(input_tokens), 0)       AS input_tokens, \
           COALESCE(SUM(output_tokens), 0)      AS output_tokens, \
           COALESCE(SUM(cache_read_tokens), 0)  AS cache_read_tokens, \
           COALESCE(SUM(cache_write_tokens), 0) AS cache_write_tokens, \
           COALESCE(SUM(cost), 0.0)             AS cost, \
           COALESCE(SUM(duration_ms), 0)        AS duration_ms \
         FROM usage_log \
         WHERE source_id = ?1 OR source_id LIKE ?2",
    )
    .bind(&swarm_id)
    .bind(&prefix)
    .fetch_one(pool)
    .await
    .map_err(|e| {
        IpcError::internal(format!("failed to aggregate swarm usage: {}", e))
            .with_id(swarm_id.clone())
    })?;

    let mut total = SwarmUsageSummary {
        input_tokens: row.get("input_tokens"),
        output_tokens: row.get("output_tokens"),
        cache_read_tokens: row.get("cache_read_tokens"),
        cache_write_tokens: row.get("cache_write_tokens"),
        cost: row.get("cost"),
        duration_ms: row.get("duration_ms"),
    };

    // Add in-memory live totals from the swarm registry if available.
    let registry = state.swarm_registry.clone();
    if let Some(acc) = registry.get_usage_accumulator(&swarm_id).await {
        let live = acc.snapshot();
        total.input_tokens += live.input_tokens;
        total.output_tokens += live.output_tokens;
        total.cache_read_tokens += live.cache_read_tokens;
        total.cache_write_tokens += live.cache_write_tokens;
        total.cost += live.cost;
        total.duration_ms += live.duration_ms;
    }

    // Add live tokens from every currently-busy Pi session owned by this
    // swarm. Without this, the swarm totals only tick up when an agent
    // *finishes* (via `record_session_usage`), so a 10-minute Worker run
    // appears frozen in the UI until it ends. Filtering on `is_busy()`
    // avoids double-counting sessions that just finished — at agent end,
    // `record_session_usage` flips busy=false before flushing stats to the
    // DB, so the SQL `SUM` above already captures them.
    //
    // Three session kinds are intentionally summed here:
    //   1. Scout/worker/guard sessions (`SessionOwner::Swarm`)
    //   2. Hivemind context-gather sessions — they use `SessionOwner::Swarm`
    //      with role `hivemind-context-*`, so they fall under arm (1).
    //   3. Hivemind merge sessions (`SessionOwner::Merge` with a matching
    //      `swarm_id` set). These keep the `Merge` variant so existing
    //      match sites (`owner_kind_and_key`, eviction, kill-scope) are
    //      unchanged; the new optional `swarm_id` field lets us attribute
    //      tokens to the parent swarm without swapping the owner kind.
    //
    // The matching `record_usage` calls in `gather_context_phase` and
    // `spawn_merge_pi` write to the DB at session end; this live walk
    // catches the brief window before the DB row is visible.
    use crate::pi::session::SessionOwner;
    let live_sessions = state.pi_manager.list_sessions().await;
    for (_id, session) in live_sessions {
        let owner = session.owner();
        let matches_swarm = match &owner {
            SessionOwner::Swarm {
                swarm_id: ref sid, ..
            } if sid == &swarm_id => true,
            SessionOwner::Merge {
                swarm_id: Some(ref sid),
                ..
            } if sid == &swarm_id => true,
            _ => false,
        };
        if !matches_swarm || !session.is_busy() {
            continue;
        }
        if let Ok(stats) = session.get_session_stats().await {
            total.input_tokens += stats.input as i64;
            total.output_tokens += stats.output as i64;
            total.cache_read_tokens += stats.cache_read as i64;
            total.cache_write_tokens += stats.cache_write as i64;
            total.cost += stats.cost;
        }
    }

    Ok(total)
}

/// Run swarm-readiness checks for a swarm's plan.
///
/// Phase 4B of the autonomy plan: after the Queen produces a plan, the
/// frontend forwards the plan's `readiness_manifest` here BEFORE calling
/// `start_swarm`. If `report.all_ok` is false, the frontend MUST block the
/// launch and surface the failing checks to the user. This command is
/// intentionally independent from `start_swarm` so the two responsibilities
/// stay separable.
///
/// The swarm's working directory is read from disk / registry — the
/// frontend doesn't need to (and shouldn't) supply it.
#[tracing::instrument(skip(state, manifest), fields(swarm_id = %swarm_id))]
#[tauri::command]
pub async fn check_swarm_readiness(
    state: tauri::State<'_, AppState>,
    swarm_id: String,
    manifest: serde_json::Value,
) -> Result<crate::core::readiness::ReadinessReport, IpcError> {
    validate_id(&swarm_id).map_err(IpcError::validation)?;
    // Bound the inbound JSON payload before any work.
    check_payload_size(&manifest).map_err(IpcError::validation)?;

    // Resolve working_directory: registry first (running/just-created), then
    // disk (created in a previous session).
    let working_directory: String = match state.swarm_registry.get_state(&swarm_id).await {
        Some(s) => s.working_directory,
        None => match state.swarm_store.read_state(&swarm_id).await {
            Ok(Some(s)) => s.working_directory,
            Ok(None) => return Err(IpcError::not_found("swarm", swarm_id.clone())),
            Err(e) => {
                return Err(IpcError::internal(format!(
                    "failed to read swarm '{}' state from disk: {}",
                    swarm_id, e
                ))
                .with_id(swarm_id.clone()));
            }
        },
    };

    // Audit 1.11: even though the `working_directory` here came from disk
    // (not the IPC payload), enforce the allowlist as defense-in-depth —
    // a swarm's on-disk state.json could have been hand-edited to point
    // somewhere outside the allowlist, and `check_readiness` makes
    // filesystem probes against that path.
    let working_dir = validate_working_dir_with_allowlist(&state, &working_directory)
        .await
        .map_err(IpcError::not_approved)?;

    let parsed: crate::core::readiness::ReadinessManifest = serde_json::from_value(manifest)
        .map_err(|e| IpcError::validation(format!("invalid readiness_manifest: {}", e)))?;

    let report = crate::core::readiness::check_readiness(&parsed, &working_dir).await;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_working_dir_rejects_empty() {
        assert!(validate_working_dir("").is_err());
        assert!(validate_working_dir("   ").is_err());
    }

    #[test]
    fn validate_working_dir_rejects_null_byte() {
        assert!(validate_working_dir("/tmp/\0bad").is_err());
    }

    #[test]
    fn validate_working_dir_rejects_nonexistent_path() {
        assert!(validate_working_dir("/this/path/does/not/exist/abc123xyz").is_err());
    }

    #[test]
    fn validate_working_dir_accepts_tempdir_and_canonicalizes() {
        let td = tempfile::TempDir::new().expect("create tempdir");
        let raw = td.path().to_string_lossy().to_string();
        // Trailing whitespace must be trimmed.
        let with_ws = format!("  {}  ", raw);
        let canonical = validate_working_dir(&with_ws).expect("ok");
        assert!(canonical.is_dir());
        let expected =
            crate::commands::util::canonicalize_clean(td.path()).expect("canonicalize tempdir");
        assert_eq!(canonical, expected);
    }

    #[test]
    fn validate_working_dir_expands_tilde_alone() {
        if let Some(home) = dirs::home_dir() {
            if home.exists() {
                let canonical = validate_working_dir("~").expect("ok");
                let expected =
                    crate::commands::util::canonicalize_clean(&home).expect("canon home");
                assert_eq!(canonical, expected);
            }
        }
    }

    // ---- resolve_stop_from_disk ----------------------------------------

    fn make_state(status: SwarmStatus) -> SwarmState {
        use crate::domain::swarm::{ModelSettings, SwarmConfig};
        let config = SwarmConfig {
            name: "stop-test".into(),
            description: "".into(),
            working_directory: "/tmp".into(),
            model_settings: ModelSettings::default(),
            features: vec![],
            milestones: vec![],
        };
        let mut s = SwarmState::from_config(&config);
        s.set_status(status);
        s
    }

    #[test]
    fn resolve_stop_none_is_not_found() {
        assert_eq!(resolve_stop_from_disk(None), StopOutcome::NotFound);
    }

    #[test]
    fn resolve_stop_implementing_updates_disk() {
        let s = make_state(SwarmStatus::Implementing);
        assert_eq!(resolve_stop_from_disk(Some(&s)), StopOutcome::UpdatedDisk);
    }

    #[test]
    fn resolve_stop_paused_updates_disk() {
        let s = make_state(SwarmStatus::Paused);
        assert_eq!(resolve_stop_from_disk(Some(&s)), StopOutcome::UpdatedDisk);
    }

    #[test]
    fn resolve_stop_interrupted_updates_disk() {
        let s = make_state(SwarmStatus::Interrupted);
        assert_eq!(resolve_stop_from_disk(Some(&s)), StopOutcome::UpdatedDisk);
    }

    #[test]
    fn resolve_stop_planning_updates_disk() {
        let s = make_state(SwarmStatus::Planning);
        assert_eq!(resolve_stop_from_disk(Some(&s)), StopOutcome::UpdatedDisk);
    }

    #[test]
    fn resolve_stop_completed_is_noop() {
        let s = make_state(SwarmStatus::Completed);
        assert_eq!(resolve_stop_from_disk(Some(&s)), StopOutcome::NoOp);
    }

    #[test]
    fn resolve_stop_failed_is_noop() {
        let s = make_state(SwarmStatus::Failed);
        assert_eq!(resolve_stop_from_disk(Some(&s)), StopOutcome::NoOp);
    }

    #[test]
    fn resolve_stop_cancelled_is_noop() {
        let s = make_state(SwarmStatus::Cancelled);
        assert_eq!(resolve_stop_from_disk(Some(&s)), StopOutcome::NoOp);
    }

    #[test]
    fn length_cap_constants_are_sane() {
        assert_eq!(MAX_GOAL_LEN, 64 * 1024);
        assert_eq!(MAX_FEATURE_DESC_LEN, 64 * 1024);
        assert!(MAX_GOAL_LEN > 1024);
        assert!(MAX_FEATURE_DESC_LEN > 1024);
    }

    #[tokio::test]
    async fn start_swarm_fallback_from_disk() {
        use crate::domain::swarm::{ModelSettings, SwarmConfig};
        use crate::state::store::SwarmStore;
        use tempfile::TempDir;

        let tmp = TempDir::new().expect("tempdir");
        let store = SwarmStore::new(tmp.path());

        let config = SwarmConfig {
            name: "test-swarm".into(),
            description: "".into(),
            working_directory: tmp.path().display().to_string(),
            model_settings: ModelSettings::default(),
            features: vec![],
            milestones: vec![],
        };
        let state = crate::domain::swarm::SwarmState::from_config(&config);
        let swarm_id = state.id.clone();
        store.init_swarm(&swarm_id).await.expect("init");
        store.write_state(&swarm_id, &state).await.expect("write");

        // Verify round-trip
        let loaded = store
            .read_state(&swarm_id)
            .await
            .expect("read")
            .expect("state present");
        assert_eq!(loaded.id, swarm_id);
        assert_eq!(loaded.name, "test-swarm");
    }

    // ---- update_swarm helper tests ---------------------------------------
    //
    // We don't have a Tauri `State<AppState>` available in unit tests, so
    // these tests exercise the underlying SwarmStore / SwarmRegistry
    // round-trip that `update_swarm` is built on. The Tauri command itself
    // is just a thin wrapper around these primitives plus
    // `validate_working_dir`, which has its own coverage above.

    #[tokio::test]
    async fn update_swarm_roundtrip_model_settings_via_store() {
        use crate::domain::swarm::{ModelSettings, SwarmConfig, SwarmState};
        use crate::state::store::SwarmStore;
        use tempfile::TempDir;

        let tmp = TempDir::new().expect("tempdir");
        let store = SwarmStore::new(tmp.path());

        // Create with hivemind OFF on both queen and scout.
        let mut settings = ModelSettings::default();
        settings.use_hivemind_on_queen = false;
        settings.use_hivemind_on_scout = false;
        settings.queen_thinking_level = "high".into();
        settings.scout_thinking_level = "medium".into();
        let config = SwarmConfig {
            name: "swarm-edit".into(),
            description: "".into(),
            working_directory: tmp.path().display().to_string(),
            model_settings: settings,
            features: vec![],
            milestones: vec![],
        };
        let mut state = SwarmState::from_config(&config);
        let swarm_id = state.id.clone();
        store.init_swarm(&swarm_id).await.expect("init");
        store.write_state(&swarm_id, &state).await.expect("write");

        // Simulate an edit: flip hivemind ON for queen, change thinking level.
        state.model_settings.use_hivemind_on_queen = true;
        state.model_settings.hivemind_id = Some("enhance".into());
        state.model_settings.queen_thinking_level = "low".into();
        store.write_state(&swarm_id, &state).await.expect("rewrite");

        // Read back — settings reflect the edit.
        let loaded = store
            .read_state(&swarm_id)
            .await
            .expect("read")
            .expect("present");
        assert!(loaded.model_settings.use_hivemind_on_queen);
        assert!(!loaded.model_settings.use_hivemind_on_scout);
        assert_eq!(
            loaded.model_settings.hivemind_id.as_deref(),
            Some("enhance")
        );
        assert_eq!(loaded.model_settings.queen_thinking_level, "low");
    }

    fn sample_state(name: &str, cwd: &str) -> SwarmState {
        use crate::domain::swarm::{ModelSettings, SwarmConfig};
        let config = SwarmConfig {
            name: name.into(),
            description: "".into(),
            working_directory: cwd.into(),
            model_settings: ModelSettings::default(),
            features: vec![],
            milestones: vec![],
        };
        SwarmState::from_config(&config)
    }

    #[test]
    fn update_swarm_rejects_empty_name() {
        let td = tempfile::TempDir::new().expect("tempdir");
        let cwd = td.path().display().to_string();
        let current = sample_state("orig", &cwd);
        let err = apply_update(current, "   ".into(), cwd.clone(), serde_json::json!({}))
            .expect_err("should reject empty/whitespace name");
        assert!(err.contains("name is empty"), "got: {}", err);
    }

    #[test]
    fn update_swarm_rejects_name_too_long() {
        let td = tempfile::TempDir::new().expect("tempdir");
        let cwd = td.path().display().to_string();
        let current = sample_state("orig", &cwd);
        let long_name = "x".repeat(MAX_NAME_LEN + 1);
        let err = apply_update(current, long_name, cwd, serde_json::json!({}))
            .expect_err("should reject overlong name");
        assert!(err.contains("name too long"), "got: {}", err);
    }

    #[test]
    fn update_swarm_rejects_invalid_working_dir() {
        let td = tempfile::TempDir::new().expect("tempdir");
        let cwd = td.path().display().to_string();
        let current = sample_state("orig", &cwd);
        let err = apply_update(
            current,
            "new-name".into(),
            "/this/path/does/not/exist/abc999".into(),
            serde_json::json!({}),
        )
        .expect_err("should reject nonexistent dir");
        assert!(
            err.contains("working directory invalid") || err.contains("not a directory"),
            "got: {}",
            err
        );
    }

    #[test]
    fn update_swarm_rejects_when_hivemind_id_missing_but_flags_set() {
        let td = tempfile::TempDir::new().expect("tempdir");
        let cwd = td.path().display().to_string();
        let current = sample_state("orig", &cwd);
        let settings = serde_json::json!({
            "primary_model": "claude-opus-4",
            "scout_model": "claude-sonnet-4",
            "use_hivemind_on_scout": true,
            "use_hivemind_on_queen": false,
            "hivemind_id": null,
        });
        let err = apply_update(current, "new".into(), cwd, settings)
            .expect_err("should reject missing hivemind_id");
        assert!(err.contains("hivemind_id must be set"), "got: {}", err);
    }

    #[test]
    fn update_swarm_rejects_running_swarm() {
        let td = tempfile::TempDir::new().expect("tempdir");
        let cwd = td.path().display().to_string();
        let mut current = sample_state("orig", &cwd);
        current.set_status(SwarmStatus::Implementing);
        let err = apply_update(current, "new".into(), cwd, serde_json::json!({}))
            .expect_err("should reject editing a running swarm");
        assert!(err.contains("running swarm"), "got: {}", err);
    }

    #[test]
    fn update_swarm_applies_changes_and_touches_updated_at() {
        let td = tempfile::TempDir::new().expect("tempdir");
        let cwd = td.path().display().to_string();
        let mut current = sample_state("orig", &cwd);
        current.status = SwarmStatus::Planning;
        let original_updated = current.updated_at;
        std::thread::sleep(std::time::Duration::from_millis(2));

        let settings = serde_json::json!({
            "primary_model": "claude-opus-4",
            "scout_model": "claude-sonnet-4",
            "guard_model": "gpt-5-codex",
            "use_hivemind_on_scout": false,
            "use_hivemind_on_queen": false,
            "hivemind_id": null,
        });
        let updated = apply_update(current, "renamed".into(), cwd.clone(), settings).expect("ok");
        assert_eq!(updated.name, "renamed");
        assert_eq!(updated.model_settings.primary_model, "claude-opus-4");
        assert_eq!(updated.model_settings.scout_model, "claude-sonnet-4");
        assert_eq!(
            updated.model_settings.guard_model.as_deref(),
            Some("gpt-5-codex")
        );
        assert!(updated.updated_at > original_updated);
    }

    #[tokio::test]
    async fn update_swarm_disk_roundtrip() {
        use crate::state::store::SwarmStore;
        use tempfile::TempDir;

        let tmp = TempDir::new().expect("tempdir");
        let store = SwarmStore::new(tmp.path());
        let cwd = tmp.path().display().to_string();
        let state = sample_state("orig", &cwd);
        let swarm_id = state.id.clone();
        store.init_swarm(&swarm_id).await.expect("init");
        store.write_state(&swarm_id, &state).await.expect("write");

        // Simulate update.
        let settings = serde_json::json!({
            "primary_model": "claude-opus-4",
            "scout_model": "claude-sonnet-4",
            "use_hivemind_on_scout": false,
            "use_hivemind_on_queen": false,
            "hivemind_id": null,
        });
        let updated =
            apply_update(state, "renamed".into(), cwd.clone(), settings).expect("apply ok");
        store
            .write_state(&swarm_id, &updated)
            .await
            .expect("rewrite");

        let loaded = store
            .read_state(&swarm_id)
            .await
            .expect("read")
            .expect("present");
        assert_eq!(loaded.id, swarm_id);
        assert_eq!(loaded.name, "renamed");
        assert_eq!(loaded.model_settings.primary_model, "claude-opus-4");
    }

    #[tokio::test]
    async fn replace_state_updates_registry_snapshot() {
        use crate::state::swarm_registry::SwarmRegistry;
        use tokio_util::sync::CancellationToken;

        let registry = SwarmRegistry::new();
        let td = tempfile::TempDir::new().expect("tempdir");
        let mut state = sample_state("orig", &td.path().display().to_string());
        registry
            .register(state.id.clone(), state.clone(), CancellationToken::new())
            .await;

        state.name = "renamed".into();
        registry
            .replace_state(&state.id, state.clone())
            .await
            .expect("replace ok");

        let snapshot = registry.get_state(&state.id).await.expect("present");
        assert_eq!(snapshot.name, "renamed");

        // Unknown id returns Err.
        assert!(registry
            .replace_state("does-not-exist", state)
            .await
            .is_err());
    }

    // reconcile_orphaned_swarms tests live in `crate::core::recovery::tests`
    // since the function itself moved there.

    #[tokio::test]
    async fn get_start_lock_serialises_concurrent_attempts() {
        use crate::state::swarm_registry::SwarmRegistry;
        use std::sync::Arc;

        let registry = SwarmRegistry::new();
        let lock1 = registry.get_start_lock("swarm-a").await;
        let lock2 = registry.get_start_lock("swarm-a").await;

        // Both calls return the same Arc.
        assert!(Arc::ptr_eq(&lock1, &lock2));

        // The mutex should be lockable — verify exclusivity.
        let guard1 = lock1.try_lock();
        assert!(guard1.is_ok());
        drop(guard1);

        // Different swarm_id gets a different mutex.
        let lock3 = registry.get_start_lock("swarm-b").await;
        assert!(!Arc::ptr_eq(&lock1, &lock3));
    }

    // ---- reset_in_flight_features tests ---------------------------------

    fn feat(id: &str, status: FeatureStatus) -> Feature {
        let mut f = Feature::new(id.into(), id.into(), "".into());
        f.status = status;
        f
    }

    #[test]
    fn test_reset_in_flight_features_resets_all_in_flight() {
        let mut features = vec![
            feat("a", FeatureStatus::Scouting),
            feat("b", FeatureStatus::Implementing),
            feat("c", FeatureStatus::Reviewing),
            feat("d", FeatureStatus::Validating),
        ];
        let n = reset_in_flight_features(&mut features);
        assert_eq!(n, 4);
        for f in &features {
            assert_eq!(f.status, FeatureStatus::Pending);
        }
    }

    #[test]
    fn test_reset_in_flight_features_preserves_terminal_and_pending() {
        let mut features = vec![
            feat("a", FeatureStatus::Pending),
            feat("b", FeatureStatus::Completed),
            feat("c", FeatureStatus::Failed),
            feat("d", FeatureStatus::Skipped),
        ];
        let n = reset_in_flight_features(&mut features);
        assert_eq!(n, 0);
        assert_eq!(features[0].status, FeatureStatus::Pending);
        assert_eq!(features[1].status, FeatureStatus::Completed);
        assert_eq!(features[2].status, FeatureStatus::Failed);
        assert_eq!(features[3].status, FeatureStatus::Skipped);
    }

    #[test]
    fn test_reset_in_flight_features_mixed_returns_correct_count() {
        let mut features = vec![
            feat("a", FeatureStatus::Completed),
            feat("b", FeatureStatus::Implementing),
            feat("c", FeatureStatus::Pending),
            feat("d", FeatureStatus::Validating),
            feat("e", FeatureStatus::Failed),
        ];
        let n = reset_in_flight_features(&mut features);
        assert_eq!(n, 2);
        assert_eq!(features[0].status, FeatureStatus::Completed);
        assert_eq!(features[1].status, FeatureStatus::Pending);
        assert_eq!(features[2].status, FeatureStatus::Pending);
        assert_eq!(features[3].status, FeatureStatus::Pending);
        assert_eq!(features[4].status, FeatureStatus::Failed);
    }

    // ---- reset_features_for_full_resume tests ---------------------------

    #[test]
    fn test_full_resume_resets_in_flight() {
        let mut features = vec![
            feat("a", FeatureStatus::Scouting),
            feat("b", FeatureStatus::Implementing),
            feat("c", FeatureStatus::Reviewing),
            feat("d", FeatureStatus::Validating),
        ];
        // Seed non-zero fix_attempt_count to verify it's reset.
        for f in features.iter_mut() {
            f.fix_attempt_count = 2;
        }
        let n = reset_features_for_full_resume(&mut features);
        assert_eq!(n, 4);
        for f in &features {
            assert_eq!(f.status, FeatureStatus::Pending);
            assert_eq!(f.fix_attempt_count, 0);
            assert!(!f.interrupted);
            assert!(!f.resumable);
        }
    }

    #[test]
    fn test_full_resume_resets_terminal_failed() {
        let mut features = vec![feat("a", FeatureStatus::Failed)];
        features[0].fix_attempt_count = 3;
        features[0].interrupted = false;
        features[0].resumable = false;
        let n = reset_features_for_full_resume(&mut features);
        assert_eq!(n, 1);
        assert_eq!(features[0].status, FeatureStatus::Pending);
        assert_eq!(features[0].fix_attempt_count, 0);
        assert!(!features[0].interrupted);
        assert!(!features[0].resumable);
    }

    #[test]
    fn test_full_resume_resets_interrupted_failed() {
        let mut features = vec![feat("a", FeatureStatus::Failed)];
        features[0].fix_attempt_count = 2;
        features[0].interrupted = true;
        features[0].resumable = true;
        let n = reset_features_for_full_resume(&mut features);
        assert_eq!(n, 1);
        assert_eq!(features[0].status, FeatureStatus::Pending);
        assert_eq!(features[0].fix_attempt_count, 0);
        assert!(!features[0].interrupted);
        assert!(!features[0].resumable);
    }

    #[test]
    fn test_full_resume_preserves_completed_skipped_pending() {
        let mut features = vec![
            feat("a", FeatureStatus::Completed),
            feat("b", FeatureStatus::Skipped),
            feat("c", FeatureStatus::Pending),
            feat("d", FeatureStatus::Failed),
        ];
        let n = reset_features_for_full_resume(&mut features);
        assert_eq!(n, 1);
        assert_eq!(features[0].status, FeatureStatus::Completed);
        assert_eq!(features[1].status, FeatureStatus::Skipped);
        assert_eq!(features[2].status, FeatureStatus::Pending);
        assert_eq!(features[3].status, FeatureStatus::Pending);
    }

    #[test]
    fn test_full_resume_returns_correct_count() {
        let mut features = vec![
            feat("a", FeatureStatus::Completed),
            feat("b", FeatureStatus::Implementing),
            feat("c", FeatureStatus::Pending),
            feat("d", FeatureStatus::Failed),
            feat("e", FeatureStatus::Skipped),
        ];
        let n = reset_features_for_full_resume(&mut features);
        assert_eq!(n, 2, "Implementing + Failed reset; others preserved");
        assert_eq!(features[0].status, FeatureStatus::Completed);
        assert_eq!(features[1].status, FeatureStatus::Pending);
        assert_eq!(features[2].status, FeatureStatus::Pending);
        assert_eq!(features[3].status, FeatureStatus::Pending);
        assert_eq!(features[4].status, FeatureStatus::Skipped);
    }

    // migrate_legacy_reconciled_failures tests live in
    // `crate::core::recovery::tests` since the function itself moved there.

    // ---- resume rehydration pre-spawn boundary --------------------------

    /// Integration-lite: replays the pre-spawn portion of `resume_swarm`
    /// against a seeded `SwarmStore` and asserts the resulting on-disk state.
    #[tokio::test]
    async fn test_resume_rehydration_resets_in_flight_and_persists() {
        use crate::state::store::SwarmStore;
        use tempfile::TempDir;

        let tmp = TempDir::new().expect("tempdir");
        let store = SwarmStore::new(tmp.path());
        let cwd = tmp.path().display().to_string();

        let mut s = sample_state("resume", &cwd);
        s.set_status(SwarmStatus::Interrupted);
        s.error = Some(crate::core::recovery::INTERRUPTED_BY_RESTART_MSG.to_string());
        let sid = s.id.clone();
        store.init_swarm(&sid).await.expect("init");
        store.write_state(&sid, &s).await.expect("write state");

        let features = vec![
            feat("f1", FeatureStatus::Completed),
            feat("f2", FeatureStatus::Implementing),
            feat("f3", FeatureStatus::Validating),
            feat("f4", FeatureStatus::Pending),
        ];
        store
            .write_features(&sid, &features)
            .await
            .expect("write feats");
        store
            .write_milestones(&sid, &Vec::<Milestone>::new())
            .await
            .expect("write ms");

        // --- Pre-spawn portion of resume_swarm ---
        let mut state = store
            .read_state(&sid)
            .await
            .expect("read")
            .expect("present");
        assert!(state.status.is_resumable());

        let mut loaded_features = store.read_features(&sid).await.expect("read feats");
        let _milestones = store.read_milestones(&sid).await.expect("read ms");
        let reset = reset_in_flight_features(&mut loaded_features);
        assert_eq!(reset, 2, "f2 and f3 should be reset");

        store
            .write_features(&sid, &loaded_features)
            .await
            .expect("persist reset feats");

        state.error = None;
        state.set_status(SwarmStatus::Implementing);
        store
            .write_state(&sid, &state)
            .await
            .expect("persist state");

        // --- Assertions on disk ---
        let final_state = store
            .read_state(&sid)
            .await
            .expect("read")
            .expect("present");
        assert_eq!(final_state.status, SwarmStatus::Implementing);
        assert!(final_state.error.is_none());

        let final_feats = store.read_features(&sid).await.expect("read");
        assert_eq!(final_feats[0].status, FeatureStatus::Completed);
        assert_eq!(final_feats[1].status, FeatureStatus::Pending);
        assert_eq!(final_feats[2].status, FeatureStatus::Pending);
        assert_eq!(final_feats[3].status, FeatureStatus::Pending);
    }

    /// Integration-lite: replays the pre-spawn portion of `resume_swarm`
    /// for a `Failed` swarm and asserts that terminal-failed features are
    /// re-Pended with their `fix_attempt_count` reset to 0.
    #[tokio::test]
    async fn test_full_resume_rehydration_resets_terminal_failed_and_persists() {
        use crate::state::store::SwarmStore;
        use tempfile::TempDir;

        let tmp = TempDir::new().expect("tempdir");
        let store = SwarmStore::new(tmp.path());
        let cwd = tmp.path().display().to_string();

        let mut s = sample_state("resume-failed", &cwd);
        s.set_status(SwarmStatus::Failed);
        s.error = Some("something broke".into());
        let sid = s.id.clone();
        store.init_swarm(&sid).await.expect("init");
        store.write_state(&sid, &s).await.expect("write state");

        let mut features = vec![
            feat("f1", FeatureStatus::Completed),
            feat("f2", FeatureStatus::Failed),
            feat("f3", FeatureStatus::Pending),
        ];
        features[1].fix_attempt_count = 3;
        store
            .write_features(&sid, &features)
            .await
            .expect("write feats");
        store
            .write_milestones(&sid, &Vec::<Milestone>::new())
            .await
            .expect("write ms");

        // --- Pre-spawn portion (Failed branch) of resume_swarm ---
        let mut state = store
            .read_state(&sid)
            .await
            .expect("read")
            .expect("present");
        assert!(state.status.is_resumable());
        assert_eq!(state.status, SwarmStatus::Failed);

        let mut loaded_features = store.read_features(&sid).await.expect("read feats");
        let reset = reset_features_for_full_resume(&mut loaded_features);
        assert_eq!(reset, 1, "only the terminal-failed feature should be reset");

        store
            .write_features(&sid, &loaded_features)
            .await
            .expect("persist reset feats");

        state.error = None;
        state.set_status(SwarmStatus::Implementing);
        store
            .write_state(&sid, &state)
            .await
            .expect("persist state");

        // --- Assertions on disk ---
        let final_state = store
            .read_state(&sid)
            .await
            .expect("read")
            .expect("present");
        assert_eq!(final_state.status, SwarmStatus::Implementing);
        assert!(final_state.error.is_none());

        let final_feats = store.read_features(&sid).await.expect("read");
        assert_eq!(final_feats[0].status, FeatureStatus::Completed);
        assert_eq!(final_feats[1].status, FeatureStatus::Pending);
        assert_eq!(
            final_feats[1].fix_attempt_count, 0,
            "fix_attempt_count must be reset for a fresh retry budget"
        );
        assert_eq!(final_feats[2].status, FeatureStatus::Pending);
    }

    /// Audit 2.12: shared helper exercising the supervise-wrapped cleanup
    /// closure the swarm fire-and-forget tasks use (event_forwarder,
    /// activity_forwarder, progress_writer, queen_orchestrator). The
    /// cleanup must:
    ///   1. emit a `swarm-event` of type `failed` on the app handle, and
    ///   2. flip the in-memory SwarmRegistry status to `Failed`, and
    ///   3. persist that status to the on-disk SwarmStore.
    ///
    /// We run the panic injection by forcing the supervised body to call
    /// `panic_for_test`. The macro itself is identical to the one used at
    /// every live spawn site.
    async fn run_swarm_supervise_panic_check(component: &'static str) {
        use crate::state::store::SwarmStore;
        use crate::state::swarm_registry::SwarmRegistry;
        use tauri::Listener;
        use tokio_util::sync::CancellationToken;

        let app = tauri::test::mock_app();
        let app_handle = app.handle().clone();

        let tmp = tempfile::TempDir::new().expect("tempdir");
        let store = std::sync::Arc::new(SwarmStore::new(tmp.path()));
        let registry = std::sync::Arc::new(SwarmRegistry::new());

        // Seed an Implementing swarm.
        let st = make_state(SwarmStatus::Implementing);
        let sid = st.id.clone();
        store
            .write_state(&sid, &st)
            .await
            .expect("persist initial state");
        registry
            .register(sid.clone(), st.clone(), CancellationToken::new())
            .await;

        // Listen for `swarm-event` payloads referencing our swarm with
        // event_type=failed.
        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let captured_in = captured.clone();
        let target_sid = sid.clone();
        let _listener = app_handle.listen("swarm-event", move |evt| {
            let payload = evt.payload();
            if payload.contains(&target_sid) && payload.contains("\"failed\"") {
                captured_in.lock().unwrap().push(payload.to_string());
            }
        });

        // Drive the panic through the same `on_panic` shape used at the
        // live call sites.
        let panic_app = app_handle.clone();
        let panic_sid = sid.clone();
        let panic_store = store.clone();
        let panic_registry = registry.clone();
        let component_owned = component.to_string();
        let supervised = crate::supervise!(
            context = format!("swarm={} component={}", panic_sid, component),
            on_panic = move |panic_msg: String| {
                let _ = panic_app.emit(
                    "swarm-event",
                    serde_json::json!({
                        "swarm_id": &panic_sid,
                        "event_type": "failed",
                        "message": format!("internal task panicked ({component_owned}): {panic_msg}"),
                    }),
                );
                let pid = panic_sid.clone();
                let preg = panic_registry.clone();
                let pstore = panic_store.clone();
                tokio::spawn(async move {
                    preg.update_status(&pid, SwarmStatus::Failed).await;
                    if let Some(s) = preg.get_state(&pid).await {
                        let _ = pstore.write_state(&pid, &s).await;
                    }
                });
            },
            async move {
                crate::util::supervise::panic_for_test("swarm_test");
            }
        );

        tokio::spawn(supervised)
            .await
            .expect("supervisor must absorb the panic");

        // Wait briefly for both the Tauri emit and the inner cleanup
        // spawn (which is async) to finish.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // 1. swarm-event emitted with event_type=failed
        let got = captured.lock().unwrap().clone();
        assert!(
            !got.is_empty(),
            "{component}: expected at least one swarm-event of type failed, got: {:?}",
            got
        );

        // 2. in-memory registry status flipped to Failed
        let in_mem = registry.get_state(&sid).await.expect("registry has swarm");
        assert_eq!(
            in_mem.status,
            SwarmStatus::Failed,
            "{component}: in-memory status should be Failed"
        );

        // 3. on-disk status flipped to Failed
        let on_disk = store
            .read_state(&sid)
            .await
            .expect("read")
            .expect("present");
        assert_eq!(
            on_disk.status,
            SwarmStatus::Failed,
            "{component}: on-disk status should be Failed"
        );
    }

    #[tokio::test]
    async fn swarm_event_forwarder_panic_marks_failed() {
        run_swarm_supervise_panic_check("event_forwarder").await;
    }

    #[tokio::test]
    async fn swarm_activity_forwarder_panic_marks_failed() {
        run_swarm_supervise_panic_check("activity_forwarder").await;
    }

    #[tokio::test]
    async fn swarm_progress_writer_panic_marks_failed() {
        run_swarm_supervise_panic_check("progress_writer").await;
    }

    #[tokio::test]
    async fn swarm_queen_orchestrator_panic_marks_failed() {
        run_swarm_supervise_panic_check("queen_orchestrator").await;
    }

    /// Exercises the queen-orchestrator-specific failure path that the shared
    /// `supervise!` harness above does NOT cover: the live queen task uses
    /// `run_supervised` (not the macro) and runs a custom `Err(panic_msg)` arm
    /// that must populate `state.error` and publish a synthetic `SwarmFailed`
    /// progress event so `progress_log.jsonl` carries a failure marker.
    /// Regression guard against the silent-failure bug described in the plan
    /// at ~/.claude/plans/whats-going-on-with-sprightly-peacock.md.
    #[tokio::test]
    async fn swarm_queen_failure_persists_error_and_emits_swarm_failed() {
        use crate::state::progress::ProgressEventType;
        use crate::state::store::SwarmStore;
        use crate::state::swarm_registry::SwarmRegistry;
        use tokio::sync::broadcast;
        use tokio_util::sync::CancellationToken;

        let tmp = tempfile::TempDir::new().expect("tempdir");
        let store = std::sync::Arc::new(SwarmStore::new(tmp.path()));
        let registry = std::sync::Arc::new(SwarmRegistry::new());

        let st = make_state(SwarmStatus::Implementing);
        let sid = st.id.clone();
        store.write_state(&sid, &st).await.expect("persist");
        registry
            .register(sid.clone(), st.clone(), CancellationToken::new())
            .await;

        let (event_tx, mut event_rx) = broadcast::channel::<ProgressEvent>(16);

        // Simulate the failure-branch wiring from start_swarm's queen task:
        // when the inner result is Err, set_error + send a SwarmFailed event +
        // persist the in-memory state to disk.
        let err_str = "dependency cycle detected: feat-a -> feat-b -> feat-a".to_string();
        registry.set_error(&sid, err_str.clone()).await;
        let evt = ProgressEvent::new(ProgressEventType::SwarmFailed, sid.clone(), err_str.clone());
        let _ = event_tx.send(evt);
        let final_state = registry.get_state(&sid).await.expect("registered");
        store
            .write_state(&sid, &final_state)
            .await
            .expect("write state");

        // 1. on-disk state has error populated and status=Failed.
        let on_disk = store
            .read_state(&sid)
            .await
            .expect("read ok")
            .expect("present");
        assert_eq!(on_disk.status, SwarmStatus::Failed);
        assert_eq!(on_disk.error.as_deref(), Some(err_str.as_str()));

        // 2. broadcast carries the synthetic SwarmFailed event the persist
        //    task would write to progress_log.jsonl.
        let got = event_rx.recv().await.expect("recv ok");
        assert!(matches!(got.event_type, ProgressEventType::SwarmFailed));
        assert_eq!(got.swarm_id, sid);
        assert_eq!(got.message, err_str);
    }
}
