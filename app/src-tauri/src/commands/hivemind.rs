use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use tauri::Emitter;
use tracing::{debug, error, info, Instrument};

use crate::hivemind::engine::{ReviewEngine, Stance};
use crate::hivemind::phase::compute_review_phase;
use crate::hivemind::review_log;
use crate::hivemind::review_log::ReviewLogger;
use crate::hivemind::store::{Job, JobStep, RoundVerdict};
use crate::state::app_state::AppState;
use crate::state::ipc_error::IpcError;
use crate::state::usage_store::SessionUsageSummary;

// Re-export moved types so callers that historically imported them from
// `commands::hivemind` keep compiling. New code should import from
// `crate::hivemind::events` / `crate::hivemind::phase` directly. The
// `commands/` module is being narrowed to host only IPC adapters.
pub use crate::hivemind::events::HivemindProgressEvent;
pub use crate::hivemind::phase::ReviewResumePhase;

/// Maximum allowed plan length in bytes for `start_review`. Plans larger than
/// this are rejected up-front to bound memory use and SQLite row size.
const MAX_PLAN_LEN: usize = 1_048_576; // 1 MiB

use crate::commands::util::{
    check_payload_size, validate_hivemind_id, validate_id, validate_review_id, validate_session_id,
    validate_task_id, MAX_ROUNDS_CONFIG,
};

/// Status information for a hivemind review job.
#[derive(Debug, Clone, Serialize)]
pub struct ReviewStatus {
    pub job_id: String,
    pub status: String,
    pub current_round: i64,
    pub total_rounds: i64,
    pub steps: Vec<StepSummary>,
    pub error: Option<String>,
    pub final_output: Option<String>,
    pub total_cost: f64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub created_at: String,
    pub completed_at: Option<String>,
}

/// Summary of a single model invocation within a review.
#[derive(Debug, Clone, Serialize)]
pub struct StepSummary {
    pub model_id: String,
    pub provider: String,
    pub status: String,
    pub output_preview: String,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub duration_ms: Option<i64>,
    pub round_number: i64,
}

/// Full state snapshot for a review job, used by the frontend to resync
/// from SQLite on mount, focus, or after a dropped event. Mirrors
/// `ReviewStatus` but with untruncated step outputs and a derived
/// `is_running` flag.
#[derive(Debug, Clone, Serialize)]
pub struct ReviewStateSnapshot {
    pub job_id: String,
    pub status: String,
    pub is_running: bool,
    pub current_round: i64,
    pub total_rounds: i64,
    pub steps: Vec<StepFull>,
    pub error: Option<String>,
    pub final_output: Option<String>,
    pub total_cost: f64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub created_at: String,
    pub completed_at: Option<String>,
}

/// Full (untruncated) view of a single review step.
#[derive(Debug, Clone, Serialize)]
pub struct StepFull {
    pub model_id: String,
    pub provider: String,
    pub status: String,
    /// Untruncated output. Empty string if the step has not produced output yet.
    pub output: String,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub duration_ms: Option<i64>,
    pub round_number: i64,
    pub cost: Option<f64>,
    pub prompt: Option<String>,
    pub error: Option<String>,
}

/// Summary information for listing reviews.
#[derive(Debug, Clone, Serialize)]
pub struct ReviewSummary {
    pub job_id: String,
    pub status: String,
    pub created_at: String,
    pub stance: String,
    pub plan_preview: String,
    pub name: Option<String>,
    pub total_cost: f64,
    pub num_rounds: i64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub completed_at: Option<String>,
    pub hivemind_id: Option<String>,
    pub num_models: i64,
    pub child_job_ids: Vec<String>,
    /// Absolute project path the review ran against (when known). Drives the
    /// project filter on the All Reviews page. `None` for legacy / pre-0019
    /// review rows that were stored before the column existed.
    pub project_path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ListReviewsResponse {
    pub reviews: Vec<ReviewSummary>,
    pub total_runs: u32,
}

// ---------------------------------------------------------------------------
// Resumable review snapshot (returned by `get_resumable_review_for_task`)
// ---------------------------------------------------------------------------
//
// Note: `ReviewResumePhase` lives in `crate::hivemind::phase` and is
// re-exported from this module for backwards compatibility.

/// One reviewer model entry for a resumable round — `thinking` is `None`
/// because the per-step `job_steps` schema does not record a thinking level.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResumableModelSpec {
    pub model_id: String,
    pub provider: String,
    pub stance: String,
    pub thinking: Option<String>,
}

/// Completed step output that should rehydrate into the resumed run so the
/// merge step does not need to re-dispatch already-finished models.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResumableStepOutput {
    pub model_id: String,
    pub provider: String,
    pub output: String,
}

/// Full snapshot the frontend needs to drive the per-phase resume flow.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResumableReviewSnapshot {
    pub review_id: String,
    pub latest_job_id: String,
    pub task_id: String,
    pub phase: ReviewResumePhase,
    pub round: u32,
    pub total_rounds: u32,
    pub plan_text: String,
    pub models: Vec<ResumableModelSpec>,
    pub completed_step_outputs: Vec<ResumableStepOutput>,
    pub merge_output: Option<String>,
    pub message: String,
}

// `HivemindProgressEvent` lives in `crate::hivemind::events` and is
// re-exported from this module above for backwards compatibility.

/// Start a new hivemind multi-model review.
///
/// Creates a review job, spawns a background task to run the review engine,
/// and returns the job ID immediately. Progress events are emitted as
/// `"hivemind-progress"` Tauri events.
#[tracing::instrument(
    skip(app, state, plan),
    fields(
        review_id = %review_id.as_deref().unwrap_or(""),
        plan_len = plan.len()
    )
)]
#[tauri::command]
pub async fn start_review(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    plan: String,
    stance: Option<String>,
    num_rounds: Option<u32>,
    timeout_seconds: Option<u32>,
    models: Option<Vec<String>>,
    review_id: Option<String>,
    hivemind_id: Option<String>,
    name: Option<String>,
    task_id: Option<String>,
    // Absolute path of the project the review was launched against. Stored
    // on the `jobs` row so the All Reviews page can filter by project.
    // Optional — callers that don't have a project context (subscription
    // flows, legacy frontends) may omit it.
    project_path: Option<String>,
    // 1-based cumulative round number for the Tasks-view multi-round flow.
    // The Tasks runtime dispatches each round as a separate `start_review`
    // call with `num_rounds = 1` (see app/src/lib/taskRuntime.tsx and the
    // comment above the FE-driven advancement branch); without this field
    // the engine reports `round = 1` for every call and round 2's capture
    // files (`merge-r1.txt`, `output-*-r1.txt`) silently overwrite round 1's.
    // Defaults to 1 (single-round behaviour) when omitted.
    round_number: Option<u32>,
    // Optional per-model context-window override map, keyed by
    // "provider/model_id". Used to populate ReviewModelConfig::context_window
    // from values captured at model selection time. Missing entries fall
    // through to the hardcoded get_model_context_window table.
    model_context_windows: Option<HashMap<String, u64>>,
    // Optional per-model sampling overrides, keyed by "provider/model_id".
    // Missing entries leave the corresponding ReviewModelConfig field at None,
    // which omits the field from the outbound provider request body.
    model_temperatures: Option<HashMap<String, f64>>,
    model_top_ps: Option<HashMap<String, f64>>,
    // Optional provider-qualified orchestrator override
    // ("provider/model_id" or bare). When supplied, replaces the
    // hivemind's stored orchestrator entirely — the frontend resolves
    // the `inherit_orchestrator` flag against the Task's current model
    // and sends the result here. When omitted, the backend falls back to
    // (stored orchestrator → last-reviewer-in-round), which is the legacy
    // behaviour callers that don't know about inheritance rely on.
    orchestrator_model: Option<String>,
) -> Result<String, IpcError> {
    // Bound the plan size up-front so a runaway frontend payload can't blow up
    // SQLite rows or memory before we even start dispatching models.
    if plan.len() > MAX_PLAN_LEN {
        return Err(IpcError::internal(format!(
            "plan too large: {} bytes (max {})",
            plan.len(),
            MAX_PLAN_LEN
        )));
    }

    // The frontend may pass review_id / hivemind_id; both flow into SQLite and
    // (for review_id) onto the filesystem under `reviews_dir`. Validate before
    // we touch anything. job_id is generated server-side so no input check needed.
    if let Some(ref rid) = review_id {
        validate_review_id(rid)?;
    }
    if let Some(ref hid) = hivemind_id {
        validate_hivemind_id(hid)?;
    }
    if let Some(ref tid) = task_id {
        validate_task_id(tid)?;
    }

    // Stance is hardcoded to 'against' — empirically yields the best critique quality.
    // Any value passed from the frontend is ignored.
    let _ = stance;
    let effective_stance = "against".to_string();
    let effective_rounds = num_rounds.unwrap_or(1);
    // Convert the 1-based cumulative round number from the FE into the
    // 0-based offset the engine adds to its internal `round_idx + 1`. A
    // missing or zero value collapses to offset=0 (engine starts at round 1).
    let round_offset = round_number.unwrap_or(1).saturating_sub(1);
    let effective_timeout = timeout_seconds.unwrap_or(300);
    let effective_models = models
        .unwrap_or_else(|| vec!["claude-sonnet-4-20250514".to_string(), "gpt-4o".to_string()]);

    info!(
        stance = %effective_stance,
        rounds = effective_rounds,
        model_count = effective_models.len(),
        "start_review invoked"
    );
    tracing::debug!(plan_preview = %plan.chars().take(1000).collect::<String>(), "start_review plan preview");

    // Generate job ID
    let job_id = uuid::Uuid::new_v4().to_string();

    // Create the job in the store
    let store = Arc::clone(&state.hivemind_store);
    let normalized_project_path = project_path
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    store
        .create_job(
            &job_id,
            &plan,
            &effective_stance,
            effective_rounds as i64,
            effective_timeout as i64,
            review_id.as_deref(),
            hivemind_id.as_deref(),
            name.as_deref(),
            task_id.as_deref(),
            normalized_project_path.as_deref(),
        )
        .await
        .map_err(|e| format!("failed to create review job: {}", e))?;

    let job_id_clone = job_id.clone();
    let review_id_for_events = review_id.clone();

    // Parse stance — hardcoded to Against only (multi-stance support removed).
    let parsed_stance = Stance::Against;

    // Build round-model configs. Tasks-flow IPC sends a flat model list and
    // a num_rounds count; we replicate the same model set across every round.
    // Provider-qualified ids ("provider/model_id") take precedence over the
    // bare form; bare ids fall through to a heuristic provider inference.
    let round_models: Vec<crate::hivemind::engine::RoundModel> = effective_models
        .iter()
        .map(|m| {
            let parts: Vec<&str> = m.splitn(2, '/').collect();
            let (provider_name, model_id, qualified_key) = if parts.len() == 2 {
                (parts[0].to_string(), parts[1].to_string(), m.clone())
            } else {
                let provider = if m.starts_with("claude") {
                    "anthropic"
                } else if m.starts_with("gpt") || m.starts_with("o3") || m.starts_with("o1") {
                    "openai"
                } else {
                    "openrouter"
                };
                let qualified = format!("{}/{}", provider, m);
                (provider.to_string(), m.clone(), qualified)
            };
            let temperature = model_temperatures
                .as_ref()
                .and_then(|map| map.get(&qualified_key))
                .copied();
            let top_p = model_top_ps
                .as_ref()
                .and_then(|map| map.get(&qualified_key))
                .copied();
            crate::hivemind::engine::RoundModel {
                id: model_id,
                provider: provider_name,
                temperature,
                top_p,
                custom_prompt_id: None,
                thinking: None,
            }
        })
        .collect();

    let rounds: Vec<crate::hivemind::engine::RoundCfg> = (0..effective_rounds)
        .map(|_| crate::hivemind::engine::RoundCfg {
            models: round_models.clone(),
            timeout: effective_timeout,
        })
        .collect();

    // Orchestrator selection priority:
    //   1. Caller-supplied `orchestrator_model` override — the frontend uses
    //      this to honour the hivemind's `inherit_orchestrator` flag (when
    //      set, it passes the Task's currently-active model in here).
    //   2. Hivemind row's explicit `orchestrator_model` / `orchestrator_provider`.
    //   3. The LAST reviewer in the round.
    //
    // Without (1) the inherit flag is unreachable from the backend (start_review
    // has no knowledge of the calling Task's model), and a hivemind toggled to
    // "inherit" silently falls through to (3) — picking whichever reviewer
    // happened to be ordered last in the round, which is rarely what the user
    // configured. See `commands/hivemind.rs:545` and the inherit chain in
    // `app/src/lib/taskRuntime.tsx`.
    let orchestrator = if let Some(ref override_model) = orchestrator_model {
        orchestrator_from_override(override_model)
    } else if let Some(ref hid) = hivemind_id {
        match state.hivemind_store.get_hivemind(hid).await {
            Ok(Some(cfg)) => {
                if let (Some(model), Some(provider)) = (
                    cfg.orchestrator_model.clone(),
                    cfg.orchestrator_provider.clone(),
                ) {
                    crate::hivemind::engine::OrchestratorCfg {
                        model,
                        provider,
                        system_prompt: None,
                    }
                } else {
                    last_model_as_orchestrator(&round_models)
                }
            }
            _ => last_model_as_orchestrator(&round_models),
        }
    } else {
        last_model_as_orchestrator(&round_models)
    };

    let (concurrency_cap, custom_prompts) = {
        let cfg = state.config.read().await;
        (cfg.concurrency_cap, cfg.custom_prompts.clone())
    };

    let logger: Option<Arc<ReviewLogger>> = if let Some(ref rid) = review_id {
        let mut loggers = state.review_loggers.lock().await;
        if let Some(existing) = loggers.get(rid) {
            Some(Arc::clone(existing))
        } else if let Some(new_logger) = review_log::create_if_debug(&state.reviews_dir, rid).await
        {
            loggers.insert(rid.clone(), Arc::clone(&new_logger));
            Some(new_logger)
        } else {
            None
        }
    } else {
        None
    };

    let cancel_token = tokio_util::sync::CancellationToken::new();
    {
        let mut reviews = state.running_reviews.write().await;
        reviews.insert(job_id_clone.clone(), cancel_token.clone());
    }
    let running_reviews = Arc::clone(&state.running_reviews);
    let review_loggers_arc = Arc::clone(&state.review_loggers);

    let pi_manager = state.pi_manager.clone();
    let usage_store = state.usage_store.clone();
    let provider_registry = state.provider_registry.clone();
    let merge_capture_registry = state.merge_capture.clone();
    let reviews_dir = state.reviews_dir.clone();
    let nurse_engine = state.nurse_engine().cloned();
    let response_cache = Arc::clone(&state.response_cache);
    let review_id_for_cleanup = review_id.clone();
    let task_id_for_attrib = task_id.clone();
    let source_label = task_id
        .clone()
        .map(|tid| format!("Task {}", tid))
        .unwrap_or_else(|| "Hivemind review".to_string());

    tokio::spawn(
        async move {
            struct ReviewCleanupGuard {
                job_id: String,
                review_id: Option<String>,
                running_reviews:
                    Arc<tokio::sync::RwLock<HashMap<String, tokio_util::sync::CancellationToken>>>,
                review_loggers: Arc<tokio::sync::Mutex<HashMap<String, Arc<ReviewLogger>>>>,
            }
            impl Drop for ReviewCleanupGuard {
                fn drop(&mut self) {
                    let job_id = self.job_id.clone();
                    let review_id = self.review_id.clone();
                    let running_reviews = Arc::clone(&self.running_reviews);
                    let review_loggers = Arc::clone(&self.review_loggers);
                    tokio::spawn(
                        async move {
                            {
                                let mut reviews = running_reviews.write().await;
                                reviews.remove(&job_id);
                            }
                            if let Some(rid) = review_id {
                                let mut loggers = review_loggers.lock().await;
                                loggers.remove(&rid);
                            }
                        }
                        .instrument(tracing::Span::current()),
                    );
                }
            }
            let _cleanup_guard = ReviewCleanupGuard {
                job_id: job_id_clone.clone(),
                review_id: review_id_for_cleanup,
                running_reviews: Arc::clone(&running_reviews),
                review_loggers: Arc::clone(&review_loggers_arc),
            };

            // Use the process-wide ResponseCache singleton from AppState so hits
            // accumulate across reviews. (Previously each review constructed its
            // own throwaway cache, driving the effective hit rate to zero.)
            let engine = ReviewEngine::new(response_cache);
            let attribution = crate::hivemind::engine::ReviewAttribution {
                review_id: review_id_for_events.clone(),
                task_id: task_id_for_attrib,
                swarm_id: None,
                feature_id: None,
                project_path: normalized_project_path.clone(),
                source_label,
            };
            let run_config = crate::hivemind::engine::HivemindRunConfig {
                hivemind_id: hivemind_id.unwrap_or_default(),
                rounds,
                orchestrator,
                stance: parsed_stance,
                concurrency_cap,
                context: crate::hivemind::engine::ContextSpec::None,
                initial_plan: plan,
                attribution,
                existing_job_id: Some(job_id_clone.clone()),
                round_offset,
            };
            let deps = crate::hivemind::engine::EngineDeps {
                pi_manager,
                store: store.clone(),
                provider_registry,
                usage_store,
                merge_capture_registry,
                reviews_dir,
                app: app.clone(),
                review_logger: logger.clone(),
                activity_tx: None,
                nurse_engine,
                custom_prompts,
            };

            let result = engine.run(run_config, deps, cancel_token).await;

            {
                let mut reviews = running_reviews.write().await;
                reviews.remove(&job_id_clone);
            }
            if let Some(ref l) = logger {
                l.log("review_completed", serde_json::json!({})).await;
            }

            match result {
                Ok(outcome) => {
                    info!(
                        job_id = %job_id_clone,
                        output_len = outcome.refined_plan.len(),
                        "review completed successfully"
                    );
                }
                Err(e) => {
                    let error_msg = e.to_string();
                    let cancelled = crate::hivemind::error::is_cancellation(&e);
                    if cancelled {
                        // Cancellation is user intent, not a failure. Mark the
                        // job as `cancelled` and emit the `cancelled` event so
                        // the UI renders a neutral pill instead of a red "Failed"
                        // pill. The inner engine path may have already emitted a
                        // `cancelled` event from the round-boundary check, but
                        // this is the catch-all for the cases where cancellation
                        // tripped between phases.
                        let _ = store.update_job_status(&job_id_clone, "cancelled").await;
                        let _ = app.emit(
                            "hivemind-progress",
                            HivemindProgressEvent {
                                job_id: job_id_clone.clone(),
                                review_id: review_id_for_events.clone(),
                                event_type: "cancelled".to_string(),
                                round: 0,
                                model_id: String::new(),
                                message: "Review cancelled".to_string(),
                                phase: Some("cancelled".to_string()),
                                ..Default::default()
                            },
                        );
                        info!(job_id = %job_id_clone, "review cancelled by user");
                    } else {
                        let _ = store.fail_job(&job_id_clone, &error_msg).await;
                        let _ = app.emit(
                            "hivemind-progress",
                            HivemindProgressEvent {
                                job_id: job_id_clone.clone(),
                                review_id: review_id_for_events.clone(),
                                event_type: "failed".to_string(),
                                round: 0,
                                model_id: String::new(),
                                message: error_msg.clone(),
                                phase: Some("failed".to_string()),
                                ..Default::default()
                            },
                        );
                        error!(job_id = %job_id_clone, error = %error_msg, "review failed");
                    }
                }
            }
        }
        .instrument(tracing::Span::current()),
    );

    Ok(job_id)
}

