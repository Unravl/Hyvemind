use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{anyhow, Context as _, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tauri::Emitter;
use tokio::sync::Mutex;
use tokio::task::{Id as TaskId, JoinSet};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn, Instrument, Span};

use crate::hivemind::cache::{CachedResponse, ResponseCache};
use crate::hivemind::error::OrchestratorError;
use crate::hivemind::merge_capture::MergeCapture;
use crate::hivemind::output_capture;
use crate::hivemind::review_log::ReviewLogger;
use crate::hivemind::store::HivemindStore;
use crate::hivemind::verdicts;
use crate::pi::events::PiEvent;
use crate::pi::manager::PiManager;
use crate::pi::rpc::PiSessionOptions;
use crate::pi::session::SessionOwner;
use crate::providers::{CallRequest, ModelResponse, ProviderRegistry, StreamChunk};
use crate::state::log_redact::redact_all;
use crate::state::usage_store::UsageStore;
use crate::tunables;

/// Per-emit throttling window for forwarded `model_chunk` events. Keeps the
/// frontend smooth without flooding the IPC bridge — accumulated deltas are
/// coalesced into a single payload at most every `CHUNK_EMIT_INTERVAL_MS`.
const CHUNK_EMIT_INTERVAL_MS: u64 = 50;

// Maximum number of concurrent model calls — see
// [`tunables::hivemind_concurrency_cap`] (default 8, env override
// `HYVEMIND_CONCURRENCY_CAP`).

/// Base system prompt template for every Hivemind reviewer call. The literal
/// `{STANCE_SUFFIX}` token is replaced at runtime with the per-stance bias
/// text from [`Stance::system_prompt_suffix`]. Exposed publicly so the
/// Settings → Prompts catalog renders the exact same template that runs in
/// production.
pub const REVIEWER_BASE_TEMPLATE: &str =
    "You are an expert code reviewer analysing an implementation plan. \
The user prompt contains the plan, relevant source context, and review instructions. \
Follow those instructions precisely.\n\n\
{STANCE_SUFFIX}\n\n\
REVIEW LAYERS (prioritise earlier layers):\n\
- Layer 1 — Architecture & Structure: Phase ordering, file dependencies, missing pieces\n\
- Layer 2 — Logic & Algorithms: Data structures (O(1) vs O(n)), correctness, logic bugs\n\
- Layer 3 — Edge Cases & Error Handling: Null checks, race conditions, boundary conditions\n\
- Layer 4 — Performance: Hot path allocations, unnecessary copies, scaling concerns\n\n\
RESPONSE FORMAT\n\
You MUST submit your review by calling the `submit_review` tool. \
Do NOT also write a Markdown response — the tool arguments are the entire deliverable. \
The tool has these arguments:\n\n\
- `verdict` (required, string): one sentence overall assessment of the plan's quality and readiness.\n\
- `issues` (array): each entry is an object with:\n\
    - `layer` (1-4): which review layer the issue belongs to.\n\
    - `title` (string): short imperative title.\n\
    - `file_path` (string): exact path to the file the issue references.\n\
    - `description` (string): what the issue is and why it matters.\n\
    - `suggested_fix` (string, optional): concrete remediation.\n\
- `strengths` (array of strings): 2-3 things the plan gets right.\n\
- `key_takeaways` (array of strings): 3-5 actionable bullet points — the most critical risks or recommendations.\n\n\
RULES\n\
- Cite exact file paths and specific code for every issue\n\
- Do not guess behaviour not shown in the provided source — say you need more context instead\n\
- Be thorough but concise\n\
- Call `submit_review` exactly once at the end of your turn — no preamble, no follow-up text";

/// Review stance that determines the system prompt bias.
/// Only the Against variant is retained — the backend always hardcodes "against".
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stance {
    Against,
}

impl Stance {
    /// Returns a system prompt suffix that biases the reviewer.
    pub fn system_prompt_suffix(&self) -> &str {
        "CRITICAL PERSPECTIVE\n\
         Critique the proposal rigorously:\n\
         - Identify legitimate risks, overlooked complexity, and failure modes\n\
         - You MUST still acknowledge when something is fundamentally sound\n\
         - Being \"against\" means rigorous scrutiny for quality, not undermining good ideas"
    }
}

/// Identifies a model and its provider for use in a review.
#[derive(Debug, Clone)]
pub struct ReviewModelConfig {
    pub model_id: String,
    pub provider_name: String,
    /// Per-model sampling temperature. `None` means the provider's default
    /// (the field is omitted from the request body).
    pub temperature: Option<f64>,
    /// Per-model nucleus sampling `top_p`. `None` means the provider's default.
    pub top_p: Option<f64>,
    /// Resolved user-defined system-prompt suffix, appended after the base
    /// reviewer template + stance suffix when this model is dispatched.
    /// Resolved from `RoundModel::custom_prompt_id` at the command/engine
    /// boundary; dangling ids surface as `None` so the engine never has to
    /// know about deletion semantics.
    pub custom_prompt_body: Option<String>,
    /// Per-model thinking level (`"off"` | `"low"` | `"medium"` | `"high"`)
    /// carried through from `RoundModel::thinking`. Currently only honoured
    /// by Pi-subscription providers (claude-sub / chatgpt) — other providers
    /// drive thinking through their own native fields. `None` means "use the
    /// provider default" (Pi-subscription falls back to `Medium`, matching
    /// the Tasks-view default).
    pub thinking: Option<String>,
}

/// One round of a unified Hivemind run. Mirrors the on-disk
/// `rounds_config` JSON shape. Moved here from `core/scout_review.rs`
/// so the engine owns the round shape.
#[derive(Debug, Clone)]
pub struct RoundCfg {
    pub models: Vec<RoundModel>,
    pub timeout: u32,
}

#[derive(Debug, Clone)]
pub struct RoundModel {
    pub id: String,
    pub provider: String,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    /// Optional reference to a user-defined custom prompt stored in
    /// `Config::custom_prompts`. Resolved to a body at the engine boundary
    /// (see `resolve_custom_prompt_body` in `commands/hivemind.rs`); dangling
    /// ids silently fall through to no suffix.
    pub custom_prompt_id: Option<String>,
    /// On-disk thinking level from `rounds_config.json` (`"off"` | `"low"` |
    /// `"medium"` | `"high"`). Propagated into `ReviewModelConfig.thinking`
    /// and ultimately onto Pi's `--thinking` flag for Pi-subscription
    /// reviewers. Previously parsed-and-dropped, which silently downgraded
    /// every claude-sub / chatgpt reviewer call to `off` regardless of what
    /// the user configured — see `PiSubscriptionProvider::call_inner`.
    pub thinking: Option<String>,
}

/// Look up a `custom_prompt_id` against the snapshot of `Config::custom_prompts`
/// taken at run start. Returns the body to append to the system prompt, or
/// `None` if the id is absent / unset / unknown (dangling). Dangling ids
/// occur when a user deletes a custom prompt that a Hivemind still references;
/// the engine treats that as "no suffix" rather than an error.
pub fn resolve_custom_prompt_body(
    prompts: &[crate::state::config::CustomPrompt],
    id: Option<&str>,
) -> Option<String> {
    let id = id?;
    prompts.iter().find(|p| p.id == id).map(|p| p.body.clone())
}

