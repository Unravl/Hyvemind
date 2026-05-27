//! Stability test orchestrator (phase state machine).
//!
//! Driven by `commands::tests::run_stability_test`. Runs in a background
//! task; emits `test-progress` Tauri events for the UI.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use tauri::{Emitter, Listener, Manager};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::{GateResult, TestRunRecord, VerifierVerdict};
use crate::commands::chat::{send_message, ChatEvent};
use crate::commands::hivemind::{get_review_state, start_review, ReviewStateSnapshot};
use crate::hivemind::events::HivemindProgressEvent;
use crate::pi::rpc::{PiSessionOptions, ThinkingLevel, ToolSet};
use crate::pi::session::SessionOwner;
use crate::state::app_state::AppState;
use crate::state::store::atomic_write;

/// Test prompt fed into the planning agent.
const STABILITY_TEST_PROMPT: &str = include_str!("../../../prompts/stability_test_task.md");

/// System prompt for the AI verifier session.
const STABILITY_TEST_VERIFIER_PROMPT: &str =
    include_str!("../../../prompts/stability_test_verifier.md");

const SANDBOX_RETENTION: usize = 5;
const TASK_PHASE_TIMEOUT: Duration = Duration::from_secs(300);
const HIVEMIND_TIMEOUT: Duration = Duration::from_secs(600);
const IMPL_TIMEOUT: Duration = Duration::from_secs(300);
const VERIFIER_TIMEOUT: Duration = Duration::from_secs(180);

/// Planning system prompt. Mirrors `app/src/lib/plan-mode.ts::PLAN_SYSTEM_PROMPT`
/// so the test exercises the same agent contract the Tasks-view UI uses.
const PLAN_SYSTEM_PROMPT: &str = r#"You are a planning agent. Your job is to research the codebase and produce a detailed implementation plan.

CONSTRAINTS:
- You have READ-ONLY access to the project. Do NOT edit, write, or delete any files.
- Do NOT run state-modifying commands (no git commit, no npm install, no file writes).

WORKFLOW:
1. Research: Explore the codebase to understand the current architecture, relevant files, and dependencies.
2. Analyze: Identify what needs to change, potential risks, and the best approach.
3. Plan: Produce a structured implementation plan.

QUESTIONS (optional):
If you need clarification before planning, call `submit_stability_questions({"questions": [...]})`. Each question needs an `id`, `kind` ("choice" or "text"), and `title`. Choice questions take an `options` array; mark one option `"recommended": true`. After calling the tool, STOP and wait for answers.

OUTPUT FORMAT:
Submit the final plan by calling `submit_stability_plan({"plan_markdown": "..."})` with a valid markdown body containing Overview, Steps, Files to Modify, and Verification sections. There is no fallback — the tool call is mandatory."#;

/// Implementation system prompt. Mirrors
/// `app/src/lib/plan-mode.ts::IMPL_SYSTEM_PROMPT`.
const IMPL_SYSTEM_PROMPT: &str = r#"You are an implementation agent. Your job is to execute an approved implementation plan exactly as written, using your full coding toolset (read, write, edit, bash).

RULES:
- Do NOT re-plan, re-analyze, or ask clarifying questions. The plan has already been reviewed and approved.
- Implement every step in order. If a step is ambiguous, use your best judgment and continue.
- You have full read/write access to the working directory.

COMPLETION:
When every step in the plan is finished, call `submit_stability_impl_complete({})` to signal completion. Do NOT call it until all steps are done."#;

