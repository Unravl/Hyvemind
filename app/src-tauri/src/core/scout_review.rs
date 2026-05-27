//! Thin shim that runs a Scout-plan or Queen-master-plan Hivemind review
//! through the unified [`crate::hivemind::engine::ReviewEngine::run`] entry
//! point. All orchestration (context gather, parallel rounds, per-round
//! orchestrator merge) lives in the engine; this file only wires up the
//! swarm-specific attribution + activity-stream emissions.

use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use tauri::Emitter;
use tokio::sync::{broadcast, RwLock};
use tokio_util::sync::CancellationToken;

use crate::domain::swarm::{Feature, ModelSettings};
use crate::hivemind::cache::ResponseCache;
use crate::hivemind::engine::{
    parse_rounds_config, ContextSpec, EngineDeps, HivemindRunConfig, OrchestratorCfg,
    ReviewAttribution, ReviewEngine, Stance,
};
use crate::hivemind::events::HivemindProgressEvent;
use crate::hivemind::store::HivemindStore;
use crate::pi::manager::PiManager;
use crate::providers::ProviderRegistry;
use crate::state::progress::{ProgressEvent, ProgressEventType};
use crate::state::usage_store::UsageStore;
use crate::tunables;

// Re-export engine types so existing call sites that imported them from
// this module keep compiling. The canonical definitions live in
// `hivemind::engine`.
pub use crate::hivemind::engine::RoundCfg;

#[derive(Clone)]
pub struct ScoutReviewContext {
    pub hivemind_store: Arc<HivemindStore>,
    pub provider_registry: Arc<RwLock<ProviderRegistry>>,
    pub usage_store: Arc<UsageStore>,
    pub app_handle: tauri::AppHandle,
    pub pi_manager: Arc<PiManager>,
    pub merge_capture_registry: Arc<
        std::sync::RwLock<
            std::collections::HashMap<String, Arc<crate::hivemind::merge_capture::MergeCapture>>,
        >,
    >,
    pub reviews_dir: std::path::PathBuf,
    /// Used by the model-call failure path to synthesize a visible Nurse
    /// intervention on circuit-breaker trips during scout-plan review.
    /// v2 push-mode engine — `report_synthesized` is used for the Hivemind
    /// pseudo session IDs that don't have a registered Pi session.
    pub nurse_engine: Option<Arc<crate::nurse::engine::NurseEngine>>,
    /// Handle to the app config, used to snapshot `custom_prompts` when
    /// building `EngineDeps`. Read-locked once per scout-review entry.
    pub config: Arc<RwLock<crate::state::config::Config>>,
    /// Shared LLM response cache singleton (pulled from `AppState`). Each
    /// scout/queen review reuses this Arc so hits accumulate across all
    /// reviews instead of being thrown away with per-call caches.
    pub response_cache: Arc<ResponseCache>,
}

pub struct ScoutReviewOutcome {
    pub refined_plan: String,
    pub job_id: String,
}