/// Parse a frontend-supplied orchestrator override into an
/// [`OrchestratorCfg`]. Accepts the canonical `"provider/model_id"` form
/// the rest of the codebase uses, and falls back to the same
/// provider-by-prefix heuristic as reviewer-model parsing when the slash
/// is missing. An empty string is treated as a missing override by the
/// caller; this function would still parse it (provider="openrouter",
/// model=""), so callers must guard upstream.
fn orchestrator_from_override(spec: &str) -> crate::hivemind::engine::OrchestratorCfg {
    let parts: Vec<&str> = spec.splitn(2, '/').collect();
    let (provider, model) = if parts.len() == 2 {
        (parts[0].to_string(), parts[1].to_string())
    } else {
        let provider = if spec.starts_with("claude") {
            "anthropic"
        } else if spec.starts_with("gpt") || spec.starts_with("o3") || spec.starts_with("o1") {
            "openai"
        } else {
            "openrouter"
        };
        (provider.to_string(), spec.to_string())
    };
    crate::hivemind::engine::OrchestratorCfg {
        model,
        provider,
        system_prompt: None,
    }
}

fn last_model_as_orchestrator(
    round_models: &[crate::hivemind::engine::RoundModel],
) -> crate::hivemind::engine::OrchestratorCfg {
    let last = round_models.last().cloned();
    match last {
        Some(m) => crate::hivemind::engine::OrchestratorCfg {
            model: m.id,
            provider: m.provider,
            system_prompt: None,
        },
        None => crate::hivemind::engine::OrchestratorCfg {
            model: "claude-sonnet-4-20250514".to_string(),
            provider: "anthropic".to_string(),
            system_prompt: None,
        },
    }
}

/// Cancel an in-flight hivemind review.
///
/// Signals cancellation via the registered `CancellationToken`, marks the job
/// `cancelled` in SQLite, and emits a `hivemind-progress { event_type:
/// "cancelled" }` event so the frontend can clear in-memory orchestration
/// state. Safe to call when the job has already finished — the SQLite update
/// is harmless and the registry lookup simply finds no token to cancel.
#[tracing::instrument(
    skip(app, state),
    fields(job_id = %job_id, review_id = tracing::field::Empty)
)]
#[tauri::command]
pub async fn cancel_review(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    job_id: String,
) -> Result<(), IpcError> {
    validate_id(&job_id)?;
    info!(job_id = %job_id, "cancel_review invoked");

    let token_opt = {
        let reviews = state.running_reviews.read().await;
        reviews.get(&job_id).cloned()
    };
    if let Some(token) = token_opt {
        token.cancel();
        info!(job_id = %job_id, "review cancellation signalled");
    }

    state
        .hivemind_store
        .update_job_status(&job_id, "cancelled")
        .await
        .map_err(|e| format!("failed to mark job cancelled: {}", e))?;

    let resolved_review_id = state
        .hivemind_store
        .get_job(&job_id)
        .await
        .ok()
        .flatten()
        .and_then(|j| j.review_id);
    if let Some(ref rid) = resolved_review_id {
        tracing::Span::current().record("review_id", rid.as_str());
    }

    let _ = app.emit(
        "hivemind-progress",
        HivemindProgressEvent {
            job_id: job_id.clone(),
            review_id: resolved_review_id,
            event_type: "cancelled".to_string(),
            round: 0,
            model_id: String::new(),
            message: "Review cancelled".to_string(),
            // Distinct phase tag — lets the UI pick a neutral/amber pill
            // instead of falling back to the red "failed" tone.
            phase: Some("cancelled".to_string()),
            ..Default::default()
        },
    );

    Ok(())
}

/// Delete a logical review run by its run_id (e.g. "hmr-a1b2c3d4" or a numeric job id).
/// Removes all database rows and on-disk artifacts.
/// Returns an error if any job in the run is still running or pending.
#[tracing::instrument(skip(state))]
#[tauri::command]
pub async fn delete_review(
    state: tauri::State<'_, AppState>,
    run_id: String,
) -> Result<(), IpcError> {
    validate_id(&run_id)?;

    info!(run_id = %run_id, "delete_review invoked");

    // Guard: refuse to delete a running review
    let has_running = state
        .hivemind_store
        .any_job_in_status_for_logical_run(&run_id, &["pending", "running"])
        .await
        .map_err(|e| format!("failed to check running jobs: {}", e))?;
    if has_running {
        return Err(IpcError::validation(
            "Cannot delete a running review. Cancel it first.",
        ));
    }

    // Also check for round_N statuses (mid-review)
    let has_round_active = state
        .hivemind_store
        .any_job_in_status_for_logical_run(
            &run_id,
            &["round_1", "round_2", "round_3", "round_4", "round_5"],
        )
        .await
        .map_err(|e| format!("failed to check active round jobs: {}", e))?;
    if has_round_active {
        return Err(IpcError::validation(
            "Cannot delete a running review. Cancel it first.",
        ));
    }

    // Collect merge run output paths before deletion
    let output_paths: Vec<String> = sqlx::query_scalar(
        "SELECT output_path FROM merge_runs \
         WHERE job_id IN ( \
           SELECT id FROM jobs \
           WHERE COALESCE(NULLIF(review_id, ''), id) = ?1 \
         )",
    )
    .bind(&run_id)
    .fetch_all(state.hivemind_store.pool())
    .await
    .map_err(|e| format!("failed to fetch merge output paths: {}", e))?;

    // Delete database rows
    state
        .hivemind_store
        .delete_logical_run(&run_id)
        .await
        .map_err(|e| format!("failed to delete logical run: {}", e))?;

    // Clean up on-disk artifacts
    let reviews_dir = &state.reviews_dir;

    // Review log file: {reviews_dir}/{run_id}.jsonl
    let log_path = reviews_dir.join(format!("{}.jsonl", run_id));
    let _ = tokio::fs::remove_file(&log_path).await;

    // Output captures directory: {reviews_dir}/{run_id}/
    let captures_dir = reviews_dir.join(&run_id);
    let _ = tokio::fs::remove_dir_all(&captures_dir).await;

    // Merge output files
    for path in &output_paths {
        let _ = tokio::fs::remove_file(path).await;
    }

    info!(run_id = %run_id, "delete_review completed");
    Ok(())
}

/// Get the current status and details of a review job.
#[tracing::instrument(skip(state))]
#[tauri::command]
pub async fn get_review_status(
    state: tauri::State<'_, AppState>,
    job_id: String,
) -> Result<ReviewStatus, IpcError> {
    validate_id(&job_id)?;
    info!(job_id = %job_id, "get_review_status invoked");

    let job = state
        .hivemind_store
        .get_job(&job_id)
        .await
        .map_err(|e| format!("failed to fetch job: {}", e))?
        .ok_or_else(|| format!("review job '{}' not found", job_id))?;

    let job_steps = state
        .hivemind_store
        .get_job_steps(&job_id)
        .await
        .map_err(|e| format!("failed to fetch job steps: {}", e))?;

    let steps = job_steps
        .iter()
        .map(|s| {
            let output_preview = s
                .output
                .as_deref()
                .map(|o| truncate_preview(o, 200))
                .unwrap_or_default();
            StepSummary {
                model_id: s.model_id.clone(),
                provider: s.provider.clone(),
                status: s.status.clone(),
                output_preview,
                input_tokens: s.input_tokens,
                output_tokens: s.output_tokens,
                duration_ms: s.duration_ms,
                round_number: s.round_number,
            }
        })
        .collect();

    Ok(ReviewStatus {
        job_id: job.id,
        status: job.status,
        current_round: job.current_round,
        total_rounds: job.num_rounds,
        steps,
        error: job.error,
        final_output: job.final_output,
        total_cost: job.total_cost,
        total_input_tokens: job.total_input_tokens,
        total_output_tokens: job.total_output_tokens,
        created_at: job.created_at,
        completed_at: job.completed_at,
    })
}