/// Run a full stability test end-to-end. Spawned in a background task by
/// `commands::tests::run_stability_test`. Emits `test-progress` events as
/// it walks the phase state machine and persists a `TestRunRecord` when
/// done (success / failure / cancellation).
pub async fn run_stability_test_inner(
    app: tauri::AppHandle,
    run_id: String,
    cancel_token: CancellationToken,
) -> Result<TestRunRecord> {
    let started_at = chrono::Utc::now();
    let run_start_ms = started_at.timestamp_millis() as u64;

    let state = app.state::<AppState>();
    let sandbox_root = state.test_sandbox_dir.clone();

    let mut record = TestRunRecord {
        run_id: run_id.clone(),
        status: "running".to_string(),
        started_at: started_at.to_rfc3339(),
        completed_at: None,
        duration_ms: 0,
        task_id: format!("stability-test-{}", run_id),
        session_id: None,
        plan_session_id: None,
        hivemind_job_id: None,
        sandbox_dir: String::new(),
        total_cost: 0.0,
        gates: Vec::new(),
        verdict: None,
        error: None,
    };

    // Phase: setup.
    emit_progress(&app, &run_id, "setup", "started", "Creating sandbox", None);
    let sandbox = match prepare_sandbox(&sandbox_root, &run_id) {
        Ok(p) => p,
        Err(e) => {
            return finalize_with_error(
                &app,
                record,
                run_start_ms,
                format!("sandbox setup failed: {}", e),
            )
            .await;
        }
    };
    record.sandbox_dir = sandbox.to_string_lossy().to_string();
    emit_progress(
        &app,
        &run_id,
        "setup",
        "completed",
        &format!("Sandbox ready at {}", sandbox.display()),
        None,
    );

    let (task_model, hivemind_id, hivemind_name, hivemind_models, hivemind_rounds, verifier_model) =
        resolve_model_config(&state).await;
    if task_model.is_empty() {
        return finalize_with_error(
            &app, record, run_start_ms,
            "no Task model configured \u{2014} set one in the Tests screen or pick a default model in Settings"
                .into(),
        ).await;
    }
    if hivemind_models.is_empty() {
        return finalize_with_error(
            &app,
            record,
            run_start_ms,
            "no Hivemind reviewer models configured".into(),
        )
        .await;
    }
    if verifier_model.is_empty() {
        return finalize_with_error(
            &app,
            record,
            run_start_ms,
            "no Verifier model configured".into(),
        )
        .await;
    }

    // Subscribe to chat-event and hivemind-progress streams.
    //
    // Bounded at 4096 to match other event-stream channels — a stalled
    // stability-test consumer (slow disk, parsing back-pressure) won't
    // balloon RAM by buffering a full multi-hour test's chat-event stream.
    // The subscribers use `try_send` and drop with a rate-limited warn on
    // `Full`; see `subscribe_chat_events` / `subscribe_hivemind_events`.
    let (chat_tx, mut chat_rx) = mpsc::channel::<ChatEvent>(4096);
    let (hm_tx, mut hm_rx) = mpsc::channel::<HivemindProgressEvent>(4096);
    let chat_listener_id = subscribe_chat_events(&app, chat_tx);
    let hm_listener_id = subscribe_hivemind_events(&app, hm_tx);

    let session_id = uuid::Uuid::new_v4().to_string();
    record.session_id = Some(session_id.clone());
    record.plan_session_id = Some(session_id.clone());

    // Phase: task intake (planning send).
    emit_progress(
        &app,
        &run_id,
        "task_intake",
        "started",
        "Sending test prompt to planning agent",
        None,
    );
    let send_state = app.state::<AppState>();
    let send_result = send_message(
        app.clone(),
        send_state,
        STABILITY_TEST_PROMPT.to_string(),
        Some(task_model.clone()),
        Some(session_id.clone()),
        Some(sandbox.to_string_lossy().to_string()),
        Some("medium".to_string()),
        Some(PLAN_SYSTEM_PROMPT.to_string()),
        Some("read_only".to_string()),
        None,
        None,
    )
    .await;
    if let Err(e) = send_result {
        app.unlisten(chat_listener_id);
        app.unlisten(hm_listener_id);
        return finalize_with_error(
            &app,
            record,
            run_start_ms,
            format!("send_message failed: {}", e),
        )
        .await;
    }

    // Tag session ownership.
    if let Some(session) = state.pi_manager.get_session(&session_id).await {
        session.set_owner(SessionOwner::Task {
            task_id: record.task_id.clone(),
        });
    }

    let mut questions_text: Option<String> = None;
    let mut plan_text: Option<String> = None;
    let mut planning_done = false;
    let mut error_seen: Option<String> = None;
    let mut tool_calls_observed_planning: u32 = 0;
    let mut auto_answered = false;

    emit_progress(
        &app,
        &run_id,
        "waiting_questions",
        "started",
        "Waiting for clarifying questions",
        None,
    );

    let task_deadline = tokio::time::Instant::now() + TASK_PHASE_TIMEOUT;

    while !planning_done {
        tokio::select! {
            _ = cancel_token.cancelled() => {
                app.unlisten(chat_listener_id);
                app.unlisten(hm_listener_id);
                return finalize_cancelled(&app, record, run_start_ms).await;
            }
            _ = tokio::time::sleep_until(task_deadline) => {
                app.unlisten(chat_listener_id);
                app.unlisten(hm_listener_id);
                return finalize_with_error(&app, record, run_start_ms,
                    "task phase timed out before plan was ready".into()).await;
            }
            evt = chat_rx.recv() => {
                let Some(evt) = evt else { break; };
                if evt.session_id != session_id { continue; }
                match evt.event_type.as_str() {
                    "chunk" => { /* text chunks ignored; tool args carry the payload */ }
                    "structured_stability_questions" => {
                        if questions_text.is_none() {
                            // Re-serialise the questions array so the
                            // downstream `parse_questions` (which expects a
                            // JSON array string) reads it cleanly.
                            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&evt.content) {
                                let questions = v.get("questions").cloned().unwrap_or(v);
                                let serialised = questions.to_string();
                                emit_progress(&app, &run_id, "waiting_questions", "completed",
                                    &format!("Questions tool call received ({} chars)", serialised.len()), None);
                                questions_text = Some(serialised);
                            }
                        }
                    }
                    "structured_stability_plan" => {
                        if plan_text.is_none() {
                            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&evt.content) {
                                if let Some(p) = v.get("plan_markdown").and_then(|x| x.as_str()) {
                                    emit_progress(&app, &run_id, "waiting_plan", "completed",
                                        &format!("Plan tool call received ({} chars)", p.len()), None);
                                    plan_text = Some(p.to_string());
                                }
                            }
                        }
                    }
                    "tool_start" => { tool_calls_observed_planning += 1; }
                    "error" => { error_seen = Some(evt.content.clone()); }
                    "done" => {
                        if questions_text.is_some() && plan_text.is_none() && !auto_answered {
                            let questions = parse_questions(questions_text.as_deref().unwrap_or(""))
                                .unwrap_or_default();
                            let answer_prompt = build_answer_prompt(&questions);
                            emit_progress(&app, &run_id, "auto_answer", "started",
                                &format!("Auto-answering {} question(s)", questions.len()),
                                None);
                            let resume_state = app.state::<AppState>();
                            if let Err(e) = send_message(
                                app.clone(),
                                resume_state,
                                answer_prompt,
                                Some(task_model.clone()),
                                Some(session_id.clone()),
                                Some(sandbox.to_string_lossy().to_string()),
                                Some("medium".to_string()),
                                Some(PLAN_SYSTEM_PROMPT.to_string()),
                                Some("read_only".to_string()),
                                None,
                                None,
                            ).await {
                                error_seen = Some(format!("answer send_message failed: {}", e));
                            }
                            auto_answered = true;
                            emit_progress(&app, &run_id, "waiting_plan", "started",
                                "Waiting for plan block", None);
                        } else if plan_text.is_some() {
                            planning_done = true;
                        } else {
                            planning_done = true;
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    record.gates.push(GateResult {
        name: "questions_emitted".into(),
        passed: questions_text.is_some(),
        detail: questions_text
            .as_ref()
            .map(|q| format!("{} chars", q.len()))
            .unwrap_or_else(|| "no QUESTIONS block observed".into()),
    });
    record.gates.push(GateResult {
        name: "plan_emitted".into(),
        passed: plan_text
            .as_deref()
            .map(|p| p.trim().len() >= 100)
            .unwrap_or(false),
        detail: plan_text
            .as_ref()
            .map(|p| format!("{} chars", p.trim().len()))
            .unwrap_or_else(|| "no PLAN block observed".into()),
    });
    record.gates.push(GateResult {
        name: "planning_tool_calls".into(),
        passed: tool_calls_observed_planning >= 1,
        detail: format!(
            "{} tool call(s) during planning",
            tool_calls_observed_planning
        ),
    });

    let Some(plan_text) = plan_text else {
        app.unlisten(chat_listener_id);
        app.unlisten(hm_listener_id);
        return finalize_with_error(
            &app,
            record,
            run_start_ms,
            format!(
                "plan was never emitted{}",
                error_seen
                    .as_deref()
                    .map(|e| format!(" (chat-event error: {})", e))
                    .unwrap_or_default()
            ),
        )
        .await;
    };

    // Phase: hivemind.
    let hm_label = hivemind_name
        .as_deref()
        .map(|n| format!(" \u{201C}{}\u{201D}", n))
        .unwrap_or_default();
    info!(
        run_id = %run_id,
        hivemind_id = ?hivemind_id,
        hivemind_name = ?hivemind_name,
        models = ?hivemind_models,
        rounds = hivemind_rounds,
        "stability_test: starting hivemind review"
    );
    emit_progress(
        &app,
        &run_id,
        "hivemind",
        "started",
        &format!(
            "Starting Hivemind review{} ({} model(s), {} round(s))",
            hm_label,
            hivemind_models.len(),
            hivemind_rounds
        ),
        None,
    );
    let review_id = uuid::Uuid::new_v4().to_string();
    let hm_state = app.state::<AppState>();
    let hm_job_id = match start_review(
        app.clone(),
        hm_state,
        plan_text.clone(),
        Some("against".to_string()),
        Some(hivemind_rounds),
        Some(HIVEMIND_TIMEOUT.as_secs() as u32),
        Some(hivemind_models.clone()),
        Some(review_id),
        hivemind_id.clone(),
        Some(format!("stability-test-{}", run_id)),
        Some(record.task_id.clone()),
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .await
    {
        Ok(id) => id,
        Err(e) => {
            app.unlisten(chat_listener_id);
            app.unlisten(hm_listener_id);
            return finalize_with_error(
                &app,
                record,
                run_start_ms,
                format!("start_review failed: {}", e),
            )
            .await;
        }
    };
    record.hivemind_job_id = Some(hm_job_id.clone());

    let hivemind_deadline = tokio::time::Instant::now() + HIVEMIND_TIMEOUT;
    let mut hivemind_terminal: Option<String> = None;
    while hivemind_terminal.is_none() {
        tokio::select! {
            _ = cancel_token.cancelled() => {
                app.unlisten(chat_listener_id);
                app.unlisten(hm_listener_id);
                return finalize_cancelled(&app, record, run_start_ms).await;
            }
            _ = tokio::time::sleep_until(hivemind_deadline) => {
                hivemind_terminal = Some("timeout".into());
            }
            evt = hm_rx.recv() => {
                let Some(evt) = evt else { break; };
                if evt.job_id != hm_job_id { continue; }
                match evt.event_type.as_str() {
                    "completed" => hivemind_terminal = Some("completed".into()),
                    "failed" | "error" => hivemind_terminal = Some(format!("failed: {}", evt.message)),
                    "cancelled" => hivemind_terminal = Some("cancelled".into()),
                    _ => {}
                }
            }
        }
    }

    let snap_state = app.state::<AppState>();
    let hivemind_state: Option<ReviewStateSnapshot> =
        get_review_state(snap_state, hm_job_id.clone()).await.ok();
    let final_output_nonempty: Option<String> = hivemind_state
        .as_ref()
        .and_then(|s| s.final_output.as_deref())
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string);
    let merged_plan = final_output_nonempty
        .clone()
        .unwrap_or_else(|| plan_text.clone());
    record.total_cost += hivemind_state.as_ref().map(|s| s.total_cost).unwrap_or(0.0);

    record.gates.push(GateResult {
        name: "hivemind_completed".into(),
        passed: matches!(hivemind_terminal.as_deref(), Some("completed")),
        detail: hivemind_terminal
            .clone()
            .unwrap_or_else(|| "no terminal event".into()),
    });
    record.gates.push(GateResult {
        name: "hivemind_cost_positive".into(),
        passed: hivemind_state
            .as_ref()
            .map(|s| s.total_cost > 0.0)
            .unwrap_or(false),
        detail: hivemind_state
            .as_ref()
            .map(|s| format!("cost ${:.6}", s.total_cost))
            .unwrap_or_else(|| "no state".into()),
    });
    let raw_final_output = hivemind_state
        .as_ref()
        .and_then(|s| s.final_output.as_ref());
    record.gates.push(GateResult {
        name: "hivemind_output_present".into(),
        passed: final_output_nonempty.is_some(),
        detail: match raw_final_output {
            Some(o) if o.trim().is_empty() => "empty merge output — fell back to plan_text".into(),
            Some(o) => format!("{} chars", o.len()),
            None => "no final_output".into(),
        },
    });

    if !matches!(hivemind_terminal.as_deref(), Some("completed")) {
        app.unlisten(chat_listener_id);
        app.unlisten(hm_listener_id);
        return finalize_with_error(
            &app,
            record,
            run_start_ms,
            format!(
                "hivemind did not complete cleanly: {}",
                hivemind_terminal.unwrap_or_else(|| "unknown".into())
            ),
        )
        .await;
    }

    // Phase: implementation.
    //
    // `send_message` ignores `system_prompt` / `tool_set` when reusing an
    // alive session (see commands/chat.rs). The planning session was
    // spawned with the read-only PLAN_SYSTEM_PROMPT + ReadOnlyTools, so
    // reusing it here leaves Pi unable to write files (matches what the
    // Tasks-view UI does at lib/taskRuntime.tsx:5180 — kill plan session,
    // mint a fresh impl session).
    if let Err(e) = state.pi_manager.kill_session(&session_id).await {
        warn!(session_id = %session_id, error = %e,
            "stability_test: failed to kill planning session before impl");
    }
    let impl_session_id = uuid::Uuid::new_v4().to_string();
    record.session_id = Some(impl_session_id.clone());
    emit_progress(
        &app,
        &run_id,
        "implement",
        "started",
        "Sending implementation prompt",
        None,
    );
    let impl_prompt = build_implement_prompt(&merged_plan);
    let impl_state = app.state::<AppState>();
    if let Err(e) = send_message(
        app.clone(),
        impl_state,
        impl_prompt,
        Some(task_model.clone()),
        Some(impl_session_id.clone()),
        Some(sandbox.to_string_lossy().to_string()),
        Some("medium".to_string()),
        Some(IMPL_SYSTEM_PROMPT.to_string()),
        Some("coding".to_string()),
        None,
        None,
    )
    .await
    {
        app.unlisten(chat_listener_id);
        app.unlisten(hm_listener_id);
        return finalize_with_error(
            &app,
            record,
            run_start_ms,
            format!("implement send_message failed: {}", e),
        )
        .await;
    }

    let mut tool_calls_observed_impl: u32 = 0;
    let mut impl_done = false;
    let mut impl_complete_signal = false;
    let impl_deadline = tokio::time::Instant::now() + IMPL_TIMEOUT;
    while !impl_done {
        tokio::select! {
            _ = cancel_token.cancelled() => {
                app.unlisten(chat_listener_id);
                app.unlisten(hm_listener_id);
                return finalize_cancelled(&app, record, run_start_ms).await;
            }
            _ = tokio::time::sleep_until(impl_deadline) => {
                impl_done = true;
            }
            evt = chat_rx.recv() => {
                let Some(evt) = evt else { break; };
                if evt.session_id != impl_session_id { continue; }
                match evt.event_type.as_str() {
                    "chunk" => { /* text chunks unused — completion is signalled via tool call */ }
                    "tool_start" => { tool_calls_observed_impl += 1; }
                    "structured_stability_impl_complete" => { impl_complete_signal = true; }
                    "error" => { error_seen = Some(evt.content.clone()); }
                    "done" => { impl_done = true; }
                    _ => {}
                }
            }
        }
    }

    app.unlisten(chat_listener_id);
    app.unlisten(hm_listener_id);

    let sandbox_modified = sandbox_was_modified(&sandbox, run_start_ms);
    record.gates.push(GateResult {
        name: "impl_tool_calls".into(),
        passed: tool_calls_observed_impl >= 1,
        detail: format!(
            "{} tool call(s) during implementation",
            tool_calls_observed_impl
        ),
    });
    record.gates.push(GateResult {
        name: "impl_complete_signal".into(),
        passed: impl_complete_signal,
        detail: if impl_complete_signal {
            "submit_stability_impl_complete tool call observed".into()
        } else {
            "no submit_stability_impl_complete tool call".into()
        },
    });
    record.gates.push(GateResult {
        name: "sandbox_modified".into(),
        passed: sandbox_modified,
        detail: "modified files newer than run start".into(),
    });
    record.gates.push(GateResult {
        name: "no_pi_errors".into(),
        passed: error_seen.is_none(),
        detail: error_seen
            .clone()
            .unwrap_or_else(|| "no chat-event errors".into()),
    });

    // Phase: AI verifier.
    emit_progress(
        &app,
        &run_id,
        "ai_verify",
        "started",
        "Running AI verifier",
        None,
    );
    let verifier_state_arc = Arc::clone(&state.pi_manager);
    let session_transcript_path = state.chat_sessions_dir.join(format!(
        "{}.jsonl",
        record.session_id.as_deref().unwrap_or("")
    ));
    let plan_transcript_path = record
        .plan_session_id
        .as_deref()
        .map(|sid| state.chat_sessions_dir.join(format!("{}.jsonl", sid)));
    let verdict = run_verifier(
        verifier_state_arc,
        &run_id,
        &record,
        &sandbox,
        &session_transcript_path,
        plan_transcript_path.as_deref(),
        &verifier_model,
        cancel_token.clone(),
    )
    .await
    .unwrap_or_else(|e| VerifierVerdict {
        passed: false,
        confidence: 0.0,
        issues: vec![format!("verifier error: {}", e)],
        summary: "verifier failed to produce a verdict".into(),
    });
    record.verdict = Some(verdict.clone());

    let all_programmatic_passed = record.gates.iter().all(|g| g.passed);
    let final_pass = all_programmatic_passed && verdict.passed;
    record.status = if final_pass {
        "passed".into()
    } else {
        "failed".into()
    };

    finalize(&app, record, run_start_ms).await
}

fn prepare_sandbox(root: &Path, run_id: &str) -> Result<PathBuf> {
    prune_old_sandboxes(root);
    let dir = root.join(run_id);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create sandbox {}", dir.display()))?;
    std::fs::write(
        dir.join("README.md"),
        "Hyvemind stability test sandbox. Auto-created \u{2014} safe to delete.\n",
    )
    .context("failed to write README.md")?;
    std::fs::write(
        dir.join("sample.txt"),
        "Stability test sandbox file.\nThe test agent will edit this file.\n",
    )
    .context("failed to write sample.txt")?;
    Ok(dir)
}

fn prune_old_sandboxes(root: &Path) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    let mut dirs: Vec<(PathBuf, std::time::SystemTime)> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|ft| ft.is_dir()).unwrap_or(false))
        .filter_map(|e| {
            let mtime = e.metadata().and_then(|m| m.modified()).ok()?;
            Some((e.path(), mtime))
        })
        .collect();
    dirs.sort_by(|a, b| b.1.cmp(&a.1));
    for (path, _) in dirs.into_iter().skip(SANDBOX_RETENTION) {
        let _ = std::fs::remove_dir_all(&path);
    }
}

/// Minimal mirror of the per-round model entry inside `rounds_config`.
/// We only need provider + id to flatten into the `start_review` model list.
#[derive(Debug, Deserialize)]
struct StabilityRoundModel {
    #[serde(default)]
    id: String,
    #[serde(default)]
    provider: String,
}

#[derive(Debug, Deserialize)]
struct StabilityRoundConfig {
    #[serde(default)]
    models: Vec<StabilityRoundModel>,
}

async fn resolve_model_config(
    state: &tauri::State<'_, AppState>,
) -> (
    String,         // task_model
    Option<String>, // hivemind_id
    Option<String>, // hivemind_name
    Vec<String>,    // flattened reviewer models ("provider/id")
    u32,            // rounds
    String,         // verifier_model
) {
    let cfg = state.config.read().await;
    let default = cfg.default_model.clone().unwrap_or_default();
    let task_model = if cfg.stability_test.task_model.trim().is_empty() {
        default.clone()
    } else {
        cfg.stability_test.task_model.clone()
    };
    let verifier_model = if cfg.stability_test.verifier_model.trim().is_empty() {
        default.clone()
    } else {
        cfg.stability_test.verifier_model.clone()
    };
    let hivemind_id = cfg
        .stability_test
        .hivemind_id
        .as_ref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    drop(cfg);

    let mut hivemind_name: Option<String> = None;
    let mut models: Vec<String> = Vec::new();
    let mut rounds: u32 = 1;

    if let Some(ref id) = hivemind_id {
        match state.hivemind_store.get_hivemind(id).await {
            Ok(Some(hm)) => {
                hivemind_name = Some(hm.name.clone());
                match serde_json::from_str::<Vec<StabilityRoundConfig>>(&hm.rounds_config) {
                    Ok(parsed) => {
                        rounds = parsed.len().max(1) as u32;
                        let mut seen = std::collections::HashSet::new();
                        for round in &parsed {
                            for m in &round.models {
                                let provider = m.provider.trim();
                                let id = m.id.trim();
                                if provider.is_empty() || id.is_empty() {
                                    continue;
                                }
                                let key = format!("{}/{}", provider, id);
                                if seen.insert(key.clone()) {
                                    models.push(key);
                                }
                            }
                        }
                    }
                    Err(e) => {
                        warn!(
                            hivemind_id = %id,
                            error = %e,
                            "stability_test: failed to parse rounds_config JSON; treating as empty"
                        );
                    }
                }
            }
            Ok(None) => {
                warn!(
                    hivemind_id = %id,
                    "stability_test: configured hivemind_id not found in store"
                );
            }
            Err(e) => {
                warn!(
                    hivemind_id = %id,
                    error = %e,
                    "stability_test: failed to load hivemind from store"
                );
            }
        }
    }

    (
        task_model,
        hivemind_id,
        hivemind_name,
        models,
        rounds,
        verifier_model,
    )
}

/// Rate-limited drop counters for the stability-test event-bridge
/// channels. Shared across all listener invocations so the warn line
/// reports cumulative drops rather than per-event.
static CHAT_EVENT_DROP_WARN: crate::state::channel_drop::DropWarner =
    crate::state::channel_drop::DropWarner::new("stability_test_chat_event");
static HM_EVENT_DROP_WARN: crate::state::channel_drop::DropWarner =
    crate::state::channel_drop::DropWarner::new("stability_test_hm_event");

fn subscribe_chat_events(app: &tauri::AppHandle, tx: mpsc::Sender<ChatEvent>) -> tauri::EventId {
    app.listen("chat-event", move |event| {
        if let Ok(payload) = serde_json::from_str::<ChatEvent>(event.payload()) {
            // try_send avoids blocking the Tauri event dispatcher on a
            // slow stability-test consumer. Drops are surfaced via the
            // shared rate-limited warner; Closed is silent because
            // listener cleanup races with channel teardown at end-of-run.
            match tx.try_send(payload) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => CHAT_EVENT_DROP_WARN.note_drop(),
                Err(mpsc::error::TrySendError::Closed(_)) => {}
            }
        }
    })
}