#[allow(clippy::too_many_arguments)]
pub async fn run_scout_hivemind_review(
    review_ctx: &ScoutReviewContext,
    _pi_manager: &Arc<PiManager>,
    swarm_id: &str,
    feature_id: &str,
    feature: &Feature,
    scout_plan: &str,
    model_settings: &ModelSettings,
    hivemind_id: &str,
    working_dir: &Path,
    cancel_token: &CancellationToken,
    event_tx: &broadcast::Sender<ProgressEvent>,
    activity_tx: Option<&crate::core::queen::ActivityTx>,
) -> Result<ScoutReviewOutcome> {
    let hivemind = review_ctx
        .hivemind_store
        .get_hivemind(hivemind_id)
        .await
        .with_context(|| format!("failed to load hivemind '{}'", hivemind_id))?
        .ok_or_else(|| anyhow!("hivemind '{}' not found", hivemind_id))?;

    let rounds = parse_rounds_config(&hivemind.rounds_config)
        .with_context(|| format!("failed to parse rounds_config for '{}'", hivemind_id))?;
    if rounds.is_empty() || rounds[0].models.is_empty() {
        return Err(anyhow!(
            "hivemind '{}' has no rounds or models configured",
            hivemind_id
        ));
    }

    let orchestrator = resolve_orchestrator(&hivemind, &rounds, &model_settings.primary_model);
    let context = ContextSpec::PiGather {
        role: format!("hivemind-context-{}", feature_id),
        model: model_settings.primary_model.clone(),
        working_dir: working_dir.to_path_buf(),
        system_prompt: CONTEXT_SYSTEM_PROMPT.to_string(),
        prompt: build_context_prompt(feature, scout_plan),
    };

    emit_review_progress(
        event_tx,
        swarm_id,
        feature_id,
        ProgressEventType::HivemindReviewStarted,
        &format!(
            "Hivemind '{}' reviewing Scout plan ({} rounds × {} models)",
            hivemind.name,
            rounds.len(),
            rounds[0].models.len()
        ),
        Some(serde_json::json!({
            "hivemind_id": hivemind_id,
        })),
    );

    let config = HivemindRunConfig {
        hivemind_id: hivemind_id.to_string(),
        rounds,
        orchestrator,
        stance: Stance::Against,
        concurrency_cap: tunables::hivemind_concurrency_cap(),
        context,
        initial_plan: scout_plan.to_string(),
        attribution: ReviewAttribution {
            review_id: None,
            task_id: None,
            swarm_id: Some(swarm_id.to_string()),
            feature_id: Some(feature_id.to_string()),
            project_path: Some(working_dir.to_string_lossy().to_string()),
            source_label: format!("Scout: {}", feature_id),
        },
        existing_job_id: None,
        round_offset: 0,
    };

    // Shared cache singleton from AppState — see ScoutReviewContext docs.
    let engine = ReviewEngine::new(Arc::clone(&review_ctx.response_cache));
    let custom_prompts = review_ctx.config.read().await.custom_prompts.clone();
    let deps = EngineDeps {
        pi_manager: review_ctx.pi_manager.clone(),
        store: review_ctx.hivemind_store.clone(),
        provider_registry: review_ctx.provider_registry.clone(),
        usage_store: review_ctx.usage_store.clone(),
        merge_capture_registry: review_ctx.merge_capture_registry.clone(),
        reviews_dir: review_ctx.reviews_dir.clone(),
        app: review_ctx.app_handle.clone(),
        review_logger: None,
        activity_tx: activity_tx.cloned(),
        nurse_engine: review_ctx.nurse_engine.clone(),
        custom_prompts,
    };

    let outcome = engine.run(config, deps, cancel_token.clone()).await?;

    let _ = review_ctx.app_handle.emit(
        "hivemind-progress",
        HivemindProgressEvent {
            job_id: outcome.job_id.clone(),
            review_id: None,
            event_type: "completed".to_string(),
            round: 0,
            model_id: String::new(),
            message: "Scout-plan review completed".to_string(),
            output_len: Some(outcome.refined_plan.len() as i64),
            swarm_id: Some(swarm_id.to_string()),
            feature_id: Some(feature_id.to_string()),
            phase: Some("completed".to_string()),
            ..Default::default()
        },
    );

    emit_review_progress(
        event_tx,
        swarm_id,
        feature_id,
        ProgressEventType::HivemindReviewCompleted,
        &format!(
            "Hivemind review complete ({} bytes refined plan)",
            outcome.refined_plan.len()
        ),
        Some(serde_json::json!({
            "job_id": outcome.job_id,
            "refined_plan_len": outcome.refined_plan.len(),
        })),
    );

    Ok(ScoutReviewOutcome {
        refined_plan: outcome.refined_plan,
        job_id: outcome.job_id,
    })
}

fn resolve_orchestrator(
    hivemind: &crate::hivemind::store::HivemindConfig,
    rounds: &[RoundCfg],
    fallback_model: &str,
) -> OrchestratorCfg {
    if let (Some(model), Some(provider)) = (
        hivemind.orchestrator_model.clone(),
        hivemind.orchestrator_provider.clone(),
    ) {
        OrchestratorCfg {
            model,
            provider,
            system_prompt: None,
        }
    } else {
        let last = rounds.last().and_then(|r| r.models.last()).cloned();
        match last {
            Some(m) => OrchestratorCfg {
                model: m.id,
                provider: m.provider,
                system_prompt: None,
            },
            None => OrchestratorCfg {
                model: fallback_model.to_string(),
                provider: "anthropic".to_string(),
                system_prompt: None,
            },
        }
    }
}

const CONTEXT_SYSTEM_PROMPT: &str = "You are gathering codebase context for a Hivemind \
review of a plan. Use the read-only tools (read, grep, find, ls) to explore the \
files relevant to the plan, then submit a terse but specific summary of the \
existing architecture, conventions, and any constraints the reviewers need to \
know. Quote file paths and short code excerpts.\n\n\
## OUTPUT FORMAT\n\n\
Call the `submit_context` tool with the full markdown summary as the `summary` \
argument:\n\n\
```\n\
submit_context({\"summary\": \"## Existing Architecture\\n...\\n\\n## Conventions\\n...\"})\n\
```\n\n\
There is no fallback — if you don't call the tool, the context gather fails.";

fn build_context_prompt(feature: &Feature, scout_plan: &str) -> String {
    format!(
        "## Feature\n\n{} — {}\n\n## Scout's Plan\n\n{}\n\nRead the files this plan touches and \
        summarise the existing code, conventions, and constraints reviewers should know. \
        Cite exact file paths. Submit the summary via `submit_context`.",
        feature.id, feature.name, scout_plan
    )
}

fn emit_review_progress(
    event_tx: &broadcast::Sender<ProgressEvent>,
    swarm_id: &str,
    feature_id: &str,
    event_type: ProgressEventType,
    message: &str,
    metadata: Option<serde_json::Value>,
) {
    let mut evt = ProgressEvent::new(event_type, swarm_id.to_string(), message.to_string())
        .with_feature(feature_id.to_string());
    if let Some(m) = metadata {
        evt = evt.with_metadata(m);
    }
    let _ = event_tx.send(evt);
}