/// True for statuses that are finished: completed, failed, cancelled.
fn is_terminal_status(s: &str) -> bool {
    matches!(s, "completed" | "failed" | "cancelled")
}

/// True for statuses that are in-progress: pending, running, or an active round_N state.
fn is_running_status(s: &str) -> bool {
    matches!(s, "pending" | "running") || s.starts_with("round_")
}

fn logical_run_id(job: &Job) -> String {
    job.review_id
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or(&job.id)
        .to_string()
}

fn aggregate_status<'a>(statuses: impl IntoIterator<Item = &'a str>) -> String {
    let mut any_running = false;
    let mut any_failed = false;
    let mut any_cancelled = false;
    for status in statuses {
        any_running |= is_running_status(status);
        any_failed |= status == "failed";
        any_cancelled |= status == "cancelled";
    }
    if any_running {
        "running".to_string()
    } else if any_failed {
        "failed".to_string()
    } else if any_cancelled {
        "cancelled".to_string()
    } else {
        "completed".to_string()
    }
}

fn map_step_full(s: &JobStep) -> StepFull {
    StepFull {
        model_id: s.model_id.clone(),
        provider: s.provider.clone(),
        status: s.status.clone(),
        output: s.output.clone().unwrap_or_default(),
        input_tokens: s.input_tokens,
        output_tokens: s.output_tokens,
        duration_ms: s.duration_ms,
        round_number: s.round_number,
        cost: s.cost,
        prompt: s.prompt.clone(),
        error: s.error.clone(),
    }
}

fn single_job_snapshot(job: Job, job_steps: Vec<JobStep>) -> ReviewStateSnapshot {
    let steps = job_steps.iter().map(map_step_full).collect();
    let is_running = is_running_status(&job.status);
    ReviewStateSnapshot {
        job_id: job.id,
        status: job.status,
        is_running,
        current_round: job.current_round,
        total_rounds: job.num_rounds,
        steps,
        error: job.error,
        final_output: job.final_output,
        total_cost: job.total_cost,
        total_input_tokens: job.total_input_tokens,
        total_output_tokens: job.total_output_tokens,
        created_at: job.created_at,
        completed_at: job.completed_at,
    }
}

// Invariant: `job_steps.round_number`, `merge_runs.round_number`, and
// `round_verdicts.round_number` are written by the engine as CUMULATIVE round
// numbers across all child jobs sharing a `review_id`. See
// `hivemind/engine.rs` ~line 705 — `round_num = (round_idx + 1) + round_offset`
// is the only place those columns are populated, and `round_offset` is the
// per-call offset derived from the Tasks-view's 1-based cumulative
// `roundNumber`. Per-child rounds always start at the child's own offset.
//
// Consequence: the aggregator MUST NOT add a per-child offset on the read
// path. Doing so double-applies the offset and shifts round 2's rows out
// to round 3, which the frontend then renders as "No model results yet".
// `map_step_full` is intentionally parameterless to make any reintroduction
// of offset arithmetic a compile error.
fn aggregate_review_state(
    job_id: &str,
    mut jobs: Vec<Job>,
    steps: Vec<JobStep>,
) -> ReviewStateSnapshot {
    jobs.sort_by(|a, b| {
        a.created_at
            .cmp(&b.created_at)
            .then_with(|| a.id.cmp(&b.id))
    });
    let mut steps_by_job: HashMap<String, Vec<JobStep>> = HashMap::new();
    for step in steps {
        steps_by_job
            .entry(step.job_id.clone())
            .or_default()
            .push(step);
    }
    for v in steps_by_job.values_mut() {
        v.sort_by(|a, b| {
            a.round_number
                .cmp(&b.round_number)
                .then_with(|| a.sort_order.cmp(&b.sort_order))
                .then_with(|| a.id.cmp(&b.id))
        });
    }

    let total_rounds: i64 = jobs.iter().map(|j| j.num_rounds).sum();
    let status = aggregate_status(jobs.iter().map(|j| j.status.as_str()));
    let is_running = jobs.iter().any(|j| is_running_status(&j.status));

    // Best-effort `current_round` derivation.
    //
    // `jobs.current_round` is unreliable — the migration defaults the column
    // to 0 and no UPDATE statement in `store.rs` ever writes to it. We derive
    // the round purely from the two signals the engine actually emits:
    //   1. `jobs.status` shaped as `round_{N}` (cumulative — set via a
    //      `tokio::spawn(update_job_status)` in `engine.rs` ~line 730).
    //   2. `max(job_steps.round_number)` per job (also cumulative — written
    //      by `dispatch_round` in `engine.rs` ~line 1374).
    //
    // The status update is fire-and-forget so step rows can land first. We
    // take the max of BOTH signals across every job (terminal or not) so a
    // stuck earlier child stays out of the way once a later child has moved
    // on (crash-recovery case). Pre-dispatch transient ("running" with no
    // steps) falls back to the previous child's max — that under-reports by
    // at most one round, and `current_round` is observability-only.
    let current_round = if !is_running {
        total_rounds
    } else {
        let mut best: i64 = 0;
        for job in &jobs {
            let from_status = job
                .status
                .strip_prefix("round_")
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(0);
            let from_steps = steps_by_job
                .get(&job.id)
                .and_then(|steps| steps.iter().map(|s| s.round_number).max())
                .unwrap_or(0);
            best = best.max(from_status).max(from_steps);
        }
        best.clamp(0, total_rounds.max(0))
    };

    let mut full_steps = Vec::new();
    for job in &jobs {
        if let Some(job_steps) = steps_by_job.get(&job.id) {
            full_steps.extend(job_steps.iter().map(map_step_full));
        }
    }

    let error = {
        let errors: Vec<String> = jobs.iter().filter_map(|j| j.error.clone()).collect();
        if errors.is_empty() {
            None
        } else {
            Some(errors.join("; "))
        }
    };
    let final_output = jobs.iter().rev().find_map(|j| j.final_output.clone());
    let created_at = jobs
        .first()
        .map(|j| j.created_at.clone())
        .unwrap_or_default();
    let completed_at = if jobs
        .iter()
        .all(|j| is_terminal_status(&j.status) && j.completed_at.is_some())
    {
        jobs.iter().filter_map(|j| j.completed_at.clone()).max()
    } else {
        None
    };

    ReviewStateSnapshot {
        job_id: job_id.to_string(),
        status,
        is_running,
        current_round,
        total_rounds,
        steps: full_steps,
        error,
        final_output,
        total_cost: jobs.iter().map(|j| j.total_cost).sum(),
        total_input_tokens: jobs.iter().map(|j| j.total_input_tokens).sum(),
        total_output_tokens: jobs.iter().map(|j| j.total_output_tokens).sum(),
        created_at,
        completed_at,
    }
}

/// Build a `ReviewStateSnapshot` directly from a `HivemindStore`.
///
/// Extracted into a free function so tests can exercise the SQLite read path
/// without constructing a full `tauri::State<AppState>`.
async fn build_review_state_snapshot(
    store: &crate::hivemind::store::HivemindStore,
    job_id: &str,
) -> Result<ReviewStateSnapshot, String> {
    let child_count = store
        .count_jobs_with_review_id(job_id)
        .await
        .map_err(|e| format!("failed to count child jobs: {}", e))?;
    if child_count > 0 {
        let jobs = store
            .list_jobs_by_review_id(job_id)
            .await
            .map_err(|e| format!("failed to fetch child jobs: {}", e))?;
        let job_ids: Vec<String> = jobs.iter().map(|j| j.id.clone()).collect();
        let steps = store
            .fetch_steps_for_jobs(&job_ids)
            .await
            .map_err(|e| format!("failed to fetch job steps: {}", e))?;
        return Ok(aggregate_review_state(job_id, jobs, steps));
    }

    // ── Primary: try to load the job directly
    match store
        .get_job(job_id)
        .await
        .map_err(|e| format!("failed to fetch job: {}", e))?
    {
        Some(job) if job.review_id.as_deref().filter(|s| !s.is_empty()).is_some() => {
            // ── Fallback: this job is a child (has a non-empty review_id),
            // so look up siblings via the parent review_id. Handles the case
            // where the frontend passes a child job UUID instead of the
            // logical run ID.
            let parent_review_id = job.review_id.as_deref().unwrap();
            let parent_count = store
                .count_jobs_with_review_id(parent_review_id)
                .await
                .map_err(|e| format!("failed to count parent child jobs: {}", e))?;
            if parent_count > 0 {
                let parent_jobs = store
                    .list_jobs_by_review_id(parent_review_id)
                    .await
                    .map_err(|e| format!("failed to fetch parent jobs: {}", e))?;
                let parent_ids: Vec<String> = parent_jobs.iter().map(|j| j.id.clone()).collect();
                let parent_steps = store
                    .fetch_steps_for_jobs(&parent_ids)
                    .await
                    .map_err(|e| format!("failed to fetch parent job steps: {}", e))?;
                return Ok(aggregate_review_state(
                    parent_review_id,
                    parent_jobs,
                    parent_steps,
                ));
            }
            // No sibling jobs found — fall through to single-job path
        }
        _ => {}
    }

    // ── Single-job path: load the job (which must exist at this point)
    let job = store
        .get_job(job_id)
        .await
        .map_err(|e| format!("failed to fetch job: {}", e))?
        .ok_or_else(|| format!("review job '{}' not found", job_id))?;

    let job_steps = store
        .get_job_steps(job_id)
        .await
        .map_err(|e| format!("failed to fetch job steps: {}", e))?;

    Ok(single_job_snapshot(job, job_steps))
}

/// Get the full canonical state of a review job from SQLite.
///
/// This is the resync path used by the frontend to reconcile after navigating
/// away, after a window-focus event, or when a `hivemind-progress` event was
/// dropped. Unlike `get_review_status`, step outputs are returned untruncated,
/// and `is_running` is derived server-side.
#[tracing::instrument(skip(state))]
#[tauri::command]
pub async fn get_review_state(
    state: tauri::State<'_, AppState>,
    job_id: String,
) -> Result<ReviewStateSnapshot, IpcError> {
    validate_id(&job_id).map_err(IpcError::validation)?;
    info!(job_id = %job_id, "get_review_state invoked");
    build_review_state_snapshot(&state.hivemind_store, &job_id)
        .await
        .map_err(IpcError::internal)
}

fn summarize_logical_run(
    run_id: &str,
    mut jobs: Vec<Job>,
    model_counts_by_job_id: &HashMap<String, i64>,
) -> Option<ReviewSummary> {
    if jobs.is_empty() {
        return None;
    }
    jobs.sort_by(|a, b| {
        a.created_at
            .cmp(&b.created_at)
            .then_with(|| a.id.cmp(&b.id))
    });
    let first = jobs.first()?;
    let status = aggregate_status(jobs.iter().map(|j| j.status.as_str()));
    let all_terminal_with_completed_at = jobs
        .iter()
        .all(|j| is_terminal_status(&j.status) && j.completed_at.is_some());
    let completed_at = if all_terminal_with_completed_at {
        jobs.iter().filter_map(|j| j.completed_at.clone()).max()
    } else {
        None
    };
    let child_job_ids = jobs.iter().map(|j| j.id.clone()).collect::<Vec<_>>();
    let name = first.name.clone();
    let name = if name.is_empty() { None } else { Some(name) };
    // Use the first child job that has a project_path. Multi-round runs are
    // always launched from the same task / project, so the value is uniform
    // across children; using `first()` deterministically picks the row we'd
    // have chosen for `name`/`stance`/`plan_preview`.
    let project_path = jobs
        .iter()
        .find_map(|j| j.project_path.clone())
        .filter(|s| !s.trim().is_empty());
    Some(ReviewSummary {
        job_id: run_id.to_string(),
        status,
        created_at: jobs
            .iter()
            .map(|j| j.created_at.clone())
            .min()
            .unwrap_or_default(),
        stance: first.stance.clone(),
        plan_preview: truncate_preview(&first.plan, 200),
        name,
        total_cost: jobs.iter().map(|j| j.total_cost).sum(),
        num_rounds: jobs.iter().map(|j| j.num_rounds).sum(),
        total_input_tokens: jobs.iter().map(|j| j.total_input_tokens).sum(),
        total_output_tokens: jobs.iter().map(|j| j.total_output_tokens).sum(),
        completed_at,
        hivemind_id: first.hivemind_id.clone(),
        num_models: jobs
            .iter()
            .map(|j| model_counts_by_job_id.get(&j.id).copied().unwrap_or(0))
            .max()
            .unwrap_or(0),
        child_job_ids,
        project_path,
    })
}