fn subscribe_hivemind_events(
    app: &tauri::AppHandle,
    tx: mpsc::Sender<HivemindProgressEvent>,
) -> tauri::EventId {
    app.listen("hivemind-progress", move |event| {
        if let Ok(payload) = serde_json::from_str::<HivemindProgressEvent>(event.payload()) {
            match tx.try_send(payload) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => HM_EVENT_DROP_WARN.note_drop(),
                Err(mpsc::error::TrySendError::Closed(_)) => {}
            }
        }
    })
}

#[derive(Debug, Clone, Deserialize)]
struct TestQuestionOption {
    #[allow(dead_code)]
    id: String,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    recommended: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct TestQuestion {
    id: String,
    kind: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    options: Vec<TestQuestionOption>,
}

fn parse_questions(json: &str) -> Option<Vec<TestQuestion>> {
    serde_json::from_str::<Vec<TestQuestion>>(json).ok()
}

fn build_answer_prompt(questions: &[TestQuestion]) -> String {
    let mut lines = vec![
        "Here are my answers to your questions:".to_string(),
        String::new(),
    ];
    for q in questions {
        lines.push(format!(
            "Q: {}",
            q.title.clone().unwrap_or_else(|| q.id.clone())
        ));
        match q.kind.as_str() {
            "choice" => {
                let pick = q
                    .options
                    .iter()
                    .find(|o| o.recommended)
                    .or_else(|| q.options.first());
                if let Some(opt) = pick {
                    lines.push(format!(
                        "A: {}",
                        opt.label.clone().unwrap_or_else(|| opt.id.clone())
                    ));
                } else {
                    lines.push("A: (no options provided)".into());
                }
            }
            _ => {
                lines.push("A: Use sensible defaults \u{2014} keep it minimal.".into());
            }
        }
        lines.push(String::new());
    }
    lines.push("Please proceed with the plan based on these answers.".into());
    lines.join("\n")
}

fn build_implement_prompt(plan_text: &str) -> String {
    format!(
        "You have been given the following implementation plan. Execute it step by step.\n\n\
        Do NOT re-plan or ask clarifying questions \u{2014} just implement exactly what the plan describes.\n\n\
        ---\n\n{}\n\n---\n\n\
        Begin implementing now. When you have completed ALL steps in the plan, call \
        `submit_stability_impl_complete({{}})` to signal completion.",
        plan_text
    )
}

fn sandbox_was_modified(dir: &Path, since_ms: u64) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let Ok(modified) = meta.modified() else {
            continue;
        };
        let Ok(epoch) = modified.duration_since(std::time::UNIX_EPOCH) else {
            continue;
        };
        if epoch.as_millis() as u64 > since_ms {
            return true;
        }
    }
    false
}