pub fn parse_rounds_config(json: &str) -> Result<Vec<RoundCfg>> {
    let value: serde_json::Value =
        serde_json::from_str(json).context("rounds_config is not valid JSON")?;
    let arr = value
        .as_array()
        .ok_or_else(|| anyhow!("rounds_config is not a JSON array"))?;

    let mut rounds = Vec::with_capacity(arr.len());
    for round in arr {
        let models = round
            .get("models")
            .and_then(|m| m.as_array())
            .map(|ms| {
                ms.iter()
                    .filter_map(|m| {
                        let id = m.get("id")?.as_str()?.to_string();
                        let provider = m
                            .get("provider")
                            .and_then(|p| p.as_str())
                            .unwrap_or("")
                            .to_string();
                        if id.is_empty() || provider.is_empty() {
                            return None;
                        }
                        let temperature = m.get("temperature").and_then(|t| t.as_f64());
                        let top_p = m.get("top_p").and_then(|t| t.as_f64());
                        let custom_prompt_id = m
                            .get("custom_prompt_id")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());
                        let thinking = m
                            .get("thinking")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());
                        Some(RoundModel {
                            id,
                            provider,
                            temperature,
                            top_p,
                            custom_prompt_id,
                            thinking,
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let timeout = round
            .get("timeout")
            .and_then(|t| t.as_u64())
            .map(|t| t as u32)
            .unwrap_or(450);
        rounds.push(RoundCfg { models, timeout });
    }
    Ok(rounds)
}

/// Configures the Pi-driven orchestrator used for the context-gather
/// (optional) and per-round merge phases of a unified Hivemind run.
#[derive(Debug, Clone)]
pub struct OrchestratorCfg {
    pub model: String,
    pub provider: String,
    pub system_prompt: Option<String>,
}

fn qualify_model_for_provider(provider: &str, model: &str) -> String {
    if provider.is_empty() || model.starts_with(&format!("{}/", provider)) {
        model.to_string()
    } else {
        format!("{}/{}", provider, model)
    }
}

/// Pre-round context-gather spec. `None` means the plan is already
/// enriched (Tasks origin); `PiGather` spawns a swarm-owned Pi session
/// that produces a context blob appended to the plan before round 1.
#[derive(Debug, Clone)]
pub enum ContextSpec {
    None,
    PiGather {
        role: String,
        model: String,
        working_dir: PathBuf,
        system_prompt: String,
        prompt: String,
    },
}

/// Origin metadata for a unified Hivemind run. Threaded into every
/// `hivemind-progress` event so the frontend can route the event to the
/// right surface (Tasks bottom bar, SwarmControl per-feature panel,
/// SwarmControl Queen-plan panel).
#[derive(Debug, Clone, Default)]
pub struct ReviewAttribution {
    pub review_id: Option<String>,
    pub task_id: Option<String>,
    pub swarm_id: Option<String>,
    pub feature_id: Option<String>,
    pub project_path: Option<String>,
    pub source_label: String,
}

/// Full run configuration for [`ReviewEngine::run`].
#[derive(Debug, Clone)]
pub struct HivemindRunConfig {
    pub hivemind_id: String,
    pub rounds: Vec<RoundCfg>,
    pub orchestrator: OrchestratorCfg,
    pub stance: Stance,
    pub concurrency_cap: usize,
    pub context: ContextSpec,
    pub initial_plan: String,
    pub attribution: ReviewAttribution,
    /// When `Some`, reuse this `job_id` instead of generating one — and skip
    /// the engine's internal `create_job` call (the caller has already
    /// inserted the row). Used by Tasks `start_review` so the IPC can return
    /// the same job_id the frontend will receive events for.
    pub existing_job_id: Option<String>,
    /// 0-based offset applied to round numbers emitted on `hivemind-progress`,
    /// the review event log, and capture filenames (`merge-r{N}.txt`,
    /// `output-{model}-r{N}.txt`). The Tasks view dispatches each round as a
    /// separate `start_review` call with `rounds.len() == 1`; without this
    /// offset every round reports `round=1` and round 2's capture files
    /// silently overwrite round 1's. Default is `0` (engine starts at round 1).
    pub round_offset: u32,
}

/// Subsystem handles passed to [`ReviewEngine::run`]. Cheaply clonable.
pub struct EngineDeps {
    pub pi_manager: Arc<PiManager>,
    pub store: Arc<HivemindStore>,
    pub provider_registry: Arc<tokio::sync::RwLock<ProviderRegistry>>,
    pub usage_store: Arc<UsageStore>,
    pub merge_capture_registry: Arc<std::sync::RwLock<HashMap<String, Arc<MergeCapture>>>>,
    pub reviews_dir: PathBuf,
    pub app: tauri::AppHandle,
    pub review_logger: Option<Arc<ReviewLogger>>,
    pub activity_tx: Option<crate::core::queen::ActivityTx>,
    /// Used by the model-call failure path to synthesize a visible Nurse
    /// intervention when a provider's circuit breaker trips during a review.
    /// Optional so unit tests can construct `EngineDeps` without the full
    /// AppState. v2 push-mode engine — handles all synthesized intervention
    /// dispatch through `report_synthesized` (Hivemind sessions have no
    /// registered Pi process behind them).
    pub nurse_engine: Option<Arc<crate::nurse::engine::NurseEngine>>,
    /// Snapshot of `Config::custom_prompts` taken at run start. Used by
    /// `run()` to resolve `RoundModel::custom_prompt_id` into the dispatched
    /// `ReviewModelConfig::custom_prompt_body`. Dangling ids resolve to `None`,
    /// matching the "silently treat as None" delete-cascade behaviour.
    pub custom_prompts: Vec<crate::state::config::CustomPrompt>,
}

#[derive(Debug, Clone)]
pub struct EngineOutcome {
    pub refined_plan: String,
    pub job_id: String,
}

/// Build the `round_started` payload (pre-attribution). Extracted into a
/// helper so backend tests can verify the field set, especially that the
/// `models` array is included. The frontend reducer relies on `models` to
/// seed spinner rows the instant a round starts, before any model produces
/// its first chunk.
fn build_round_started_payload(
    job_id: &str,
    review_id: Option<&str>,
    round_num: u32,
    total_rounds: usize,
    model_ids: &[String],
) -> serde_json::Value {
    // Build the richer `model_instances` shape from the same ordered list of
    // model IDs. The legacy `models` array is retained for back-compat with
    // any in-flight frontend that hasn't been reloaded (Tauri rebuilds reload
    // the renderer, but this is belt-and-braces).
    let model_instances: Vec<serde_json::Value> = model_ids
        .iter()
        .enumerate()
        .map(|(idx, id)| {
            serde_json::json!({
                "model_id": id,
                "model_idx": idx,
            })
        })
        .collect();
    serde_json::json!({
        "job_id": job_id,
        "review_id": review_id,
        "event_type": "round_started",
        "round": round_num,
        "model_id": "",
        "message": format!("Round {} of {} started", round_num, total_rounds),
        "models": model_ids,
        "model_instances": model_instances,
    })
}

/// Inject `task_id` / `source_label` (and the existing swarm/feature/phase
/// fields) into a `hivemind-progress` payload built by `serde_json::json!`.
/// Used by [`ReviewEngine::run`] so every event carries the full attribution
/// the frontend reducer needs.
fn add_event_attribution(
    mut value: serde_json::Value,
    attribution: &ReviewAttribution,
    phase: Option<&str>,
) -> serde_json::Value {
    if let Some(obj) = value.as_object_mut() {
        if let Some(sid) = &attribution.swarm_id {
            obj.insert(
                "swarm_id".to_string(),
                serde_json::Value::String(sid.clone()),
            );
        }
        if let Some(fid) = &attribution.feature_id {
            obj.insert(
                "feature_id".to_string(),
                serde_json::Value::String(fid.clone()),
            );
        }
        if let Some(tid) = &attribution.task_id {
            obj.insert(
                "task_id".to_string(),
                serde_json::Value::String(tid.clone()),
            );
        }
        if !attribution.source_label.is_empty() {
            obj.insert(
                "source_label".to_string(),
                serde_json::Value::String(attribution.source_label.clone()),
            );
        }
        if let Some(p) = phase {
            obj.insert(
                "phase".to_string(),
                serde_json::Value::String(p.to_string()),
            );
        }
    }
    value
}

/// Spawn a Tasks-view `hivemind-progress` forwarder for a single turn of
/// the context-gather session. The task drains the session's event
/// broadcast, emits structured `context_*` events, and exits cleanly on
/// the first `AgentEnd` / `TurnComplete` (so the caller can `await` the
/// handle to know the turn is fully drained before inspecting tool args).
/// Re-entrant: a fresh forwarder must be spawned for each retry attempt
/// because the previous task breaks out of its loop at end-of-turn.
fn spawn_context_chunk_forwarder(
    session: &crate::pi::session::PiSession,
    app: tauri::AppHandle,
    attribution: ReviewAttribution,
    job_id: String,
    session_id: String,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    let mut rx = session.subscribe_events();
    tokio::spawn(async move {
        use tokio::sync::broadcast::error::RecvError;
        let emit = |payload: serde_json::Value| {
            let _ = app.emit(
                "hivemind-progress",
                add_event_attribution(payload, &attribution, Some("context")),
            );
        };
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                recv = rx.recv() => {
                    match recv {
                        Ok(PiEvent::TextDelta(text)) => {
                            emit(serde_json::json!({
                                "job_id": job_id,
                                "review_id": attribution.review_id,
                                "event_type": "context_text",
                                "round": 0,
                                "model_id": "",
                                "message": "",
                                "delta": text,
                                "session_id": session_id,
                            }));
                        }
                        Ok(PiEvent::ThinkingDelta(text)) => {
                            emit(serde_json::json!({
                                "job_id": job_id,
                                "review_id": attribution.review_id,
                                "event_type": "context_thinking",
                                "round": 0,
                                "model_id": "",
                                "message": "",
                                "delta": text,
                                "session_id": session_id,
                            }));
                        }
                        Ok(PiEvent::ToolExecutionStart { tool_call_id, name, args }) => {
                            emit(serde_json::json!({
                                "job_id": job_id,
                                "review_id": attribution.review_id,
                                "event_type": "context_tool_start",
                                "round": 0,
                                "model_id": "",
                                "message": "",
                                "session_id": session_id,
                                "tool_call_id": tool_call_id,
                                "tool_name": name,
                                "tool_args": args,
                            }));
                        }
                        Ok(PiEvent::ToolExecutionUpdate { tool_call_id, output }) => {
                            emit(serde_json::json!({
                                "job_id": job_id,
                                "review_id": attribution.review_id,
                                "event_type": "context_tool_update",
                                "round": 0,
                                "model_id": "",
                                "message": "",
                                "session_id": session_id,
                                "tool_call_id": tool_call_id,
                                "tool_output": output,
                            }));
                        }
                        Ok(PiEvent::ToolExecutionEnd { tool_call_id, result }) => {
                            emit(serde_json::json!({
                                "job_id": job_id,
                                "review_id": attribution.review_id,
                                "event_type": "context_tool_end",
                                "round": 0,
                                "model_id": "",
                                "message": "",
                                "session_id": session_id,
                                "tool_call_id": tool_call_id,
                                "tool_result": result,
                            }));
                        }
                        Ok(PiEvent::AgentEnd) | Ok(PiEvent::TurnComplete) => break,
                        Err(RecvError::Lagged(n)) => {
                            warn!(
                                session_id = %session_id,
                                dropped = n,
                                "context structured-chunk forwarder broadcast lagged"
                            );
                            continue;
                        }
                        Err(RecvError::Closed) => break,
                        _ => {}
                    }
                }
            }
        }
    })
}

/// Maximum number of additional attempts to make if the context model
/// finishes its turn without calling `submit_context`. The initial turn
/// counts as attempt 1; this is the cap on follow-up nudges.
const MAX_SUBMIT_CONTEXT_RETRIES: u32 = 2;

/// The review engine that orchestrates multi-model reviews.
pub struct ReviewEngine {
    cache: Arc<ResponseCache>,
}

impl ReviewEngine {
    /// Create a new review engine with the given response cache.
    pub fn new(cache: Arc<ResponseCache>) -> Self {
        Self { cache }
    }

    /// Build the cross-review prompt for a given round.
    fn build_cross_review_prompt(
        &self,
        original_plan: &str,
        round_outputs: &[Vec<(String, ModelResponse)>],
    ) -> String {
        let mut prompt = format!("# Plan Under Review\n\n{}\n\n", original_plan);

        for (round_idx, outputs) in round_outputs.iter().enumerate() {
            prompt.push_str(&format!(
                "---\n\n# Previous Review Round {}\n\n",
                round_idx + 1
            ));
            for (provider, response) in outputs {
                prompt.push_str(&format!(
                    "## Review by {}/{}\n\n{}\n\n",
                    provider, response.model_id, response.output
                ));
            }
        }

        if round_outputs.is_empty() {
            prompt.push_str("# Your Task\n\nProvide a thorough review of the plan above.\n");
        } else {
            prompt.push_str(
                "---\n\n# Your Task\n\n\
                 Review the plan above, taking into account the previous reviews. \
                 Provide your own independent assessment, noting where you agree or \
                 disagree with prior reviewers. Focus on adding new insights.\n",
            );
        }

        prompt
    }

    /// Unified entry point that owns the full Hivemind run lifecycle:
    /// optional Pi-driven context-gather, parallel multi-round model
    /// dispatch, and per-round Pi-driven orchestrator merge. Replaces
    /// the two separate code paths in `commands/hivemind.rs::start_review`
    /// (Tasks, frontend-merged) and `core/scout_review.rs` (Swarms,
    /// engine-only) with a single backend-driven flow.
    #[tracing::instrument(
        skip_all,
        fields(hivemind_id = %config.hivemind_id, source = %config.attribution.source_label)
    )]
    pub async fn run(
        &self,
        config: HivemindRunConfig,
        deps: EngineDeps,
        cancel: CancellationToken,
    ) -> Result<EngineOutcome> {
        let HivemindRunConfig {
            hivemind_id,
            rounds,
            orchestrator,
            stance,
            concurrency_cap,
            context,
            initial_plan,
            attribution,
            existing_job_id,
            round_offset,
        } = config;

        if rounds.is_empty() || rounds[0].models.is_empty() {
            return Err(anyhow!("hivemind run requires at least one round + model"));
        }

        let job_id = existing_job_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let stance_str = "against";
        let timeout_seconds = rounds
            .iter()
            .map(|r| r.timeout)
            .max()
            .unwrap_or_else(|| tunables::hivemind_round_timeout_secs() as u32);

        if existing_job_id.is_none() {
            let _ = deps
                .store
                .create_job(
                    &job_id,
                    &initial_plan,
                    stance_str,
                    rounds.len() as i64,
                    timeout_seconds as i64,
                    attribution.review_id.as_deref(),
                    Some(&hivemind_id),
                    None,
                    attribution.task_id.as_deref(),
                    attribution.project_path.as_deref(),
                )
                .await;
        }

        let _ = deps.app.emit(
            "hivemind-progress",
            add_event_attribution(
                serde_json::json!({
                    "job_id": job_id,
                    "review_id": attribution.review_id,
                    "event_type": "started",
                    "round": 0,
                    "model_id": "",
                    "message": format!("Hivemind run started ({} rounds)", rounds.len()),
                }),
                &attribution,
                Some("started"),
            ),
        );

        if let Some(logger) = &deps.review_logger {
            logger
                .log(
                    "engine_started",
                    serde_json::json!({
                        "hivemind_id": hivemind_id,
                        "rounds": rounds.len(),
                        "source_label": attribution.source_label,
                    }),
                )
                .await;
        }

        let enriched_plan = match self
            .gather_context_phase(
                &context,
                &attribution,
                &job_id,
                &orchestrator.provider,
                &deps,
                &cancel,
            )
            .await
        {
            Ok(blob) => {
                if blob.is_empty() {
                    initial_plan.clone()
                } else {
                    format!(
                        "{}\n\n## Codebase Context\n\n{}",
                        initial_plan.trim_end(),
                        blob.trim()
                    )
                }
            }
            Err(e) => {
                let msg = e.to_string();
                let cancelled = crate::hivemind::error::is_cancellation(&e);
                let event_type = if cancelled { "cancelled" } else { "failed" };
                let phase = if cancelled { "cancelled" } else { "failed" };
                let ui_message = if cancelled {
                    "Run cancelled".to_string()
                } else {
                    msg.clone()
                };
                let _ = deps.app.emit(
                    "hivemind-progress",
                    add_event_attribution(
                        serde_json::json!({
                            "job_id": job_id,
                            "review_id": attribution.review_id,
                            "event_type": event_type,
                            "round": 0,
                            "model_id": "",
                            "message": ui_message,
                        }),
                        &attribution,
                        Some(phase),
                    ),
                );
                if cancelled {
                    let _ = deps.store.update_job_status(&job_id, "cancelled").await;
                } else {
                    let _ = deps.store.fail_job(&job_id, &msg).await;
                }
                return Err(e);
            }
        };

        let system_prompt =
            REVIEWER_BASE_TEMPLATE.replace("{STANCE_SUFFIX}", stance.system_prompt_suffix());
        let effective_cap = concurrency_cap.min(tunables::hivemind_concurrency_cap());
        let semaphore = Arc::new(tokio::sync::Semaphore::new(effective_cap));

        let mut current_plan = enriched_plan;

        for (round_idx, round_cfg) in rounds.iter().enumerate() {
            let round_num = (round_idx + 1) as u32 + round_offset;
            // Compute model IDs once per round so the `round_started` emit
            // below can pre-seed frontend spinner rows. Typically 2-6
            // entries; the alloc is negligible against the network calls
            // that follow.
            let model_ids: Vec<String> = round_cfg.models.iter().map(|m| m.id.clone()).collect();
            if cancel.is_cancelled() {
                let _ = deps.app.emit(
                    "hivemind-progress",
                    add_event_attribution(
                        serde_json::json!({
                            "job_id": job_id,
                            "review_id": attribution.review_id,
                            "event_type": "cancelled",
                            "round": round_num,
                            "model_id": "",
                            "message": "Run cancelled",
                        }),
                        &attribution,
                        // Distinct phase so the frontend can tell cancellation
                        // apart from a real failure when picking a pill colour.
                        Some("cancelled"),
                    ),
                );
                let _ = deps.store.update_job_status(&job_id, "cancelled").await;
                return Err(OrchestratorError::Cancelled.into());
            }

            let _ = deps.app.emit(
                "hivemind-progress",
                add_event_attribution(
                    build_round_started_payload(
                        &job_id,
                        attribution.review_id.as_deref(),
                        round_num,
                        rounds.len(),
                        &model_ids,
                    ),
                    &attribution,
                    Some("round"),
                ),
            );
            // Fire-and-forget observability writes. Neither the review
            // log entry nor the job-status SQLite write is on the
            // user-visible critical path — the `round_started` emit
            // above is what unblocks the UI "Round N/N" pill, and the
            // first `dispatch_round` step must not be delayed by
            // observability I/O. This is defence-in-depth for the
            // merge_completed → round_started gap closure.
            if let Some(logger) = deps.review_logger.clone() {
                let model_count = round_cfg.models.len();
                tokio::spawn(async move {
                    logger
                        .log(
                            "round_started",
                            serde_json::json!({
                                "round": round_num,
                                "model_count": model_count,
                            }),
                        )
                        .await;
                });
            }
            {
                let store = deps.store.clone();
                let job_id_owned = job_id.to_string();
                let status = format!("round_{}", round_num);
                tokio::spawn(async move {
                    let _ = store.update_job_status(&job_id_owned, &status).await;
                });
            }

            let model_configs: Vec<ReviewModelConfig> = round_cfg
                .models
                .iter()
                .map(|m| ReviewModelConfig {
                    model_id: m.id.clone(),
                    provider_name: m.provider.clone(),
                    temperature: m.temperature,
                    top_p: m.top_p,
                    custom_prompt_body: resolve_custom_prompt_body(
                        &deps.custom_prompts,
                        m.custom_prompt_id.as_deref(),
                    ),
                    thinking: m.thinking.clone(),
                })
                .collect();

            let round_responses = self
                .dispatch_round(
                    round_num,
                    &model_configs,
                    &current_plan,
                    &system_prompt,
                    stance_str,
                    round_cfg.timeout.max(60),
                    semaphore.clone(),
                    &deps,
                    &attribution,
                    &job_id,
                    &cancel,
                )
                .await?;

            if let Some(logger) = &deps.review_logger {
                logger
                    .log(
                        "round_completed",
                        serde_json::json!({
                            "round": round_num,
                            "responses_count": round_responses.len(),
                        }),
                    )
                    .await;
            }
            let _ = deps.app.emit(
                "hivemind-progress",
                add_event_attribution(
                    serde_json::json!({
                        "job_id": job_id,
                        "review_id": attribution.review_id,
                        "event_type": "round_completed",
                        "round": round_num,
                        "model_id": "",
                        "message": format!("Round {} completed ({} responses)", round_num, round_responses.len()),
                    }),
                    &attribution,
                    Some("round"),
                ),
            );

            let merge_outcome = self
                .run_merge_phase(
                    round_num,
                    rounds.len() as u32,
                    &round_responses,
                    &current_plan,
                    &orchestrator,
                    &attribution,
                    &job_id,
                    &deps,
                    &cancel,
                )
                .await;

            match merge_outcome {
                Ok(MergePhaseOutcome { merged_plan }) => {
                    current_plan = merged_plan;
                }
                Err(e) => {
                    let is_last_round = (round_idx + 1) == rounds.len();
                    warn!(
                        round = round_num,
                        error = %e,
                        is_last_round,
                        "merge phase failed; preserving prior plan content"
                    );
                    if let Some(logger) = &deps.review_logger {
                        logger
                            .log(
                                "merge_failed",
                                serde_json::json!({
                                    "round": round_num,
                                    "error": e.to_string(),
                                    "is_last_round": is_last_round,
                                }),
                            )
                            .await;
                    }
                    if !is_last_round {
                        current_plan =
                            self.build_cross_review_prompt(&current_plan, &[round_responses]);
                    }
                }
            }
        }

        let total_cost: f64 = deps
            .store
            .get_job_steps(&job_id)
            .await
            .unwrap_or_default()
            .iter()
            .filter_map(|s| s.cost)
            .sum();
        let total_input: i64 = deps
            .store
            .get_job_steps(&job_id)
            .await
            .unwrap_or_default()
            .iter()
            .filter_map(|s| s.input_tokens)
            .sum();
        let total_output: i64 = deps
            .store
            .get_job_steps(&job_id)
            .await
            .unwrap_or_default()
            .iter()
            .filter_map(|s| s.output_tokens)
            .sum();
        let _ = deps
            .store
            .complete_job(
                &job_id,
                &current_plan,
                total_cost,
                total_input,
                total_output,
            )
            .await;

        if let Some(logger) = &deps.review_logger {
            logger
                .log(
                    "engine_completed",
                    serde_json::json!({
                        "total_rounds": rounds.len(),
                        "output_len": current_plan.len(),
                    }),
                )
                .await;
        }

        let _ = deps.app.emit(
            "hivemind-progress",
            add_event_attribution(
                serde_json::json!({
                    "job_id": job_id,
                    "review_id": attribution.review_id,
                    "event_type": "completed",
                    "round": rounds.len() as u32 + round_offset,
                    "model_id": "",
                    "message": "Hivemind run completed",
                    "output_len": current_plan.len() as i64,
                }),
                &attribution,
                Some("completed"),
            ),
        );

        Ok(EngineOutcome {
            refined_plan: current_plan,
            job_id,
        })
    }

    async fn gather_context_phase(
        &self,
        context: &ContextSpec,
        attribution: &ReviewAttribution,
        job_id: &str,
        provider: &str,
        deps: &EngineDeps,
        cancel: &CancellationToken,
    ) -> Result<String> {
        let (role, model, working_dir, system_prompt, prompt) = match context {
            ContextSpec::None => return Ok(String::new()),
            ContextSpec::PiGather {
                role,
                model,
                working_dir,
                system_prompt,
                prompt,
            } => (
                role.clone(),
                model.clone(),
                working_dir.clone(),
                system_prompt.clone(),
                prompt.clone(),
            ),
        };

        // Pre-allocate the session id so we can carry it on `context_started`
        // — Tasks-side reducer registers the session as "internal" before
        // the first structured chunk arrives.
        let session_id = format!("hivemind-context-{}", uuid::Uuid::new_v4());

        let _ = deps.app.emit(
            "hivemind-progress",
            add_event_attribution(
                serde_json::json!({
                    "job_id": job_id,
                    "review_id": attribution.review_id,
                    "event_type": "context_started",
                    "round": 0,
                    "model_id": model,
                    "message": "Gathering codebase context",
                    "session_id": session_id,
                }),
                attribution,
                Some("context"),
            ),
        );
        if let Some(logger) = &deps.review_logger {
            logger
                .log("context_started", serde_json::json!({ "model": model }))
                .await;
        }

        let options = PiSessionOptions::for_scout(&model, &system_prompt);
        let session = deps
            .pi_manager
            .spawn_session_with_options(&session_id, &options, &working_dir)
            .await
            .map_err(|e| anyhow!(e))
            .context("failed to spawn hivemind context session")?;
        if let Some(sid) = attribution.swarm_id.as_deref() {
            session.set_owner(SessionOwner::Swarm {
                swarm_id: sid.to_string(),
                role: role.clone(),
            });
        }

        if let (Some(tx), Some(swarm_id), Some(feature_id)) = (
            deps.activity_tx.as_ref(),
            attribution.swarm_id.as_ref(),
            attribution.feature_id.as_ref(),
        ) {
            let _ = tx.send(serde_json::json!({
                "swarm_id": swarm_id,
                "feature_id": feature_id,
                "agent": "hivemind-context",
                "session_id": session_id,
                "timestamp": chrono::Utc::now().to_rfc3339(),
                "kind": "agent_start",
                "model": model,
            }));
            crate::core::queen::spawn_agent_forwarder_public(
                &session,
                swarm_id.to_string(),
                feature_id.to_string(),
                "hivemind-context".to_string(),
                session_id.clone(),
                tx.clone(),
            );
        }

        // Structured-chunk forwarder for the `hivemind-progress` Tasks-view
        // stream. Mirrors the merge-phase forwarder so Tasks-origin reviews
        // (and the audit pipeline) see context reasoning / text / tool calls
        // inline. The SwarmControl surface is fed independently via
        // `spawn_agent_forwarder_public` above. The helper is re-entrant —
        // the retry loop below spawns fresh forwarders for each nudge.
        let context_forwarder = spawn_context_chunk_forwarder(
            &session,
            deps.app.clone(),
            attribution.clone(),
            job_id.to_string(),
            session_id.clone(),
            cancel.clone(),
        );

        // Capture the start instant *before* send_prompt so the recorded
        // duration covers prompt-send + collect-response.
        let context_start = Instant::now();
        let send_result = session
            .send_prompt(&prompt, None)
            .await
            .map_err(|e| anyhow!(e))
            .context("hivemind context send_prompt failed");

        let collect_result = if send_result.is_ok() {
            tokio::select! {
                r = session.collect_response() => r
                    .map_err(|e| anyhow!(e))
                    .context("hivemind context collect_response failed"),
                _ = cancel.cancelled() => Err(anyhow::Error::new(OrchestratorError::Cancelled)
                    .context("hivemind context cancelled")),
            }
        } else {
            Err(send_result.err().unwrap())
        };

        if let (Some(tx), Some(swarm_id), Some(feature_id)) = (
            deps.activity_tx.as_ref(),
            attribution.swarm_id.as_ref(),
            attribution.feature_id.as_ref(),
        ) {
            let _ = tx.send(serde_json::json!({
                "swarm_id": swarm_id,
                "feature_id": feature_id,
                "agent": "hivemind-context",
                "session_id": session_id,
                "timestamp": chrono::Utc::now().to_rfc3339(),
                "kind": "agent_end",
                "success": collect_result.is_ok(),
            }));
        }

        // Stop the structured-chunk forwarder before draining tool args /
        // killing the session. Errors are logged inside the forwarder.
        let _ = context_forwarder.await;

        let response = collect_result?;

        // Persist context-gather usage for swarm-attributed reviews so the
        // Swarm Control header's `swarm·in / swarm·out` reflects tokens
        // spent during context gather (the live-busy walk in
        // `get_swarm_usage` only catches the in-flight window). For
        // Tasks-view reviews (no swarm_id) this is skipped entirely
        // — byte-identical behavior to before.
        if let Some(sid) = attribution.swarm_id.as_deref() {
            let duration_ms = context_start.elapsed().as_millis() as i64;
            match session.get_session_stats().await {
                Ok(stats) => {
                    let entry = crate::state::usage_store::UsageEntry {
                        source: "hivemind".to_string(),
                        source_id: Some(hivemind_source_id(Some(sid), job_id, "context")),
                        model_id: model.clone(),
                        provider: provider.to_string(),
                        input_tokens: stats.input as i64,
                        output_tokens: stats.output as i64,
                        cache_read_tokens: stats.cache_read as i64,
                        cache_write_tokens: stats.cache_write as i64,
                        cost: stats.cost,
                        duration_ms,
                    };
                    if let Err(e) = deps.usage_store.record_usage(entry).await {
                        tracing::warn!(
                            swarm_id = %sid,
                            error = %e,
                            "failed to record hivemind context usage"
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        swarm_id = %sid,
                        error = %e,
                        "failed to read hivemind context session stats"
                    );
                }
            }
        }

        // The context model MUST call `submit_context`. Drain BEFORE killing
        // the session — the captured map lives on `PiSession`.
        let mut context_tool_args = session.take_tool_args("submit_context");

        // If the initial turn finished without calling the tool, send a
        // small number of steering nudges before giving up. Weaker / smaller
        // models routinely "explain" their context summary as prose instead
        // of routing it through the tool; one or two pointed reminders
        // usually fixes it. Each retry spawns a fresh structured-chunk
        // forwarder so the UI sees the nudged turn stream in.
        let mut retry_count: u32 = 0;
        while context_tool_args.is_none() && retry_count < MAX_SUBMIT_CONTEXT_RETRIES {
            if cancel.is_cancelled() {
                break;
            }
            retry_count += 1;
            let attempt = retry_count + 1; // 1-indexed total attempts (initial + retries)
            let nudge = "You finished your turn without calling the `submit_context` tool. \
                         You MUST call `submit_context` now with the full markdown summary as \
                         the `summary` argument. Do not produce additional prose, reasoning, \
                         or commentary — invoke the tool with your gathered context.";

            let _ = deps.app.emit(
                "hivemind-progress",
                add_event_attribution(
                    serde_json::json!({
                        "job_id": job_id,
                        "review_id": attribution.review_id,
                        "event_type": "context_retry",
                        "round": 0,
                        "model_id": model,
                        "message": format!(
                            "Context model did not call submit_context — nudging (attempt {}/{})",
                            attempt,
                            MAX_SUBMIT_CONTEXT_RETRIES + 1,
                        ),
                        "attempt": attempt,
                        "max_attempts": MAX_SUBMIT_CONTEXT_RETRIES + 1,
                        "session_id": session_id,
                    }),
                    attribution,
                    Some("context"),
                ),
            );
            if let Some(logger) = &deps.review_logger {
                logger
                    .log(
                        "context_retry",
                        serde_json::json!({
                            "attempt": attempt,
                            "reason": "submit_context not called",
                        }),
                    )
                    .await;
            }

            let retry_forwarder = spawn_context_chunk_forwarder(
                &session,
                deps.app.clone(),
                attribution.clone(),
                job_id.to_string(),
                session_id.clone(),
                cancel.clone(),
            );

            let send_retry = session
                .send_prompt(nudge, None)
                .await
                .map_err(|e| anyhow!(e))
                .context("hivemind context retry send_prompt failed");
            if let Err(e) = send_retry {
                let _ = retry_forwarder.await;
                warn!(error = %e, "hivemind context retry: send_prompt failed; aborting retry loop");
                break;
            }
            let collect_retry = tokio::select! {
                r = session.collect_response() => r
                    .map_err(|e| anyhow!(e))
                    .context("hivemind context retry collect_response failed"),
                _ = cancel.cancelled() => Err(anyhow::Error::new(OrchestratorError::Cancelled)
                    .context("hivemind context cancelled during retry")),
            };
            let _ = retry_forwarder.await;
            if let Err(e) = collect_retry {
                warn!(error = %e, "hivemind context retry: collect_response failed; aborting retry loop");
                break;
            }
            context_tool_args = session.take_tool_args("submit_context");
        }

        let _ = deps.pi_manager.kill_session(&session_id).await;

        let args = match context_tool_args {
            Some(a) => a,
            None => {
                // Final failure after exhausting retries. Surface a Nurse
                // intervention so the user sees a structured "protocol
                // violation" card instead of just a toast — and so the
                // intervention is recorded for the Dashboard / Settings
                // surfaces alongside other nurse activity.
                // session_id IS a registered Pi session here, so route
                // through `report_error` — the engine injects a synthetic
                // signal on the session and dispatches via the three-tier
                // pipeline.
                if let Some(engine) = &deps.nurse_engine {
                    let owner = crate::nurse::synthesized::InterventionOwner {
                        session_id: Some(session_id.clone()),
                        task_id: attribution.task_id.clone(),
                        swarm_id: attribution.swarm_id.clone(),
                        feature_id: attribution.feature_id.clone(),
                        review_id: attribution.review_id.clone(),
                        job_id: Some(job_id.to_string()),
                        round: Some(0),
                    };
                    engine.report_error(
                        crate::nurse::synthesized::SynthesizedKind::ProtocolViolation {
                            agent: "hivemind-context".to_string(),
                            expected_tool: "submit_context".to_string(),
                            attempts: retry_count + 1,
                        },
                        session_id.clone(),
                        owner,
                    );
                }
                return Err(anyhow!(
                    "hivemind context session did not call submit_context (after {} attempt(s))",
                    retry_count + 1
                ));
            }
        };
        let trimmed = args
            .get("summary")
            .and_then(|s| s.as_str())
            .map(|s| s.trim().to_string())
            .ok_or_else(|| anyhow!("submit_context args missing or non-string `summary` field"))?;
        if trimmed.is_empty() {
            return Err(anyhow!("hivemind context session returned empty output"));
        }
        let _ = response; // response text is unused now that tool-args is mandatory.

        let _ = deps.app.emit(
            "hivemind-progress",
            add_event_attribution(
                serde_json::json!({
                    "job_id": job_id,
                    "review_id": attribution.review_id,
                    "event_type": "context_completed",
                    "round": 0,
                    "model_id": model,
                    "message": "Context gather complete",
                    "output_len": trimmed.len() as i64,
                }),
                attribution,
                Some("context"),
            ),
        );
        if let Some(logger) = &deps.review_logger {
            logger
                .log(
                    "context_completed",
                    serde_json::json!({ "output_len": trimmed.len() }),
                )
                .await;
        }

        Ok(trimmed)
    }

    #[allow(clippy::too_many_arguments)]
    async fn dispatch_round(
        &self,
        round: u32,
        models: &[ReviewModelConfig],
        user_prompt: &str,
        system_prompt: &str,
        stance_str: &str,
        timeout_seconds: u32,
        semaphore: Arc<tokio::sync::Semaphore>,
        deps: &EngineDeps,
        attribution: &ReviewAttribution,
        job_id: &str,
        cancel: &CancellationToken,
    ) -> Result<Vec<(String, ModelResponse)>> {
        let mut join_set: JoinSet<
            std::result::Result<
                (String, String, ModelResponse, u64),
                (String, String, String, anyhow::Error),
            >,
        > = JoinSet::new();

        // Side-map keyed by `tokio::task::Id`. See the `run_review` site for
        // the rationale — anything left here when the round deadline fires is
        // reaped via `reap_stranded_tasks_attr` so its step row flips to
        // `failed` and a `model_failed` event is emitted to the frontend.
        // Value tuple: (step_id, model_id, provider, model_idx).
        let mut in_flight: HashMap<TaskId, (String, String, String, usize)> = HashMap::new();

        let first_chunk_seen: Arc<Mutex<HashSet<(u32, String)>>> =
            Arc::new(Mutex::new(HashSet::new()));
        let per_call_timeout = tokio::time::Duration::from_secs(timeout_seconds as u64);
        let review_id_owned = attribution.review_id.clone();

        // Pre-hash prompts once per round (not once per model).
        let system_prompt_hash = ResponseCache::hash_str(system_prompt);
        let user_prompt_hash = ResponseCache::hash_str(user_prompt);

        for (model_idx, model_config) in models.iter().enumerate() {
            let model_id = model_config.model_id.clone();
            let provider_name = model_config.provider_name.clone();
            let temperature = model_config.temperature;
            let top_p = model_config.top_p;
            let thinking = model_config.thinking.clone();
            let (sys_prompt, sys_hash) = match &model_config.custom_prompt_body {
                Some(suffix) => {
                    let combined = format!("{}\n\n{}", system_prompt, suffix);
                    let hash = ResponseCache::hash_str(&combined);
                    (combined, hash)
                }
                None => (system_prompt.to_string(), system_prompt_hash),
            };
            let usr_prompt = user_prompt.to_string();
            let sem = semaphore.clone();
            let cache = self.cache.clone();
            let cancel_clone = cancel.clone();
            let registry = deps.provider_registry.clone();

            let step_id = uuid::Uuid::new_v4().to_string();
            let job_id_owned = job_id.to_string();
            let stance_owned = stance_str.to_string();

            let store_clone = deps.store.clone();
            let _ = deps
                .store
                .create_job_step(
                    &step_id,
                    &job_id_owned,
                    round as i64,
                    model_idx as i64,
                    &model_id,
                    &provider_name,
                    &stance_owned,
                    user_prompt,
                )
                .await;

            let step_id_clone = step_id.clone();
            let logger_clone = deps.review_logger.clone();
            let step_id_err = step_id.clone();
            let model_id_err = model_id.clone();
            let provider_err = provider_name.clone();
            // `model_idx` (a `usize`) is Copy; capture the immutable
            // closure-friendly copy for use in spawned-task closures and
            // logger payloads below.
            let model_idx_for_step = model_idx;

            // Side-map registration values — see `run_review` site.
            let step_id_inflight = step_id.clone();
            let model_id_inflight = model_id.clone();
            let provider_inflight = provider_name.clone();

            let app_for_task = deps.app.clone();
            let first_chunk_seen_clone = Arc::clone(&first_chunk_seen);
            let review_id_for_task = review_id_owned.clone();
            let attribution_for_task = attribution.clone();
            let reviews_dir_for_task = deps.reviews_dir.clone();

            let model_span = tracing::info_span!(
                parent: Span::current(),
                "review_model_call",
                round = round,
                model_id = %model_id,
                provider = %provider_name,
            );

            let abort = join_set.spawn(async move {
                let inner: anyhow::Result<(String, String, ModelResponse, u64)> = async move {
                    let _permit = sem
                        .acquire()
                        .await
                        .map_err(|e| anyhow!("semaphore closed: {}", e))?;

                    if cancel_clone.is_cancelled() {
                        return Err(anyhow!("cancelled"));
                    }

                    if let Some(ref logger) = logger_clone {
                        logger
                            .log(
                                "model_call_started",
                                serde_json::json!({
                                    "model_id": model_id,
                                    "provider": provider_name,
                                    "model_idx": model_idx_for_step,
                                }),
                            )
                            .await;
                    }
                    let _ = store_clone.update_job_step_started(&step_id_clone).await;

                    let start = Instant::now();
                    let cache_key = ResponseCache::make_key(
                        &model_id,
                        sys_hash,
                        user_prompt_hash,
                        &provider_name,
                        temperature,
                        top_p,
                        None,
                    );

                    if let Some(cached) = cache.get(&cache_key).await {
                        info!(model = %model_id, "cache hit");
                        // Provider-cache fields (Anthropic / DeepSeek) stay 0
                        // here on purpose — the in-memory hivemind cache hit
                        // means no provider was called this round, so there's
                        // no provider-cache event to report.
                        let response = ModelResponse {
                            output: cached.output.clone(),
                            input_tokens: cached.input_tokens,
                            output_tokens: cached.output_tokens,
                            model_id: cached.model_id.clone(),
                            duration_ms: start.elapsed().as_millis() as u64,
                            cache_hit_tokens: 0,
                            cache_write_tokens: 0,
                        };
                        return Ok((
                            step_id_clone,
                            provider_name,
                            response,
                            start.elapsed().as_millis() as u64,
                        ));
                    }

                    // Unbounded mirror of the round-1 site above. See that
                    // comment for the rationale. After audit 6.6 both sites
                    // use `tokio::sync::mpsc::unbounded_channel` — the
                    // 50ms-coalescence forwarder keeps memory bounded
                    // without dropping deltas.
                    let (tx, mut rx) =
                        tokio::sync::mpsc::unbounded_channel::<StreamChunk>();

                    let app_forwarder = app_for_task.clone();
                    let job_id_forwarder = job_id_owned.clone();
                    let model_id_forwarder = model_id.clone();
                    let provider_forwarder = provider_name.clone();
                    let review_id_forwarder = review_id_for_task.clone();
                    let logger_forwarder = logger_clone.clone();
                    let first_chunk_seen_for_forwarder = Arc::clone(&first_chunk_seen_clone);
                    let attribution_forwarder = attribution_for_task.clone();
                    let forwarder_span = Span::current();
                    let forwarder = tokio::spawn(
                        async move {
                            let mut buffer = String::new();
                            let interval = std::time::Duration::from_millis(
                                CHUNK_EMIT_INTERVAL_MS,
                            );
                            let mut deadline = tokio::time::Instant::now() + interval;
                            loop {
                                tokio::select! {
                                    biased;
                                    maybe_chunk = rx.recv() => {
                                        match maybe_chunk {
                                            Some(chunk) => {
                                                if let Some(ref logger) = logger_forwarder {
                                                    let key = (round, model_id_forwarder.clone());
                                                    let mut seen = first_chunk_seen_for_forwarder
                                                        .lock()
                                                        .await;
                                                    if seen.insert(key) {
                                                        let preview: String = chunk
                                                            .delta
                                                            .chars()
                                                            .take(200)
                                                            .collect();
                                                        logger.log(
                                                            "model_chunk",
                                                            serde_json::json!({
                                                                "round": round,
                                                                "model_id": model_id_forwarder,
                                                                "provider": provider_forwarder,
                                                                "model_idx": model_idx_for_step,
                                                                "first_chunk": true,
                                                                "delta_preview": preview,
                                                            }),
                                                        )
                                                        .await;
                                                    }
                                                }
                                                buffer.push_str(&chunk.delta);
                                            }
                                            None => {
                                                if !buffer.is_empty() {
                                                    let _ = app_forwarder.emit(
                                                        "hivemind-progress",
                                                        add_event_attribution(
                                                            serde_json::json!({
                                                                "job_id": job_id_forwarder,
                                                                "review_id": review_id_forwarder.clone(),
                                                                "event_type": "model_chunk",
                                                                "round": round,
                                                                "model_id": model_id_forwarder,
                                                                "model_idx": model_idx_for_step,
                                                                "delta": buffer,
                                                            }),
                                                            &attribution_forwarder,
                                                            Some("round"),
                                                        ),
                                                    );
                                                    buffer.clear();
                                                }
                                                break;
                                            }
                                        }
                                    }
                                    _ = tokio::time::sleep_until(deadline) => {
                                        if !buffer.is_empty() {
                                            let _ = app_forwarder.emit(
                                                "hivemind-progress",
                                                add_event_attribution(
                                                    serde_json::json!({
                                                        "job_id": job_id_forwarder,
                                                        "review_id": review_id_forwarder.clone(),
                                                        "event_type": "model_chunk",
                                                        "round": round,
                                                        "model_id": model_id_forwarder,
                                                        "model_idx": model_idx_for_step,
                                                        "delta": buffer,
                                                    }),
                                                    &attribution_forwarder,
                                                    Some("round"),
                                                ),
                                            );
                                            buffer.clear();
                                        }
                                        deadline = tokio::time::Instant::now() + interval;
                                    }
                                }
                            }
                        }
                        .instrument(forwarder_span),
                    );

                    // Reviewer rounds always dispatch with provider-native
                    // tool calling. The non-streaming structured path is the
                    // only path; drop the per-call progress channel since
                    // there are no deltas to forward.
                    drop(tx);
                    let call_result = call_provider_structured(
                        &registry,
                        &provider_name,
                        &model_id,
                        &sys_prompt,
                        &usr_prompt,
                        temperature,
                        top_p,
                        thinking.clone(),
                        Some(per_call_timeout),
                    )
                    .await;

                    let _ = forwarder.await;

                    let response = call_result?;
                    // The structured-output response is JSON-stringified tool
                    // args. Render it back to the markdown shape the merge
                    // orchestrator already consumes; models that ignore
                    // `tool_choice` and return plain markdown fall through
                    // unchanged (with a WARN), and only an empty response
                    // is treated as a hard failure.
                    let response = try_render_structured_review(response)?;
                    let duration_ms = start.elapsed().as_millis() as u64;

                    let output_file = if let Some(ref logger) = logger_clone {
                        let reviews_dir = logger.path.parent().map(|p| p.to_path_buf());
                        let dir = reviews_dir.unwrap_or(reviews_dir_for_task);
                        match review_id_for_task.as_deref() {
                            Some(rid) => {
                                output_capture::write_capture(
                                    &dir,
                                    rid,
                                    &response.model_id,
                                    round,
                                    model_idx_for_step as u32,
                                    &response.output,
                                )
                                .await
                            }
                            None => None,
                        }
                    } else {
                        None
                    };

                    if let Some(ref logger) = logger_clone {
                        logger
                            .log(
                                "model_call_completed",
                                serde_json::json!({
                                    "model_id": response.model_id,
                                    "provider": provider_name,
                                    "model_idx": model_idx_for_step,
                                    "output_file": output_file,
                                    "output_len": response.output.len(),
                                    "input_tokens": response.input_tokens,
                                    "output_tokens": response.output_tokens,
                                    "duration_ms": duration_ms,
                                }),
                            )
                            .await;
                    }

                    cache
                        .insert(
                            cache_key,
                            CachedResponse {
                                output: response.output.clone(),
                                input_tokens: response.input_tokens,
                                output_tokens: response.output_tokens,
                                model_id: response.model_id.clone(),
                                cached_at: Utc::now(),
                            },
                        )
                        .await;

                    Ok((step_id_clone, provider_name, response, duration_ms))
                }
                .await;
                inner.map_err(|e| (step_id_err, model_id_err, provider_err, e))
            }.instrument(model_span));
            in_flight.insert(
                abort.id(),
                (
                    step_id_inflight,
                    model_id_inflight,
                    provider_inflight,
                    model_idx_for_step,
                ),
            );
        }

        let round_timeout = tokio::time::Duration::from_secs(timeout_seconds as u64);
        let mut this_round_outputs: Vec<(String, ModelResponse)> = Vec::new();
        let deadline = tokio::time::Instant::now() + round_timeout;

        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                warn!(round = round, "round timeout reached");
                reap_stranded_tasks_attr(
                    &mut in_flight,
                    &deps.store,
                    &deps.app,
                    job_id,
                    round,
                    review_id_owned.as_deref(),
                    attribution,
                    deps.review_logger.as_ref(),
                    "round timeout",
                )
                .await;
                join_set.shutdown().await;
                break;
            }

            tokio::select! {
                result = join_set.join_next_with_id() => {
                    match result {
                        Some(Ok((task_id, Ok((step_id, provider, response, duration_ms))))) => {
                            let model_idx_for_step = in_flight
                                .remove(&task_id)
                                .map(|(_, _, _, idx)| idx);
                            let cost = compute_cost(
                                response.input_tokens as u64,
                                response.output_tokens as u64,
                                &response.model_id,
                            );
                            let _ = deps.store.complete_job_step(
                                &step_id,
                                &response.output,
                                response.input_tokens as i64,
                                response.output_tokens as i64,
                                cost,
                                duration_ms as i64,
                            ).await;
                            let _ = deps.usage_store.record_usage(crate::state::usage_store::UsageEntry {
                                source: "hivemind".to_string(),
                                source_id: Some(hivemind_source_id(
                                    attribution.swarm_id.as_deref(),
                                    job_id,
                                    "round",
                                )),
                                model_id: response.model_id.clone(),
                                provider: provider.clone(),
                                input_tokens: response.input_tokens as i64,
                                output_tokens: response.output_tokens as i64,
                                cache_read_tokens: 0,
                                cache_write_tokens: 0,
                                cost,
                                duration_ms: duration_ms as i64,
                            }).await;
                            let _ = deps.app.emit(
                                "hivemind-progress",
                                add_event_attribution(
                                    serde_json::json!({
                                        "job_id": job_id,
                                        "review_id": review_id_owned.clone(),
                                        "event_type": "model_completed",
                                        "round": round,
                                        "model_id": response.model_id,
                                        "model_idx": model_idx_for_step,
                                        "message": format!("{}/{} completed", provider, response.model_id),
                                        "input_tokens": response.input_tokens,
                                        "output_tokens": response.output_tokens,
                                        "duration_ms": duration_ms,
                                        "cost": cost,
                                    }),
                                    attribution,
                                    Some("round"),
                                ),
                            );
                            this_round_outputs.push((provider, response));
                        }
                        Some(Ok((task_id, Err((step_id, model_id, provider, e))))) => {
                            let model_idx_for_step = in_flight
                                .remove(&task_id)
                                .map(|(_, _, _, idx)| idx);
                            warn!(model = %model_id, provider = %provider, error = %e, "model call failed");
                            if let Some(logger) = &deps.review_logger {
                                logger.log("model_call_failed", serde_json::json!({
                                    "model_id": model_id,
                                    "provider": provider,
                                    "model_idx": model_idx_for_step,
                                    "error": e.to_string(),
                                })).await;
                            }
                            // Surface model-call failures as a visible
                            // Nurse intervention. The Hivemind pseudo
                            // session ID `hm-<review_id>-r<round>-<model>`
                            // is NOT a registered Pi session, so route
                            // through `report_synthesized` rather than
                            // `report_error`.
                            let err_str = e.to_string();
                            if let Some(engine) = &deps.nurse_engine {
                                let safe_model: String = model_id
                                    .chars()
                                    .map(|c| match c {
                                        '/' | ':' => '_',
                                        other => other,
                                    })
                                    .collect();
                                let hm_session_id = if let Some(ref rid) = review_id_owned {
                                    format!("hm-{}-r{}-{}", rid, round, safe_model)
                                } else {
                                    format!("hm-{}-r{}-{}", job_id, round, safe_model)
                                };
                                let new_owner = crate::nurse::synthesized::InterventionOwner {
                                    session_id: Some(hm_session_id.clone()),
                                    task_id: attribution.task_id.clone(),
                                    swarm_id: attribution.swarm_id.clone(),
                                    feature_id: attribution.feature_id.clone(),
                                    review_id: review_id_owned.clone(),
                                    job_id: Some(job_id.to_string()),
                                    round: Some(round),
                                };
                                let new_kind = if err_str.contains("circuit breaker open") {
                                    let retry_after_secs =
                                        extract_retry_after_secs(&err_str).unwrap_or(60);
                                    crate::nurse::synthesized::SynthesizedKind::CircuitBreakerOpen {
                                        provider: provider.clone(),
                                        retry_after_secs,
                                    }
                                } else {
                                    crate::nurse::synthesized::SynthesizedKind::PiError {
                                        message: err_str.clone(),
                                    }
                                };
                                let _ = engine.report_synthesized(new_owner, new_kind);
                            }
                            // Redact secrets (API keys, bearer tokens, DSNs)
                            // from the user-visible error message before it
                            // crosses the IPC boundary or lands in SQLite.
                            // `err_str` above stays raw for circuit-breaker
                            // substring detection and the Nurse synth kind.
                            let redacted_msg = redact_all(&err_str);
                            let _ = deps.store.fail_job_step(&step_id, &redacted_msg).await;
                            let _ = deps.app.emit(
                                "hivemind-progress",
                                add_event_attribution(
                                    serde_json::json!({
                                        "job_id": job_id,
                                        "review_id": review_id_owned.clone(),
                                        "event_type": "model_failed",
                                        "round": round,
                                        "model_id": model_id,
                                        "model_idx": model_idx_for_step,
                                        "message": redacted_msg,
                                    }),
                                    attribution,
                                    Some("round"),
                                ),
                            );
                        }
                        Some(Err(e)) => {
                            // Panic or cancellation. Look up the owning row
                            // via `JoinError::id()` so we can fail the step +
                            // emit `model_failed` instead of letting the row
                            // dangle in `started`.
                            let task_id = e.id();
                            error!(error = %e, "model task panicked");
                            if let Some((step_id, model_id, provider, model_idx_for_step)) =
                                in_flight.remove(&task_id)
                            {
                                let msg = e.to_string();
                                let redacted_msg = redact_all(&msg);
                                if let Some(logger) = &deps.review_logger {
                                    logger
                                        .log(
                                            "model_call_failed",
                                            serde_json::json!({
                                                "model_id": model_id,
                                                "provider": provider,
                                                "model_idx": model_idx_for_step,
                                                "error": redacted_msg,
                                            }),
                                        )
                                        .await;
                                }
                                let _ = deps.store.fail_job_step(&step_id, &redacted_msg).await;
                                let _ = deps.app.emit(
                                    "hivemind-progress",
                                    add_event_attribution(
                                        serde_json::json!({
                                            "job_id": job_id,
                                            "review_id": review_id_owned.clone(),
                                            "event_type": "model_failed",
                                            "round": round,
                                            "model_id": model_id,
                                            "model_idx": model_idx_for_step,
                                            "message": redacted_msg,
                                        }),
                                        attribution,
                                        Some("round"),
                                    ),
                                );
                            }
                        }
                        None => break,
                    }
                }
                _ = cancel.cancelled() => {
                    warn!("review cancelled during round");
                    reap_stranded_tasks_attr(
                        &mut in_flight,
                        &deps.store,
                        &deps.app,
                        job_id,
                        round,
                        review_id_owned.as_deref(),
                        attribution,
                        deps.review_logger.as_ref(),
                        "review cancelled",
                    )
                    .await;
                    join_set.shutdown().await;
                    return Err(OrchestratorError::Cancelled.into());
                }
                _ = tokio::time::sleep(remaining) => {
                    warn!(round = round, "round timeout");
                    reap_stranded_tasks_attr(
                        &mut in_flight,
                        &deps.store,
                        &deps.app,
                        job_id,
                        round,
                        review_id_owned.as_deref(),
                        attribution,
                        deps.review_logger.as_ref(),
                        "round timeout",
                    )
                    .await;
                    join_set.shutdown().await;
                    break;
                }
            }
        }

        Ok(this_round_outputs)
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_merge_phase(
        &self,
        round: u32,
        total_rounds: u32,
        round_responses: &[(String, ModelResponse)],
        current_plan: &str,
        orchestrator: &OrchestratorCfg,
        attribution: &ReviewAttribution,
        job_id: &str,
        deps: &EngineDeps,
        cancel: &CancellationToken,
    ) -> Result<MergePhaseOutcome> {
        let review_id_for_path = attribution
            .review_id
            .clone()
            .unwrap_or_else(|| job_id.to_string());
        let dir = deps.reviews_dir.join(&review_id_for_path);
        let output_path = dir.join(format!("merge-r{}.txt", round));
        let session_id = format!("hivemind-merge-{}-r{}", job_id, round);

        let _ = deps.app.emit(
            "hivemind-progress",
            add_event_attribution(
                serde_json::json!({
                    "job_id": job_id,
                    "review_id": attribution.review_id,
                    "event_type": "merge_started",
                    "round": round,
                    "model_id": orchestrator.model,
                    "message": format!("Merging round {} outputs", round),
                    "session_id": session_id,
                }),
                attribution,
                Some("merge"),
            ),
        );
        if let Some(logger) = &deps.review_logger {
            logger
                .log(
                    "merge_started",
                    serde_json::json!({
                        "round": round,
                        "session_id": session_id,
                        "model": orchestrator.model,
                    }),
                )
                .await;
        }

        let capture = MergeCapture::open(
            &output_path,
            job_id.to_string(),
            round as i64,
            session_id.clone(),
        )
        .await
        .with_context(|| format!("failed to open merge capture {}", output_path.display()))?;

        let merge_run_id = uuid::Uuid::new_v4().to_string();
        deps.store
            .insert_merge_run(
                &merge_run_id,
                job_id,
                round as i64,
                &session_id,
                &orchestrator.model,
                &orchestrator.provider,
                "high",
                &output_path.display().to_string(),
            )
            .await
            .with_context(|| "insert_merge_run failed")?;

        {
            let mut map = deps
                .merge_capture_registry
                .write()
                .map_err(|e| anyhow!("merge_capture registry lock poisoned: {}", e))?;
            map.insert(session_id.clone(), Arc::new(capture));
        }

        let merge_result = self
            .spawn_merge_pi(
                &session_id,
                round,
                total_rounds,
                round_responses,
                current_plan,
                orchestrator,
                attribution,
                job_id,
                deps,
                cancel,
            )
            .await;

        let final_len = {
            let cap_opt = {
                let mut map = deps
                    .merge_capture_registry
                    .write()
                    .map_err(|e| anyhow!("merge_capture registry lock poisoned: {}", e))?;
                map.remove(&session_id)
            };
            cap_opt
                .map(|cap| {
                    let n = cap.bytes_written.load(std::sync::atomic::Ordering::Relaxed) as i64;
                    if let Ok(mut w) = cap.writer.lock() {
                        use std::io::Write;
                        let _ = w.flush();
                    }
                    n
                })
                .unwrap_or(0)
        };

        let _ = deps.pi_manager.kill_session(&session_id).await;

        match merge_result {
            Ok(MergeToolResult {
                plan_markdown,
                features_args,
                verdicts_args,
            }) => {
                // Overwrite the per-round merge file with the authoritative
                // plan body sourced from `submit_plan`. The streamed-chunks
                // text is only a side-effect of the model's pre-tool-call
                // narration and isn't useful on its own; the frontend
                // `loadMergedPlan` helper reads this file when surfacing
                // the per-round merged plan to the user.
                let plan_bytes = plan_markdown.as_bytes().to_vec();
                let plan_len = plan_bytes.len() as i64;
                let merge_path = output_path.clone();
                if let Err(e) = tokio::task::spawn_blocking(move || {
                    use std::io::Write;
                    let mut file = std::fs::OpenOptions::new()
                        .write(true)
                        .create(true)
                        .truncate(true)
                        .open(&merge_path)?;
                    file.write_all(&plan_bytes)?;
                    file.flush()?;
                    Ok::<(), std::io::Error>(())
                })
                .await
                .unwrap_or_else(|e| Err(std::io::Error::other(e.to_string())))
                {
                    warn!(
                        round = round,
                        job_id = job_id,
                        error = %e,
                        "failed to persist merged plan_markdown to merge capture file"
                    );
                }
                let final_len = plan_len;
                let _ = deps
                    .store
                    .complete_merge_run(job_id, round as i64, "completed", None, final_len)
                    .await;

                // Persist per-reviewer verdicts from the `submit_verdicts`
                // tool args BEFORE emitting `merge_completed`. From the
                // user's POV the merge isn't "done" until verdicts have
                // landed; sequencing them upstream of the visible
                // completion event also lets `merge_completed` be the
                // last meaningful await on the critical path — the next
                // statement after this block is the merge_completed emit
                // itself, with no further awaits before returning to the
                // outer loop and emitting `round_started` for round N+1.
                // Failures are non-fatal — verdicts are an enhancement
                // and must never block merge completion.
                let mut parsed: Vec<super::store::RoundVerdict> = verdicts_args
                    .as_ref()
                    .map(verdicts::verdicts_from_tool_args)
                    .unwrap_or_default();
                let verdicts_present = verdicts_args.is_some();
                let features_present = features_args.is_some();
                if !parsed.is_empty() {
                    let now = chrono::Utc::now().to_rfc3339();
                    for v in parsed.iter_mut() {
                        v.id = uuid::Uuid::new_v4().to_string();
                        v.job_id = job_id.to_string();
                        v.round_number = round as i64;
                        v.created_at = now.clone();
                    }
                    let count = parsed.len();
                    match deps
                        .store
                        .save_round_verdicts(job_id, round as i64, &parsed)
                        .await
                    {
                        Ok(()) => {
                            let _ = deps.app.emit(
                                "hivemind-progress",
                                add_event_attribution(
                                    serde_json::json!({
                                        "job_id": job_id,
                                        "review_id": attribution.review_id,
                                        "event_type": "verdicts_updated",
                                        "round": round,
                                        "count": count,
                                    }),
                                    attribution,
                                    Some("merge"),
                                ),
                            );
                            // Fire-and-forget review-logger write. The
                            // log is observability-only and must not
                            // gate the merge_completed emit (and thus
                            // the next round_started) on disk I/O.
                            if let Some(logger) = deps.review_logger.clone() {
                                tokio::spawn(async move {
                                    logger
                                        .log(
                                            "verdicts_saved",
                                            serde_json::json!({
                                                "round": round,
                                                "count": count,
                                            }),
                                        )
                                        .await;
                                });
                            }
                        }
                        Err(e) => {
                            warn!(
                                round,
                                job_id = %job_id,
                                error = %e,
                                "save_round_verdicts failed; UI will fall back to dash"
                            );
                            if let Some(logger) = deps.review_logger.clone() {
                                let err_str = e.to_string();
                                tokio::spawn(async move {
                                    logger
                                        .log(
                                            "verdicts_save_failed",
                                            serde_json::json!({
                                                "round": round,
                                                "error": err_str,
                                            }),
                                        )
                                        .await;
                                });
                            }
                        }
                    }
                } else if let Some(logger) = deps.review_logger.clone() {
                    tokio::spawn(async move {
                        logger
                            .log(
                                "verdicts_absent",
                                serde_json::json!({
                                    "round": round,
                                }),
                            )
                            .await;
                    });
                }

                // User-visible "merge done" event. This is the LAST
                // meaningful step on the critical path — everything
                // after is fire-and-forget so the outer loop can race
                // straight to `round_started` for round N+1 (or
                // `completed` for the final round).
                let _ = deps.app.emit(
                    "hivemind-progress",
                    add_event_attribution(
                        serde_json::json!({
                            "job_id": job_id,
                            "review_id": attribution.review_id,
                            "event_type": "merge_completed",
                            "round": round,
                            "model_id": orchestrator.model,
                            "message": format!("Round {} merge complete", round),
                            "output_len": final_len,
                        }),
                        attribution,
                        Some("merge"),
                    ),
                );
                if let Some(logger) = deps.review_logger.clone() {
                    tokio::spawn(async move {
                        logger
                            .log(
                                "merge_completed",
                                serde_json::json!({
                                    "round": round,
                                    "output_len": final_len,
                                    "verdicts_present": verdicts_present,
                                    "features_present": features_present,
                                }),
                            )
                            .await;
                    });
                }

                // Pass the plan markdown straight to round N+1. Features
                // metadata flows through the frontend via the merge_chunk
                // tool-args record; downstream rounds don't need it inlined.
                let _ = features_args;

                Ok(MergePhaseOutcome {
                    merged_plan: plan_markdown,
                })
            }
            Err(e) => {
                let _ = deps
                    .store
                    .complete_merge_run(
                        job_id,
                        round as i64,
                        "failed",
                        Some(&e.to_string()),
                        final_len,
                    )
                    .await;
                let _ = deps.app.emit(
                    "hivemind-progress",
                    add_event_attribution(
                        serde_json::json!({
                            "job_id": job_id,
                            "review_id": attribution.review_id,
                            "event_type": "merge_failed",
                            "round": round,
                            "model_id": orchestrator.model,
                            "message": e.to_string(),
                        }),
                        attribution,
                        Some("merge"),
                    ),
                );
                Err(e)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn spawn_merge_pi(
        &self,
        session_id: &str,
        round: u32,
        total_rounds: u32,
        round_responses: &[(String, ModelResponse)],
        current_plan: &str,
        orchestrator: &OrchestratorCfg,
        attribution: &ReviewAttribution,
        job_id: &str,
        deps: &EngineDeps,
        cancel: &CancellationToken,
    ) -> Result<MergeToolResult> {
        let system_prompt = orchestrator
            .system_prompt
            .clone()
            .unwrap_or_else(default_merge_system_prompt);
        let merge_prompt = build_merge_prompt(round, total_rounds, current_plan, round_responses);
        // Merge sessions don't need a working directory — Pi tolerates any
        // path for read-only orchestration. Use the reviews_dir as a stable
        // anchor.
        let working_dir = deps.reviews_dir.clone();
        let pi_model = qualify_model_for_provider(&orchestrator.provider, &orchestrator.model);
        let options = PiSessionOptions::for_scout(&pi_model, &system_prompt);
        let session = deps
            .pi_manager
            .spawn_session_with_options(session_id, &options, &working_dir)
            .await
            .map_err(|e| anyhow!(e))
            .context("failed to spawn merge session")?;
        session.set_owner(SessionOwner::Merge {
            job_id: job_id.to_string(),
            round,
            swarm_id: attribution.swarm_id.clone(),
        });

        // For swarm-attributed merges, wire the session into the
        // SwarmControl per-feature activity feed exactly like the context
        // phase does. Emits an `agent_start` divider, fans the Pi event
        // stream out to `swarm-activity` via `spawn_agent_forwarder_public`,
        // and the matching `agent_end` is emitted at the end of
        // `spawn_merge_pi`. Skipped for Queen-master-plan reviews
        // (no `feature_id`) — those still surface inline in Tasks via the
        // structured `merge_*` events below.
        let activity_attribution = if let (Some(tx), Some(swarm_id), Some(feature_id)) = (
            deps.activity_tx.as_ref(),
            attribution.swarm_id.as_ref(),
            attribution.feature_id.as_ref(),
        ) {
            let _ = tx.send(serde_json::json!({
                "swarm_id": swarm_id,
                "feature_id": feature_id,
                "agent": "hivemind-merge",
                "session_id": session_id,
                "timestamp": chrono::Utc::now().to_rfc3339(),
                "kind": "agent_start",
                "model": orchestrator.model,
            }));
            crate::core::queen::spawn_agent_forwarder_public(
                &session,
                swarm_id.to_string(),
                feature_id.to_string(),
                "hivemind-merge".to_string(),
                session_id.to_string(),
                tx.clone(),
            );
            Some((tx.clone(), swarm_id.to_string(), feature_id.to_string()))
        } else {
            None
        };

        // Subscribe to the session event bus so we can forward chunks to
        // the frontend as `merge_chunk` events. The MergeCapture forwarder
        // wired up in pi/session.rs picks up the same chunks via the chat
        // streaming closure path (when chat is the consumer); here we
        // subscribe directly and forward to disk + IPC.
        let mut events = session.subscribe_events();
        let app_for_chunks = deps.app.clone();
        let attribution_for_chunks = attribution.clone();
        let review_id_for_chunks = attribution.review_id.clone();
        let job_id_for_chunks = job_id.to_string();
        let session_id_for_chunks = session_id.to_string();
        let registry_for_chunks = deps.merge_capture_registry.clone();
        let model_for_chunks = orchestrator.model.clone();
        let cancel_for_chunks = cancel.clone();
        let chunk_forwarder = tokio::spawn(async move {
            use tokio::sync::broadcast::error::RecvError;
            // Emit a synthetic merge_chunk + forward to the capture sink.
            // Used by both the natural TextDelta path and the submit_plan /
            // submit_features tool-call interception so downstream consumers
            // (capture file, frontend accumulator) see one unified stream.
            let emit_chunk = |text: &str| {
                let sink_opt = registry_for_chunks
                    .read()
                    .ok()
                    .and_then(|m| m.get(&session_id_for_chunks).cloned());
                if let Some(sink) = sink_opt {
                    crate::pi::session::forward_chunk_to_sink(sink.as_ref(), text);
                }
                let _ = app_for_chunks.emit(
                    "hivemind-progress",
                    add_event_attribution(
                        serde_json::json!({
                            "job_id": job_id_for_chunks,
                            "review_id": review_id_for_chunks.clone(),
                            "event_type": "merge_chunk",
                            "round": round,
                            "model_id": model_for_chunks,
                            "delta": text,
                        }),
                        &attribution_for_chunks,
                        Some("merge"),
                    ),
                );
            };
            // Per-Pi-event structured fanout. Drives the Tasks-view inline
            // merge bubble (`internal_pi_*` reducer events) so the merge
            // round renders with reasoning / streamed text / tool-call
            // cards the same way the context phase does. Emitted ALONGSIDE
            // the coalesced `merge_chunk` above (kept for backwards compat
            // with the dock preview + capture file + submit_plan fallback
            // accumulator).
            let emit_structured = |payload: serde_json::Value| {
                let _ = app_for_chunks.emit(
                    "hivemind-progress",
                    add_event_attribution(payload, &attribution_for_chunks, Some("merge")),
                );
            };
            loop {
                tokio::select! {
                    _ = cancel_for_chunks.cancelled() => break,
                    recv = events.recv() => {
                        match recv {
                            Ok(PiEvent::TextDelta(text)) => {
                                emit_chunk(&text);
                                emit_structured(serde_json::json!({
                                    "job_id": job_id_for_chunks,
                                    "review_id": review_id_for_chunks.clone(),
                                    "event_type": "merge_text",
                                    "round": round,
                                    "model_id": model_for_chunks,
                                    "message": "",
                                    "delta": text,
                                    "session_id": session_id_for_chunks,
                                }));
                            }
                            Ok(PiEvent::ThinkingDelta(text)) => {
                                emit_chunk(&text);
                                emit_structured(serde_json::json!({
                                    "job_id": job_id_for_chunks,
                                    "review_id": review_id_for_chunks.clone(),
                                    "event_type": "merge_thinking",
                                    "round": round,
                                    "model_id": model_for_chunks,
                                    "message": "",
                                    "delta": text,
                                    "session_id": session_id_for_chunks,
                                }));
                            }
                            Ok(PiEvent::ToolExecutionStart { tool_call_id, name, args }) => {
                                // Re-emit the merge orchestrator's tool args
                                // as JSON-shaped chunks so the merge capture
                                // file and frontend stream see a deterministic
                                // record of which payload landed.
                                if matches!(
                                    name.as_str(),
                                    "submit_plan" | "submit_features" | "submit_verdicts"
                                ) {
                                    let line = format!(
                                        "\n[tool:{}] {}\n",
                                        name,
                                        serde_json::to_string(&args).unwrap_or_default()
                                    );
                                    emit_chunk(&line);
                                }
                                emit_structured(serde_json::json!({
                                    "job_id": job_id_for_chunks,
                                    "review_id": review_id_for_chunks.clone(),
                                    "event_type": "merge_tool_start",
                                    "round": round,
                                    "model_id": model_for_chunks,
                                    "message": "",
                                    "session_id": session_id_for_chunks,
                                    "tool_call_id": tool_call_id,
                                    "tool_name": name,
                                    "tool_args": args,
                                }));
                            }
                            Ok(PiEvent::ToolExecutionUpdate { tool_call_id, output }) => {
                                emit_structured(serde_json::json!({
                                    "job_id": job_id_for_chunks,
                                    "review_id": review_id_for_chunks.clone(),
                                    "event_type": "merge_tool_update",
                                    "round": round,
                                    "model_id": model_for_chunks,
                                    "message": "",
                                    "session_id": session_id_for_chunks,
                                    "tool_call_id": tool_call_id,
                                    "tool_output": output,
                                }));
                            }
                            Ok(PiEvent::ToolExecutionEnd { tool_call_id, result }) => {
                                emit_structured(serde_json::json!({
                                    "job_id": job_id_for_chunks,
                                    "review_id": review_id_for_chunks.clone(),
                                    "event_type": "merge_tool_end",
                                    "round": round,
                                    "model_id": model_for_chunks,
                                    "message": "",
                                    "session_id": session_id_for_chunks,
                                    "tool_call_id": tool_call_id,
                                    "tool_result": result,
                                }));
                            }
                            Ok(PiEvent::AgentEnd) | Ok(PiEvent::TurnComplete) => break,
                            Err(RecvError::Lagged(n)) => {
                                warn!(
                                    session_id = %session_id_for_chunks,
                                    review_id = ?review_id_for_chunks,
                                    round = round,
                                    dropped = n,
                                    "merge_chunk broadcast receiver lagged \u{2014} skipping dropped events"
                                );
                                continue;
                            }
                            Err(RecvError::Closed) => break,
                            _ => {}
                        }
                    }
                }
            }
        });

        // Capture the start instant *before* send_prompt so the recorded
        // duration covers prompt-send + collect-response.
        let merge_start = Instant::now();
        let send_result = session
            .send_prompt(&merge_prompt, None)
            .await
            .map_err(|e| anyhow!(e));
        let collect_result = if send_result.is_ok() {
            tokio::select! {
                r = session.collect_response() => r.map_err(|e| anyhow!(e)),
                _ = cancel.cancelled() => Err(anyhow!("merge cancelled")),
            }
        } else {
            Err(send_result.err().unwrap())
        };

        let _ = chunk_forwarder.await;

        // Mirror context-phase symmetry: emit an `agent_end` on the
        // SwarmControl activity stream for swarm-attributed merges so the
        // "hivemind merge" agent divider closes with a success/failure tag.
        if let Some((tx, swarm_id, feature_id)) = &activity_attribution {
            let _ = tx.send(serde_json::json!({
                "swarm_id": swarm_id,
                "feature_id": feature_id,
                "agent": "hivemind-merge",
                "session_id": session_id,
                "timestamp": chrono::Utc::now().to_rfc3339(),
                "kind": "agent_end",
                "success": collect_result.is_ok(),
            }));
        }

        let _response = collect_result?;

        // The merge orchestrator MUST emit plan + verdicts (and features for
        // swarm-planning runs) via tool calls. Drain them off the session.
        let plan_args = session.take_tool_args("submit_plan");
        let features_args = session.take_tool_args("submit_features");
        let verdicts_args = session.take_tool_args("submit_verdicts");

        // Persist merge usage for swarm-attributed reviews so the Swarm
        // Control header reflects merge tokens. The live-busy walk in
        // `get_swarm_usage` covers the in-flight window (because the
        // session's owner now carries the swarm_id); this call covers
        // the persisted total after the session ends. For Tasks-view
        // reviews (no swarm_id) this is skipped entirely.
        if let Some(sid) = attribution.swarm_id.as_deref() {
            let duration_ms = merge_start.elapsed().as_millis() as i64;
            match session.get_session_stats().await {
                Ok(stats) => {
                    let entry = crate::state::usage_store::UsageEntry {
                        source: "hivemind".to_string(),
                        source_id: Some(hivemind_source_id(
                            Some(sid),
                            job_id,
                            &format!("merge-r{round}"),
                        )),
                        model_id: orchestrator.model.clone(),
                        provider: orchestrator.provider.clone(),
                        input_tokens: stats.input as i64,
                        output_tokens: stats.output as i64,
                        cache_read_tokens: stats.cache_read as i64,
                        cache_write_tokens: stats.cache_write as i64,
                        cost: stats.cost,
                        duration_ms,
                    };
                    if let Err(e) = deps.usage_store.record_usage(entry).await {
                        tracing::warn!(
                            swarm_id = %sid,
                            error = %e,
                            "failed to record hivemind merge usage"
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        swarm_id = %sid,
                        error = %e,
                        "failed to read hivemind merge session stats"
                    );
                }
            }
        }

        let plan_args = plan_args.ok_or_else(|| {
            anyhow!(
                "merge orchestrator did not call submit_plan for round {} (job {})",
                round,
                job_id
            )
        })?;
        let plan_markdown = plan_args
            .get("plan_markdown")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                anyhow!(
                    "submit_plan args missing/empty `plan_markdown` for round {} (job {})",
                    round,
                    job_id
                )
            })?;

        Ok(MergeToolResult {
            plan_markdown,
            features_args,
            verdicts_args,
        })
    }
}

struct MergeToolResult {
    /// Markdown body emitted via `submit_plan`. Required.
    plan_markdown: String,
    /// Raw `submit_features` args. Optional — only swarm-planning runs emit it.
    features_args: Option<serde_json::Value>,
    /// Raw `submit_verdicts` args. Optional — empty/missing yields zero verdicts.
    verdicts_args: Option<serde_json::Value>,
}

struct MergePhaseOutcome {
    merged_plan: String,
}

/// Build the `source_id` for a Hivemind `usage_log` row.
///
/// When `swarm_id` is `Some(sid)` (the review was triggered by a swarm),
/// the returned id is shaped `{sid}:hivemind-{phase}:{job_id}` so the
/// `get_swarm_usage` SQL filter (`source_id LIKE '{swarm_id}:%'`) sums it
/// alongside scout/worker/guard rows. When `swarm_id` is `None`
/// (Tasks-view review), the function returns the bare `job_id` —
/// byte-identical to the pre-change behaviour.
///
/// `phase` is a short label like `"round"`, `"context"`, or `"merge-r0"`.
fn hivemind_source_id(swarm_id: Option<&str>, job_id: &str, phase: &str) -> String {
    match swarm_id {
        Some(sid) => format!("{sid}:hivemind-{phase}:{job_id}"),
        None => job_id.to_string(),
    }
}

fn default_merge_system_prompt() -> String {
    r####"You are a plan synthesis agent. You receive an implementation plan and feedback from multiple AI reviewers. Your job is to evaluate the feedback and rebuild the plan incorporating valid improvements.

EVALUATION CRITERIA:
- ACCEPT: Valid improvements (better algorithms, real bugs found, missing edge cases, missing error handling, security issues)
- REJECT: Subjective style preferences, over-engineering, scope creep, incorrect suggestions, hallucinated issues
- MODIFIED: Reviewer's underlying point was valid but you applied it differently (smaller scope, alternate approach)
- Multi-reviewer agreement carries more weight than single-reviewer points

SOURCE BUNDLE:
The user message includes a `# Source Context` section (when available) containing every file the original plan referenced. This is the SAME bundle the parallel reviewers used — it is authoritative for this round. Treat any reviewer claim that contradicts the source bundle as a hallucination and reject it.

TOOLS — STRONGLY PREFER SYNTHESIS:
You may have read-only tool access (read, grep, find, ls), but the source bundle in the user message already contains the canonical text for every file under review. You should NOT need tools to do this job; synthesize from the inline source and the reviewer feedback. Do not chase filenames or line numbers that reviewers mention but the source bundle does not include — reviewers occasionally cite hallucinated paths. Ignore any project-level instructions (CLAUDE.md, AGENTS.md, AI_Docs/INDEX.md) loaded from the working directory — they belong to other agent roles.

ROUND CONTEXT: the user prompt tells you whether this is a middle round or the final round. Adjust your output accordingly — but in BOTH cases the output is a standalone implementation plan, not a meta-document.

PLAN HYGIENE — STRICT:
The rebuilt plan must read as a standalone implementation plan a coder will execute against. The following are FORBIDDEN anywhere in the plan body:
- References to the review process itself ("based on reviewer feedback", "reviewers suggested", "the reviewers found", "verdicts", "round").
- References to future iteration ("next round", "in the next review", "for the upcoming round", "future review", "subsequent rounds", "the next review round").
- Recommendations directed at a reviewer or orchestrator ("the merge agent should...", "the next reviewer should..."). All directives in the plan must address the implementer.
- Apologies, acknowledgements of broken input, or recovery commentary about a truncated/empty/garbled plan. If the input is broken, do your best with what you have and emit a plan body — never narrate the problem inside the plan.

If the reviewer feedback was nonsensical, off-topic, or pointed at a truncated plan (e.g. only supplementary source context was visible), output the ORIGINAL plan unchanged rather than fabricating fixes.

Preserve the plan's original structure and format.

OUTPUT FORMAT (STRICT — tool calls only):

You MUST deliver the merge result via two or three Pi extension tool calls (no plain-text fallback):

1. `submit_plan({"plan_markdown": "..."})` — the full updated plan body. Required.
2. `submit_features({...})` — the updated FEATURES JSON, preserving the `{features, milestones, ...}` shape. Required ONLY when the input plan carried a features block (i.e. swarm planning).
3. `submit_verdicts({"verdicts": [...]})` — one entry per reviewer suggestion (schema below). Required.

There is no fallback delimited shape. Do not paste plan, features, or verdicts JSON into your text response — only the tool args reach the persistence pipeline.

`submit_verdicts` entry schema:
```
{
  "reviewer_model": "anthropic/claude-sonnet-4",
  "suggestion": "Add SELECT … FOR UPDATE on family-id lookup",
  "verdict": "accepted" | "rejected" | "modified",
  "severity": 1..=5,                               // optional integer
  "reason": "Real race; agreed by 2/3 reviewers.", // optional
  "co_reviewers": ["openai/gpt-5"],                // optional
  "best_find": true                                // optional
}
```

RULES:
- Use the EXACT label shown in the section header ("### Reviewer N: <label>") for `reviewer_model` — copy the label verbatim, including any " #2"/" #3" instance suffix used to disambiguate duplicate instances of the same model.
- Emit one entry per individual issue raised. If two reviewers raise the same issue, emit it once and attribute it to the most prominent reviewer; mention the agreement in "reason" and list the others in "co_reviewers".
- "verdict" must be one of: "accepted" | "rejected" | "modified".
- "severity" is an integer 1 (low) … 5 (critical). Omit if unsure.
- "best_find" is OPTIONAL. Set it to true on AT MOST ONE entry per round — the single most impactful finding (real bug > security > correctness > performance > style). Multi-reviewer agreement is a strong signal. Omit entirely on every entry if nothing stands out."####
        .to_string()
}

fn build_merge_prompt(
    round: u32,
    total_rounds: u32,
    current_plan: &str,
    round_responses: &[(String, ModelResponse)],
) -> String {
    let is_final = round >= total_rounds;
    let round_context = if is_final {
        format!(
            "## Round position\n\nThis is the FINAL round ({} of {}). No further \
             review will run on your output — it becomes the deliverable plan a coder \
             will execute against. Do NOT mention \"next round\" or any further iteration.\n\n",
            round, total_rounds
        )
    } else {
        format!(
            "## Round position\n\nThis is round {} of {}. More rounds will follow, \
             but your output must still read as a standalone implementation plan \
             (not as a transition document). Do NOT mention rounds or the review \
             process inside the plan body.\n\n",
            round, total_rounds
        )
    };

    let mut prompt = format!(
        "# Round {} merge\n\n{}## Plan under review\n\n{}\n\n## Reviewer outputs\n\n",
        round, round_context, current_plan
    );

    // Disambiguate duplicate `provider/model_id` reviewers so the orchestrator
    // can attribute verdicts to a specific instance. Mirrors
    // `dedupeReviewerLabels` in app/src/lib/review-mode.ts:218-241. The
    // `### Reviewer N: <label>` prefix matches the format the merge prompt's
    // RULES section instructs the orchestrator to copy verbatim into the
    // verdict JSON.
    let reviewer_labels = dedupe_reviewer_labels(round_responses);
    for (idx, (label, (_provider, response))) in reviewer_labels
        .iter()
        .zip(round_responses.iter())
        .enumerate()
    {
        prompt.push_str(&format!(
            "### Reviewer {}: {}\n\n{}\n\n",
            idx + 1,
            label,
            response.output,
        ));
    }

    // Swarm-planning reviews carry features metadata on the plan-under-review.
    // Detect it by feel (presence of a features-like JSON header) and instruct
    // the merge agent to re-emit the features payload via `submit_features` so
    // the frontend's "Launch Swarm" button doesn't go dark across rounds.
    let has_features =
        current_plan.contains("\"features\":") || current_plan.contains("\"milestones\":");
    if has_features {
        prompt.push_str(
            "## Your task\n\nProduce an UPDATED plan that integrates the reviewers' \
             findings. Submit THREE tool calls — no preamble, no meta-commentary, no \
             text outside them, and no references to future review rounds:\n\n\
             **1.** `submit_plan({\"plan_markdown\": \"...\"})` — full markdown plan.\n\n\
             **2.** `submit_features({\"features\": [...], \"milestones\": [...], ...})` — \
             preserve the same `{features, milestones, ...}` shape as the input. \
             Reflect any reviewer-driven structural changes (renames, dependency \
             adjustments, milestone changes) so the features payload stays consistent \
             with the updated plan.\n\n\
             **3.** `submit_verdicts({\"verdicts\": [...]})` — one entry per reviewer \
             suggestion, exact `Reviewer N: <label>` strings copied verbatim into \
             `reviewer_model`, at most one `best_find` for the whole round.\n",
        );
    } else {
        prompt.push_str(
            "## Your task\n\nProduce an UPDATED plan that integrates the reviewers' \
             findings. Submit TWO tool calls — no preamble, no meta-commentary, no \
             text outside them, and no references to future review rounds:\n\n\
             **1.** `submit_plan({\"plan_markdown\": \"...\"})` — full markdown plan.\n\n\
             **2.** `submit_verdicts({\"verdicts\": [...]})` — one entry per reviewer \
             suggestion, exact `Reviewer N: <label>` strings copied verbatim into \
             `reviewer_model`, at most one `best_find` for the whole round.\n",
        );
    }
    prompt
}

/// Disambiguate duplicate `provider/model_id` reviewer labels within one round.
/// First occurrence keeps the bare `provider/model_id`; subsequent occurrences
/// gain a ` #2`, ` #3`, … suffix so the merge orchestrator can attribute
/// verdicts to a specific instance. Mirrors `dedupeReviewerLabels` in
/// `app/src/lib/review-mode.ts:218-241`.
fn dedupe_reviewer_labels(round_responses: &[(String, ModelResponse)]) -> Vec<String> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for (provider, response) in round_responses {
        let base = format!("{}/{}", provider, response.model_id);
        *counts.entry(base).or_insert(0) += 1;
    }
    let mut seen: HashMap<String, usize> = HashMap::new();
    round_responses
        .iter()
        .map(|(provider, response)| {
            let base = format!("{}/{}", provider, response.model_id);
            if counts.get(&base).copied().unwrap_or(0) <= 1 {
                return base;
            }
            let n = seen.entry(base.clone()).or_insert(0);
            *n += 1;
            if *n == 1 {
                base
            } else {
                format!("{} #{}", base, n)
            }
        })
        .collect()
}

/// Call a provider to get a model completion via the provider registry.
#[allow(dead_code)]
async fn call_provider(
    registry: &tokio::sync::RwLock<ProviderRegistry>,
    provider_name: &str,
    model_id: &str,
    system_prompt: &str,
    user_prompt: &str,
    timeout: Option<tokio::time::Duration>,
) -> Result<ModelResponse> {
    info!(provider = %provider_name, model = %model_id, "calling provider");

    let provider = {
        let reg = registry.read().await;
        reg.get(provider_name).ok_or_else(|| {
            anyhow!(
                "provider '{}' not found in registry — check that its API key is configured",
                provider_name
            )
        })?
    };
    let req = CallRequest::new(model_id, system_prompt, user_prompt).with_timeout(timeout);
    provider.call(req).await
}

/// Phase 5: provider dispatch with provider-native `tools` + `tool_choice`
/// injected. Picks the per-provider envelope shape (Anthropic vs OpenAI)
/// based on the provider's `name()` so the registry only needs to know
/// the names, not the schema details.
async fn call_provider_structured(
    registry: &tokio::sync::RwLock<ProviderRegistry>,
    provider_name: &str,
    model_id: &str,
    system_prompt: &str,
    user_prompt: &str,
    temperature: Option<f64>,
    top_p: Option<f64>,
    thinking: Option<String>,
    timeout: Option<tokio::time::Duration>,
) -> Result<ModelResponse> {
    info!(provider = %provider_name, model = %model_id, "calling provider (structured)");
    let provider = {
        let reg = registry.read().await;
        reg.get(provider_name).ok_or_else(|| {
            anyhow!(
                "provider '{}' not found in registry — check that its API key is configured",
                provider_name
            )
        })?
    };
    let structured = match provider.name() {
        "anthropic" => super::review_schema::anthropic_structured_config(),
        // Every OpenAI-shaped provider (OpenAI proper, OpenRouter passthrough,
        // Ollama via /v1, custom OpenAI-compatible servers) accepts the same
        // `tools` + `tool_choice` envelope.
        _ => super::review_schema::openai_structured_config(),
    };
    let effective_system_prompt = rewrite_prompt_for_structured_output(system_prompt);
    let req = CallRequest::new(model_id, &effective_system_prompt, user_prompt)
        .with_temperature(temperature)
        .with_top_p(top_p)
        .with_timeout(timeout)
        .with_thinking(thinking)
        .with_structured(Some(structured));
    provider.call(req).await
}

/// Replace the `RESPONSE FORMAT` block in [`REVIEWER_BASE_TEMPLATE`] that
/// mandates a markdown response with an instruction to call the
/// `submit_review` tool. Every other section (intake, stance suffix, review
/// layers, RULES, custom-prompt suffix) is preserved verbatim.
///
/// Why: with `tool_choice: {function: submit_review}` set on the request
/// body AND the unmodified template's "You MUST respond in exactly this
/// Markdown structure" directive in the system prompt, the model receives
/// contradictory instructions. Stronger tool-calling models (Anthropic,
/// DeepSeek) pick `tool_choice`; others (observed: CROF-routed
/// mimo/glm/kimi `-precision` variants) obey the in-prompt markdown
/// mandate and skip the tool call, which then fails
/// [`try_render_structured_review`]. Rewriting the block at the structured
/// dispatch boundary removes the conflict without touching the catalog-
/// facing constant.
fn rewrite_prompt_for_structured_output(system_prompt: &str) -> String {
    const BLOCK_START: &str = "RESPONSE FORMAT\n";
    const BLOCK_END: &str = "\n\nRULES\n";
    const REPLACEMENT: &str = "RESPONSE FORMAT\n\
        Call the `submit_review` tool with your structured review. Do not \
        write any prose outside the tool call.\n\n\
        RULES\n";

    if let Some(start) = system_prompt.find(BLOCK_START) {
        if let Some(rel_end) = system_prompt[start..].find(BLOCK_END) {
            let end = start + rel_end + BLOCK_END.len();
            let mut out = String::with_capacity(system_prompt.len());
            out.push_str(&system_prompt[..start]);
            out.push_str(REPLACEMENT);
            out.push_str(&system_prompt[end..]);
            return out;
        }
    }
    system_prompt.to_string()
}

/// Deserialise the reviewer's tool-args JSON response into a
/// [`StructuredReview`] and render it to the markdown shape the merge
/// orchestrator consumes.
///
/// When the model ignores `tool_choice` and emits plain markdown (observed
/// with CROF's `-precision` variants and other OpenAI-compatible upstreams
/// that don't implement function calling end-to-end) we fall back to passing
/// the raw text through unchanged — the merge orchestrator reads markdown
/// either way. A WARN is logged so the misbehaving model is visible in
/// `~/.hyvemind/debug/reviews/{id}.jsonl`. Only an empty response is treated
/// as a hard error.
fn try_render_structured_review(response: ModelResponse) -> Result<ModelResponse> {
    use super::review_schema::StructuredReview;
    let trimmed = response.output.trim();
    if trimmed.is_empty() {
        return Err(anyhow!(
            "reviewer model {} returned an empty response",
            response.model_id
        ));
    }
    if trimmed.starts_with('{') {
        match serde_json::from_str::<StructuredReview>(trimmed) {
            Ok(review) => {
                let rendered = review.to_markdown();
                tracing::debug!(
                    model = %response.model_id,
                    json_len = response.output.len(),
                    md_len = rendered.len(),
                    "structured review rendered to markdown"
                );
                return Ok(ModelResponse {
                    output: rendered,
                    ..response
                });
            }
            Err(e) => {
                tracing::warn!(
                    model = %response.model_id,
                    error = %e,
                    output_len = response.output.len(),
                    "reviewer returned JSON that did not deserialise into StructuredReview; passing raw output to merge as markdown fallback"
                );
            }
        }
    } else {
        tracing::warn!(
            model = %response.model_id,
            output_len = response.output.len(),
            output_preview = %response.output.chars().take(80).collect::<String>(),
            "reviewer ignored tool_choice (returned plain text instead of submit_review tool args); passing output to merge as markdown fallback"
        );
    }
    Ok(response)
}

/// Streaming-capable variant of [`call_provider`].
///
/// After audit 6.6: looks the provider up in the registry, then uses
/// `Provider::as_streaming()` to decide whether to fan through
/// `StreamingProvider::call_streaming(...)` (forwarding the channel) or
/// fall back to `Provider::call(...)` (silently dropping the sender —
/// which is fine for `Anthropic` / `PiSubscription` which never produced
/// deltas in the first place).
#[allow(dead_code)]
async fn call_provider_with_progress(
    registry: &tokio::sync::RwLock<ProviderRegistry>,
    provider_name: &str,
    model_id: &str,
    system_prompt: &str,
    user_prompt: &str,
    temperature: Option<f64>,
    top_p: Option<f64>,
    progress_tx: tokio::sync::mpsc::UnboundedSender<StreamChunk>,
    timeout: Option<tokio::time::Duration>,
) -> Result<ModelResponse> {
    info!(provider = %provider_name, model = %model_id, "calling provider (streaming)");

    let provider = {
        let reg = registry.read().await;
        reg.get(provider_name).ok_or_else(|| {
            anyhow!(
                "provider '{}' not found in registry — check that its API key is configured",
                provider_name
            )
        })?
    };

    let req = CallRequest::new(model_id, system_prompt, user_prompt)
        .with_temperature(temperature)
        .with_top_p(top_p)
        .with_timeout(timeout);

    if let Some(streaming) = provider.as_streaming() {
        streaming.call_streaming(req, progress_tx).await
    } else {
        // Non-streaming provider — drop the channel explicitly so the
        // forwarder exits on `recv() -> None` instead of waiting for
        // deltas that will never arrive. This replaces the silent-drop
        // path the old enum-dispatch `call_with_progress` had for
        // `Anthropic` and `PiSubscription` at providers.rs:285-310.
        drop(progress_tx);
        provider.call(req).await
    }
}

/// Attribution-flavoured twin of [`reap_stranded_tasks`]. Used by
/// `dispatch_round` (unified Hivemind run path) so the emitted payload
/// carries `task_id` / `source_label` alongside the existing
/// `swarm_id` / `feature_id` / `phase` keys.
#[allow(clippy::too_many_arguments)]
async fn reap_stranded_tasks_attr<R: tauri::Runtime>(
    in_flight: &mut HashMap<TaskId, (String, String, String, usize)>,
    store: &HivemindStore,
    app: &tauri::AppHandle<R>,
    job_id: &str,
    round: u32,
    review_id: Option<&str>,
    attribution: &ReviewAttribution,
    review_logger: Option<&Arc<ReviewLogger>>,
    reason: &str,
) {
    if in_flight.is_empty() {
        return;
    }
    let stranded: Vec<(String, String, String, usize)> =
        in_flight.drain().map(|(_, v)| v).collect();
    let review_id_owned = review_id.map(str::to_string);
    for (step_id, model_id, provider, model_idx) in stranded {
        warn!(
            job_id = %job_id,
            round = round,
            model = %model_id,
            provider = %provider,
            model_idx = model_idx,
            reason = reason,
            "reaping stranded model task"
        );
        let redacted_reason = redact_all(reason);
        if let Some(logger) = review_logger {
            logger
                .log(
                    "model_call_failed",
                    serde_json::json!({
                        "model_id": model_id,
                        "provider": provider,
                        "model_idx": model_idx,
                        "error": redacted_reason,
                    }),
                )
                .await;
        }
        let _ = store.fail_job_step(&step_id, &redacted_reason).await;
        let _ = app.emit(
            "hivemind-progress",
            add_event_attribution(
                serde_json::json!({
                    "job_id": job_id,
                    "review_id": review_id_owned.clone(),
                    "event_type": "model_failed",
                    "round": round,
                    "model_id": model_id,
                    "model_idx": model_idx,
                    "message": redacted_reason,
                }),
                attribution,
                Some("round"),
            ),
        );
    }
}

/// Pull the integer "retry after" hint (in seconds) out of a circuit-breaker
/// error string of the form "circuit breaker open, retry after 123.456s".
fn extract_retry_after_secs(err: &str) -> Option<u64> {
    let after = err.split("retry after").nth(1)?;
    let trimmed = after.trim();
    let mut chars = trimmed.chars();
    let mut num = String::new();
    while let Some(c) = chars.next() {
        if c.is_ascii_digit() {
            num.push(c);
        } else {
            break;
        }
    }
    num.parse::<u64>().ok()
}

/// Compute the cost of a model call based on token counts and model pricing.
///
/// NOTE: For Anthropic responses, the `input_tokens` value has been adjusted
/// to exclude `cache_creation_input_tokens` and `cache_read_input_tokens`
/// (see `AnthropicProvider::call()`). This means the first round of a review
/// (cold cache) will slightly underestimate cost because cache creation tokens
/// are billed at ~1.25× the standard input rate. For a typical 160K cache
/// write with claude-sonnet-4-20250514 this is ~$0.12 per call, which is
/// accepted as negligible relative to total review cost and avoids the
/// complexity of threading raw token counts through the system.
pub fn compute_cost(input_tokens: u64, output_tokens: u64, model_id: &str) -> f64 {
    let (cost_in, cost_out) = get_model_costs(model_id);
    (input_tokens as f64 / 1_000_000.0) * cost_in + (output_tokens as f64 / 1_000_000.0) * cost_out
}

/// Look up pricing for a model by ID. After audit 6.6 the canonical pricing
/// table lives on each `impl Provider::cost_per_1m_tokens(...)`; this helper
/// keeps the engine-internal fallback for the rare path where no provider
/// is in scope (e.g. early in startup or in tests without a registry).
///
/// Delegates to `crate::providers::legacy_well_known_model_cost(...)` for
/// the well-known model IDs, and falls back to `(1.0, 5.0)` otherwise —
/// preserving the legacy default rate that used to live inline here.
fn get_model_costs(model_id: &str) -> (f64, f64) {
    crate::providers::legacy_well_known_model_cost(model_id).unwrap_or((1.0, 5.0))
}

/// Look up the context window for a model by ID.
///
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stance_against_prompt_suffix() {
        assert!(Stance::Against.system_prompt_suffix().contains("CRITICAL"));
    }

    #[test]
    fn test_compute_cost() {
        let cost = compute_cost(1_000_000, 1_000_000, "claude-sonnet-4-20250514");
        assert!((cost - 18.0).abs() < 0.001);

        let cost = compute_cost(500_000, 200_000, "gpt-4o");
        assert!((cost - 3.25).abs() < 0.001);

        let cost = compute_cost(0, 0, "gpt-4o");
        assert!((cost - 0.0).abs() < 0.001);
    }

    #[test]
    fn test_get_model_costs() {
        let (inp, out) = get_model_costs("claude-sonnet-4-20250514");
        assert!((inp - 3.0).abs() < 0.001);
        assert!((out - 15.0).abs() < 0.001);

        let (inp, out) = get_model_costs("unknown");
        assert!((inp - 1.0).abs() < 0.001);
        assert!((out - 5.0).abs() < 0.001);
    }

    #[test]
    fn test_build_cross_review_prompt_no_prior_rounds() {
        let engine = ReviewEngine::new(Arc::new(ResponseCache::new(
            100,
            std::time::Duration::from_secs(60),
        )));
        let prompt = engine.build_cross_review_prompt("my plan", &[]);
        assert!(prompt.contains("my plan"));
        assert!(prompt.contains("Your Task"));
        assert!(!prompt.contains("Previous Review"));
    }

    #[test]
    fn test_build_cross_review_prompt_with_prior_rounds() {
        let engine = ReviewEngine::new(Arc::new(ResponseCache::new(
            100,
            std::time::Duration::from_secs(60),
        )));

        let prior = vec![vec![(
            "anthropic".to_string(),
            ModelResponse {
                output: "looks good".to_string(),
                input_tokens: 100,
                output_tokens: 50,
                model_id: "claude-sonnet-4-20250514".to_string(),
                duration_ms: 1000,
                ..Default::default()
            },
        )]];

        let prompt = engine.build_cross_review_prompt("my plan", &prior);
        assert!(prompt.contains("my plan"));
        assert!(prompt.contains("Previous Review Round 1"));
        assert!(prompt.contains("looks good"));
        assert!(prompt.contains("agree or disagree"));
    }

    // ---- merge prompt tests (round-position framing + hygiene) ----

    fn fake_round_responses() -> Vec<(String, ModelResponse)> {
        vec![(
            "anthropic".to_string(),
            ModelResponse {
                output: "reviewer wrote: looks fine".to_string(),
                input_tokens: 0,
                output_tokens: 0,
                model_id: "claude-sonnet-4-20250514".to_string(),
                duration_ms: 0,
                ..Default::default()
            },
        )]
    }

    #[test]
    fn build_merge_prompt_marks_final_round_when_round_equals_total() {
        let prompt = build_merge_prompt(1, 1, "plan body", &fake_round_responses());
        assert!(
            prompt.contains("FINAL round (1 of 1)"),
            "expected final-round framing, got: {}",
            prompt
        );
        assert!(
            prompt.contains("Do NOT mention \"next round\""),
            "expected explicit no-next-round instruction in final round"
        );
        assert!(prompt.contains("plan body"));
    }

    #[test]
    fn build_merge_prompt_marks_middle_round_when_more_rounds_follow() {
        let prompt = build_merge_prompt(1, 3, "plan body", &fake_round_responses());
        assert!(
            prompt.contains("round 1 of 3"),
            "expected middle-round framing, got: {}",
            prompt
        );
        assert!(
            prompt.contains("More rounds will follow"),
            "expected middle-round to flag that more rounds follow"
        );
        assert!(
            prompt.contains("Do NOT mention rounds or the review process inside the plan body"),
            "expected middle-round hygiene instruction"
        );
    }

    #[test]
    fn build_merge_prompt_treats_round_past_total_as_final() {
        // Defensive: a `round > total_rounds` shouldn't fall into the middle-round branch.
        let prompt = build_merge_prompt(5, 3, "plan body", &fake_round_responses());
        assert!(prompt.contains("FINAL round"));
    }

    #[test]
    fn build_merge_prompt_task_instruction_forbids_future_round_language() {
        // Both the plain and FEATURES branches must include the no-future-rounds
        // suffix so the model can't anchor on a permissive earlier line.
        let plain = build_merge_prompt(1, 1, "plan body", &fake_round_responses());
        assert!(plain.contains("no references to future review rounds"));

        let with_features = build_merge_prompt(
            1,
            1,
            r#"plan body
{"features":[{"id":"feat-1","name":"X","description":"y"}],"milestones":[]}"#,
            &fake_round_responses(),
        );
        assert!(with_features.contains("no references to future review rounds"));
        assert!(with_features.contains("submit_features"));
    }

    #[test]
    fn default_merge_system_prompt_no_longer_invites_next_round_language() {
        let sys = default_merge_system_prompt();
        // The old prompt explicitly said "ready for the next round of review";
        // that wording itself encouraged the leak the user reported.
        assert!(
            !sys.contains("next round of review"),
            "expected the leaky `next round of review` phrasing to be removed"
        );
        // And the forbidden-language rule must be present so the model knows
        // not to write transition-document prose into the plan body.
        assert!(sys.contains("FORBIDDEN"));
        assert!(sys.contains("next round"));
        assert!(sys.contains("future review"));
    }

    // ---- parse_rounds_config tests (migrated from core/scout_review.rs) ----

    #[test]
    fn parses_single_round() {
        let json = r#"[{"models":[{"id":"claude-sonnet-4-20250514","provider":"anthropic","thinking":"none","max_tokens":16384}],"timeout":300}]"#;
        let rounds = parse_rounds_config(json).expect("parse");
        assert_eq!(rounds.len(), 1);
        assert_eq!(rounds[0].models.len(), 1);
        assert_eq!(rounds[0].models[0].id, "claude-sonnet-4-20250514");
        assert_eq!(rounds[0].models[0].provider, "anthropic");
        assert_eq!(rounds[0].timeout, 300);
    }

    #[test]
    fn parses_multi_round_multi_model() {
        let json = r#"[
            {"models":[{"id":"a","provider":"anthropic"},{"id":"b","provider":"openai"}],"timeout":120},
            {"models":[{"id":"c","provider":"anthropic"}],"timeout":240}
        ]"#;
        let rounds = parse_rounds_config(json).expect("parse");
        assert_eq!(rounds.len(), 2);
        assert_eq!(rounds[0].models.len(), 2);
        assert_eq!(rounds[1].models.len(), 1);
        assert_eq!(rounds[0].timeout, 120);
        assert_eq!(rounds[1].timeout, 240);
    }

    #[test]
    fn skips_models_with_empty_id_or_provider() {
        let json = r#"[{"models":[{"id":"","provider":"anthropic"},{"id":"valid","provider":""},{"id":"ok","provider":"anthropic"}],"timeout":60}]"#;
        let rounds = parse_rounds_config(json).expect("parse");
        assert_eq!(rounds[0].models.len(), 1);
        assert_eq!(rounds[0].models[0].id, "ok");
    }

    #[test]
    fn errors_on_non_array() {
        let err = parse_rounds_config(r#"{"not":"array"}"#).unwrap_err();
        assert!(err.to_string().contains("not a JSON array"));
    }

    // ──────────────────────────────────────────────────────────────────────
    // Phase 5 — try_render_structured_review
    // ──────────────────────────────────────────────────────────────────────

    #[test]
    fn try_render_structured_review_renders_valid_json_to_markdown() {
        let json = serde_json::json!({
            "verdict": "Approve.",
            "issues": [{
                "layer": 2,
                "title": "Race",
                "file_path": "src/x.rs",
                "description": "shared write"
            }],
            "strengths": ["Clean"],
            "key_takeaways": ["Ship after fix"]
        })
        .to_string();
        let resp = ModelResponse {
            output: json,
            input_tokens: 10,
            output_tokens: 20,
            model_id: "m".to_string(),
            duration_ms: 5,
            ..Default::default()
        };
        let rendered = try_render_structured_review(resp).expect("valid review renders");
        assert!(rendered.output.contains("## Verdict"));
        assert!(rendered.output.contains("Approve."));
        assert!(rendered.output.contains("**[L2] Race**"));
    }

    #[test]
    fn try_render_structured_review_passes_plain_text_through_as_markdown_fallback() {
        // Reviewer ignored tool_choice and returned plain markdown — common
        // on CROF `-precision` variants and other OpenAI-compatible upstreams
        // without function-calling support. The merge orchestrator reads
        // markdown either way so we pass the output through unchanged.
        let original = "## Verdict\n\nLooks good".to_string();
        let resp = ModelResponse {
            output: original.clone(),
            input_tokens: 10,
            output_tokens: 20,
            model_id: "m".to_string(),
            duration_ms: 5,
            ..Default::default()
        };
        let out = try_render_structured_review(resp).expect("plain text falls back");
        assert_eq!(out.output, original);
    }

    #[test]
    fn try_render_structured_review_passes_malformed_json_through_as_fallback() {
        // JSON that parses but isn't a StructuredReview also falls back —
        // the merge step gets the raw string and can still summarise.
        let original = "{\"unrelated\":\"junk\"}".to_string();
        let resp = ModelResponse {
            output: original.clone(),
            input_tokens: 0,
            output_tokens: 0,
            model_id: "m".to_string(),
            duration_ms: 0,
            ..Default::default()
        };
        let out = try_render_structured_review(resp).expect("malformed JSON falls back");
        assert_eq!(out.output, original);
    }

    #[test]
    fn try_render_structured_review_errors_on_empty_response() {
        let resp = ModelResponse {
            output: "   \n\t  ".to_string(),
            input_tokens: 0,
            output_tokens: 0,
            model_id: "m".to_string(),
            duration_ms: 0,
            ..Default::default()
        };
        assert!(try_render_structured_review(resp).is_err());
    }

    #[test]
    fn rewrite_prompt_for_structured_output_swaps_markdown_block_for_tool_call() {
        let resolved = REVIEWER_BASE_TEMPLATE
            .replace("{STANCE_SUFFIX}", Stance::Against.system_prompt_suffix());
        let out = rewrite_prompt_for_structured_output(&resolved);

        assert!(
            !out.contains("MUST respond in exactly this Markdown structure"),
            "markdown directive should be stripped from the structured prompt"
        );
        assert!(
            !out.contains("## Verdict\nOne sentence"),
            "verdict heading template should be stripped"
        );
        assert!(
            !out.contains("## Issues Found"),
            "issues heading template should be stripped"
        );
        assert!(
            out.contains("Call the `submit_review` tool"),
            "replacement should instruct the tool call"
        );
        assert!(
            out.contains("CRITICAL PERSPECTIVE"),
            "stance section preserved"
        );
        assert!(
            out.contains("REVIEW LAYERS"),
            "review-layers section preserved"
        );
        assert!(
            out.contains("Cite exact file paths"),
            "RULES section preserved"
        );
    }

    #[test]
    fn rewrite_prompt_for_structured_output_preserves_custom_suffix() {
        let resolved = REVIEWER_BASE_TEMPLATE
            .replace("{STANCE_SUFFIX}", Stance::Against.system_prompt_suffix());
        let with_suffix = format!("{}\n\nCUSTOM REVIEWER ADDITION", resolved);
        let out = rewrite_prompt_for_structured_output(&with_suffix);

        assert!(out.contains("Call the `submit_review` tool"));
        assert!(
            out.ends_with("CUSTOM REVIEWER ADDITION"),
            "appended custom suffix must survive the rewrite"
        );
    }

    #[test]
    fn rewrite_prompt_for_structured_output_passes_through_unrecognised_prompt() {
        let alien = "totally unrelated system prompt with no RESPONSE FORMAT block";
        assert_eq!(rewrite_prompt_for_structured_output(alien), alien);
    }

    // ----------------------------------------------------------------
    // Round-timeout stranded-task reaping (regression: 3.5)
    // ----------------------------------------------------------------
    //
    // Before this fix, when a round timeout fired with in-flight model calls
    // still pending, `join_set.shutdown().await` aborted those tasks without
    // updating either:
    //   - the SQLite step row (stays in `started`), or
    //   - the frontend (no `model_failed` event — pill stays orange forever).
    //
    // The two helpers below now drain the side-map of in-flight task ids and
    // synthesize a failure event for each — this test exercises that path on
    // the attribution-flavoured helper used by `dispatch_round`.
    use crate::hivemind::store::HivemindStore;
    use tempfile::TempDir;

    async fn fresh_store_for_reap() -> (HivemindStore, TempDir) {
        let tmp = TempDir::new().expect("tempdir");
        let db_path = tmp.path().join("hm.sqlite");
        let store = HivemindStore::new(&db_path).await.expect("open store");
        (store, tmp)
    }

    /// Seed the store with a job and N step rows in the `started` state, then
    /// return the (job_id, [(step_id, model_id, provider)]) side-map values
    /// — what `dispatch_round` would have populated at spawn time.
    async fn seed_started_steps(
        store: &HivemindStore,
        job_id: &str,
        steps: &[(&str, &str)], // (model_id, provider)
    ) -> Vec<(String, String, String, usize)> {
        store
            .create_job(
                job_id, "plan", "against", 1, 60, None, None, None, None, None,
            )
            .await
            .expect("create job");

        let mut out = Vec::new();
        for (idx, (model_id, provider)) in steps.iter().enumerate() {
            let step_id = format!("step-{}-{}", job_id, idx);
            store
                .create_job_step(
                    &step_id, job_id, 1, idx as i64, model_id, provider, "against", "prompt",
                )
                .await
                .expect("create step");
            store
                .update_job_step_started(&step_id)
                .await
                .expect("mark started");
            out.push((
                step_id,
                (*model_id).to_string(),
                (*provider).to_string(),
                idx,
            ));
        }
        out
    }

    /// Round-timeout reaping flips every stranded step to `failed`, emits a
    /// `model_failed` IPC event per stranded model, and drains the side-map.
    #[tokio::test]
    async fn reap_stranded_tasks_attr_fails_rows_and_emits_events() {
        use std::sync::Mutex as StdMutex;
        use tauri::Listener;

        let (store, _tmp) = fresh_store_for_reap().await;
        let job_id = "job-reap-1";
        let seeded = seed_started_steps(
            &store,
            job_id,
            &[
                ("model-a", "anthropic"),
                ("model-b", "openai"),
                ("model-c", "openrouter"),
            ],
        )
        .await;

        // Wrap the seeded values in the same side-map shape `dispatch_round`
        // builds at spawn time. We fabricate task ids by spawning trivial
        // futures into a JoinSet purely so we have valid `TaskId` values to
        // key on — the runtime cleans them up before we call the helper.
        let mut tmp_set: JoinSet<()> = JoinSet::new();
        let mut in_flight: HashMap<TaskId, (String, String, String, usize)> = HashMap::new();
        for entry in &seeded {
            let abort = tmp_set.spawn(async {});
            in_flight.insert(abort.id(), entry.clone());
        }
        // Let those dummy tasks finish so the runtime drops their handles.
        while tmp_set.join_next().await.is_some() {}

        // Capture every `hivemind-progress` event the helper emits.
        let app = tauri::test::mock_app();
        let app_handle = app.handle().clone();
        let captured: Arc<StdMutex<Vec<serde_json::Value>>> = Arc::new(StdMutex::new(Vec::new()));
        let cap_clone = Arc::clone(&captured);
        app_handle.listen("hivemind-progress", move |event| {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(event.payload()) {
                cap_clone.lock().unwrap().push(v);
            }
        });

        let attribution = ReviewAttribution::default();
        reap_stranded_tasks_attr(
            &mut in_flight,
            &store,
            &app_handle,
            job_id,
            1,
            None,
            &attribution,
            None,
            "round timeout",
        )
        .await;

        // Side-map drained.
        assert!(in_flight.is_empty(), "side-map should be empty after reap");

        // Every step row was updated to `failed` with our reason.
        let rows = store.get_job_steps(job_id).await.expect("fetch steps");
        assert_eq!(rows.len(), 3);
        for row in &rows {
            assert_eq!(row.status, "failed", "step {} should be failed", row.id);
            assert_eq!(row.error.as_deref(), Some("round timeout"));
        }

        // Mock-runtime events fire synchronously through the listener registry
        // but the listen registration itself is sync-completion via a deferred
        // task spawn; give the executor one tick so the listener has been
        // wired up before we assert. (In CI this is essentially zero.)
        tokio::task::yield_now().await;

        let events = captured.lock().unwrap().clone();
        // Exactly one `model_failed` per stranded model.
        let failed: Vec<&serde_json::Value> = events
            .iter()
            .filter(|v| v.get("event_type").and_then(|t| t.as_str()) == Some("model_failed"))
            .collect();
        assert_eq!(
            failed.len(),
            3,
            "expected 3 model_failed events, got {} (all events: {:?})",
            failed.len(),
            events
        );
        let mut got_models: Vec<String> = failed
            .iter()
            .map(|e| {
                e.get("model_id")
                    .and_then(|m| m.as_str())
                    .unwrap_or("")
                    .to_string()
            })
            .collect();
        got_models.sort();
        assert_eq!(got_models, vec!["model-a", "model-b", "model-c"]);
        // Each event carries the timeout reason as `message`.
        for e in &failed {
            assert_eq!(
                e.get("message").and_then(|m| m.as_str()),
                Some("round timeout")
            );
            assert_eq!(e.get("job_id").and_then(|j| j.as_str()), Some(job_id));
            assert_eq!(e.get("round").and_then(|r| r.as_u64()), Some(1));
        }
    }

    /// When the side-map is empty (everything completed before the timeout),
    /// reap_stranded_tasks is a no-op.
    #[tokio::test]
    async fn reap_stranded_tasks_attr_is_noop_when_empty() {
        let (store, _tmp) = fresh_store_for_reap().await;
        let app = tauri::test::mock_app();
        let app_handle = app.handle().clone();
        let mut in_flight: HashMap<TaskId, (String, String, String, usize)> = HashMap::new();
        let attribution = ReviewAttribution::default();
        reap_stranded_tasks_attr(
            &mut in_flight,
            &store,
            &app_handle,
            "job-none",
            1,
            None,
            &attribution,
            None,
            "round timeout",
        )
        .await;
        assert!(in_flight.is_empty());
    }

    /// End-to-end style: simulate the spawn → timeout → reap flow that
    /// `dispatch_round` runs internally, using a real `MockProvider` that
    /// sleeps longer than our `tokio::time::timeout` deadline. The full
    /// `dispatch_round` entry point requires a `tauri::AppHandle<Wry>` for
    /// the IPC emit step (Wry can't be constructed in tests), so this test
    /// inlines the same JoinSet + side-map + timeout pattern with a
    /// `MockRuntime` app handle.
    #[tokio::test]
    async fn round_timeout_reaps_stranded_mock_provider_calls() {
        use crate::providers::{CallRequest, MockProvider, Provider, ProviderRegistry};
        use std::sync::Mutex as StdMutex;
        use std::time::Duration;
        use tauri::Listener;

        let (store, _tmp) = fresh_store_for_reap().await;

        // Mock provider that sleeps far longer than the test's round budget.
        let mock = MockProvider::with_delay("ignored", Duration::from_secs(5));
        let mut registry = ProviderRegistry::new();
        registry.register("mock", Arc::new(mock) as Arc<dyn Provider>);
        let registry = Arc::new(tokio::sync::RwLock::new(registry));

        let app = tauri::test::mock_app();
        let app_handle = app.handle().clone();
        let captured: Arc<StdMutex<Vec<serde_json::Value>>> = Arc::new(StdMutex::new(Vec::new()));
        let cap_clone = Arc::clone(&captured);
        app_handle.listen("hivemind-progress", move |event| {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(event.payload()) {
                cap_clone.lock().unwrap().push(v);
            }
        });

        let job_id = "job-spawn-reap";
        let seeded =
            seed_started_steps(&store, job_id, &[("slow-a", "mock"), ("slow-b", "mock")]).await;

        // Spawn one task per seeded step. Each calls the slow mock provider
        // — none will finish before the 200ms round budget below.
        let mut join_set: JoinSet<()> = JoinSet::new();
        let mut in_flight: HashMap<TaskId, (String, String, String, usize)> = HashMap::new();
        for (step_id, model_id, provider, model_idx) in &seeded {
            let reg = registry.clone();
            let model_id_c = model_id.clone();
            let provider_c = provider.clone();
            let abort = join_set.spawn(async move {
                let p = {
                    let reg = reg.read().await;
                    reg.get(&provider_c).expect("mock provider")
                };
                let _ = p.call(CallRequest::new(&model_id_c, "", "")).await;
            });
            in_flight.insert(
                abort.id(),
                (
                    step_id.clone(),
                    model_id.clone(),
                    provider.clone(),
                    *model_idx,
                ),
            );
        }

        // Tight round timeout — guarantees the mock tasks are still in flight
        // when the deadline fires. Mirrors the real `dispatch_round` loop:
        // sleep on the remaining budget, then reap.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let attribution = ReviewAttribution::default();
        reap_stranded_tasks_attr(
            &mut in_flight,
            &store,
            &app_handle,
            job_id,
            1,
            None,
            &attribution,
            None,
            "round timeout",
        )
        .await;
        join_set.shutdown().await;

        // All step rows now `failed`.
        let rows = store.get_job_steps(job_id).await.expect("fetch steps");
        assert_eq!(rows.len(), 2);
        for row in &rows {
            assert_eq!(row.status, "failed", "step {} should be failed", row.id);
            assert_eq!(row.error.as_deref(), Some("round timeout"));
        }

        // One `model_failed` event per stranded model.
        tokio::task::yield_now().await;
        let events = captured.lock().unwrap().clone();
        let failed: Vec<&serde_json::Value> = events
            .iter()
            .filter(|v| v.get("event_type").and_then(|t| t.as_str()) == Some("model_failed"))
            .collect();
        assert_eq!(
            failed.len(),
            2,
            "expected 2 model_failed events, got {} (all: {:?})",
            failed.len(),
            events
        );
    }

    // ──────────────────────────────────────────────────────────────────────
    // Structured `merge_*` / `context_*` chunk event shape
    // ──────────────────────────────────────────────────────────────────────
    //
    // The merge / context forwarders inside `spawn_merge_pi` and
    // `gather_context_phase` subscribe to a Pi session's event broadcast
    // and translate each `PiEvent` into a `hivemind-progress` payload the
    // Tasks-view reducer treats as an `internal_pi_*` event. These tests
    // exercise the translation in isolation by driving a `MockRpcClient`
    // and asserting on the captured `tauri::AppHandle::emit` calls.
    //
    // The forwarder body is intentionally duplicated below so the test
    // does not need a full `EngineDeps` / `PiManager` / `MergeCapture`
    // setup. The two structures must stay in sync; any divergence is
    // caught by manual review during code change.
    use crate::pi::events::PiEvent as MockPiEvent;
    use crate::pi::mock::mock_session;

    fn drain_forwarder_events(
        captured: &std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
        event_type: &str,
    ) -> Vec<serde_json::Value> {
        captured
            .lock()
            .unwrap()
            .iter()
            .filter(|v| v.get("event_type").and_then(|t| t.as_str()) == Some(event_type))
            .cloned()
            .collect()
    }

    #[tokio::test]
    async fn merge_started_event_carries_session_id() {
        // Regression: the Tasks-view inline merge bubble depends on the
        // session_id being available on `merge_started` so the reducer
        // can register the session as "internal" before any
        // `merge_text` deltas arrive. Plain-shape assertion only.
        let attribution = ReviewAttribution {
            review_id: Some("rev-1".to_string()),
            task_id: Some("task-1".to_string()),
            ..ReviewAttribution::default()
        };
        let payload = add_event_attribution(
            serde_json::json!({
                "job_id": "job-1",
                "review_id": "rev-1",
                "event_type": "merge_started",
                "round": 2,
                "model_id": "claude-sonnet-4",
                "message": "Merging round 2 outputs",
                "session_id": "hivemind-merge-job-1-r2",
            }),
            &attribution,
            Some("merge"),
        );
        let parsed: crate::hivemind::events::HivemindProgressEvent =
            serde_json::from_value(payload).expect("deserialize");
        assert_eq!(
            parsed.session_id.as_deref(),
            Some("hivemind-merge-job-1-r2")
        );
        assert_eq!(parsed.phase.as_deref(), Some("merge"));
        assert_eq!(parsed.task_id.as_deref(), Some("task-1"));
        assert_eq!(parsed.round, 2);
    }

    /// End-to-end: drive a mock PiSession bus, run a slim copy of the
    /// merge structured-event forwarder, and assert the resulting payloads
    /// have the shape the Tasks-view reducer expects (`merge_text`,
    /// `merge_thinking`, `merge_tool_start` with `tool_name`/`tool_args`).
    #[tokio::test]
    async fn merge_structured_forwarder_emits_per_pi_event_payloads() {
        use tauri::Listener;
        let (session, mock) = mock_session("hivemind-merge-test");
        let app = tauri::test::mock_app();
        let app_handle = app.handle().clone();
        let captured: std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>> =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let cap = std::sync::Arc::clone(&captured);
        app_handle.listen("hivemind-progress", move |event| {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(event.payload()) {
                cap.lock().unwrap().push(v);
            }
        });

        let mut events = session.subscribe_events();
        let app_for_chunks = app_handle.clone();
        let attribution = ReviewAttribution {
            review_id: Some("rev-1".to_string()),
            task_id: Some("task-1".to_string()),
            ..ReviewAttribution::default()
        };
        let attribution_for_chunks = attribution.clone();
        let session_id_for_chunks = "hivemind-merge-test".to_string();
        let job_id_for_chunks = "job-1".to_string();
        let review_id_for_chunks = Some("rev-1".to_string());
        let model_for_chunks = "claude-sonnet-4".to_string();
        let round: u32 = 1;
        let cancel = CancellationToken::new();
        let cancel_for_chunks = cancel.clone();
        let forwarder = tokio::spawn(async move {
            use tokio::sync::broadcast::error::RecvError;
            let emit_structured = |payload: serde_json::Value| {
                let _ = app_for_chunks.emit(
                    "hivemind-progress",
                    add_event_attribution(payload, &attribution_for_chunks, Some("merge")),
                );
            };
            loop {
                tokio::select! {
                    _ = cancel_for_chunks.cancelled() => break,
                    recv = events.recv() => {
                        match recv {
                            Ok(MockPiEvent::TextDelta(text)) => {
                                emit_structured(serde_json::json!({
                                    "job_id": job_id_for_chunks,
                                    "review_id": review_id_for_chunks.clone(),
                                    "event_type": "merge_text",
                                    "round": round,
                                    "model_id": model_for_chunks,
                                    "message": "",
                                    "delta": text,
                                    "session_id": session_id_for_chunks,
                                }));
                            }
                            Ok(MockPiEvent::ThinkingDelta(text)) => {
                                emit_structured(serde_json::json!({
                                    "job_id": job_id_for_chunks,
                                    "review_id": review_id_for_chunks.clone(),
                                    "event_type": "merge_thinking",
                                    "round": round,
                                    "model_id": model_for_chunks,
                                    "message": "",
                                    "delta": text,
                                    "session_id": session_id_for_chunks,
                                }));
                            }
                            Ok(MockPiEvent::ToolExecutionStart { tool_call_id, name, args }) => {
                                emit_structured(serde_json::json!({
                                    "job_id": job_id_for_chunks,
                                    "review_id": review_id_for_chunks.clone(),
                                    "event_type": "merge_tool_start",
                                    "round": round,
                                    "model_id": model_for_chunks,
                                    "message": "",
                                    "session_id": session_id_for_chunks,
                                    "tool_call_id": tool_call_id,
                                    "tool_name": name,
                                    "tool_args": args,
                                }));
                            }
                            Ok(MockPiEvent::ToolExecutionEnd { tool_call_id, result }) => {
                                emit_structured(serde_json::json!({
                                    "job_id": job_id_for_chunks,
                                    "review_id": review_id_for_chunks.clone(),
                                    "event_type": "merge_tool_end",
                                    "round": round,
                                    "model_id": model_for_chunks,
                                    "message": "",
                                    "session_id": session_id_for_chunks,
                                    "tool_call_id": tool_call_id,
                                    "tool_result": result,
                                }));
                            }
                            Ok(MockPiEvent::AgentEnd) | Ok(MockPiEvent::TurnComplete) => break,
                            Err(RecvError::Closed) => break,
                            _ => {}
                        }
                    }
                }
            }
        });

        // Give the spawned forwarder a chance to subscribe before we emit.
        tokio::task::yield_now().await;

        mock.emit(MockPiEvent::ThinkingDelta("thinking about it".to_string()));
        mock.emit(MockPiEvent::TextDelta("# Plan\n\nbody".to_string()));
        mock.emit(MockPiEvent::ToolExecutionStart {
            tool_call_id: "call-1".to_string(),
            name: "submit_plan".to_string(),
            args: serde_json::json!({"plan_markdown": "# Updated"}),
        });
        mock.emit(MockPiEvent::ToolExecutionEnd {
            tool_call_id: "call-1".to_string(),
            result: serde_json::json!({"ok": true}),
        });
        mock.emit(MockPiEvent::AgentEnd);

        // Forwarder exits on AgentEnd.
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), forwarder)
            .await
            .expect("forwarder did not exit");
        // Listener callbacks run on a runtime task; yield so they flush.
        tokio::task::yield_now().await;

        let texts = drain_forwarder_events(&captured, "merge_text");
        assert_eq!(
            texts.len(),
            1,
            "events captured: {:?}",
            captured.lock().unwrap()
        );
        assert_eq!(
            texts[0].get("delta").and_then(|v| v.as_str()),
            Some("# Plan\n\nbody")
        );
        assert_eq!(
            texts[0].get("session_id").and_then(|v| v.as_str()),
            Some("hivemind-merge-test")
        );
        assert_eq!(
            texts[0].get("phase").and_then(|v| v.as_str()),
            Some("merge")
        );

        let thinkings = drain_forwarder_events(&captured, "merge_thinking");
        assert_eq!(thinkings.len(), 1);
        assert_eq!(
            thinkings[0].get("delta").and_then(|v| v.as_str()),
            Some("thinking about it")
        );

        let starts = drain_forwarder_events(&captured, "merge_tool_start");
        assert_eq!(starts.len(), 1);
        assert_eq!(
            starts[0].get("tool_call_id").and_then(|v| v.as_str()),
            Some("call-1")
        );
        assert_eq!(
            starts[0].get("tool_name").and_then(|v| v.as_str()),
            Some("submit_plan")
        );
        assert_eq!(
            starts[0]
                .get("tool_args")
                .and_then(|v| v.get("plan_markdown"))
                .and_then(|v| v.as_str()),
            Some("# Updated")
        );

        let ends = drain_forwarder_events(&captured, "merge_tool_end");
        assert_eq!(ends.len(), 1);
        assert_eq!(
            ends[0].get("tool_call_id").and_then(|v| v.as_str()),
            Some("call-1")
        );
    }

    /// Regression guard for the "phase pill stuck on Merging R{N}" bug.
    ///
    /// Drives a fake emit sequence that mirrors the post-fix ordering
    /// `run_merge_phase` produces on success, captures the events
    /// through the same `tauri::test::mock_app()` channel the
    /// production code uses, and asserts:
    ///
    /// 1. `verdicts_updated` strictly precedes `merge_completed` (so
    ///    the verdict SQLite write is not on the critical path between
    ///    `merge_completed` and the next `round_started`).
    /// 2. The events captured between `merge_completed(R1)` and
    ///    `round_started(R2)` contain no `verdicts_updated` /
    ///    `verdicts_saved` markers — the user-visible gap is exactly
    ///    the cancel-check + loop-iterate path, with no awaitable
    ///    persistence.
    ///
    /// This is a source-ordering assertion plus a runtime capture
    /// assertion. The full `ReviewEngine::run` path requires a real
    /// `PiManager` + `ProviderRegistry` and is exercised manually
    /// against a live review; the inline harness below is the
    /// finest-grained automation that doesn't drag in those
    /// subsystems.
    #[tokio::test]
    async fn merge_completed_emit_precedes_round_started_with_no_intervening_persistence() {
        use std::sync::Mutex as StdMutex;
        use tauri::Listener;

        let app = tauri::test::mock_app();
        let app_handle = app.handle().clone();
        let captured: Arc<StdMutex<Vec<serde_json::Value>>> = Arc::new(StdMutex::new(Vec::new()));
        let cap_clone = Arc::clone(&captured);
        app_handle.listen("hivemind-progress", move |event| {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(event.payload()) {
                cap_clone.lock().unwrap().push(v);
            }
        });

        let attribution = ReviewAttribution {
            review_id: Some("rev-ordering".to_string()),
            task_id: Some("task-1".to_string()),
            ..ReviewAttribution::default()
        };
        let job_id = "job-ordering";

        // Mirror the post-fix `run_merge_phase` emit sequence for R1.
        // verdicts_updated strictly precedes merge_completed.
        let _ = app_handle.emit(
            "hivemind-progress",
            add_event_attribution(
                serde_json::json!({
                    "job_id": job_id,
                    "review_id": attribution.review_id,
                    "event_type": "verdicts_updated",
                    "round": 1,
                    "count": 2,
                }),
                &attribution,
                Some("merge"),
            ),
        );
        let _ = app_handle.emit(
            "hivemind-progress",
            add_event_attribution(
                serde_json::json!({
                    "job_id": job_id,
                    "review_id": attribution.review_id,
                    "event_type": "merge_completed",
                    "round": 1,
                    "model_id": "claude-sonnet-4",
                    "message": "Round 1 merge complete",
                    "output_len": 1024,
                }),
                &attribution,
                Some("merge"),
            ),
        );
        // Outer loop in `ReviewEngine::run` iterates here; the only
        // permitted work between the merge_completed emit and the next
        // round_started emit is a cancel check (sync) and the
        // `current_plan` reassignment.
        let _ = app_handle.emit(
            "hivemind-progress",
            add_event_attribution(
                serde_json::json!({
                    "job_id": job_id,
                    "review_id": attribution.review_id,
                    "event_type": "round_started",
                    "round": 2,
                    "model_id": "",
                    "message": "Round 2 of 2 started",
                }),
                &attribution,
                Some("round"),
            ),
        );

        tokio::task::yield_now().await;

        let events = captured.lock().unwrap().clone();
        let order: Vec<&str> = events
            .iter()
            .filter_map(|v| v.get("event_type").and_then(|t| t.as_str()))
            .collect();

        // Locate the three critical events.
        let vu_idx = order
            .iter()
            .position(|t| *t == "verdicts_updated")
            .expect("verdicts_updated should appear");
        let mc_idx = order
            .iter()
            .position(|t| *t == "merge_completed")
            .expect("merge_completed should appear");
        let rs_idx = order
            .iter()
            .position(|t| *t == "round_started")
            .expect("round_started should appear");

        assert!(
            vu_idx < mc_idx,
            "verdicts_updated must precede merge_completed (fix for stuck \"Merging R{{N}}\" pill); order: {:?}",
            order
        );
        assert!(
            mc_idx < rs_idx,
            "merge_completed must precede the next round_started; order: {:?}",
            order
        );
        // Nothing observability-related (verdicts_updated /
        // verdicts_saved) should sit between merge_completed and the
        // next round_started — those events now precede merge_completed.
        for evt in &order[mc_idx + 1..rs_idx] {
            assert!(
                *evt != "verdicts_updated" && *evt != "verdicts_saved",
                "persistence event {:?} leaked between merge_completed and round_started; order: {:?}",
                evt,
                order
            );
        }

        // Source-positional guard: catch a future refactor that
        // accidentally reorders the production code back to the old
        // (broken) ordering, even if the runtime test above were to
        // drift out of date.
        let src = include_str!("engine.rs");
        let vu_src = src
            .find("\"event_type\": \"verdicts_updated\"")
            .expect("verdicts_updated emit literal must exist in engine.rs");
        let mc_src = src
            .find("\"event_type\": \"merge_completed\"")
            .expect("merge_completed emit literal must exist in engine.rs");
        assert!(
            vu_src < mc_src,
            "engine.rs source must emit verdicts_updated before merge_completed (vu={}, mc={})",
            vu_src,
            mc_src
        );
    }

    /// Regression guard for the `models` array on `round_started`.
    ///
    /// The frontend reducer (`hivemindReducer.ts` `case "round_started"`)
    /// seeds one spinner row per model ID so buffered providers don't
    /// leave the dock blank until they finish. If the backend drops the
    /// `models` field the UI silently regresses to the pre-fix
    /// "Waiting for model data…" behaviour for non-streaming providers.
    #[test]
    fn build_round_started_payload_includes_models() {
        let model_ids = vec![
            "anthropic/claude-sonnet-4".to_string(),
            "openai/gpt-4o".to_string(),
        ];
        let payload = build_round_started_payload("job-1", Some("rev-1"), 2, 3, &model_ids);

        assert_eq!(payload["event_type"], "round_started");
        assert_eq!(payload["job_id"], "job-1");
        assert_eq!(payload["review_id"], "rev-1");
        assert_eq!(payload["round"], 2);
        assert_eq!(payload["model_id"], "");
        assert_eq!(payload["message"], "Round 2 of 3 started");

        let models = payload["models"]
            .as_array()
            .expect("models must be a JSON array on round_started");
        let ids: Vec<&str> = models.iter().filter_map(|v| v.as_str()).collect();
        assert_eq!(ids, vec!["anthropic/claude-sonnet-4", "openai/gpt-4o"]);

        // The richer `model_instances` shape must also be present so the
        // frontend can disambiguate duplicate-model rows by `(model_id,
        // model_idx)`. Order mirrors the `models` array.
        let instances = payload["model_instances"]
            .as_array()
            .expect("model_instances must be present on round_started");
        assert_eq!(instances.len(), 2);
        assert_eq!(instances[0]["model_id"], "anthropic/claude-sonnet-4");
        assert_eq!(instances[0]["model_idx"], 0);
        assert_eq!(instances[1]["model_id"], "openai/gpt-4o");
        assert_eq!(instances[1]["model_idx"], 1);
    }

    /// Empty `models` array is still emitted (not omitted) so the
    /// frontend can distinguish "backend known to have sent the list,
    /// it was just empty" from "backend is on an old build that
    /// doesn't emit models at all".
    #[test]
    fn build_round_started_payload_emits_empty_models_array() {
        let payload = build_round_started_payload("job-1", None, 1, 1, &[]);
        let models = payload["models"]
            .as_array()
            .expect("models field must always be present, even if empty");
        assert!(models.is_empty());
        let instances = payload["model_instances"]
            .as_array()
            .expect("model_instances field must always be present, even if empty");
        assert!(instances.is_empty());
    }

    #[test]
    fn qualify_model_for_provider_prefixes_provider_when_model_has_upstream_namespace() {
        assert_eq!(
            qualify_model_for_provider("neuralwatt", "moonshotai/Kimi-K2.6"),
            "neuralwatt/moonshotai/Kimi-K2.6"
        );
    }

    #[test]
    fn qualify_model_for_provider_does_not_double_prefix() {
        assert_eq!(
            qualify_model_for_provider("neuralwatt", "neuralwatt/moonshotai/Kimi-K2.6"),
            "neuralwatt/moonshotai/Kimi-K2.6"
        );
    }
}