/// List recent logical review runs with pagination.
#[tracing::instrument(skip(state))]
#[tauri::command]
pub async fn list_reviews(
    state: tauri::State<'_, AppState>,
    limit: Option<u32>,
    offset: Option<u32>,
    hivemind_id: Option<String>,
) -> Result<ListReviewsResponse, IpcError> {
    if let Some(ref hid) = hivemind_id {
        validate_id(hid)?;
    }
    let effective_limit = limit.unwrap_or(20) as i64;
    let effective_offset = offset.unwrap_or(0) as i64;

    info!(
        limit = effective_limit,
        offset = effective_offset,
        hivemind_id = ?hivemind_id,
        "list_reviews invoked"
    );

    let total_runs = state
        .hivemind_store
        .count_logical_runs(hivemind_id.as_deref())
        .await
        .map_err(|e| format!("failed to count logical review runs: {}", e))?;

    let page = state
        .hivemind_store
        .list_logical_run_page(hivemind_id.as_deref(), effective_limit, effective_offset)
        .await
        .map_err(|e| format!("failed to list logical review runs: {}", e))?;

    debug!(job_count = page.jobs.len(), run_count = page.run_ids.len(), total_runs, hivemind_id = ?hivemind_id, "list_reviews query returned");

    let mut jobs_by_run: HashMap<String, Vec<Job>> = HashMap::new();
    for job in page.jobs {
        jobs_by_run
            .entry(logical_run_id(&job))
            .or_default()
            .push(job);
    }

    let reviews = page
        .run_ids
        .iter()
        .filter_map(|run_id| {
            let jobs = jobs_by_run.remove(run_id).unwrap_or_default();
            summarize_logical_run(run_id, jobs, &page.model_counts_by_job_id)
        })
        .collect();

    Ok(ListReviewsResponse {
        reviews,
        total_runs,
    })
}

// ---------------------------------------------------------------------------
// Hivemind config CRUD
// ---------------------------------------------------------------------------

/// Summary of a saved hivemind configuration.
#[derive(Debug, Clone, Serialize)]
pub struct HivemindSummary {
    pub id: String,
    pub name: String,
    pub description: String,
    pub rounds_config: String,
    pub inherit_orchestrator: bool,
    pub orchestrator_model: Option<String>,
    pub orchestrator_provider: Option<String>,
    pub orchestrator_thinking: String,
    pub orchestrator_context_window: Option<i64>,
    pub orchestrator_max_output: Option<i64>,
    pub runs: i64,
    pub created_at: String,
    pub updated_at: String,
}

#[tracing::instrument(skip(state))]
#[tauri::command]
pub async fn create_hivemind(
    state: tauri::State<'_, AppState>,
    name: String,
    description: String,
    rounds_config: String,
    inherit_orchestrator: Option<bool>,
    orchestrator_model: Option<String>,
    orchestrator_provider: Option<String>,
    orchestrator_thinking: Option<String>,
    orchestrator_context_window: Option<i64>,
    orchestrator_max_output: Option<i64>,
) -> Result<HivemindSummary, IpcError> {
    // Cap the free-form rounds_config blob at the IPC boundary before it
    // hits SQLite. 64 KiB easily fits a 12-model, 5-round config.
    if rounds_config.len() > MAX_ROUNDS_CONFIG {
        return Err(IpcError::internal(format!(
            "rounds_config too large: {} bytes (max {})",
            rounds_config.len(),
            MAX_ROUNDS_CONFIG
        )));
    }

    let id = uuid::Uuid::new_v4().to_string();
    info!(id = %id, name = %name, "create_hivemind invoked");

    let inherit = inherit_orchestrator.unwrap_or(true);
    let thinking = orchestrator_thinking.unwrap_or_else(|| "high".to_string());

    state
        .hivemind_store
        .create_hivemind(
            &id,
            &name,
            &description,
            &rounds_config,
            inherit,
            orchestrator_model.as_deref(),
            orchestrator_provider.as_deref(),
            &thinking,
            orchestrator_context_window,
            orchestrator_max_output,
        )
        .await
        .map_err(|e| format!("failed to create hivemind: {}", e))?;

    let config = state
        .hivemind_store
        .get_hivemind(&id)
        .await
        .map_err(|e| format!("failed to fetch created hivemind: {}", e))?
        .ok_or_else(|| "created hivemind not found".to_string())?;

    Ok(config_to_summary(config, 0))
}

#[tracing::instrument(skip(state))]
#[tauri::command]
pub async fn list_hiveminds(
    state: tauri::State<'_, AppState>,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<Vec<HivemindSummary>, IpcError> {
    let effective_limit = limit.unwrap_or(100) as i64;
    let effective_offset = offset.unwrap_or(0) as i64;

    info!(
        limit = effective_limit,
        offset = effective_offset,
        "list_hiveminds invoked"
    );

    let configs = state
        .hivemind_store
        .list_hiveminds(effective_limit, effective_offset)
        .await
        .map_err(|e| format!("failed to list hiveminds: {}", e))?;

    let ids: Vec<String> = configs.iter().map(|c| c.id.clone()).collect();
    let counts = state
        .hivemind_store
        .batch_count_hivemind_runs(&ids)
        .await
        .map_err(|e| format!("failed to count hivemind runs: {}", e))?;
    let summaries: Vec<HivemindSummary> = configs
        .into_iter()
        .map(|config| {
            let runs = counts.get(&config.id).copied().unwrap_or(0);
            config_to_summary(config, runs)
        })
        .collect();

    Ok(summaries)
}

#[tracing::instrument(skip(state))]
#[tauri::command]
pub async fn update_hivemind(
    state: tauri::State<'_, AppState>,
    hivemind_id: String,
    name: String,
    description: String,
    rounds_config: String,
    inherit_orchestrator: Option<bool>,
    orchestrator_model: Option<String>,
    orchestrator_provider: Option<String>,
    orchestrator_thinking: Option<String>,
    orchestrator_context_window: Option<i64>,
    orchestrator_max_output: Option<i64>,
) -> Result<HivemindSummary, IpcError> {
    validate_hivemind_id(&hivemind_id)?;
    // Cap the free-form rounds_config blob at the IPC boundary.
    if rounds_config.len() > MAX_ROUNDS_CONFIG {
        return Err(IpcError::internal(format!(
            "rounds_config too large: {} bytes (max {})",
            rounds_config.len(),
            MAX_ROUNDS_CONFIG
        )));
    }
    info!(hivemind_id = %hivemind_id, "update_hivemind invoked");

    let inherit = inherit_orchestrator.unwrap_or(true);
    let thinking = orchestrator_thinking.unwrap_or_else(|| "high".to_string());

    state
        .hivemind_store
        .update_hivemind(
            &hivemind_id,
            &name,
            &description,
            &rounds_config,
            inherit,
            orchestrator_model.as_deref(),
            orchestrator_provider.as_deref(),
            &thinking,
            orchestrator_context_window,
            orchestrator_max_output,
        )
        .await
        .map_err(|e| format!("failed to update hivemind: {}", e))?;

    let config = state
        .hivemind_store
        .get_hivemind(&hivemind_id)
        .await
        .map_err(|e| format!("failed to fetch updated hivemind: {}", e))?
        .ok_or_else(|| format!("hivemind '{}' not found", hivemind_id))?;

    let runs = state
        .hivemind_store
        .count_hivemind_runs(&hivemind_id)
        .await
        .map_err(|e| format!("failed to count hivemind runs: {}", e))?;

    Ok(config_to_summary(config, runs))
}

fn config_to_summary(config: crate::hivemind::store::HivemindConfig, runs: i64) -> HivemindSummary {
    HivemindSummary {
        id: config.id,
        name: config.name,
        description: config.description,
        rounds_config: config.rounds_config,
        inherit_orchestrator: config.inherit_orchestrator,
        orchestrator_model: config.orchestrator_model,
        orchestrator_provider: config.orchestrator_provider,
        orchestrator_thinking: config.orchestrator_thinking,
        orchestrator_context_window: config.orchestrator_context_window,
        orchestrator_max_output: config.orchestrator_max_output,
        runs,
        created_at: config.created_at,
        updated_at: config.updated_at,
    }
}

#[tracing::instrument(skip(state))]
#[tauri::command]
pub async fn delete_hivemind(
    state: tauri::State<'_, AppState>,
    hivemind_id: String,
) -> Result<(), IpcError> {
    validate_hivemind_id(&hivemind_id)?;
    info!(hivemind_id = %hivemind_id, "delete_hivemind invoked");

    state
        .hivemind_store
        .delete_hivemind(&hivemind_id)
        .await
        .map_err(|e| format!("failed to delete hivemind: {}", e))?;

    Ok(())
}

/// Full output for a single review step (no truncation).
#[derive(Debug, Clone, Serialize)]
pub struct StepOutput {
    pub model_id: String,
    pub provider: String,
    pub round_number: i64,
    pub output: String,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub duration_ms: Option<i64>,
    pub cost: Option<f64>,
}

/// Get full (untruncated) outputs for all steps of a review job.
///
/// Used by the frontend review flow to feed complete model outputs
/// into the merge agent.
#[tracing::instrument(skip(state))]
#[tauri::command]
pub async fn get_review_step_outputs(
    state: tauri::State<'_, AppState>,
    job_id: String,
) -> Result<Vec<StepOutput>, IpcError> {
    validate_id(&job_id)?;
    info!(job_id = %job_id, "get_review_step_outputs invoked");

    let job_steps = state
        .hivemind_store
        .get_job_steps(&job_id)
        .await
        .map_err(|e| format!("failed to fetch job steps: {}", e))?;

    let outputs = job_steps
        .into_iter()
        .filter_map(|s| {
            s.output.map(|output| StepOutput {
                model_id: s.model_id,
                provider: s.provider,
                round_number: s.round_number,
                output,
                input_tokens: s.input_tokens,
                output_tokens: s.output_tokens,
                duration_ms: s.duration_ms,
                cost: s.cost,
            })
        })
        .collect();

    Ok(outputs)
}

/// Persist the orchestrator's per-suggestion verdicts for a single review round.
///
/// Verdicts are sourced from the merge orchestrator's `submit_verdicts` tool
/// call and persisted on each round's merge completion. The store method is
/// idempotent — re-saving overwrites any prior verdicts for `(job_id, round_number)`.
#[tracing::instrument(skip(app, state, verdicts), fields(verdict_count = verdicts.len()))]
#[tauri::command]
pub async fn save_round_verdicts(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    job_id: String,
    round_number: u32,
    verdicts: Vec<RoundVerdict>,
) -> Result<(), IpcError> {
    validate_id(&job_id)?;
    info!(
        job_id = %job_id,
        round = round_number,
        count = verdicts.len(),
        "save_round_verdicts invoked"
    );

    state
        .hivemind_store
        .save_round_verdicts(&job_id, round_number as i64, &verdicts)
        .await
        .map_err(|e| format!("failed to save round verdicts: {}", e))?;

    let review_id = state
        .hivemind_store
        .get_job(&job_id)
        .await
        .ok()
        .flatten()
        .and_then(|j| j.review_id);

    let _ = app.emit(
        "hivemind-progress",
        HivemindProgressEvent {
            job_id: job_id.clone(),
            review_id,
            event_type: "verdicts_updated".to_string(),
            round: round_number,
            model_id: String::new(),
            message: "Round verdicts updated".to_string(),
            ..Default::default()
        },
    );

    Ok(())
}