async fn run_verifier(
    pi_manager: Arc<crate::pi::manager::PiManager>,
    run_id: &str,
    record: &TestRunRecord,
    sandbox: &Path,
    session_transcript_path: &Path,
    plan_transcript_path: Option<&Path>,
    verifier_model: &str,
    cancel_token: CancellationToken,
) -> Result<VerifierVerdict> {
    if verifier_model.trim().is_empty() {
        return Err(anyhow!("verifier model not configured"));
    }

    let verifier_session_id = format!("stability-test-verifier-{}", run_id);
    let mut tools: Vec<String> = vec![
        "read".to_string(),
        "bash".to_string(),
        "grep".to_string(),
        "find".to_string(),
        "ls".to_string(),
    ];
    tools.extend(
        crate::pi::rpc::HYVEMIND_EXTENSION_TOOLS
            .iter()
            .map(|s| s.to_string()),
    );
    let options = PiSessionOptions {
        model: verifier_model.to_string(),
        thinking_level: ThinkingLevel::Medium,
        tool_set: ToolSet::Custom(tools),
        system_prompt: Some(STABILITY_TEST_VERIFIER_PROMPT.to_string()),
        resume_session: false,
        session_file: None,
    };
    let session = pi_manager
        .spawn_session_with_options(&verifier_session_id, &options, sandbox)
        .await
        .map_err(|e| anyhow!("failed to spawn verifier session: {}", e))?;
    session.set_owner(SessionOwner::Unknown);

    let plan_transcript_line = match plan_transcript_path {
        Some(p) => format!("Plan-phase Pi transcript: `{}`\n", p.display()),
        None => String::new(),
    };
    let user_prompt = format!(
        "## Stability test verification\n\n\
        Run ID: {}\n\
        Task ID: {}\n\
        Implementation Pi session: {}\n\
        Plan Pi session: {}\n\
        Sandbox: {}\n\
        Hivemind job: {}\n\n\
        ## Programmatic gates\n\n{}\n\n\
        ## Your job\n\n\
        Verify the test ran correctly by inspecting:\n\
        1. The implementation Pi transcript at `{}` (cat / grep it; it's a JSONL of Pi events).\n\
        {}\
        2. The sandbox directory `{}` \u{2014} `sample.txt` should have been edited.\n\
        3. Look for any signs of failure that the programmatic gates may have missed (gibberish output, refused tasks, etc.).\n\n\
        Submit your verdict by calling `submit_stability_verdict({{passed, confidence, issues, summary}})`. There is no fallback — the tool call is mandatory.",
        record.run_id,
        record.task_id,
        record.session_id.as_deref().unwrap_or("(none)"),
        record.plan_session_id.as_deref().unwrap_or("(none)"),
        sandbox.display(),
        record.hivemind_job_id.as_deref().unwrap_or("(none)"),
        record
            .gates
            .iter()
            .map(|g| format!("- {} = {} ({})", g.name, if g.passed { "PASS" } else { "FAIL" }, g.detail))
            .collect::<Vec<_>>()
            .join("\n"),
        session_transcript_path.display(),
        plan_transcript_line,
        sandbox.display(),
    );

    if let Err(e) = session.send_prompt(&user_prompt, None).await {
        // Capture stderr before the session is dropped — send_prompt
        // failures (e.g. provider auth error, unknown model) may leave
        // diagnostic info in stderr.
        let stderr = session.stderr_snapshot().await;
        let _ = pi_manager.kill_session(&verifier_session_id).await;
        let detail = if stderr.trim().is_empty() {
            format!("verifier send_prompt failed: {}", e)
        } else {
            format!(
                "verifier send_prompt failed: {} (stderr: {})",
                e,
                stderr.trim()
            )
        };
        return Err(anyhow!("{}", detail));
    }

    let _response = tokio::select! {
        _ = cancel_token.cancelled() => {
            let _ = pi_manager.kill_session(&verifier_session_id).await;
            return Err(anyhow!("verifier cancelled"));
        }
        r = tokio::time::timeout(VERIFIER_TIMEOUT, session.collect_response()) => {
            match r {
                Ok(Ok(text)) => text,
                Ok(Err(e)) => {
                    // Capture stderr snapshot while the process is still alive, then kill.
                    let stderr_captured = session.stderr_snapshot().await;
                    let _ = pi_manager.kill_session(&verifier_session_id).await;
                    let detail = if stderr_captured.trim().is_empty() {
                        format!("{}", e)
                    } else {
                        // When stderr is available, prefer it over the generic
                        // "pi process stdout closed unexpectedly" message from the
                        // stdout-reader task.
                        format!("process crashed: {}", stderr_captured.trim())
                    };
                    return Err(anyhow!("verifier collect_response failed: {}", detail));
                }
                Err(_) => {
                    // The session may have crashed silently during timeout — capture
                    // stderr before killing in case the Pi process died with a diagnostic.
                    let stderr_captured = session.stderr_snapshot().await;
                    let _ = pi_manager.kill_session(&verifier_session_id).await;
                    let detail = if stderr_captured.trim().is_empty() {
                        "verifier timed out".to_string()
                    } else {
                        format!("verifier timed out (stderr: {})", stderr_captured.trim())
                    };
                    return Err(anyhow!("{}", detail));
                }
            }
        }
    };

    // The verifier MUST call `submit_stability_verdict`.
    let tool_args = session.take_tool_args("submit_stability_verdict");
    let _ = pi_manager.kill_session(&verifier_session_id).await;
    let args =
        tool_args.ok_or_else(|| anyhow!("verifier did not call submit_stability_verdict"))?;
    serde_json::from_value::<VerifierVerdict>(args.clone())
        .with_context(|| format!("verdict args failed to deserialise: {}", args))
}

fn emit_progress(
    app: &tauri::AppHandle,
    run_id: &str,
    phase: &str,
    status: &str,
    message: &str,
    extra: Option<serde_json::Value>,
) {
    let mut payload = serde_json::json!({
        "run_id": run_id,
        "phase": phase,
        "status": status,
        "message": message,
    });
    if let Some(e) = extra {
        if let (Some(obj), Some(eobj)) = (payload.as_object_mut(), e.as_object()) {
            for (k, v) in eobj {
                obj.insert(k.clone(), v.clone());
            }
        }
    }
    let _ = app.emit("test-progress", payload);

    // Mirror the latest snapshot onto `state.active_test_run` so a late
    // subscriber (e.g. the frontend rehydrating after a tab switch or
    // app restart) sees a coherent panel without waiting for the next
    // event. Coarse-grained events — write lock contention is a non-issue.
    let state = app.state::<AppState>();
    let active = Arc::clone(&state.active_test_run);
    let run_id = run_id.to_string();
    let phase = phase.to_string();
    let status = status.to_string();
    let message = message.to_string();
    tokio::spawn(async move {
        let mut guard = active.write().await;
        if let Some(rec) = guard.as_mut() {
            if rec.run_id == run_id {
                rec.last_phase = Some(phase);
                rec.last_status = Some(status);
                rec.last_message = Some(message);
            }
        }
    });
}