/// Resolve round verdicts for either a single job or a logical multi-job run.
///
/// Returns `Ok(None)` when neither a job row nor any child rows match `job_id`
/// (the command layer maps this to `IpcError::not_found`). Verdicts are
/// returned with their stored `round_number` untouched — the engine writes
/// cumulative round numbers across child jobs (see the invariant comment on
/// `aggregate_review_state`), so no offset arithmetic happens here.
async fn list_round_verdicts_for_review(
    store: &crate::hivemind::store::HivemindStore,
    job_id: &str,
) -> Result<Option<Vec<RoundVerdict>>, String> {
    if store
        .get_job(job_id)
        .await
        .map_err(|e| format!("failed to fetch job: {}", e))?
        .is_some()
    {
        let verdicts = store
            .list_round_verdicts(job_id)
            .await
            .map_err(|e| format!("failed to list round verdicts: {}", e))?;
        return Ok(Some(verdicts));
    }

    let child_count = store
        .count_jobs_with_review_id(job_id)
        .await
        .map_err(|e| format!("failed to count child jobs: {}", e))?;
    if child_count == 0 {
        return Ok(None);
    }

    let jobs = store
        .list_jobs_by_review_id(job_id)
        .await
        .map_err(|e| format!("failed to fetch child jobs: {}", e))?;
    let job_ids: Vec<String> = jobs.iter().map(|j| j.id.clone()).collect();

    let mut verdicts = store
        .fetch_round_verdicts_for_jobs(&job_ids)
        .await
        .map_err(|e| format!("failed to list round verdicts: {}", e))?;
    verdicts.sort_by(|a, b| {
        a.round_number
            .cmp(&b.round_number)
            .then_with(|| a.reviewer_model.cmp(&b.reviewer_model))
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok(Some(verdicts))
}

/// Fetch all persisted orchestrator verdicts for a review job or logical run,
/// ordered by `(round_number, reviewer_model)`.
#[tracing::instrument(skip(state))]
#[tauri::command]
pub async fn list_round_verdicts(
    state: tauri::State<'_, AppState>,
    job_id: String,
) -> Result<Vec<RoundVerdict>, IpcError> {
    validate_id(&job_id).map_err(IpcError::validation)?;
    info!(job_id = %job_id, "list_round_verdicts invoked");

    match list_round_verdicts_for_review(&state.hivemind_store, &job_id).await {
        Ok(Some(verdicts)) => Ok(verdicts),
        Ok(None) => Err(IpcError::not_found("review", job_id.clone())),
        Err(e) => Err(IpcError::internal(e).with_id(job_id.clone())),
    }
}

/// Clear the process-wide LLM response cache and return the metrics snapshot
/// captured immediately before the wipe. Useful for forcing a fresh round of
/// model calls during local debugging — gated to debug builds only so it
/// can't be invoked from a release build.
#[cfg(debug_assertions)]
#[tauri::command]
pub async fn clear_response_cache(
    state: tauri::State<'_, AppState>,
) -> Result<crate::hivemind::cache::CacheMetricsSnapshot, IpcError> {
    let snapshot = state.response_cache.metrics();
    state.response_cache.clear().await;
    info!(
        hits = snapshot.hits,
        misses = snapshot.misses,
        inserts = snapshot.inserts,
        skipped_oversized = snapshot.skipped_oversized,
        entry_count = snapshot.entry_count,
        "response cache cleared via debug IPC",
    );
    Ok(snapshot)
}

/// Log an event to a review's JSONL log file.
///
/// Silently succeeds if no logger exists (debug mode off or unknown review_id).
#[tracing::instrument(skip(state, data), fields(review_id = %review_id))]
#[tauri::command]
pub async fn log_review_event(
    state: tauri::State<'_, AppState>,
    review_id: String,
    event_type: String,
    data: serde_json::Value,
) -> Result<(), IpcError> {
    validate_review_id(&review_id)?;
    // Bound the inbound JSON payload — the logger writes the whole blob to
    // a JSONL file; an unbounded frontend could fill the disk.
    check_payload_size(&data)?;
    let loggers = state.review_loggers.lock().await;
    if let Some(logger) = loggers.get(&review_id) {
        logger.log(&event_type, data).await;
    }
    Ok(())
}

/// Retrieve the full enriched prompt (plan + source context) from a review's
/// first round job. Used by the Replay feature to re-run a completed review
/// with a different Hivemind without re-gathering context.
#[tracing::instrument(skip(state))]
#[tauri::command]
pub async fn get_review_plan(
    state: tauri::State<'_, AppState>,
    review_id: String,
) -> Result<String, IpcError> {
    validate_review_id(&review_id).map_err(IpcError::validation)?;
    info!(review_id = %review_id, "get_review_plan invoked");

    // Path 1: logical review_id (hmr-*) → fetch child jobs
    let child_count = state
        .hivemind_store
        .count_jobs_with_review_id(&review_id)
        .await
        .map_err(|e| IpcError::internal(format!("failed to count child jobs: {}", e)))?;
    if child_count > 0 {
        let jobs = state
            .hivemind_store
            .list_jobs_by_review_id(&review_id)
            .await
            .map_err(|e| IpcError::internal(format!("failed to fetch child jobs: {}", e)))?;
        // list_jobs_by_review_id returns ORDER BY created_at ASC, id ASC
        // so the first element is always the round 1 job.
        return match jobs.into_iter().next() {
            Some(j) if j.plan.trim().is_empty() => Err(IpcError::validation(format!(
                "review '{}' has no stored plan data",
                review_id
            ))
            .with_id(review_id.clone())),
            Some(j) => Ok(j.plan),
            None => Err(IpcError::not_found("review", review_id.clone())),
        };
    }

    // Path 2: direct job UUID (or standalone job with no review_id)
    match state
        .hivemind_store
        .get_job(&review_id)
        .await
        .map_err(|e| IpcError::internal(format!("failed to fetch job: {}", e)))?
    {
        Some(job) if job.review_id.as_deref().filter(|s| !s.is_empty()).is_some() => {
            // This is a child job — redirect to the first sibling's plan
            let parent_review_id = job.review_id.as_deref().unwrap();
            let sibling_jobs = state
                .hivemind_store
                .list_jobs_by_review_id(parent_review_id)
                .await
                .map_err(|e| IpcError::internal(format!("failed to fetch sibling jobs: {}", e)))?;
            match sibling_jobs.into_iter().next() {
                Some(j) if j.plan.trim().is_empty() => Err(IpcError::validation(format!(
                    "review '{}' has no stored plan data",
                    parent_review_id
                ))
                .with_id(parent_review_id.to_string())),
                Some(j) => Ok(j.plan),
                None => Err(IpcError::not_found("review", parent_review_id.to_string())),
            }
        }
        Some(job) if job.plan.trim().is_empty() => Err(IpcError::validation(format!(
            "job '{}' has no stored plan data",
            review_id
        ))
        .with_id(review_id.clone())),
        Some(job) => Ok(job.plan),
        None => Err(IpcError::not_found("review", review_id.clone())),
    }
}

// ---------------------------------------------------------------------------
// Merge artifact read-only IPC (the merge lifecycle itself is engine-owned)
// ---------------------------------------------------------------------------

/// Read the on-disk merge output for `(job_id, round)`. Returns an empty
/// string if no row exists or the file is missing — the UI uses this to
/// preview partial text after an interruption.
#[tracing::instrument(skip(state))]
#[tauri::command]
pub async fn read_merge_output(
    state: tauri::State<'_, AppState>,
    job_id: String,
    round: i64,
) -> Result<String, IpcError> {
    validate_id(&job_id)?;
    let info = match state
        .hivemind_store
        .get_merge_run(&job_id, round)
        .await
        .map_err(|e| format!("failed to fetch merge run: {}", e))?
    {
        Some(v) => v,
        None => return Ok(String::new()),
    };

    match tokio::fs::read_to_string(&info.output_path).await {
        Ok(s) => Ok(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(IpcError::internal(format!(
            "failed to read merge output file: {}",
            e
        ))),
    }
}

// ---------------------------------------------------------------------------
// Orchestrator usage (context + merge session aggregation)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct OrchestratorUsage {
    pub model_id: String,
    pub provider: String,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub total_cost: f64,
    pub total_duration_ms: i64,
    pub context_session: Option<PhaseUsage>,
    pub merge_sessions: Vec<PhaseUsage>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PhaseUsage {
    pub round: Option<i64>,
    pub session_id: String,
    pub model_id: String,
    pub provider: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
}

/// Register the Pi context-gather session for a review so that
/// `get_orchestrator_usage` can later look up its token usage.
#[tauri::command]
pub async fn register_context_session(
    state: tauri::State<'_, AppState>,
    review_id: String,
    session_id: String,
    model_id: String,
    provider: String,
) -> Result<(), IpcError> {
    validate_review_id(&review_id).map_err(IpcError::validation)?;
    validate_session_id(&session_id).map_err(IpcError::validation)?;
    if model_id.is_empty() {
        return Err(IpcError::validation("model_id must not be empty"));
    }
    if provider.is_empty() {
        return Err(IpcError::validation("provider must not be empty"));
    }
    state
        .hivemind_store
        .upsert_review_context_session(&review_id, &session_id, &model_id, &provider)
        .await
        .map_err(|e| {
            IpcError::internal(format!("failed to register context session: {}", e))
                .with_id(review_id.clone())
        })
}

/// Aggregate orchestrator (context + merge) session usage for a review.
#[tauri::command]
pub async fn get_orchestrator_usage(
    state: tauri::State<'_, AppState>,
    review_id: String,
) -> Result<OrchestratorUsage, IpcError> {
    validate_id(&review_id)?;

    // 1. Fetch context session
    let ctx_session = state
        .hivemind_store
        .get_review_context_session(&review_id)
        .await
        .map_err(|e| format!("{}", e))?;

    // 2. Fetch merge runs via single JOIN
    let merge_runs = state
        .hivemind_store
        .list_merge_runs_by_review_id(&review_id)
        .await
        .map_err(|e| format!("{}", e))?;

    // 3. Determine block-level model_id/provider
    let model_id = ctx_session
        .as_ref()
        .map(|c| c.model_id.clone())
        .or_else(|| merge_runs.first().map(|m| m.model_id.clone()))
        .unwrap_or_default();
    let provider = ctx_session
        .as_ref()
        .map(|c| c.provider.clone())
        .or_else(|| merge_runs.first().map(|m| m.provider.clone()))
        .unwrap_or_default();

    // 4. Collect all unique session IDs
    let mut session_ids: Vec<String> = Vec::new();
    if let Some(ref ctx) = ctx_session {
        session_ids.push(ctx.session_id.clone());
    }
    for mr in &merge_runs {
        if !session_ids.contains(&mr.session_id) {
            session_ids.push(mr.session_id.clone());
        }
    }

    // 5. Aggregate usage in a single query
    let usage_map: HashMap<String, SessionUsageSummary> = state
        .usage_store
        .get_usage_for_sessions(&session_ids)
        .await
        .map_err(|e| format!("{}", e))?
        .into_iter()
        .map(|u| (u.session_id.clone(), u))
        .collect();

    // 6. Build per-phase breakdowns and accumulate totals
    let mut total_input: i64 = 0;
    let mut total_output: i64 = 0;
    let mut total_cost: f64 = 0.0;
    let mut total_duration: i64 = 0;

    fn build_phase(
        session_id: &str,
        round: Option<i64>,
        model_id: &str,
        provider: &str,
        usage_map: &HashMap<String, SessionUsageSummary>,
        total_input: &mut i64,
        total_output: &mut i64,
        total_cost: &mut f64,
        total_duration: &mut i64,
    ) -> PhaseUsage {
        let u = usage_map.get(session_id);
        let inp = u.map_or(0, |u| u.input_tokens);
        let out = u.map_or(0, |u| u.output_tokens);
        let cost = u.map_or(0.0, |u| u.cost);
        let dur = u.map_or(0, |u| u.duration_ms);
        *total_input += inp;
        *total_output += out;
        *total_cost += cost;
        *total_duration += dur;
        PhaseUsage {
            round,
            session_id: session_id.to_string(),
            model_id: model_id.to_string(),
            provider: provider.to_string(),
            input_tokens: inp,
            output_tokens: out,
        }
    }

    let context_session = ctx_session.as_ref().map(|ctx| {
        build_phase(
            &ctx.session_id,
            None,
            &ctx.model_id,
            &ctx.provider,
            &usage_map,
            &mut total_input,
            &mut total_output,
            &mut total_cost,
            &mut total_duration,
        )
    });

    let merge_sessions: Vec<PhaseUsage> = merge_runs
        .iter()
        .map(|mr| {
            build_phase(
                &mr.session_id,
                Some(mr.round_number),
                &mr.model_id,
                &mr.provider,
                &usage_map,
                &mut total_input,
                &mut total_output,
                &mut total_cost,
                &mut total_duration,
            )
        })
        .collect();

    // 7. Guard against NaN/Infinity in totals
    if total_cost.is_nan() || total_cost.is_infinite() {
        total_cost = 0.0;
    }

    Ok(OrchestratorUsage {
        model_id,
        provider,
        total_input_tokens: total_input,
        total_output_tokens: total_output,
        total_cost,
        total_duration_ms: total_duration,
        context_session,
        merge_sessions,
    })
}

// ---------------------------------------------------------------------------
// Resumable review snapshot IPC commands
// ---------------------------------------------------------------------------
//
// The pure phase classifier (`compute_review_phase`, `phase_to_str`, etc.)
// and the startup-emit helper (`derive_phase_for_emit`) live in
// `crate::hivemind::phase`. This module just consumes them.

/// Compose a full [`ResumableReviewSnapshot`] for the given job. Reads
/// `merge_output` from disk (best-effort) when the phase is between-rounds
/// or final.
async fn build_resumable_snapshot(
    state: &AppState,
    job: crate::hivemind::store::Job,
) -> Result<ResumableReviewSnapshot, String> {
    let store = &state.hivemind_store;

    let steps = store
        .get_job_steps(&job.id)
        .await
        .map_err(|e| format!("failed to fetch job steps: {}", e))?;
    let merge_runs = store
        .list_merge_runs_for_job(&job.id)
        .await
        .map_err(|e| format!("failed to list merge runs: {}", e))?;

    let decision = compute_review_phase(&job, &steps, &merge_runs);

    // Models — group by (model_id, provider, stance) across the steps of the
    // resume round; if none exist (e.g. context phase) leave the list empty.
    let target_round = decision.round;
    let mut seen = std::collections::HashSet::new();
    let mut models: Vec<ResumableModelSpec> = Vec::new();
    for step in &steps {
        if step.round_number != target_round {
            continue;
        }
        let key = (
            step.model_id.clone(),
            step.provider.clone(),
            step.stance.clone(),
        );
        if seen.insert(key) {
            models.push(ResumableModelSpec {
                model_id: step.model_id.clone(),
                provider: step.provider.clone(),
                stance: step.stance.clone(),
                thinking: None,
            });
        }
    }

    // Completed step outputs for the resume round.
    let completed_step_outputs: Vec<ResumableStepOutput> = steps
        .iter()
        .filter(|s| s.round_number == target_round && s.status == "completed")
        .filter_map(|s| {
            s.output.as_ref().map(|o| ResumableStepOutput {
                model_id: s.model_id.clone(),
                provider: s.provider.clone(),
                output: o.clone(),
            })
        })
        .collect();

    // Merge output for between-rounds / final phases: prefer the most recent
    // completed merge for the round just below `target_round` (between-rounds)
    // or the final round (final phase).
    let merge_output = match decision.phase {
        ReviewResumePhase::BetweenRounds => {
            let prior = target_round - 1;
            read_merge_output_if_present(&merge_runs, prior).await
        }
        ReviewResumePhase::Final => read_merge_output_if_present(&merge_runs, target_round).await,
        _ => None,
    };

    let review_id = job.review_id.clone().unwrap_or_else(|| job.id.clone());
    let task_id = job.task_id.clone().unwrap_or_default();

    Ok(ResumableReviewSnapshot {
        review_id,
        latest_job_id: job.id.clone(),
        task_id,
        phase: decision.phase,
        round: target_round.max(0) as u32,
        total_rounds: job.num_rounds.max(0) as u32,
        plan_text: job.plan.clone(),
        models,
        completed_step_outputs,
        merge_output,
        message: decision.message,
    })
}

/// Read the on-disk output for a `(merge_runs, round)` pair when it exists
/// and the row is `completed`. Errors collapse to `None`.
async fn read_merge_output_if_present(
    merge_runs: &[crate::hivemind::store::MergeRunInfo],
    round: i64,
) -> Option<String> {
    let info = merge_runs
        .iter()
        .find(|m| m.round_number == round && m.status == "completed")?;
    match tokio::fs::read_to_string(&info.output_path).await {
        Ok(s) => Some(s),
        Err(_) => None,
    }
}

// `phase_to_str` and `derive_phase_for_emit` live in
// `crate::hivemind::phase`. `derive_phase_for_emit` is re-exported from this
// module above so historical callers in `lib.rs` keep compiling.

/// Look up the most recent resumable review attached to `task_id`.
///
/// Returns `None` when no review is attached to the task, or the latest
/// review is already terminal (`completed`/`cancelled`) without an
/// interrupted-final state worth surfacing.
#[tracing::instrument(skip(state))]
#[tauri::command]
pub async fn get_resumable_review_for_task(
    state: tauri::State<'_, AppState>,
    task_id: String,
) -> Result<Option<ResumableReviewSnapshot>, IpcError> {
    validate_id(&task_id)?;
    info!(task_id = %task_id, "get_resumable_review_for_task invoked");

    let job = match state
        .hivemind_store
        .latest_job_for_task(&task_id)
        .await
        .map_err(|e| format!("failed to fetch latest job for task: {}", e))?
    {
        Some(j) => j,
        None => return Ok(None),
    };

    // Cancelled jobs are not resumable.
    if job.status == "cancelled" {
        return Ok(None);
    }

    // For `completed` jobs we only surface a snapshot when there is a final
    // merge output to expose (so the UI can "Apply" it). For other completed
    // states (no final merge) the review is fully consumed already.
    if job.status == "completed" {
        let merge_runs = state
            .hivemind_store
            .list_merge_runs_for_job(&job.id)
            .await
            .map_err(|e| format!("failed to list merge runs: {}", e))?;
        let has_final_merge = merge_runs
            .iter()
            .any(|m| m.round_number == job.num_rounds && m.status == "completed");
        if !has_final_merge {
            return Ok(None);
        }
        // Reuse the standard builder; it will route through the `Final` phase.
        let snap = build_resumable_snapshot(&state, job).await?;
        return Ok(Some(snap));
    }

    // Failed jobs without a path to recovery are also not resumable.
    if job.status == "failed" {
        return Ok(None);
    }

    let snap = build_resumable_snapshot(&state, job).await?;
    Ok(Some(snap))
}

/// Truncate a string to max_len characters, appending "..." if truncated.
fn truncate_preview(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        let mut end = max_len;
        while !s.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        format!("{}...", &s[..end])
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hivemind::store::HivemindStore;
    use tempfile::TempDir;

    // ------------------------------------------------------------------
    // validate_id tests — IPC-boundary defense against path traversal /
    // mishandled IDs that flow into `reviews_dir` paths or SQLite keys.
    // ------------------------------------------------------------------

    #[test]
    fn validate_id_accepts_typical_uuid() {
        assert!(validate_id("550e8400-e29b-41d4-a716-446655440000").is_ok());
    }

    #[test]
    fn validate_id_accepts_review_prefix_format() {
        // The codebase uses `hmr-<short>` ids for reviews and `hm-<short>` ids
        // for hiveminds — both must be accepted by the validator.
        assert!(validate_id("hmr-a1b2c3d4").is_ok());
        assert!(validate_id("hm-team-alpha").is_ok());
        assert!(validate_id("job_with_underscores").is_ok());
        assert!(validate_id("ABC123def").is_ok());
    }

    #[test]
    fn validate_id_rejects_empty() {
        assert!(validate_id("").is_err());
    }

    #[test]
    fn validate_id_rejects_path_traversal() {
        // Any ".." sequence is rejected outright, regardless of the rest of
        // the id, because joining it under `reviews_dir` could escape.
        assert!(validate_id("..").is_err());
        assert!(validate_id("../etc").is_err());
        assert!(validate_id("foo/../bar").is_err());
        assert!(validate_id("a..b").is_err());
    }

    #[test]
    fn validate_id_rejects_path_separators() {
        assert!(validate_id("foo/bar").is_err());
        assert!(validate_id("foo\\bar").is_err());
        assert!(validate_id("/abs").is_err());
        assert!(validate_id("\\abs").is_err());
    }

    #[test]
    fn validate_id_rejects_dot_prefix() {
        // `.hidden` files should not be addressable — we forbid leading dots
        // entirely so an id like ".env" can never become a path target.
        assert!(validate_id(".hidden").is_err());
        assert!(validate_id(".").is_err());
    }

    #[test]
    fn validate_id_rejects_null_byte() {
        assert!(validate_id("foo\0bar").is_err());
    }

    #[test]
    fn validate_id_rejects_oversize() {
        let too_long = "a".repeat(65);
        assert!(validate_id(&too_long).is_err());
        let max_ok = "a".repeat(64);
        assert!(validate_id(&max_ok).is_ok());
    }

    #[test]
    fn validate_id_rejects_disallowed_characters() {
        // Spaces, shell metacharacters, control characters, and any
        // non-ASCII-alphanumeric/underscore/hyphen char must be rejected.
        assert!(validate_id("foo bar").is_err());
        assert!(validate_id("foo;rm").is_err());
        assert!(validate_id("foo$bar").is_err());
        assert!(validate_id("foo\nbar").is_err());
        assert!(validate_id("café").is_err());
    }

    /// Create a fresh `HivemindStore` backed by a temp-dir SQLite file.
    /// Returns the `TempDir` guard alongside the store so the dir lives
    /// for the duration of the test.
    async fn fresh_store() -> (TempDir, HivemindStore) {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let db_path = tmp.path().join("hivemind.db");
        let store = HivemindStore::new(&db_path)
            .await
            .expect("open hivemind store");
        (tmp, store)
    }

    #[tokio::test]
    async fn count_hivemind_runs_counts_all_jobs_for_hivemind() {
        let (_tmp, store) = fresh_store().await;

        store
            .create_job(
                "job-round-1",
                "plan",
                "against",
                1,
                300,
                Some("hmr-review-1"),
                Some("hm-a"),
                None,
                None,
                None,
            )
            .await
            .unwrap();
        store
            .create_job(
                "job-round-2",
                "plan",
                "against",
                1,
                300,
                Some("hmr-review-1"),
                Some("hm-a"),
                None,
                None,
                None,
            )
            .await
            .unwrap();
        store
            .create_job(
                "job-legacy",
                "plan",
                "against",
                1,
                300,
                None,
                Some("hm-a"),
                None,
                None,
                None,
            )
            .await
            .unwrap();
        store
            .create_job(
                "job-other-hm",
                "plan",
                "against",
                1,
                300,
                Some("hmr-review-1"),
                Some("hm-b"),
                None,
                None,
                None,
            )
            .await
            .unwrap();

        assert_eq!(store.count_hivemind_runs("hm-a").await.unwrap(), 2);
        assert_eq!(store.count_hivemind_runs("hm-b").await.unwrap(), 1);
    }

    #[tokio::test]
    async fn batch_count_hivemind_runs_returns_grouped_counts() {
        let (_tmp, store) = fresh_store().await;

        store
            .create_job(
                "job-round-1",
                "plan",
                "against",
                1,
                300,
                Some("hmr-review-1"),
                Some("hm-a"),
                None,
                None,
                None,
            )
            .await
            .unwrap();
        store
            .create_job(
                "job-round-2",
                "plan",
                "against",
                1,
                300,
                Some("hmr-review-1"),
                Some("hm-a"),
                None,
                None,
                None,
            )
            .await
            .unwrap();
        store
            .create_job(
                "job-legacy",
                "plan",
                "against",
                1,
                300,
                None,
                Some("hm-a"),
                None,
                None,
                None,
            )
            .await
            .unwrap();
        store
            .create_job(
                "job-other-hm",
                "plan",
                "against",
                1,
                300,
                Some("hmr-review-2"),
                Some("hm-b"),
                None,
                None,
                None,
            )
            .await
            .unwrap();

        let ids = vec!["hm-a".to_string(), "hm-b".to_string(), "hm-c".to_string()];
        let counts = store.batch_count_hivemind_runs(&ids).await.unwrap();

        assert_eq!(counts.len(), 2);
        assert_eq!(counts.get("hm-a"), Some(&2));
        assert_eq!(counts.get("hm-b"), Some(&1));
        assert!(!counts.contains_key("hm-c"));
    }

    #[tokio::test]
    async fn get_review_state_pending_job_has_no_steps_and_is_running() {
        let (_tmp, store) = fresh_store().await;
        let job_id = "job-pending";
        store
            .create_job(
                job_id,
                "do a thing",
                "neutral",
                2,
                300,
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap();

        let snap = build_review_state_snapshot(&store, job_id)
            .await
            .expect("snapshot should succeed");

        assert_eq!(snap.job_id, job_id);
        assert_eq!(snap.status, "pending");
        assert!(
            snap.is_running,
            "pending status should map to is_running=true"
        );
        assert_eq!(snap.total_rounds, 2);
        assert_eq!(snap.current_round, 0);
        assert!(snap.steps.is_empty());
        assert!(snap.error.is_none());
        assert!(snap.final_output.is_none());
        assert_eq!(snap.total_cost, 0.0);
        assert_eq!(snap.total_input_tokens, 0);
        assert_eq!(snap.total_output_tokens, 0);
        assert!(snap.completed_at.is_none());
    }

    #[tokio::test]
    async fn get_review_state_completed_job_returns_full_step_outputs() {
        let (_tmp, store) = fresh_store().await;
        let job_id = "job-complete";
        store
            .create_job(
                job_id,
                "review please",
                "for",
                1,
                300,
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap();

        // Two steps: one completed with full output, one still pending.
        let step_a = "step-a";
        let step_b = "step-b";
        store
            .create_job_step(
                step_a,
                job_id,
                1,
                0,
                "claude-sonnet-4",
                "anthropic",
                "for",
                "test prompt",
            )
            .await
            .unwrap();
        store
            .create_job_step(
                step_b,
                job_id,
                1,
                1,
                "gpt-4o",
                "openai",
                "for",
                "test prompt",
            )
            .await
            .unwrap();

        // Long output to verify it is returned untruncated (preview path was 200 chars).
        let long_output = "x".repeat(5_000);
        store
            .complete_job_step(step_a, &long_output, 1234, 5678, 0.42, 9_001)
            .await
            .unwrap();

        store
            .complete_job(job_id, "FINAL OUTPUT", 0.42, 1234, 5678)
            .await
            .unwrap();

        let snap = build_review_state_snapshot(&store, job_id)
            .await
            .expect("snapshot should succeed");

        assert_eq!(snap.status, "completed");
        assert!(
            !snap.is_running,
            "completed status should map to is_running=false"
        );
        assert_eq!(snap.final_output.as_deref(), Some("FINAL OUTPUT"));
        assert_eq!(snap.total_cost, 0.42);
        assert_eq!(snap.total_input_tokens, 1234);
        assert_eq!(snap.total_output_tokens, 5678);
        assert!(snap.completed_at.is_some());
        assert_eq!(snap.steps.len(), 2);

        // Find each step by model_id (ordering from store is round, then id).
        let a = snap
            .steps
            .iter()
            .find(|s| s.model_id == "claude-sonnet-4")
            .expect("step a present");
        assert_eq!(a.status, "completed");
        assert_eq!(a.output.len(), 5_000, "output must be untruncated");
        assert_eq!(a.output, long_output);
        assert_eq!(a.input_tokens, Some(1234));
        assert_eq!(a.output_tokens, Some(5678));
        assert_eq!(a.duration_ms, Some(9_001));
        assert_eq!(a.cost, Some(0.42));
        assert_eq!(a.round_number, 1);
        assert_eq!(a.provider, "anthropic");

        let b = snap
            .steps
            .iter()
            .find(|s| s.model_id == "gpt-4o")
            .expect("step b present");
        // Step b never completed -> status is the default "pending" and output is empty.
        assert_eq!(b.status, "pending");
        assert_eq!(b.output, "", "missing output should map to empty string");
        assert!(b.input_tokens.is_none());
        assert!(b.output_tokens.is_none());
        assert!(b.duration_ms.is_none());
        assert!(b.cost.is_none());
    }

    #[tokio::test]
    async fn get_review_state_failed_job_carries_error_and_not_running() {
        let (_tmp, store) = fresh_store().await;
        let job_id = "job-failed";
        store
            .create_job(
                job_id, "plan", "against", 1, 300, None, None, None, None, None,
            )
            .await
            .unwrap();
        store
            .fail_job(job_id, "boom: provider unreachable")
            .await
            .unwrap();

        let snap = build_review_state_snapshot(&store, job_id)
            .await
            .expect("snapshot should succeed");

        assert_eq!(snap.status, "failed");
        assert!(!snap.is_running);
        assert_eq!(snap.error.as_deref(), Some("boom: provider unreachable"));
        assert!(snap.final_output.is_none());
        assert!(snap.completed_at.is_some());
    }

    #[tokio::test]
    async fn get_review_state_cancelled_job_is_not_running() {
        let (_tmp, store) = fresh_store().await;
        let job_id = "job-cancelled";
        store
            .create_job(
                job_id, "plan", "neutral", 1, 300, None, None, None, None, None,
            )
            .await
            .unwrap();
        // No public `cancel_job` API; cancellation is reflected via update_job_status.
        store.update_job_status(job_id, "cancelled").await.unwrap();

        let snap = build_review_state_snapshot(&store, job_id)
            .await
            .expect("snapshot should succeed");

        assert_eq!(snap.status, "cancelled");
        assert!(
            !snap.is_running,
            "cancelled status must derive is_running=false"
        );
    }

    #[tokio::test]
    async fn get_review_state_missing_job_returns_err() {
        let (_tmp, store) = fresh_store().await;
        let err = build_review_state_snapshot(&store, "does-not-exist")
            .await
            .expect_err("missing job should return Err");
        assert!(
            err.contains("does-not-exist"),
            "error should mention the missing job_id, got: {}",
            err
        );
    }

    #[tokio::test]
    async fn round_verdicts_round_trip_and_overwrite() {
        let (_tmp, store) = fresh_store().await;
        let job_id = "job-verdicts";
        store
            .create_job(
                job_id, "plan", "neutral", 2, 300, None, None, None, None, None,
            )
            .await
            .unwrap();

        // First save: 2 verdicts in round 1.
        let v1 = RoundVerdict {
            id: "v1".to_string(),
            job_id: job_id.to_string(),
            round_number: 1,
            reviewer_model: "anthropic/claude-sonnet-4".to_string(),
            suggestion: "Add SELECT FOR UPDATE".to_string(),
            verdict: "accepted".to_string(),
            severity: Some(4),
            reason: Some("Real race".to_string()),
            created_at: "2026-05-09T12:00:00Z".to_string(),
            best_find: true,
            co_reviewers: Some(vec!["openai/gpt-4o".to_string()]),
        };
        let v2 = RoundVerdict {
            id: "v2".to_string(),
            job_id: job_id.to_string(),
            round_number: 1,
            reviewer_model: "openai/gpt-4o".to_string(),
            suggestion: "Add docstring".to_string(),
            verdict: "rejected".to_string(),
            severity: Some(1),
            reason: None,
            created_at: "2026-05-09T12:00:00Z".to_string(),
            best_find: false,
            co_reviewers: None,
        };
        store
            .save_round_verdicts(job_id, 1, &[v1.clone(), v2.clone()])
            .await
            .unwrap();

        // Save a single verdict for round 2.
        let v3 = RoundVerdict {
            id: "v3".to_string(),
            job_id: job_id.to_string(),
            round_number: 2,
            reviewer_model: "anthropic/claude-sonnet-4".to_string(),
            suggestion: "Use atomic CAS".to_string(),
            verdict: "modified".to_string(),
            severity: Some(3),
            reason: Some("Partially correct".to_string()),
            created_at: "2026-05-09T12:05:00Z".to_string(),
            best_find: false,
            co_reviewers: None,
        };
        store
            .save_round_verdicts(job_id, 2, &[v3.clone()])
            .await
            .unwrap();

        let listed = store.list_round_verdicts(job_id).await.unwrap();
        assert_eq!(listed.len(), 3);
        // Ordered by round, then reviewer_model.
        assert_eq!(listed[0].id, "v1");
        assert_eq!(listed[1].id, "v2");
        assert_eq!(listed[2].id, "v3");
        assert_eq!(listed[0].verdict, "accepted");
        assert_eq!(listed[1].verdict, "rejected");
        assert_eq!(listed[2].verdict, "modified");
        // best_find + co_reviewers round-trip through SQLite.
        assert!(listed[0].best_find);
        assert_eq!(
            listed[0].co_reviewers.as_deref(),
            Some(["openai/gpt-4o".to_string()].as_slice())
        );
        assert!(!listed[1].best_find);
        assert!(listed[1].co_reviewers.is_none());
        assert!(!listed[2].best_find);

        // Overwrite round 1 with a different single verdict.
        let v_replace = RoundVerdict {
            id: "v-replace".to_string(),
            job_id: job_id.to_string(),
            round_number: 1,
            reviewer_model: "openai/gpt-4o".to_string(),
            suggestion: "Different suggestion".to_string(),
            verdict: "accepted".to_string(),
            severity: None,
            reason: None,
            created_at: "2026-05-09T12:10:00Z".to_string(),
            best_find: false,
            co_reviewers: None,
        };
        store
            .save_round_verdicts(job_id, 1, &[v_replace.clone()])
            .await
            .unwrap();

        let after = store.list_round_verdicts(job_id).await.unwrap();
        assert_eq!(after.len(), 2, "round 1 must have been replaced");
        assert!(
            after.iter().any(|v| v.id == "v-replace"),
            "replacement verdict should be present"
        );
        assert!(
            after.iter().any(|v| v.id == "v3"),
            "round 2 verdict should be untouched"
        );
        assert!(
            !after.iter().any(|v| v.id == "v1" || v.id == "v2"),
            "round 1's old verdicts must be gone"
        );
    }

    #[tokio::test]
    async fn round_verdicts_cascade_on_job_delete() {
        let (_tmp, store) = fresh_store().await;
        let job_id = "job-cascade";
        store
            .create_job(
                job_id, "plan", "neutral", 1, 300, None, None, None, None, None,
            )
            .await
            .unwrap();
        let v = RoundVerdict {
            id: "v-c".to_string(),
            job_id: job_id.to_string(),
            round_number: 1,
            reviewer_model: "anthropic/claude-sonnet-4".to_string(),
            suggestion: "x".to_string(),
            verdict: "accepted".to_string(),
            severity: None,
            reason: None,
            created_at: "2026-05-09T00:00:00Z".to_string(),
            best_find: false,
            co_reviewers: None,
        };
        store.save_round_verdicts(job_id, 1, &[v]).await.unwrap();

        // Cascade delete via raw SQL.
        sqlx::query("DELETE FROM jobs WHERE id = ?1")
            .bind(job_id)
            .execute(store.pool())
            .await
            .unwrap();

        let after = store.list_round_verdicts(job_id).await.unwrap();
        assert!(after.is_empty(), "verdicts must cascade-delete with job");
    }

    #[tokio::test]
    async fn get_review_state_running_status_maps_to_is_running_true() {
        let (_tmp, store) = fresh_store().await;
        let job_id = "job-running";
        store
            .create_job(
                job_id, "plan", "neutral", 1, 300, None, None, None, None, None,
            )
            .await
            .unwrap();
        store.update_job_status(job_id, "running").await.unwrap();

        let snap = build_review_state_snapshot(&store, job_id)
            .await
            .expect("snapshot should succeed");
        assert_eq!(snap.status, "running");
        assert!(snap.is_running);
    }

    // ------------------------------------------------------------------
    // Multi-round / multi-job review tests (the Round 2+ bug fix)
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn multi_job_review_parent_review_id_returns_all_rounds() {
        let (_tmp, store) = fresh_store().await;
        let parent_id = "hmr-multi";
        let child1 = "c1";
        let child2 = "c2";

        // Two child jobs sharing a parent review_id (round 1 and round 2)
        store
            .create_job(
                child1,
                "plan",
                "against",
                1,
                300,
                Some(parent_id),
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap();
        store
            .create_job(
                child2,
                "plan",
                "against",
                1,
                300,
                Some(parent_id),
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap();

        // Round 1 steps (child1)
        store
            .create_job_step(
                "s1a",
                child1,
                1,
                0,
                "claude-sonnet-4",
                "anthropic",
                "against",
                "prompt",
            )
            .await
            .unwrap();
        store
            .create_job_step("s1b", child1, 1, 1, "gpt-4o", "openai", "against", "prompt")
            .await
            .unwrap();
        store
            .complete_job_step("s1a", "r1-claude-output", 100, 200, 0.01, 5000)
            .await
            .unwrap();
        store
            .complete_job_step("s1b", "r1-gpt-output", 150, 250, 0.02, 6000)
            .await
            .unwrap();
        store
            .complete_job(child1, "round-1-complete", 0.03, 250, 450)
            .await
            .unwrap();

        // Round 2 steps (child2).
        //
        // NOTE: child2's steps are written with the CUMULATIVE round number
        // (= 2), not per-child indexing (= 1). This mirrors what
        // `hivemind/engine.rs` ~line 705 actually writes via
        // `round_num = (round_idx + 1) + round_offset` — every step row
        // and verdict row across all child jobs sharing a `review_id`
        // carries a cumulative round number. The aggregator MUST NOT add
        // a per-child offset on the read path; previously it did, and that
        // double-offset bug is why round 2 rendered as "No model results
        // yet" on the All Reviews page. Do NOT change these back to `1`
        // "for backwards compatibility" — there is no legacy on-disk data
        // that uses per-child indexing.
        store
            .create_job_step(
                "s2a",
                child2,
                2,
                0,
                "glm-4.6",
                "openrouter",
                "against",
                "prompt",
            )
            .await
            .unwrap();
        store
            .create_job_step(
                "s2b",
                child2,
                2,
                1,
                "deepseek-v3.2",
                "openrouter",
                "against",
                "prompt",
            )
            .await
            .unwrap();
        store
            .complete_job_step("s2a", "r2-glm-output", 200, 100, 0.005, 2000)
            .await
            .unwrap();
        store
            .complete_job_step("s2b", "r2-deepseek-output", 180, 90, 0.004, 1500)
            .await
            .unwrap();
        store
            .complete_job(child2, "round-2-complete", 0.009, 380, 190)
            .await
            .unwrap();

        // Call build_review_state_snapshot with the parent review_id (primary path)
        let snap = build_review_state_snapshot(&store, parent_id)
            .await
            .expect("snapshot should succeed for parent review_id");

        assert_eq!(snap.job_id, parent_id);
        assert_eq!(
            snap.total_rounds, 2,
            "parent review_id must aggregate both child jobs into 2 rounds"
        );
        assert_eq!(snap.steps.len(), 4, "all 4 steps across both rounds");

        // Check round_number distribution
        let mut r1_steps = snap
            .steps
            .iter()
            .filter(|s| s.round_number == 1)
            .collect::<Vec<_>>();
        let mut r2_steps = snap
            .steps
            .iter()
            .filter(|s| s.round_number == 2)
            .collect::<Vec<_>>();
        r1_steps.sort_by(|a, b| a.model_id.cmp(&b.model_id));
        r2_steps.sort_by(|a, b| a.model_id.cmp(&b.model_id));

        assert_eq!(r1_steps.len(), 2, "round 1 should have 2 steps");
        assert_eq!(r2_steps.len(), 2, "round 2 should have 2 steps");

        assert_eq!(r1_steps[0].model_id, "claude-sonnet-4");
        assert_eq!(r1_steps[0].round_number, 1);
        assert_eq!(r1_steps[0].output, "r1-claude-output");
        assert_eq!(r2_steps[0].model_id, "deepseek-v3.2");
        assert_eq!(r2_steps[0].round_number, 2);
        assert_eq!(r2_steps[0].output, "r2-deepseek-output");

        assert!(!snap.is_running, "all jobs completed");
        assert_eq!(snap.status, "completed");
        // Total cost = 0.03 + 0.009
        assert!((snap.total_cost - 0.039).abs() < 0.001);
    }

    #[tokio::test]
    async fn multi_job_review_child_job_id_fallback_returns_all_rounds() {
        let (_tmp, store) = fresh_store().await;
        let parent_id = "hmr-child-fallback";
        let child1 = "cf1";
        let child2 = "cf2";

        // Two child jobs sharing a parent review_id
        store
            .create_job(
                child1,
                "plan",
                "neutral",
                1,
                300,
                Some(parent_id),
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap();
        store
            .create_job(
                child2,
                "plan",
                "neutral",
                1,
                300,
                Some(parent_id),
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap();

        // Round 1 steps
        store
            .create_job_step("s1x", child1, 1, 0, "model-a", "prov-a", "neutral", "p1")
            .await
            .unwrap();
        store
            .complete_job_step("s1x", "r1-out", 50, 100, 0.005, 3000)
            .await
            .unwrap();
        store
            .complete_job(child1, "r1-done", 0.005, 50, 100)
            .await
            .unwrap();

        // Round 2 steps.
        //
        // NOTE: child2's step is written with the CUMULATIVE round number
        // (= 2), not per-child indexing (= 1). See the matching note in
        // `multi_job_review_parent_review_id_returns_all_rounds` and the
        // invariant comment on `aggregate_review_state`.
        store
            .create_job_step("s2x", child2, 2, 0, "model-b", "prov-b", "neutral", "p2")
            .await
            .unwrap();
        store
            .complete_job_step("s2x", "r2-out", 60, 80, 0.003, 2000)
            .await
            .unwrap();
        store
            .complete_job(child2, "r2-done", 0.003, 60, 80)
            .await
            .unwrap();

        // Call build_review_state_snapshot with a CHILD job ID (fallback path)
        let snap = build_review_state_snapshot(&store, child1)
            .await
            .expect("snapshot should succeed via child job fallback");

        assert_eq!(
            snap.total_rounds, 2,
            "child job ID fallback must discover siblings and return 2 rounds"
        );
        assert_eq!(snap.steps.len(), 2, "both steps across rounds");

        let r1 = snap.steps.iter().find(|s| s.round_number == 1).unwrap();
        let r2 = snap.steps.iter().find(|s| s.round_number == 2).unwrap();

        assert_eq!(r1.model_id, "model-a");
        assert_eq!(r1.output, "r1-out");
        assert_eq!(r2.model_id, "model-b");
        assert_eq!(r2.output, "r2-out");

        // The job_id in the snapshot should be the parent review_id, not the child
        assert_eq!(snap.job_id, parent_id);
    }

    #[tokio::test]
    async fn multi_job_review_single_child_no_siblings_returns_single_job() {
        // Edge case: a child job with review_id but no sibling jobs yet.
        // Should fall through to the single-job path.
        let (_tmp, store) = fresh_store().await;
        let parent_id = "hmr-orphan";
        let child = "orphan";

        store
            .create_job(
                child,
                "plan",
                "against",
                1,
                300,
                Some(parent_id),
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap();
        store
            .create_job_step("os1", child, 1, 0, "model-x", "prov-x", "against", "prompt")
            .await
            .unwrap();
        store
            .complete_job_step("os1", "orphan-output", 10, 20, 0.001, 1000)
            .await
            .unwrap();
        store
            .complete_job(child, "done", 0.001, 10, 20)
            .await
            .unwrap();

        // Call with the child job ID — fallback finds parent, but parent has no
        // other children, so it still returns the single job's data
        let snap = build_review_state_snapshot(&store, child)
            .await
            .expect("snapshot should succeed");

        assert_eq!(
            snap.total_rounds, 1,
            "single child with no siblings should return 1 round"
        );
        assert_eq!(snap.steps.len(), 1);
        assert_eq!(snap.steps[0].output, "orphan-output");
        assert_eq!(snap.steps[0].round_number, 1);
    }

    // Resumable review phase classifier tests moved to
    // `crate::hivemind::phase::tests`.

    #[tokio::test]
    async fn legacy_job_without_review_id_still_works() {
        // Legacy jobs without review_id should continue to work unchanged.
        let (_tmp, store) = fresh_store().await;
        let job_id = "legacy-no-review-id";

        store
            .create_job(job_id, "plan", "for", 1, 300, None, None, None, None, None)
            .await
            .unwrap();
        store
            .create_job_step("ls1", job_id, 1, 0, "model", "prov", "for", "p")
            .await
            .unwrap();
        store
            .complete_job_step("ls1", "legacy-output", 99, 88, 0.007, 4000)
            .await
            .unwrap();
        store
            .complete_job(job_id, "legacy-done", 0.007, 99, 88)
            .await
            .unwrap();

        let snap = build_review_state_snapshot(&store, job_id)
            .await
            .expect("snapshot should succeed for legacy job");

        assert_eq!(snap.job_id, job_id);
        assert_eq!(snap.total_rounds, 1);
        assert_eq!(snap.steps.len(), 1);
        assert_eq!(snap.steps[0].output, "legacy-output");
        assert_eq!(snap.steps[0].round_number, 1);
        assert!(!snap.is_running);
    }

    // ------------------------------------------------------------------
    // Regression tests for the cumulative-round-number invariant.
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn list_round_verdicts_multi_job_returns_cumulative_round_numbers() {
        // Reproduces the Round 2 "No model results yet" bug at the verdict
        // level: with two child jobs sharing a `review_id`, the engine writes
        // child1's verdicts with round_number=1 and child2's with
        // round_number=2. The aggregator must NOT shift child2's verdicts to
        // round_number=3.
        let (_tmp, store) = fresh_store().await;
        let parent = "hmr-verdicts-multi";
        let c1 = "vc1";
        let c2 = "vc2";

        store
            .create_job(
                c1,
                "plan",
                "against",
                1,
                300,
                Some(parent),
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap();
        store
            .create_job(
                c2,
                "plan",
                "against",
                1,
                300,
                Some(parent),
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap();

        let v1 = RoundVerdict {
            id: "vrd-1".to_string(),
            job_id: c1.to_string(),
            round_number: 1,
            reviewer_model: "anthropic/claude-sonnet-4".to_string(),
            suggestion: "Use SELECT FOR UPDATE".to_string(),
            verdict: "accepted".to_string(),
            severity: Some(4),
            reason: None,
            created_at: "2026-05-09T12:00:00Z".to_string(),
            best_find: false,
            co_reviewers: None,
        };
        let v2 = RoundVerdict {
            id: "vrd-2".to_string(),
            job_id: c2.to_string(),
            round_number: 2,
            reviewer_model: "openai/gpt-4o".to_string(),
            suggestion: "Add WAL mode".to_string(),
            verdict: "modified".to_string(),
            severity: Some(3),
            reason: None,
            created_at: "2026-05-09T12:05:00Z".to_string(),
            best_find: false,
            co_reviewers: None,
        };
        store
            .save_round_verdicts(c1, 1, &[v1.clone()])
            .await
            .unwrap();
        store
            .save_round_verdicts(c2, 2, &[v2.clone()])
            .await
            .unwrap();

        let listed = list_round_verdicts_for_review(&store, parent)
            .await
            .expect("multi-job verdict listing should succeed")
            .expect("multi-job verdict listing should find a review");

        assert_eq!(listed.len(), 2, "both children's verdicts must come back");

        let r1: Vec<_> = listed.iter().filter(|v| v.round_number == 1).collect();
        let r2: Vec<_> = listed.iter().filter(|v| v.round_number == 2).collect();
        let r3: Vec<_> = listed.iter().filter(|v| v.round_number == 3).collect();

        assert_eq!(r1.len(), 1, "round 1 should still have exactly 1 verdict");
        assert_eq!(
            r2.len(),
            1,
            "round 2 verdicts must remain at round_number=2 (not pushed to 3)"
        );
        assert!(
            r3.is_empty(),
            "no verdict may surface at round_number=3 (double-offset bug)"
        );
        assert_eq!(r2[0].id, "vrd-2");
        assert_eq!(r2[0].reviewer_model, "openai/gpt-4o");
    }

    #[tokio::test]
    async fn build_review_state_snapshot_multi_job_preserves_cumulative_step_round_numbers() {
        // The exact bug from the All Reviews page: child2's step row is stored
        // with the cumulative round_number=2; the snapshot must surface it at
        // round_number=2 (not 3) so the frontend's
        // `liveState.steps.filter(s => s.round_number === 2)` matches.
        let (_tmp, store) = fresh_store().await;
        let parent = "hmr-snap-cum";
        let c1 = "snc1";
        let c2 = "snc2";

        store
            .create_job(
                c1,
                "plan",
                "against",
                1,
                300,
                Some(parent),
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap();
        store
            .create_job(
                c2,
                "plan",
                "against",
                1,
                300,
                Some(parent),
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap();

        store
            .create_job_step("snst-1", c1, 1, 0, "m1", "p1", "against", "prompt")
            .await
            .unwrap();
        store
            .complete_job_step("snst-1", "r1-output", 10, 20, 0.001, 1000)
            .await
            .unwrap();
        store
            .complete_job(c1, "r1-done", 0.001, 10, 20)
            .await
            .unwrap();

        // Child 2's step row carries the CUMULATIVE round_number = 2.
        store
            .create_job_step("snst-2", c2, 2, 0, "m2", "p2", "against", "prompt")
            .await
            .unwrap();
        store
            .complete_job_step("snst-2", "r2-output", 30, 40, 0.002, 2000)
            .await
            .unwrap();
        store
            .complete_job(c2, "r2-done", 0.002, 30, 40)
            .await
            .unwrap();

        let snap = build_review_state_snapshot(&store, parent)
            .await
            .expect("snapshot should succeed for parent review_id");

        assert_eq!(snap.total_rounds, 2);
        let r2: Vec<_> = snap.steps.iter().filter(|s| s.round_number == 2).collect();
        assert_eq!(
            r2.len(),
            1,
            "exactly one step at round 2 — not shifted to round 3"
        );
        assert_eq!(r2[0].output, "r2-output");
        assert!(
            !snap.steps.iter().any(|s| s.round_number == 3),
            "no step may surface at round 3 (double-offset bug)"
        );
        assert!(!snap.is_running);
        // Not-running path returns total_rounds for current_round.
        assert_eq!(snap.current_round, 2);
    }

    #[tokio::test]
    async fn build_review_state_snapshot_running_review_reports_correct_current_round() {
        // Running case: child1 is complete (cumulative round 1) and child2 is
        // mid-flight with status="round_2" and one step row at cumulative
        // round_number=2. The aggregator must report current_round=2 by
        // taking the max of the per-job status signal and the per-job step
        // signal across ALL jobs (terminal and non-terminal).
        let (_tmp, store) = fresh_store().await;
        let parent = "hmr-running-cur";
        let c1 = "rc1";
        let c2 = "rc2";

        store
            .create_job(
                c1,
                "plan",
                "against",
                1,
                300,
                Some(parent),
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap();
        store
            .create_job(
                c2,
                "plan",
                "against",
                1,
                300,
                Some(parent),
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap();

        // Child 1 completed with a step at cumulative round 1.
        store
            .create_job_step("rcs1", c1, 1, 0, "m1", "p1", "against", "prompt")
            .await
            .unwrap();
        store
            .complete_job_step("rcs1", "r1", 10, 20, 0.001, 1000)
            .await
            .unwrap();
        store
            .complete_job(c1, "r1-done", 0.001, 10, 20)
            .await
            .unwrap();

        // Child 2 in round_2 with a step at cumulative round 2.
        store.update_job_status(c2, "round_2").await.unwrap();
        store
            .create_job_step("rcs2", c2, 2, 0, "m2", "p2", "against", "prompt")
            .await
            .unwrap();

        let snap = build_review_state_snapshot(&store, parent)
            .await
            .expect("snapshot should succeed for running multi-job review");

        assert!(snap.is_running, "child2 in round_2 means the run is live");
        assert_eq!(snap.total_rounds, 2);
        assert_eq!(snap.current_round, 2);
    }

    #[tokio::test]
    async fn build_review_state_snapshot_running_review_pre_dispatch_does_not_overshoot() {
        // Transient race window: child2 has been created and bumped to
        // "running", but neither its `round_N` status update nor its step
        // rows have landed yet. `current_round` must reflect the strongest
        // signal we actually have (child1 completed round 1), NOT jump
        // optimistically to total_rounds. This pins the pre-dispatch
        // off-by-one behavior so future refactors don't regress it the
        // wrong direction.
        let (_tmp, store) = fresh_store().await;
        let parent = "hmr-running-trans";
        let c1 = "tc1";
        let c2 = "tc2";

        store
            .create_job(
                c1,
                "plan",
                "against",
                1,
                300,
                Some(parent),
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap();
        store
            .create_job(
                c2,
                "plan",
                "against",
                1,
                300,
                Some(parent),
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap();

        store
            .create_job_step("trc1", c1, 1, 0, "m1", "p1", "against", "prompt")
            .await
            .unwrap();
        store
            .complete_job_step("trc1", "r1", 10, 20, 0.001, 1000)
            .await
            .unwrap();
        store
            .complete_job(c1, "r1-done", 0.001, 10, 20)
            .await
            .unwrap();

        // Child 2 in plain "running" with no steps yet (pre-dispatch).
        store.update_job_status(c2, "running").await.unwrap();

        let snap = build_review_state_snapshot(&store, parent)
            .await
            .expect("snapshot should succeed during pre-dispatch transient");

        assert!(snap.is_running, "child2 in 'running' state");
        assert_eq!(snap.total_rounds, 2);
        assert_eq!(
            snap.current_round, 1,
            "must fall back to max observed signal (child1=1), not jump to total_rounds"
        );
    }

    // ------------------------------------------------------------------
    // orchestrator_from_override — frontend-resolved inherit_orchestrator
    // ------------------------------------------------------------------

    #[test]
    fn orchestrator_override_parses_provider_qualified_form() {
        let cfg = orchestrator_from_override("claude-sub/claude-opus-4-7");
        assert_eq!(cfg.provider, "claude-sub");
        assert_eq!(cfg.model, "claude-opus-4-7");
    }

    #[test]
    fn orchestrator_override_infers_provider_for_bare_claude_id() {
        let cfg = orchestrator_from_override("claude-opus-4-7");
        assert_eq!(cfg.provider, "anthropic");
        assert_eq!(cfg.model, "claude-opus-4-7");
    }

    #[test]
    fn orchestrator_override_infers_provider_for_bare_gpt_id() {
        let cfg = orchestrator_from_override("gpt-5.5");
        assert_eq!(cfg.provider, "openai");
        assert_eq!(cfg.model, "gpt-5.5");
    }

    #[test]
    fn orchestrator_override_defaults_to_openrouter_for_unknown_bare_id() {
        let cfg = orchestrator_from_override("mimo-v2.5-pro-precision");
        assert_eq!(cfg.provider, "openrouter");
        assert_eq!(cfg.model, "mimo-v2.5-pro-precision");
    }

    #[test]
    fn orchestrator_override_keeps_provider_for_unknown_qualified_id() {
        // Provider-qualified ids are forwarded verbatim — no heuristic.
        // This is how `neuralwatt/moonshotai/Kimi-K2.6` and other
        // multi-segment provider/model forms reach the engine intact.
        let cfg = orchestrator_from_override("crof/qwen3.6-27b");
        assert_eq!(cfg.provider, "crof");
        assert_eq!(cfg.model, "qwen3.6-27b");
    }

    #[test]
    fn orchestrator_override_preserves_remainder_on_multi_segment_model() {
        // `splitn(2, '/')` keeps the second slash in the model field —
        // some providers (notably neuralwatt) namespace their models with
        // an upstream-vendor slash like `moonshotai/Kimi-K2.6`. We must
        // not drop the suffix.
        let cfg = orchestrator_from_override("neuralwatt/moonshotai/Kimi-K2.6");
        assert_eq!(cfg.provider, "neuralwatt");
        assert_eq!(cfg.model, "moonshotai/Kimi-K2.6");
    }
}