async fn finalize(
    app: &tauri::AppHandle,
    mut record: TestRunRecord,
    started_at_ms: u64,
) -> Result<TestRunRecord> {
    let now_ms = chrono::Utc::now().timestamp_millis() as u64;
    record.duration_ms = now_ms.saturating_sub(started_at_ms);
    record.completed_at = Some(chrono::Utc::now().to_rfc3339());
    let state = app.state::<AppState>();
    let path = state.test_runs_dir.join(format!("{}.json", record.run_id));
    if let Ok(json) = serde_json::to_vec_pretty(&record) {
        if let Err(e) = atomic_write(&path, &json).await {
            warn!(error = %e, path = %path.display(), "failed to persist test run record");
        }
    }
    let active = Arc::clone(&state.active_test_run);
    let run_id = record.run_id.clone();
    tokio::spawn(async move {
        let mut guard = active.write().await;
        if guard.as_ref().map(|r| r.run_id == run_id).unwrap_or(false) {
            *guard = None;
        }
    });
    let status = record.status.clone();
    let detail = record
        .verdict
        .as_ref()
        .map(|v| v.summary.clone())
        .unwrap_or_else(|| "see gate detail".into());
    emit_progress(
        app,
        &record.run_id,
        if status == "passed" {
            "complete"
        } else {
            "failed"
        },
        if status == "passed" {
            "completed"
        } else {
            "failed"
        },
        &format!("Test {}: {}", status, detail),
        Some(serde_json::json!({ "record": &record })),
    );
    info!(run_id = %record.run_id, status = %status, "stability test finished");
    Ok(record)
}

async fn finalize_with_error(
    app: &tauri::AppHandle,
    mut record: TestRunRecord,
    started_at_ms: u64,
    error: String,
) -> Result<TestRunRecord> {
    record.status = "error".into();
    record.error = Some(error.clone());
    record.gates.push(GateResult {
        name: "fatal_error".into(),
        passed: false,
        detail: error,
    });
    finalize(app, record, started_at_ms).await
}

async fn finalize_cancelled(
    app: &tauri::AppHandle,
    mut record: TestRunRecord,
    started_at_ms: u64,
) -> Result<TestRunRecord> {
    record.status = "cancelled".into();
    finalize(app, record, started_at_ms).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_answer_prompt_picks_recommended_option() {
        let questions = vec![
            TestQuestion {
                id: "q1".into(),
                kind: "choice".into(),
                title: Some("Style?".into()),
                options: vec![
                    TestQuestionOption {
                        id: "a".into(),
                        label: Some("Plain".into()),
                        recommended: false,
                    },
                    TestQuestionOption {
                        id: "b".into(),
                        label: Some("Excited".into()),
                        recommended: true,
                    },
                ],
            },
            TestQuestion {
                id: "q2".into(),
                kind: "text".into(),
                title: Some("Any preferences?".into()),
                options: vec![],
            },
        ];
        let prompt = build_answer_prompt(&questions);
        assert!(prompt.contains("A: Excited"));
        assert!(prompt.contains("Use sensible defaults"));
    }

    #[test]
    fn build_answer_prompt_falls_back_to_first_option_when_no_recommended() {
        let questions = vec![TestQuestion {
            id: "q1".into(),
            kind: "choice".into(),
            title: Some("Pick".into()),
            options: vec![
                TestQuestionOption {
                    id: "a".into(),
                    label: Some("First".into()),
                    recommended: false,
                },
                TestQuestionOption {
                    id: "b".into(),
                    label: Some("Second".into()),
                    recommended: false,
                },
            ],
        }];
        let prompt = build_answer_prompt(&questions);
        assert!(prompt.contains("A: First"));
    }
}
