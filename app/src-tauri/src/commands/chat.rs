// Internal name — surfaces as "Tasks" in the UI. See PRODUCT.md §3.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tauri::Emitter;
use tauri::Manager;
use tracing::{debug, error, info, warn, Instrument};

use crate::commands::util::{validate_id, validate_session_id};
use crate::hivemind::merge_capture::MergeCapture;
use crate::pi::events::PiEvent;
use crate::pi::manager::GraveyardEntry;
use crate::pi::rpc::{PiImage, PiSessionOptions, ThinkingLevel, ToolSet};
use crate::pi::session::SessionOwner;
use crate::state::app_state::AppState;
use crate::state::ipc_error::IpcError;
use std::collections::HashMap;

/// Maximum length (in bytes) of a chat message accepted from the frontend.
/// This is a host-side flood guard, not a model context-window cap.
/// Sized generously (16 MiB) because Hivemind merge prompts bundle the plan,
/// the source-context bundle, and N reviewer outputs into a single message;
/// modest projects can legitimately produce multi-megabyte payloads. The
/// per-merge token budget (see `truncateMergePrompt` in review-mode.ts and
/// the reviewer-loop budget in hivemind/engine.rs) is what keeps real flows
/// within the model's context window; this cap is purely defensive against
/// a runaway/buggy UI.
const MAX_MESSAGE_LEN: usize = 16 * 1024 * 1024;

/// Per-stream coalescer for `TextDelta` / `ThinkingDelta` Tauri emits.
/// The Pi runtime fires these events at token frequency (tens per second).
/// Emitting one `chat-event` per token saturates the Tauri main thread and
/// the renderer; batching them by size + time cuts IPC volume ~10x while
/// keeping the streaming feel responsive (≤50ms grain).
struct DeltaCoalescer {
    text_buf: String,
    thinking_buf: String,
    text_buf_started_at: Option<std::time::Instant>,
    thinking_buf_started_at: Option<std::time::Instant>,
}

impl DeltaCoalescer {
    const FLUSH_BYTES: usize = 256;
    const FLUSH_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);

    fn new() -> Self {
        Self {
            text_buf: String::new(),
            thinking_buf: String::new(),
            text_buf_started_at: None,
            thinking_buf_started_at: None,
        }
    }

    fn push_text(&mut self, s: &str) {
        if self.text_buf.is_empty() {
            self.text_buf_started_at = Some(std::time::Instant::now());
        }
        self.text_buf.push_str(s);
    }

    fn push_thinking(&mut self, s: &str) {
        if self.thinking_buf.is_empty() {
            self.thinking_buf_started_at = Some(std::time::Instant::now());
        }
        self.thinking_buf.push_str(s);
    }

    fn text_ready(&self) -> bool {
        if self.text_buf.is_empty() {
            return false;
        }
        if self.text_buf.len() >= Self::FLUSH_BYTES {
            return true;
        }
        self.text_buf_started_at
            .map(|t| t.elapsed() >= Self::FLUSH_INTERVAL)
            .unwrap_or(false)
    }

    fn thinking_ready(&self) -> bool {
        if self.thinking_buf.is_empty() {
            return false;
        }
        if self.thinking_buf.len() >= Self::FLUSH_BYTES {
            return true;
        }
        self.thinking_buf_started_at
            .map(|t| t.elapsed() >= Self::FLUSH_INTERVAL)
            .unwrap_or(false)
    }

    fn flush_text(&mut self, app: &tauri::AppHandle, sid: &str) {
        if self.text_buf.is_empty() {
            return;
        }
        let payload = std::mem::take(&mut self.text_buf);
        self.text_buf_started_at = None;
        let _ = app.emit(
            "chat-event",
            ChatEvent {
                session_id: sid.to_string(),
                event_type: "chunk".to_string(),
                content: payload,
            },
        );
    }

    fn flush_thinking(&mut self, app: &tauri::AppHandle, sid: &str) {
        if self.thinking_buf.is_empty() {
            return;
        }
        let payload = std::mem::take(&mut self.thinking_buf);
        self.thinking_buf_started_at = None;
        let _ = app.emit(
            "chat-event",
            ChatEvent {
                session_id: sid.to_string(),
                event_type: "thinking".to_string(),
                content: payload,
            },
        );
    }

    fn flush_all(&mut self, app: &tauri::AppHandle, sid: &str) {
        self.flush_text(app, sid);
        self.flush_thinking(app, sid);
    }
}

/// Per-send phase-tracking state. Drives the new `phase` / `heartbeat`
/// chat-events that surface Pi session lifecycle to the UI. Shared
/// between the streaming closure (single producer, fires on every
/// `PiEvent`), the periodic usage poller (writes `context_loaded_sent`),
/// and the heartbeat ticker (reads `current_phase` + `last_pi_event_at`).
///
/// Wrapped in `Arc<Mutex<>>` at the call site. All transitions are
/// once-only (idempotent flags) except `msg_starts_in_turn` which
/// resets on every `TurnStart` and `tool_running` which legitimately
/// re-fires per `ToolExecutionStart`.
struct PhaseState {
    agent_ready_sent: bool,
    prompt_loaded_sent: bool,
    awaiting_model_sent: bool,
    first_thinking_sent: bool,
    first_text_sent: bool,
    context_loaded_sent: bool,
    /// Per-turn counter — 0 before any MessageStart in the current turn,
    /// 1 after the user MessageStart, 2+ after the assistant MessageStart.
    msg_starts_in_turn: u32,
    /// Last emitted phase string; drives the heartbeat label.
    current_phase: &'static str,
    /// Wall-clock anchor for elapsed time in heartbeat emits.
    send_started_at: std::time::Instant,
    /// Updated on every received PiEvent. Heartbeat ticker uses this
    /// to compute `silent_ms`.
    last_pi_event_at: std::time::Instant,
    /// True once the agent has called `submit_task_complete` in this
    /// `send_message` turn. The model occasionally calls the tool more than
    /// once; the second call would render a second "Task Complete" chip and
    /// a duplicate tool row. We track it here so we can suppress both the
    /// `structured_task_complete` and `tool_start` emits for the duplicate.
    /// Reset implicitly because `PhaseState::new()` runs per send_message.
    task_complete_emitted: bool,
}

impl PhaseState {
    fn new() -> Self {
        let now = std::time::Instant::now();
        Self {
            agent_ready_sent: false,
            prompt_loaded_sent: false,
            awaiting_model_sent: false,
            first_thinking_sent: false,
            first_text_sent: false,
            context_loaded_sent: false,
            msg_starts_in_turn: 0,
            current_phase: "agent_starting",
            send_started_at: now,
            last_pi_event_at: now,
            task_complete_emitted: false,
        }
    }
}

/// Free helper that emits a single `phase` chat-event. Kept as a free
/// Map a Hyvemind extension tool name to the `event_type` the frontend
/// reducer uses for structured payloads. Returns `None` for built-in Pi
/// tools and any unrecognised name — those keep using the legacy
/// `tool_start` / delimiter-extraction path.
///
/// The four planning tools (Phase 3) are consumed by the frontend
/// reducer (`taskReducer.mapChatEventToTaskEvent`). The three stability
/// tools (Phase 4) are consumed by the stability-test runner
/// (`core::stability_test::runner`). The Tasks-view implementation
/// completion signal (`submit_task_complete`) is also consumed by the
/// frontend reducer (`taskReducer` case `"structured_task_complete"`) and
/// is the sole signal that marks an implementation run done — there is
/// no text-scanning fallback. All three kinds emit a JSON-stringified
/// `args` payload as the `chat-event` `content`.
fn structured_tool_event_type(name: &str) -> Option<&'static str> {
    match name {
        // Phase 3 — Tasks-view planning
        "submit_task_meta" => Some("structured_task_meta"),
        "submit_questions" => Some("structured_questions"),
        "submit_plan" => Some("structured_plan"),
        "submit_features" => Some("structured_features"),
        // Tasks-view implementation completion signal
        "submit_task_complete" => Some("structured_task_complete"),
        // Phase 4 — stability-test surfaces
        "submit_stability_questions" => Some("structured_stability_questions"),
        "submit_stability_plan" => Some("structured_stability_plan"),
        "submit_stability_verdict" => Some("structured_stability_verdict"),
        "submit_stability_impl_complete" => Some("structured_stability_impl_complete"),
        // Phase 8 — Tasks-view Hivemind review context-gather
        "submit_review_prompt" => Some("structured_review_prompt"),
        "submit_verdicts" => Some("structured_verdicts"),
        _ => None,
    }
}

/// fn (not a closure) because the streaming callback is `FnMut` and a
/// closure capturing `app_clone`/`sid_clone` by reference would clash
/// with the mutex borrow on `PhaseState` inside the same arm.
fn emit_phase(app: &tauri::AppHandle, sid: &str, content: &'static str) {
    let _ = app.emit(
        "chat-event",
        ChatEvent {
            session_id: sid.to_string(),
            event_type: "phase".to_string(),
            content: content.to_string(),
        },
    );
}

/// Validate a working-directory string from the frontend, enforcing the
/// `Config::approved_working_dirs` allowlist (audit item 1.11).
///
/// Trims whitespace, rejects empty/null-byte input, expands a leading `~` to
/// the user's home directory, canonicalizes the result, and asserts the
/// canonical path equals — or is a strict descendant of — an approved entry.
/// Returns an error if the path is not an existing directory or not approved.
async fn validate_working_dir(
    state: &tauri::State<'_, AppState>,
    p: &str,
) -> Result<std::path::PathBuf, IpcError> {
    let mut approved = state.config.read().await.approved_working_dirs.clone();
    // Always trust the per-app test sandbox root. The Tests screen scaffolds
    // a fresh `~/.hyvemind/test-sandbox/{run_id}/` per run via
    // `core::stability_test::runner::prepare_sandbox`, so we must accept any
    // descendant of `state.test_sandbox_dir` without requiring the user to
    // approve it through ProjectPicker. The path is derived from
    // `data_dir.join("test-sandbox")` — never hardcoded.
    approved.push(state.test_sandbox_dir.clone());
    crate::commands::util::validate_approved_working_dir(p, &approved)
        .map_err(IpcError::not_approved)
}

/// Extract a short human-readable summary from Pi's `auto_retry_start`
/// `errorMessage` field. The value is itself a stringified JSON envelope
/// from the underlying provider (e.g. Anthropic returns
/// `{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"},...}`).
/// We try to map the inner `error.type` to a friendly label; on parse
/// failure we return a generic fallback so the UI always has *something*
/// to display alongside the spinner.
fn humanize_pi_error(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "Pi process exited before responding".to_string();
    }
    for prefix in [
        "pi process is not available:",
        "failed to send prompt:",
        "stdin closed:",
    ] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            return humanize_pi_error(rest);
        }
    }
    if let Some(idx) = trimmed.find("Error: Model ") {
        return trimmed[idx..].trim().to_string();
    }
    if let Some(idx) = trimmed.find("Model ") {
        let tail = &trimmed[idx..];
        if tail.contains("not found") {
            return tail.trim().to_string();
        }
    }
    trimmed.to_string()
}

pub fn summarize_retry_error(raw: &str) -> String {
    let trimmed = raw.trim();
    // Pi sometimes prefixes the provider envelope with the HTTP status
    // code, e.g. `400 {"type":"error",...}` (seen on the `type:"message"
    // stopReason:"error"` path). Strip any leading non-JSON noise before
    // trying to parse.
    let candidate = match trimmed.find('{') {
        Some(idx) => trimmed[idx..].trim(),
        None => trimmed,
    };
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(candidate) {
        let err = v.get("error").unwrap_or(&v);
        let kind = err.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let msg = err.get("message").and_then(|m| m.as_str()).unwrap_or("");
        match kind {
            "overloaded_error" => return "Server overloaded".to_string(),
            "rate_limit_error" => return "Rate limited".to_string(),
            "invalid_request_error" if !msg.is_empty() => return msg.to_string(),
            "api_error" if !msg.is_empty() => return format!("Provider error: {}", msg),
            _ => {}
        }
        if !msg.is_empty() {
            return msg.to_string();
        }
    }
    // Fall back to a slice of the raw payload (capped) so debugging is
    // still possible without leaking a multi-KB blob into the UI.
    let preview: String = trimmed.chars().take(160).collect();
    if preview.is_empty() {
        "Provider error".to_string()
    } else {
        preview
    }
}

/// Merge a graveyard entry's recovered options into the request-built
/// options. Request fields always win; graveyard only fills the gaps for
/// fields the request didn't explicitly supply. Returns the input
/// `options` unchanged when no graveyard entry exists.
///
/// The `*_supplied` booleans capture whether the original send_message
/// request carried that field — we can't infer this from the built
/// `PiSessionOptions` because `tool_set` and `thinking_level` have
/// non-`Option` types whose defaults are indistinguishable from "user
/// chose this default explicitly".
pub fn merge_graveyard_into(
    mut options: PiSessionOptions,
    graveyard: Option<&GraveyardEntry>,
    system_prompt_supplied: bool,
    tool_set_supplied: bool,
    thinking_supplied: bool,
) -> PiSessionOptions {
    let Some(ge) = graveyard else {
        return options;
    };
    if !system_prompt_supplied && ge.options.system_prompt.is_some() {
        options.system_prompt = ge.options.system_prompt.clone();
    }
    if !thinking_supplied {
        options.thinking_level = ge.options.thinking_level.clone();
    }
    if !tool_set_supplied {
        options.tool_set = ge.options.tool_set.clone();
    }
    // A graveyard entry only exists when an on-disk transcript was
    // reconciled (or a live session was evicted). In every case the next
    // spawn MUST resume from that transcript — otherwise Pi starts a
    // fresh conversation and the user's prior turns vanish.
    options.resume_session = true;
    options
}

// The subscription-provider name mapping (`chatgpt` → `openai-codex`,
// `claude-sub` → `anthropic`) lives in `crate::pi::rpc::map_model_for_pi`
// and is now applied automatically by every `PiSessionOptions::for_*`
// constructor. Call sites still alias `map_model_for_pi` here so the
// existing local references in this module keep compiling.
use crate::pi::rpc::map_model_for_pi;

/// An image payload received from the frontend via Tauri IPC.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImagePayload {
    pub media_type: String,
    pub data: String,
}

impl ImagePayload {
    /// Convert to Pi SDK `ImageContent` format.
    fn to_pi_image(&self) -> PiImage {
        PiImage {
            kind: "image".to_string(),
            data: self.data.clone(),
            mime_type: self.media_type.clone(),
        }
    }
}

/// A single message in a chat conversation.
#[derive(Debug, Clone, Serialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    pub timestamp: DateTime<Utc>,
}

/// An event emitted during streaming chat responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatEvent {
    pub session_id: String,
    pub event_type: String,
    pub content: String,
}

/// Send a message to a Pi session and stream the response.
///
/// Creates a new Pi session (or reuses an existing one if `session_id` is
/// provided and already active), then spawns a background task that sends
/// the message and streams events to the frontend via `"chat-event"` Tauri
/// events in real time.
///
/// Returns the session ID immediately so the frontend can register its
/// event listener before any chunks arrive. The actual response is delivered
/// entirely through `"chat-event"` emissions (start → chunks → done/error).
#[tracing::instrument(
    skip(app, state),
    fields(
        message_len = message.len(),
        model = ?model,
        session_id = %session_id.as_deref().unwrap_or("none"),
    )
)]
#[tauri::command]
pub async fn send_message(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    message: String,
    model: Option<String>,
    session_id: Option<String>,
    working_dir: Option<String>,
    thinking_level: Option<String>,
    system_prompt: Option<String>,
    tool_set: Option<String>,
    images: Option<Vec<ImagePayload>>,
    is_steer: Option<bool>,
) -> Result<String, IpcError> {
    let is_steer = is_steer.unwrap_or(false);
    // Reject oversized messages early so the frontend gets a clear error
    // rather than a confusing failure deeper in the Pi pipeline.
    if message.len() > MAX_MESSAGE_LEN {
        return Err(IpcError::validation(format!(
            "message too long: {} bytes (~{}k chars, max {} bytes / {} MiB). This is a host-side flood guard, not a model context-window error; if you hit it during a Hivemind merge the review's source bundle is unusually large.",
            message.len(),
            message.len() / 1024,
            MAX_MESSAGE_LEN,
            MAX_MESSAGE_LEN / (1024 * 1024)
        )));
    }
    if message.len() > MAX_MESSAGE_LEN * 4 / 5 {
        warn!(
            len = message.len(),
            max = MAX_MESSAGE_LEN,
            "message approaching host-side byte cap"
        );
    }

    // Reject malformed/traversal session_ids before they are joined into the
    // chat-sessions directory path (`~/.hyvemind/chat-sessions/{sid}.jsonl`).
    if let Some(ref sid) = session_id {
        validate_id(sid).map_err(IpcError::validation)?;
    }

    let effective_model = {
        let config = state.config.read().await;
        model.unwrap_or_else(|| {
            config
                .default_model
                .clone()
                .unwrap_or_else(|| "claude-sonnet-4-20250514".to_string())
        })
    };
    // Map subscription provider names for Pi (chatgpt → openai-codex, claude-sub → anthropic)
    let pi_model = map_model_for_pi(&effective_model);

    let sid = session_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    info!(
        model = %effective_model,
        session_id = %sid,
        raw_working_dir = ?working_dir,
        "send_message invoked"
    );

    // Use the project directory from the frontend if provided, otherwise fall
    // back to cwd. Validate (trim, expand ~, canonicalize, must-be-dir) before
    // handing the path to Pi so a buggy frontend can't crash the subprocess
    // with an unusable cwd.
    let working_dir = match working_dir {
        Some(s) => validate_working_dir(&state, &s).await?,
        None => {
            // Audit 1.11: an unspecified `working_dir` falls back to the
            // app's CWD; we still enforce the approved-dirs allowlist so an
            // attacker can't escape it by simply omitting the field. If
            // CWD isn't approved we reject — the frontend MUST send an
            // explicit, approved path (it does, via ProjectPicker).
            let cwd = std::env::current_dir().map_err(|e| {
                IpcError::internal(format!("failed to resolve current directory: {}", e))
            })?;
            let mut approved = state.config.read().await.approved_working_dirs.clone();
            approved.push(state.test_sandbox_dir.clone());
            if !crate::commands::util::is_under_approved_root(&cwd, &approved) {
                return Err(IpcError::not_approved(format!(
                    "working directory not approved: {} (cwd fallback; approve a project via the picker first)",
                    cwd.display()
                )));
            }
            cwd
        }
    };

    info!(
        session_id = %sid,
        working_dir = %working_dir.display(),
        working_dir_exists = working_dir.exists(),
        "resolved working directory"
    );

    let parsed_thinking: Option<ThinkingLevel> = thinking_level
        .as_deref()
        .map(|s| s.parse().unwrap_or(ThinkingLevel::default()));

    let parsed_tool_set: Option<ToolSet> = tool_set.as_deref().map(|s| match s {
        "coding" => ToolSet::CodingTools,
        "read_only" => ToolSet::ReadOnlyTools,
        other => ToolSet::Custom(other.split(',').map(|t| t.trim().to_string()).collect()),
    });

    if system_prompt.is_some() || tool_set.is_some() {
        info!(
            session_id = %sid,
            has_system_prompt = system_prompt.is_some(),
            system_prompt_len = system_prompt.as_ref().map(|s| s.len()),
            tool_set = ?tool_set,
            "custom system_prompt/tool_set provided"
        );
    }
    if let Some(ref sp) = system_prompt {
        debug!(session_id = %sid, system_prompt = %sp, "full system prompt");
    }

    // Best-effort graveyard probe: if a maintenance sweep killed this
    // session previously, the next send_message respawns with the same
    // options the user originally chose.
    let graveyard_entry = state.pi_manager.exhume(&sid).await;

    // Check if session already exists (for follow-up messages in the same conversation).
    // If the existing session's process is dead, remove it and spawn a fresh one.
    let session = if let Some(existing) = state.pi_manager.get_session(&sid).await {
        if existing.is_alive() {
            info!(session_id = %sid, "reusing existing Pi session");
            // Update thinking level at runtime if specified
            if let Some(ref level) = parsed_thinking {
                if let Err(e) = existing.set_thinking_level(level.clone()).await {
                    info!(session_id = %sid, error = %e, "failed to set thinking level on existing session");
                }
            }
            if system_prompt.is_some() || parsed_tool_set.is_some() {
                info!(session_id = %sid, "system_prompt/tool_set ignored for existing alive session");
            }
            existing
        } else {
            info!(session_id = %sid, "existing session is dead, respawning");
            let _ = state.pi_manager.kill_session(&sid).await;

            let session_path = state.chat_sessions_dir.join(format!("{}.jsonl", &sid));
            let session_path_str = session_path.display().to_string();

            let mut options = PiSessionOptions::for_model(&pi_model)
                .with_session_file(session_path_str)
                .with_resume();
            if let Some(ref level) = parsed_thinking {
                options = options.with_thinking_level(level.clone());
            }
            if let Some(ref ts) = parsed_tool_set {
                options = options.with_tool_set(ts.clone());
            }
            if let Some(ref sp) = system_prompt {
                options = options.with_system_prompt(sp.clone());
            }

            // Use the request-built options as the base so this turn's
            // explicit choices (model, system_prompt, tool_set, thinking)
            // take precedence. Fall back to graveyard only for fields the
            // request did NOT supply — earlier versions cloned the entry
            // wholesale, which stripped the system_prompt the frontend
            // sent and made Anthropic's subscription gateway reject the
            // request with a generic "out of extra usage" 400.
            let options = merge_graveyard_into(
                options,
                graveyard_entry.as_ref(),
                system_prompt.is_some(),
                parsed_tool_set.is_some(),
                parsed_thinking.is_some(),
            );

            state
                .pi_manager
                .spawn_session_with_options(&sid, &options, &working_dir)
                .await
                .map_err(|e| {
                    IpcError::from_provider_error(format!("failed to respawn session: {}", e))
                        .with_id(sid.clone())
                })?
        }
    } else {
        let session_path = state.chat_sessions_dir.join(format!("{}.jsonl", &sid));
        let session_path_str = session_path.display().to_string();
        let file_exists = session_path.exists();

        let mut options =
            PiSessionOptions::for_model(&pi_model).with_session_file(session_path_str);
        if let Some(ref level) = parsed_thinking {
            options = options.with_thinking_level(level.clone());
        }
        if let Some(ref ts) = parsed_tool_set {
            options = options.with_tool_set(ts.clone());
        }
        if let Some(ref sp) = system_prompt {
            options = options.with_system_prompt(sp.clone());
        }
        if file_exists {
            options = options.with_resume();
            info!(session_id = %sid, "resuming chat from session file");
        }

        // Same merge rule as the dead-respawn branch: request fields win,
        // graveyard fills the gaps. Cloning the graveyard entry wholesale
        // would strip the frontend-supplied system_prompt / tool_set /
        // thinking and respawn Pi naked — the empty system prompt is the
        // root cause behind the misleading "out of extra usage" 400 from
        // claude-sub on resume.
        let options = merge_graveyard_into(
            options,
            graveyard_entry.as_ref(),
            system_prompt.is_some(),
            parsed_tool_set.is_some(),
            parsed_thinking.is_some(),
        );
        state
            .pi_manager
            .spawn_session_with_options(&sid, &options, &working_dir)
            .await
            .map_err(|e| {
                IpcError::from_provider_error(format!("failed to create session: {}", e))
                    .with_id(sid.clone())
            })?
    };

    // Tag owner for the eviction / reconcile decisions. send_message is
    // the Tasks-view conversation surface, so this is always Task-owned.
    session.set_owner(SessionOwner::Task {
        task_id: sid.clone(),
    });
    // Pin the session to keep the eviction loop hands-off for the
    // duration of this prompt + stream. Cleared in the spawned task on
    // completion or error. The pin is atomic and idempotent.
    session.set_pinned(true);

    // Convert images once — used by both the busy-steer branch and the
    // idle send_prompt branch below.
    let pi_images: Option<Vec<PiImage>> = images
        .as_ref()
        .map(|imgs| imgs.iter().map(|i| i.to_pi_image()).collect());

    // If the session is busy, give it a short grace period to reach
    // AgentEnd before falling into the steer branch. The common case is
    // the "AgentEnd race": Pi has streamed the final text of a turn (so
    // the UI shows the input as available) but the busy flag clears a
    // beat later when MessageEnd → AgentEnd fires. A user who answers a
    // `submit_questions` form during that window would otherwise have
    // their reply absorbed as a steer and never produce a new turn,
    // forcing them to re-send the same answer to make the agent move on.
    //
    // Polling is intentionally cheap: 75ms ticks for up to 8s. In normal
    // (already-idle) flow the loop body never runs. The deadline is
    // generous enough to absorb a tail-of-turn that's mid-MessageEnd but
    // tight enough that a genuinely long-running turn falls through to
    // the steer branch (the legitimate "interrupt while the model is
    // mid-thought" path) without making the IPC call feel hung.
    if session.is_busy() {
        let wait_deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
        while session.is_busy() && std::time::Instant::now() < wait_deadline {
            tokio::time::sleep(std::time::Duration::from_millis(75)).await;
        }
        if !session.is_busy() {
            debug!(
                session_id = %sid,
                "session became idle within grace period — sending as fresh prompt"
            );
        }
    }
    if session.is_busy() {
        info!(
            session_id = %sid,
            image_count = pi_images.as_ref().map(|v| v.len()).unwrap_or(0),
            "session is busy — steering instead of sending prompt"
        );
        let sid_bg = sid.clone();
        // Steer doesn't run our usual collect_response loop, so unpin
        // before spawning so the next eviction sweep is free to act on
        // genuinely idle sessions.
        session.set_pinned(false);
        let pi_images_for_steer = pi_images.clone();
        tokio::spawn(
            async move {
                if let Err(e) = session.steer(&message, pi_images_for_steer).await {
                    error!(session_id = %sid_bg, error = %e, "steer failed");
                    // Surface a visible nurse card alongside the existing
                    // error toast so the user sees Nurse react to the
                    // failure rather than just a flash of red. v2 engine is
                    // the sole dispatcher.
                    if let Some(engine) = app
                        .try_state::<AppState>()
                        .and_then(|s| s.nurse_engine().cloned())
                    {
                        let new_owner = crate::nurse::synthesized::InterventionOwner {
                            session_id: Some(sid_bg.clone()),
                            task_id: Some(sid_bg.clone()),
                            ..Default::default()
                        };
                        let _ = engine.report_synthesized(
                            new_owner,
                            crate::nurse::synthesized::SynthesizedKind::SteerFailed {
                                reason: e.to_string(),
                            },
                        );
                    }
                    let _ = app.emit(
                        "chat-event",
                        ChatEvent {
                            session_id: sid_bg,
                            event_type: "error".to_string(),
                            content: humanize_pi_error(&e.to_string()),
                        },
                    );
                } else {
                    let _ = app.emit(
                        "chat-event",
                        ChatEvent {
                            session_id: sid_bg,
                            event_type: "queued".to_string(),
                            content: message,
                        },
                    );
                }
            }
            .instrument(tracing::Span::current()),
        );
        return Ok(sid);
    }

    // The session is idle (not busy). The frontend told us the user just
    // clicked Stop and this is the redirect — prepend an interruption
    // preamble before handing the prompt to Pi so the agent treats it as a
    // course-correction rather than a continuation. The frontend's local
    // message store keeps the original text, so the user-facing transcript
    // is unaffected. Only the busy-branch above (which uses Pi's `steer`
    // RPC) carries interruption semantics natively; for the post-abort
    // idle case Pi just sees a normal `prompt`, so the marker is the only
    // signal that the previous turn was cut short.
    let message = if is_steer {
        info!(
            session_id = %sid,
            "send_message: post-stop steer — prepending interruption preamble"
        );
        format!(
            "[The user stopped your previous turn. Their new instruction follows.]\n\n{}",
            message
        )
    } else {
        message
    };

    // Return the session ID immediately; stream everything in a background task.
    // This lets the frontend register its event listener before chunks arrive.
    let sid_bg = sid.clone();
    let usage_store = state.usage_store.clone();
    let effective_model_bg = effective_model.clone();
    // Clone the merge-capture registry so the streaming closure can look up
    // a `MergeCapture` keyed by `session_id` and durably persist each chunk
    // to disk. No-op for non-merge chat sessions (lookup misses).
    let merge_capture_registry: Arc<std::sync::RwLock<HashMap<String, Arc<MergeCapture>>>> =
        state.merge_capture.clone();
    // Clone the PiManager so the spawned Nurse LLM-evaluation tasks (kicked
    // off from the streaming callback on PiEvent::Error) can pull the
    // session's recent transcript and execute steer/restart decisions.
    let pi_manager_bg = state.pi_manager.clone();
    // Audit 2.12: panic-safe wrapper around the streaming task. A panic
    // here is the canonical "phantom spinner" case — the spawn that owns
    // the stream silently dies and the UI never sees a `done`/`error`
    // chat-event. Emit a synthetic structured error event on panic.
    let panic_app_chat = app.clone();
    let panic_sid = sid_bg.clone();
    tokio::spawn(crate::supervise!(
        context = format!("chat session_id={} component=stream", panic_sid),
        on_panic = move |panic_msg: String| {
            let _ = panic_app_chat.emit(
                "chat-event",
                ChatEvent {
                    session_id: panic_sid.clone(),
                    event_type: "error".to_string(),
                    content: format!("internal task panicked: {panic_msg}"),
                },
            );
        },
        async move {
            #[cfg(test)]
            crate::util::supervise::maybe_panic_for_test("chat_stream");
        // Emit start event
        let _ = app.emit(
            "chat-event",
            ChatEvent {
                session_id: sid_bg.clone(),
                event_type: "start".to_string(),
                content: String::new(),
            },
        );

        // Snapshot cumulative output tokens BEFORE sending the prompt so we can
        // compute per-response tok/s later. Must happen before send_prompt to
        // avoid blocking between prompt send and collect_response_streaming
        // (which would cause broadcast events to be lost).
        //
        // Single get_session_stats() call (the value can race between two
        // back-to-back calls): destructure once for both deltas.
        let prev_stats = session.get_session_stats().await.ok();
        let prev_output_tokens = prev_stats.as_ref().map(|s| s.output).unwrap_or(0);
        let prev_reasoning_tokens =
            prev_stats.as_ref().map(|s| s.reasoning_tokens).unwrap_or(0);

        // Per-send phase-tracking state. Shared between the streaming closure
        // (single producer, fires on every PiEvent), the heartbeat ticker
        // (reader), and the usage poller (writer for `context_loaded_sent`).
        let phase_state = Arc::new(std::sync::Mutex::new(PhaseState::new()));

        // Send the prompt (maps to Pi SDK's session.prompt())
        // `pi_images` was built earlier (above the busy-check) so both the
        // busy-steer branch and this idle branch use the same conversion.
        if let Err(e) = session.send_prompt(&message, pi_images).await {
            error!(session_id = %sid_bg, error = %e, "send_prompt failed");
            let _ = app.emit(
                "chat-event",
                ChatEvent {
                    session_id: sid_bg.clone(),
                    event_type: "error".to_string(),
                    content: humanize_pi_error(&e.to_string()),
                },
            );
            return;
        }

        // Collect the response with real-time streaming to the frontend.
        let stream_start = Arc::new(std::time::Instant::now());
        let char_count = Arc::new(AtomicU64::new(0));
        let last_tps_char = Arc::new(AtomicU64::new(0));
        let last_tps_time = Arc::new(AtomicU64::new(0));
        let app_clone = app.clone();
        let sid_clone = sid_bg.clone();
        let tps_stream_start = stream_start.clone();
        let tps_char_count = char_count.clone();
        let tps_last_char = last_tps_char.clone();
        let tps_last_time = last_tps_time.clone();
        let merge_capture_registry_cb = merge_capture_registry.clone();

        // Periodic usage poller — keeps the bottom telemetry strip live during
        // long Pi turns (especially Hivemind merges, which otherwise look
        // frozen because there's no per-chunk token signal). Polls
        // `get_session_stats()` every 2.5s and emits a `usage` chat-event
        // identical in shape to the post-stream emission below. Stops when
        // signaled via `stop_poll`.
        let stop_poll = Arc::new(tokio::sync::Notify::new());
        let poll_session = session.clone();
        let poll_app = app.clone();
        let poll_sid = sid_bg.clone();
        let poll_stream_start = stream_start.clone();
        let poll_stop = stop_poll.clone();
        // Cancellation token signalled when the session is force-killed.
        // If the maintenance loop evicts this session out from under us,
        // the poller must drop its Arc<PiSession> promptly (otherwise it
        // would keep the semaphore permit alive forever).
        let poll_cancel = poll_session.cancellation_token();
        // Clone the cancellation token now so the heartbeat ticker can
        // share it (`poll_cancel` itself is moved into the usage poller
        // closure below). Cloned CancellationTokens fire together.
        let hb_cancel = poll_cancel.clone();
        // Clone phase state for the usage poller so it can emit
        // `context_loaded` once the first non-zero context-tokens tick
        // proves the prompt has loaded into the model's context window.
        let phase_state_for_poller = phase_state.clone();
        let usage_poll_handle = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_millis(2500));
            // Skip the immediate first tick; the first emission should fire
            // after one interval, not at t=0 (when stats are still empty).
            ticker.tick().await;
            loop {
                tokio::select! {
                    _ = poll_stop.notified() => break,
                    _ = poll_cancel.cancelled() => break,
                    _ = ticker.tick() => {}
                }
                match poll_session.get_session_stats().await {
                    Ok(stats) => {
                        let duration_ms = poll_stream_start.elapsed().as_millis() as u64;
                        let tokens_per_sec = if duration_ms > 0 && stats.output > 0 {
                            (stats.output as f64 / (duration_ms as f64 / 1000.0)).round() as u64
                        } else {
                            0
                        };
                        let _ = poll_app.emit(
                            "chat-event",
                            ChatEvent {
                                session_id: poll_sid.clone(),
                                event_type: "usage".to_string(),
                                content: serde_json::json!({
                                    "input": stats.input,
                                    "output": stats.output,
                                    "cache_read": stats.cache_read,
                                    "cache_write": stats.cache_write,
                                    "total_tokens": stats.total_tokens,
                                    "cost": stats.cost,
                                    "context_tokens": stats.context_tokens,
                                    "context_window": stats.context_window,
                                    "context_percent": stats.context_percent,
                                    "duration_ms": duration_ms,
                                    "tokens_per_sec": tokens_per_sec,
                                })
                                .to_string(),
                            },
                        );
                        // The first usage tick with `context_tokens > 0` is
                        // proof the prompt has been ingested by the model.
                        // Emit `context_loaded` once with the token count so
                        // the UI can replace "Prompt loaded" with a
                        // size-aware "Prompt loaded (Xk tokens)" label.
                        if stats.context_tokens > 0 {
                            let already = {
                                let mut s = match phase_state_for_poller.lock() {
                                    Ok(g) => g,
                                    Err(p) => p.into_inner(),
                                };
                                if s.context_loaded_sent {
                                    true
                                } else {
                                    s.context_loaded_sent = true;
                                    false
                                }
                            };
                            if !already {
                                let _ = poll_app.emit(
                                    "chat-event",
                                    ChatEvent {
                                        session_id: poll_sid.clone(),
                                        event_type: "context_loaded".to_string(),
                                        content: format!("{}", stats.context_tokens),
                                    },
                                );
                            }
                        }
                    }
                    Err(e) => {
                        debug!(session_id = %poll_sid, error = %e, "usage poll: get_session_stats failed (will retry)");
                    }
                }
            }
        }.instrument(tracing::Span::current()));

        // Heartbeat ticker — emits a `heartbeat` chat-event every 5s while
        // the stream is alive, carrying the current phase plus elapsed and
        // silent durations. The frontend uses these to drive a live timer
        // next to the phase label. Skips emission when the agent has been
        // silent for <2s (the tick is "too fresh" to be useful).
        //
        // Termination: the same `stop_poll` Notify + `poll_cancel` token
        // that gate the usage poller also gate this loop. When the
        // streaming task signals `stop_poll.notify_waiters()` on
        // done/error, the `select!` resolves and the loop breaks. When
        // the session is force-killed externally, `poll_cancel.cancelled()`
        // resolves and the loop breaks. Both paths drop the heartbeat's
        // clones of `phase_state` / `hb_app` / `hb_sid` so no further
        // emits are possible after `done`.
        let hb_stop = stop_poll.clone();
        let hb_app = app.clone();
        let hb_sid = sid_bg.clone();
        let hb_phase_state = phase_state.clone();
        let heartbeat_handle = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(5));
            // Skip the immediate first tick so the first heartbeat fires
            // after one interval, not at t=0 (when nothing useful exists yet).
            ticker.tick().await;
            loop {
                tokio::select! {
                    _ = hb_stop.notified() => break,
                    _ = hb_cancel.cancelled() => break,
                    _ = ticker.tick() => {}
                }
                let (phase, elapsed_ms, silent_ms) = {
                    let s = match hb_phase_state.lock() {
                        Ok(g) => g,
                        Err(p) => p.into_inner(),
                    };
                    (
                        s.current_phase,
                        s.send_started_at.elapsed().as_millis() as u64,
                        s.last_pi_event_at.elapsed().as_millis() as u64,
                    )
                };
                if silent_ms < 2000 {
                    continue;
                }
                let _ = hb_app.emit(
                    "chat-event",
                    ChatEvent {
                        session_id: hb_sid.clone(),
                        event_type: "heartbeat".to_string(),
                        content: serde_json::json!({
                            "phase": phase,
                            "elapsed_ms": elapsed_ms,
                            "silent_ms": silent_ms,
                        })
                        .to_string(),
                    },
                );
            }
        }.instrument(tracing::Span::current()));

        // Per-stream coalescer for TextDelta / ThinkingDelta emits. Shared
        // with the post-await flush so any trailing buffer reaches the FE
        // even if the model goes silent without a terminating non-delta
        // event.
        let coalescer = Arc::new(std::sync::Mutex::new(DeltaCoalescer::new()));
        let coalescer_cb = coalescer.clone();
        let phase_state_cb = phase_state.clone();
        // Cloned for the spawned LLM-driven Nurse evaluation kicked off in
        // the PiEvent::Error branch. The call needs a PiManager handle to
        // (a) pull the session's recent transcript for the LLM payload and
        // (b) execute steer/restart/cancel if Nurse decides on one.
        let _pi_manager_for_callbacks = pi_manager_bg.clone();

        let collect_result = session
            .collect_response_streaming(move |event| {
                // Every PiEvent counts as Pi-alive activity. Bump the
                // shared timestamp at the top of the closure so the
                // heartbeat ticker sees a fresh `last_pi_event_at` for
                // any variant (even ones we don't otherwise handle).
                if let Ok(mut s) = phase_state_cb.lock() {
                    s.last_pi_event_at = std::time::Instant::now();
                }
                match event {
                    PiEvent::AgentStart => {
                        if let Ok(mut s) = phase_state_cb.lock() {
                            if !s.agent_ready_sent {
                                s.agent_ready_sent = true;
                                s.current_phase = "agent_ready";
                                drop(s);
                                emit_phase(&app_clone, &sid_clone, "agent_ready");
                            }
                        }
                    }
                    PiEvent::TurnStart => {
                        // Each turn starts with the user MessageStart, so
                        // reset per-turn counters/flags here. No emit.
                        if let Ok(mut s) = phase_state_cb.lock() {
                            s.first_thinking_sent = false;
                            s.first_text_sent = false;
                            s.msg_starts_in_turn = 0;
                        }
                    }
                    PiEvent::MessageStart => {
                        let should_emit = if let Ok(mut s) = phase_state_cb.lock() {
                            s.msg_starts_in_turn = s.msg_starts_in_turn.saturating_add(1);
                            // The user MessageStart fires first (count=1);
                            // the assistant MessageStart is count=2. Once
                            // we see the assistant boundary we know the
                            // prompt has been loaded and Pi is waiting on
                            // the model.
                            if s.msg_starts_in_turn >= 2 && !s.awaiting_model_sent {
                                s.awaiting_model_sent = true;
                                s.current_phase = "awaiting_model";
                                true
                            } else {
                                false
                            }
                        } else {
                            false
                        };
                        if should_emit {
                            emit_phase(&app_clone, &sid_clone, "awaiting_model");
                        }
                    }
                    PiEvent::MessageEnd => {
                        // The first MessageEnd within a turn closes the
                        // user message — i.e. the prompt has been written
                        // to the Pi session file and is on its way to the
                        // model. Treat that as "prompt loaded".
                        let should_emit = if let Ok(mut s) = phase_state_cb.lock() {
                            if s.msg_starts_in_turn == 1 && !s.prompt_loaded_sent {
                                s.prompt_loaded_sent = true;
                                s.current_phase = "prompt_loaded";
                                true
                            } else {
                                false
                            }
                        } else {
                            false
                        };
                        if should_emit {
                            emit_phase(&app_clone, &sid_clone, "prompt_loaded");
                        }
                    }
                    PiEvent::TurnComplete => {
                        if let Ok(mut s) = phase_state_cb.lock() {
                            s.current_phase = "turn_complete";
                        }
                        emit_phase(&app_clone, &sid_clone, "turn_complete");
                    }
                    PiEvent::TextDelta(text) => {
                        // First text delta of the turn flips us to
                        // `streaming`. Idempotent within a turn via
                        // `first_text_sent`; the `stream_start` event the
                        // frontend already receives covers the very
                        // first-time case.
                        let should_emit = if let Ok(mut s) = phase_state_cb.lock() {
                            if !s.first_text_sent {
                                s.first_text_sent = true;
                                s.current_phase = "streaming";
                                true
                            } else {
                                false
                            }
                        } else {
                            false
                        };
                        if should_emit {
                            emit_phase(&app_clone, &sid_clone, "streaming");
                        }
                        // Live TPS estimation during streaming
                        if !text.is_empty() {
                            let delta_chars = text.chars().count() as u64;
                            if delta_chars > 0 {
                                let new_count = tps_char_count.fetch_add(delta_chars, Ordering::Relaxed) + delta_chars;
                                let elapsed_ms = tps_stream_start.elapsed().as_millis() as u64;
                                let last_char = tps_last_char.load(Ordering::Acquire);
                                let last_time = tps_last_time.load(Ordering::Acquire);
                                if (new_count - last_char >= 200) && (elapsed_ms - last_time >= 200)
                                    && tps_last_char.compare_exchange_weak(
                                        last_char,
                                        new_count,
                                        Ordering::Release,
                                        Ordering::Relaxed,
                                    ).is_ok()
                                {
                                    tps_last_time.store(elapsed_ms, Ordering::Release);
                                    let elapsed_secs = tps_stream_start.elapsed().as_secs_f64().max(0.001);
                                    let estimated_tps = ((new_count as f64) / 4.0) / elapsed_secs;
                                    let _ = app_clone.emit(
                                        "chat-event",
                                        ChatEvent {
                                            session_id: sid_clone.clone(),
                                            event_type: "tps".to_string(),
                                            content: serde_json::json!({"tps": estimated_tps.round() as u64}).to_string(),
                                        },
                                    );
                                }
                            }
                        }
                        // Push to the coalescer; flush only when batch is
                        // large enough or 50ms have elapsed since the first
                        // pushed byte.
                        if let Ok(mut c) = coalescer_cb.lock() {
                            c.push_text(text);
                            if c.text_ready() {
                                c.flush_text(&app_clone, &sid_clone);
                            }
                        }
                        // Durably persist hivemind merge chunks to disk via the
                        // generic ChunkSink forwarder. The registry lookup is
                        // a no-op for non-merge sessions (the lookup misses),
                        // and `MergeCapture` is the canonical `ChunkSink`
                        // (see `hivemind/merge_capture.rs`).
                        let sink_opt = merge_capture_registry_cb
                            .read()
                            .ok()
                            .and_then(|m| m.get(sid_clone.as_str()).cloned());
                        if let Some(sink) = sink_opt {
                            crate::pi::session::forward_chunk_to_sink(
                                sink.as_ref(),
                                text,
                            );
                        }
                    }
                    PiEvent::ThinkingDelta(text) => {
                        // First thinking delta of the turn flips us to
                        // `thinking`. Same idempotency pattern as text.
                        let should_emit = if let Ok(mut s) = phase_state_cb.lock() {
                            if !s.first_thinking_sent {
                                s.first_thinking_sent = true;
                                s.current_phase = "thinking";
                                true
                            } else {
                                false
                            }
                        } else {
                            false
                        };
                        if should_emit {
                            emit_phase(&app_clone, &sid_clone, "thinking");
                        }
                        // Live TPS estimation during streaming (same pattern as TextDelta)
                        if !text.is_empty() {
                            let delta_chars = text.chars().count() as u64;
                            if delta_chars > 0 {
                                let new_count = tps_char_count.fetch_add(delta_chars, Ordering::Relaxed) + delta_chars;
                                let elapsed_ms = tps_stream_start.elapsed().as_millis() as u64;
                                let last_char = tps_last_char.load(Ordering::Acquire);
                                let last_time = tps_last_time.load(Ordering::Acquire);
                                if (new_count - last_char >= 200) && (elapsed_ms - last_time >= 200)
                                    && tps_last_char.compare_exchange_weak(
                                        last_char,
                                        new_count,
                                        Ordering::Release,
                                        Ordering::Relaxed,
                                    ).is_ok()
                                {
                                    tps_last_time.store(elapsed_ms, Ordering::Release);
                                    let elapsed_secs = tps_stream_start.elapsed().as_secs_f64().max(0.001);
                                    let estimated_tps = ((new_count as f64) / 4.0) / elapsed_secs;
                                    let _ = app_clone.emit(
                                        "chat-event",
                                        ChatEvent {
                                            session_id: sid_clone.clone(),
                                            event_type: "tps".to_string(),
                                            content: serde_json::json!({"tps": estimated_tps.round() as u64}).to_string(),
                                        },
                                    );
                                }
                            }
                        }
                        if let Ok(mut c) = coalescer_cb.lock() {
                            c.push_thinking(text);
                            if c.thinking_ready() {
                                c.flush_thinking(&app_clone, &sid_clone);
                            }
                        }
                    }
                    PiEvent::ToolExecutionStart {
                        tool_call_id,
                        name,
                        args,
                    } => {
                        if let Ok(mut c) = coalescer_cb.lock() {
                            c.flush_all(&app_clone, &sid_clone);
                        }

                        // Idempotency: agent occasionally calls submit_task_complete more
                        // than once per turn. The first call is the authoritative completion
                        // signal — suppress all downstream IPC for any duplicates so the UI
                        // doesn't render a second "Task Complete" chip and tool row. We
                        // still let Pi process the tool (it's a no-op echo); we just drop
                        // our own emits. Logged at WARN for triage.
                        let suppress_duplicate_complete = if name == "submit_task_complete" {
                            let mut s = phase_state_cb.lock().ok();
                            let already = s.as_ref().map(|s| s.task_complete_emitted).unwrap_or(false);
                            if already {
                                tracing::warn!(
                                    session_id = %sid_clone,
                                    tool_call_id = %tool_call_id,
                                    "agent called submit_task_complete more than once; suppressing duplicate emit"
                                );
                                true
                            } else {
                                if let Some(s) = s.as_mut() { s.task_complete_emitted = true; }
                                false
                            }
                        } else {
                            false
                        };

                        if !suppress_duplicate_complete {
                            // Phase 3 hook: when the model calls a Hyvemind
                            // extension planning/handoff tool, emit a
                            // `structured_*` chat-event carrying the raw `args`
                            // payload so the frontend reducer can bypass
                            // delimiter scanning. Untouched legacy `tool_start`
                            // is emitted below for the regular tools view.
                            if let Some(event_type) = structured_tool_event_type(&name) {
                                let _ = app_clone.emit(
                                    "chat-event",
                                    ChatEvent {
                                        session_id: sid_clone.clone(),
                                        event_type: event_type.to_string(),
                                        content: args.to_string(),
                                    },
                                );
                            }
                            let _ = app_clone.emit(
                                "chat-event",
                                ChatEvent {
                                    session_id: sid_clone.clone(),
                                    event_type: "tool_start".to_string(),
                                    content: serde_json::json!({
                                        "tool_call_id": tool_call_id,
                                        "name": name,
                                    }).to_string(),
                                },
                            );
                            // Tool transition: update current_phase and emit.
                            // Intentionally NOT idempotent across distinct tool
                            // calls — every tool start is a fresh activity
                            // window the user should see. Kept inside this branch
                            // so we don't repaint "tool_running" for a suppressed call.
                            if let Ok(mut s) = phase_state_cb.lock() {
                                s.current_phase = "tool_running";
                            }
                            emit_phase(&app_clone, &sid_clone, "tool_running");
                        }
                    }
                    PiEvent::ToolExecutionUpdate { tool_call_id, output } => {
                        if let Ok(mut c) = coalescer_cb.lock() {
                            c.flush_all(&app_clone, &sid_clone);
                        }
                        let _ = app_clone.emit(
                            "chat-event",
                            ChatEvent {
                                session_id: sid_clone.clone(),
                                event_type: "tool_update".to_string(),
                                content: serde_json::json!({
                                    "tool_call_id": tool_call_id,
                                    "output": output,
                                }).to_string(),
                            },
                        );
                    }
                    PiEvent::ToolExecutionEnd { tool_call_id, result } => {
                        if let Ok(mut c) = coalescer_cb.lock() {
                            c.flush_all(&app_clone, &sid_clone);
                        }
                        let _ = app_clone.emit(
                            "chat-event",
                            ChatEvent {
                                session_id: sid_clone.clone(),
                                event_type: "tool_end".to_string(),
                                content: serde_json::json!({
                                    "tool_call_id": tool_call_id,
                                    "result": result,
                                }).to_string(),
                            },
                        );
                    }
                    PiEvent::QueueUpdate { steering, follow_up } => {
                        if let Ok(mut c) = coalescer_cb.lock() {
                            c.flush_all(&app_clone, &sid_clone);
                        }
                        let _ = app_clone.emit(
                            "chat-event",
                            ChatEvent {
                                session_id: sid_clone.clone(),
                                event_type: "queue_update".to_string(),
                                content: serde_json::json!({
                                    "steering": steering,
                                    "follow_up": follow_up,
                                }).to_string(),
                            },
                        );
                    }
                    PiEvent::Error(msg) => {
                        if let Ok(mut c) = coalescer_cb.lock() {
                            c.flush_all(&app_clone, &sid_clone);
                        }
                        let summary = summarize_retry_error(&msg);
                        // Log the raw payload at INFO so the friendly UI
                        // message keeps full detail in the per-session debug
                        // bucket for triage.
                        info!(
                            session_id = %sid_clone,
                            raw_error = %msg,
                            summary = %summary,
                            "emitting chat-event error"
                        );
                        // Fire-and-forget Nurse v2 dispatch. The engine's
                        // `report_error` injects a synthetic signal on the
                        // session and routes through the three-tier
                        // pipeline (Tier 1 deterministic / Tier 2 playbook
                        // / Tier 3 LLM classifier). MUST NOT be awaited
                        // here — the chat streaming task can't block on a
                        // multi-second classifier call.
                        if let Some(engine) = app_clone
                            .try_state::<AppState>()
                            .and_then(|s| s.nurse_engine().cloned())
                        {
                            let new_owner = crate::nurse::synthesized::InterventionOwner {
                                session_id: Some(sid_clone.clone()),
                                task_id: Some(sid_clone.clone()),
                                ..Default::default()
                            };
                            engine.report_error(
                                crate::nurse::synthesized::SynthesizedKind::PiError {
                                    message: msg.clone(),
                                },
                                sid_clone.clone(),
                                new_owner,
                            );
                        }
                        let _ = app_clone.emit(
                            "chat-event",
                            ChatEvent {
                                session_id: sid_clone.clone(),
                                event_type: "error".to_string(),
                                content: summary,
                            },
                        );
                    }
                    PiEvent::AutoRetryStart {
                        attempt,
                        max_attempts,
                        delay_ms,
                        error_message,
                    } => {
                        // Flush any buffered text so the UI snapshot is
                        // consistent with the retry banner that appears
                        // next. The retry banner replaces the spinner /
                        // phase label until AutoRetryEnd arrives.
                        if let Ok(mut c) = coalescer_cb.lock() {
                            c.flush_all(&app_clone, &sid_clone);
                        }
                        // Surface a short user-friendly summary alongside
                        // the raw error envelope so the FE can show
                        // "Server overloaded" without parsing JSON.
                        let summary = summarize_retry_error(error_message);
                        if let Ok(mut s) = phase_state_cb.lock() {
                            s.current_phase = "retrying";
                        }
                        let _ = app_clone.emit(
                            "chat-event",
                            ChatEvent {
                                session_id: sid_clone.clone(),
                                event_type: "retrying".to_string(),
                                content: serde_json::json!({
                                    "attempt": attempt,
                                    "max_attempts": max_attempts,
                                    "delay_ms": delay_ms,
                                    "error_summary": summary,
                                    "error_message": error_message,
                                })
                                .to_string(),
                            },
                        );
                    }
                    PiEvent::AutoRetryEnd { success, attempt } => {
                        let _ = app_clone.emit(
                            "chat-event",
                            ChatEvent {
                                session_id: sid_clone.clone(),
                                event_type: "retry_resumed".to_string(),
                                content: serde_json::json!({
                                    "attempt": attempt,
                                    "success": success,
                                })
                                .to_string(),
                            },
                        );
                    }
                    PiEvent::SessionStats(_) => {} // handled after streaming completes
                    _ => {}
                }
            })
            .await;

        // Drain any text/thinking still buffered (model finished mid-batch
        // or last event was a delta that didn't hit the flush threshold).
        if let Ok(mut c) = coalescer.lock() {
            c.flush_all(&app, &sid_bg);
        }

        // Stream finished (success or error) — stop the periodic usage poller.
        stop_poll.notify_waiters();
        // Best-effort wait so the poller releases its session Arc before we
        // continue. Won't block longer than its own select/await yields.
        let _ = usage_poll_handle.await;
        // Wait for the heartbeat ticker too — it shares `stop_poll`, so
        // it will see the notification on the same wakeup and break out
        // of its loop. Awaiting here guarantees no heartbeat emit can
        // fire after this point.
        let _ = heartbeat_handle.await;

        match collect_result {
            Ok(response) => {
                let duration_ms = stream_start.elapsed().as_millis() as u64;
                debug!(session_id = %sid_bg, response_preview = %response.chars().take(500).collect::<String>(), duration_ms, "send_message response preview");
                // Get token usage from Pi (single call for both frontend emit and DB recording)
                match session.get_session_stats().await {
                    Ok(stats) => {
                        let response_output = stats.output.saturating_sub(prev_output_tokens);
                        let response_reasoning = stats.reasoning_tokens.saturating_sub(prev_reasoning_tokens);
                        let total_generated = response_output + response_reasoning;
                        let tokens_per_sec = if duration_ms > 0 && total_generated > 0 {
                            (total_generated as f64 / (duration_ms as f64 / 1000.0)).round() as u64
                        } else {
                            0
                        };
                        let _ = app.emit(
                            "chat-event",
                            ChatEvent {
                                session_id: sid_bg.clone(),
                                event_type: "usage".to_string(),
                                content: serde_json::json!({
                                    "input": stats.input,
                                    "output": stats.output,
                                    "cache_read": stats.cache_read,
                                    "cache_write": stats.cache_write,
                                    "total_tokens": stats.total_tokens,
                                    "cost": stats.cost,
                                    "context_tokens": stats.context_tokens,
                                    "context_window": stats.context_window,
                                    "context_percent": stats.context_percent,
                                    "duration_ms": duration_ms,
                                    "tokens_per_sec": tokens_per_sec,
                                })
                                .to_string(),
                            },
                        );
                        // Parse provider from model_id if it contains a slash (e.g., "openrouter/anthropic/claude-3.5-sonnet")
                        let (provider_bg, model_id_bg) = if let Some((p, m)) = effective_model_bg.split_once('/') {
                            (p.to_string(), m.to_string())
                        } else {
                            ("anthropic".to_string(), effective_model_bg.clone())
                        };
                        // Record usage for dashboard tracking
                        if let Err(e) = usage_store.record_usage(crate::state::usage_store::UsageEntry {
                            source: "chat".to_string(),
                            source_id: Some(sid_bg.clone()),
                            model_id: model_id_bg,
                            provider: provider_bg,
                            input_tokens: stats.input as i64,
                            output_tokens: stats.output as i64,
                            cache_read_tokens: stats.cache_read as i64,
                            cache_write_tokens: stats.cache_write as i64,
                            cost: stats.cost,
                            duration_ms: duration_ms as i64,
                        }).await {
                            warn!(session_id = %sid_bg, error = %e, "failed to record usage to dashboard");
                        }
                    }
                    Err(e) => {
                        warn!(session_id = %sid_bg, error = %e, "failed to get session stats — usage will not be recorded");
                        // Parse provider from model_id if it contains a slash
                        let (provider_bg, model_id_bg) = if let Some((p, m)) = effective_model_bg.split_once('/') {
                            (p.to_string(), m.to_string())
                        } else {
                            ("anthropic".to_string(), effective_model_bg.clone())
                        };
                        // Record minimal entry so dashboard shows activity
                        let _ = usage_store.record_usage(crate::state::usage_store::UsageEntry {
                            source: "chat".to_string(),
                            source_id: Some(sid_bg.clone()),
                            model_id: model_id_bg,
                            provider: provider_bg,
                            input_tokens: 0,
                            output_tokens: 0,
                            cache_read_tokens: 0,
                            cache_write_tokens: 0,
                            cost: 0.0,
                            duration_ms: 0,
                        }).await;
                    }
                }
                let _ = app.emit(
                    "chat-event",
                    ChatEvent {
                        session_id: sid_bg.clone(),
                        event_type: "done".to_string(),
                        content: String::new(),
                    },
                );
                info!(session_id = %sid_bg, response_len = response.len(), "send_message complete");
            }
            Err(e) => {
                error!(session_id = %sid_bg, error = %e, "collect_response failed");
                let _ = app.emit(
                    "chat-event",
                    ChatEvent {
                        session_id: sid_bg.clone(),
                        event_type: "error".to_string(),
                        content: humanize_pi_error(&e.to_string()),
                    },
                );
            }
        }
        // Unpin the session so the eviction loop can act on it again.
        // Mirrors the SessionPinGuard::Drop semantics for the cases
        // where we couldn't structurally use the guard (the session
        // Arc is moved into this spawned task).
        session.set_pinned(false);
        }.instrument(tracing::Span::current())
    ));

    Ok(sid)
}

/// Stop an active chat session.
///
/// First attempts a graceful abort via the Pi SDK's `session.abort()` method.
/// If the session is not found (already dead), falls back to killing the
/// process. This is preferred over always killing because abort allows
/// the Pi session to clean up properly.
#[tracing::instrument(skip(state), fields(session_id = %session_id))]
#[tauri::command]
pub async fn stop_chat(
    state: tauri::State<'_, AppState>,
    session_id: String,
) -> Result<(), IpcError> {
    validate_session_id(&session_id).map_err(IpcError::validation)?;
    info!(session_id = %session_id, "stop_chat invoked");

    // Try graceful abort first (maps to Pi SDK's session.abort()). On
    // success we leave the Pi process alive so the user's next message
    // (the "steer after stop" redirect) can reuse the warm session
    // instead of paying the spawn+resume-from-JSONL cost. The normal
    // idle-eviction sweep in PiManager will reclaim it later if the
    // user walks away. We only kill the session as a fallback if abort
    // itself failed, to avoid leaving a wedged entry in the manager.
    if let Some(session) = state.pi_manager.get_session(&session_id).await {
        match session.abort().await {
            Ok(()) => {
                info!(session_id = %session_id, "session aborted gracefully — keeping process alive for steer-after-stop");
                // Give Pi a moment to process the abort (drain in-flight
                // events, emit AgentEnd / TurnComplete so `busy` flips to
                // false) before returning to the frontend.
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                return Ok(());
            }
            Err(e) => {
                info!(
                    session_id = %session_id,
                    error = %e,
                    "graceful abort failed, falling back to kill"
                );
            }
        }
    }

    // Fallback: abort failed (or session not found in manager). Drop the
    // process so the next send_message can respawn cleanly.
    match state.pi_manager.kill_session(&session_id).await {
        Ok(()) => Ok(()),
        Err(crate::pi::manager::PiManagerError::SessionNotFound { .. }) => {
            // Session already cleaned up -- not an error
            Ok(())
        }
        Err(e) => Err(IpcError::internal(format!("failed to stop session: {}", e))
            .with_id(session_id.clone())),
    }
}

/// Retrieve the chat history for a given session.
///
/// Returns all messages (user + assistant) in chronological order
/// reconstructed from the Pi event transcript.
#[tracing::instrument(skip(state), fields(session_id = %session_id))]
#[tauri::command]
pub async fn get_chat_history(
    state: tauri::State<'_, AppState>,
    session_id: String,
) -> Result<Vec<ChatMessage>, IpcError> {
    validate_session_id(&session_id).map_err(IpcError::validation)?;
    info!(session_id = %session_id, "get_chat_history invoked");

    let session = state
        .pi_manager
        .get_session(&session_id)
        .await
        .ok_or_else(|| IpcError::not_found("session", session_id.clone()))?;

    let transcript = session.get_transcript().await;
    let now = Utc::now();

    // Convert PiEvents into ChatMessages
    let mut messages = Vec::new();
    let mut current_text = String::new();

    for event in &transcript {
        match event {
            PiEvent::TextDelta(text) => {
                current_text.push_str(text);
            }
            PiEvent::AgentEnd | PiEvent::TurnComplete => {
                if !current_text.is_empty() {
                    messages.push(ChatMessage {
                        role: "assistant".to_string(),
                        content: std::mem::take(&mut current_text),
                        timestamp: now,
                    });
                }
            }
            PiEvent::ToolExecutionStart {
                tool_call_id,
                name,
                args: _,
            } => {
                messages.push(ChatMessage {
                    role: "tool".to_string(),
                    content: serde_json::json!({
                        "tool_call_id": tool_call_id,
                        "name": name,
                        "event": "start",
                    })
                    .to_string(),
                    timestamp: now,
                });
            }
            PiEvent::ToolExecutionEnd {
                tool_call_id,
                result,
            } => {
                messages.push(ChatMessage {
                    role: "tool".to_string(),
                    content: serde_json::json!({
                        "tool_call_id": tool_call_id,
                        "event": "end",
                        "result": result,
                    })
                    .to_string(),
                    timestamp: now,
                });
            }
            PiEvent::Error(msg) => {
                messages.push(ChatMessage {
                    role: "system".to_string(),
                    content: format!("Error: {}", msg),
                    timestamp: now,
                });
            }
            _ => {}
        }
    }

    // Flush any remaining text
    if !current_text.is_empty() {
        messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: current_text,
            timestamp: now,
        });
    }

    debug!(session_id = %session_id, message_count = messages.len(), "get_chat_history returning");
    Ok(messages)
}

/// List all persisted chat session IDs (stems of `.jsonl` files on disk).
#[tracing::instrument(skip(state))]
#[tauri::command]
pub async fn list_chat_sessions(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<String>, IpcError> {
    let dir = &state.chat_sessions_dir;
    let mut sessions = Vec::new();
    let entries = std::fs::read_dir(dir)
        .map_err(|e| IpcError::internal(format!("failed to read chat-sessions dir: {}", e)))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                sessions.push(stem.to_string());
            }
        }
    }
    info!(count = sessions.len(), "list_chat_sessions");
    Ok(sessions)
}

/// Delete a persisted chat session file and kill the in-memory session if active.
#[tracing::instrument(skip(state), fields(session_id = %session_id))]
#[tauri::command]
pub async fn delete_chat_session(
    state: tauri::State<'_, AppState>,
    session_id: String,
) -> Result<(), IpcError> {
    // CVE-class path-traversal guard: without this, a payload like
    // "../../../../etc/myfile" resolved to ~/.hyvemind/chat-sessions/
    // ../../../../etc/myfile.jsonl and deleted arbitrary files.
    validate_session_id(&session_id).map_err(IpcError::validation)?;
    info!(session_id = %session_id, "delete_chat_session invoked");

    // Defense in depth: reject ids that could traverse out of chat_sessions_dir
    // before we touch the filesystem. Without this, ".." would compute
    // chat_sessions_dir/...jsonl which can resolve outside the dir.
    validate_session_id(&session_id).map_err(IpcError::validation)?;

    // Kill in-memory session if active
    let _ = state.pi_manager.kill_session(&session_id).await;

    // Remove the session file from disk
    let path = state
        .chat_sessions_dir
        .join(format!("{}.jsonl", &session_id));
    if path.exists() {
        std::fs::remove_file(&path).map_err(|e| {
            IpcError::internal(format!("failed to delete session file: {}", e))
                .with_id(session_id.clone())
        })?;
    }
    Ok(())
}

/// Check whether a chat session is currently busy (processing a prompt).
///
/// Returns `false` if the session does not exist (already terminated).
#[tracing::instrument(skip(state), fields(session_id = %session_id))]
#[tauri::command]
pub async fn is_session_busy(
    state: tauri::State<'_, AppState>,
    session_id: String,
) -> Result<bool, IpcError> {
    validate_session_id(&session_id).map_err(IpcError::validation)?;
    match state.pi_manager.get_session(&session_id).await {
        Some(session) => Ok(session.is_busy()),
        None => Ok(false),
    }
}

/// Extract the concatenated text of the last assistant message from the
/// raw JSONL contents of a Pi session transcript. Pure / testable.
///
/// "Assistant message" = a JSONL entry with `type=message`, `message.role=
/// "assistant"`. "Text content" = the concatenated `text` fields of every
/// `{ type: "text" }` entry inside that message's `content` array. Thinking,
/// tool-call, and tool-result entries are ignored. We return the LAST such
/// message because that's the one whose chunks the streaming `done` handler
/// is finalizing — Pi may emit several intermediate assistant messages within
/// a single turn (interleaved with tool calls); only the final one matters
/// for plan/review-prompt extraction.
fn parse_last_assistant_text(contents: &str) -> String {
    let mut last_text = String::new();
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let obj: serde_json::Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if obj.get("type").and_then(|v| v.as_str()) != Some("message") {
            continue;
        }
        let msg = match obj.get("message") {
            Some(m) => m,
            None => continue,
        };
        if msg.get("role").and_then(|v| v.as_str()) != Some("assistant") {
            continue;
        }
        let content = match msg.get("content").and_then(|c| c.as_array()) {
            Some(c) => c,
            None => continue,
        };
        let mut buf = String::new();
        for part in content {
            if part.get("type").and_then(|v| v.as_str()) == Some("text") {
                if let Some(t) = part.get("text").and_then(|v| v.as_str()) {
                    buf.push_str(t);
                }
            }
        }
        if !buf.is_empty() {
            last_text = buf;
        }
    }
    last_text
}

/// Concatenated text content of the **last** assistant message in a Pi
/// session's authoritative JSONL transcript on disk.
///
/// This is the durable source of truth for what Pi produced — used by the
/// frontend to reconcile the in-memory message list against actual Pi output
/// when the streamed `chat-event` IPC may have dropped chunks (e.g. broadcast
/// channel lag under burst). Returns an empty string if the session file
/// doesn't exist or contains no assistant text messages.
#[tracing::instrument(skip(state), fields(session_id = %session_id))]
#[tauri::command]
pub async fn get_session_last_assistant_text(
    state: tauri::State<'_, AppState>,
    session_id: String,
) -> Result<String, IpcError> {
    // Defense in depth: session_id is joined into chat-sessions/.jsonl, so
    // a `..` payload would otherwise read arbitrary files.
    validate_session_id(&session_id).map_err(IpcError::validation)?;
    let path = state
        .chat_sessions_dir
        .join(format!("{}.jsonl", &session_id));
    if !path.exists() {
        return Ok(String::new());
    }
    let contents = tokio::fs::read_to_string(&path).await.map_err(|e| {
        IpcError::internal(format!("failed to read session file: {}", e))
            .with_id(session_id.clone())
    })?;
    let last_text = parse_last_assistant_text(&contents);
    debug!(
        session_id = %session_id,
        text_len = last_text.len(),
        "get_session_last_assistant_text returning"
    );
    Ok(last_text)
}

#[cfg(test)]
mod tests {
    use super::*;

    // The chat-level `validate_working_dir` now requires an `AppState`
    // handle so it can read the approved-dirs allowlist (audit 1.11);
    // the unit-level shape checks (empty / null byte / tilde expansion /
    // canonicalization / allowlist enforcement) live in
    // `commands::util::tests` against the underlying helpers.

    #[test]
    fn parse_last_assistant_text_returns_empty_for_empty_input() {
        assert_eq!(parse_last_assistant_text(""), "");
        assert_eq!(parse_last_assistant_text("   \n  \n"), "");
    }

    #[test]
    fn parse_last_assistant_text_ignores_non_message_lines() {
        let jsonl = concat!(
            r#"{"type":"session","version":3,"id":"x","timestamp":"2026-01-01T00:00:00Z","cwd":"/"}"#,
            "\n",
            r#"{"type":"message","id":"a","timestamp":"2026-01-01T00:00:01Z","message":{"role":"assistant","content":[{"type":"text","text":"hello"}]}}"#,
        );
        assert_eq!(parse_last_assistant_text(jsonl), "hello");
    }

    #[test]
    fn parse_last_assistant_text_ignores_user_and_tool_roles() {
        let jsonl = concat!(
            r#"{"type":"message","id":"u","timestamp":"t","message":{"role":"user","content":[{"type":"text","text":"a user msg"}]}}"#,
            "\n",
            r#"{"type":"message","id":"t","timestamp":"t","message":{"role":"toolResult","content":[{"type":"text","text":"tool stuff"}]}}"#,
            "\n",
            r#"{"type":"message","id":"a","timestamp":"t","message":{"role":"assistant","content":[{"type":"text","text":"the answer"}]}}"#,
        );
        assert_eq!(parse_last_assistant_text(jsonl), "the answer");
    }

    #[test]
    fn parse_last_assistant_text_returns_last_assistant() {
        let jsonl = concat!(
            r#"{"type":"message","id":"a1","timestamp":"t","message":{"role":"assistant","content":[{"type":"text","text":"first"}]}}"#,
            "\n",
            r#"{"type":"message","id":"a2","timestamp":"t","message":{"role":"assistant","content":[{"type":"text","text":"second"}]}}"#,
            "\n",
            r#"{"type":"message","id":"a3","timestamp":"t","message":{"role":"assistant","content":[{"type":"text","text":"third (winner)"}]}}"#,
        );
        assert_eq!(parse_last_assistant_text(jsonl), "third (winner)");
    }

    #[test]
    fn parse_last_assistant_text_concatenates_text_parts_in_a_message() {
        // A single assistant message can have multiple text parts (interleaved
        // with thinking, tool calls, etc.) — we concatenate them in order.
        let jsonl = concat!(
            r#"{"type":"message","id":"a","timestamp":"t","message":{"role":"assistant","content":["#,
            r#"{"type":"thinking","thinking":"ponder"},"#,
            r#"{"type":"text","text":"part one. "},"#,
            r#"{"type":"toolCall","name":"read"},"#,
            r#"{"type":"text","text":"part two."}"#,
            r#"]}}"#,
        );
        assert_eq!(parse_last_assistant_text(jsonl), "part one. part two.");
    }

    #[test]
    fn parse_last_assistant_text_skips_assistant_messages_with_no_text() {
        // The "last" should be the last assistant message *with text*, not the
        // last one overall — a tool-only assistant turn after the text turn
        // must not blank out the earlier text.
        let jsonl = concat!(
            r#"{"type":"message","id":"a1","timestamp":"t","message":{"role":"assistant","content":[{"type":"text","text":"the answer"}]}}"#,
            "\n",
            r#"{"type":"message","id":"a2","timestamp":"t","message":{"role":"assistant","content":[{"type":"toolCall","name":"read"}]}}"#,
        );
        assert_eq!(parse_last_assistant_text(jsonl), "the answer");
    }

    #[test]
    fn parse_last_assistant_text_preserves_multiline_text_verbatim() {
        // Regression: the parser must surface the assistant's full text
        // content from Pi's JSONL, not a truncated/streamed slice.
        let jsonl = concat!(
            r#"{"type":"message","id":"a","timestamp":"t","message":{"role":"assistant","content":[{"type":"text","text":"All context gathered.\n\nplan body here\nmore detail"}]}}"#,
        );
        let extracted = parse_last_assistant_text(jsonl);
        assert!(extracted.contains("All context gathered."));
        assert!(extracted.contains("plan body here"));
        assert!(extracted.contains("more detail"));
    }

    #[test]
    fn parse_last_assistant_text_tolerates_garbage_lines() {
        // Defensive: a corrupted line shouldn't crash the parser or skip
        // valid lines that come after.
        let jsonl = concat!(
            "not json at all\n",
            r#"{"type":"message","id":"a","timestamp":"t","message":{"role":"assistant","content":[{"type":"text","text":"valid"}]}}"#,
            "\n",
            "{not real json}\n",
        );
        assert_eq!(parse_last_assistant_text(jsonl), "valid");
    }

    #[test]
    fn summarize_retry_error_strips_http_status_prefix() {
        let raw = r#"400 {"type":"error","error":{"type":"invalid_request_error","message":"You're out of extra usage. Add more at claude.ai/settings/usage and keep going."},"request_id":"req_x"}"#;
        let summary = summarize_retry_error(raw);
        assert_eq!(
            summary,
            "You're out of extra usage. Add more at claude.ai/settings/usage and keep going."
        );
    }

    #[test]
    fn summarize_retry_error_maps_known_kinds() {
        assert_eq!(
            summarize_retry_error(
                r#"{"type":"error","error":{"type":"overloaded_error","message":"x"}}"#
            ),
            "Server overloaded"
        );
        assert_eq!(
            summarize_retry_error(
                r#"{"type":"error","error":{"type":"rate_limit_error","message":"x"}}"#
            ),
            "Rate limited"
        );
    }

    #[test]
    fn summarize_retry_error_falls_back_on_non_json() {
        assert_eq!(summarize_retry_error(""), "Provider error");
        let preview = summarize_retry_error("totally not json");
        assert_eq!(preview, "totally not json");
    }

    #[test]
    fn merge_graveyard_with_no_entry_returns_options_unchanged() {
        let base = PiSessionOptions::for_model("anthropic/claude-sonnet-4")
            .with_system_prompt("REQUEST PROMPT")
            .with_thinking_level(ThinkingLevel::Low);
        let merged = merge_graveyard_into(base.clone(), None, true, false, true);
        assert_eq!(merged.system_prompt.as_deref(), Some("REQUEST PROMPT"));
        assert!(matches!(merged.thinking_level, ThinkingLevel::Low));
        // No graveyard ⇒ resume flag is untouched (false by default).
        assert!(!merged.resume_session);
    }

    #[test]
    fn merge_graveyard_request_system_prompt_wins_over_entry() {
        let graveyard_options =
            PiSessionOptions::for_model("anthropic/claude-sonnet-4").with_system_prompt("STALE");
        let ge = GraveyardEntry {
            options: graveyard_options,
        };
        let request_options = PiSessionOptions::for_model("anthropic/claude-opus-4-7")
            .with_system_prompt("FRESH FROM FRONTEND")
            .with_thinking_level(ThinkingLevel::High)
            .with_tool_set(ToolSet::CodingTools);
        let merged = merge_graveyard_into(request_options, Some(&ge), true, true, true);
        // Every explicitly-supplied request field survives the merge.
        assert_eq!(merged.system_prompt.as_deref(), Some("FRESH FROM FRONTEND"));
        assert_eq!(merged.model, "anthropic/claude-opus-4-7");
        assert!(matches!(merged.thinking_level, ThinkingLevel::High));
        assert!(matches!(merged.tool_set, ToolSet::CodingTools));
        // Resume is always forced on when a graveyard entry exists.
        assert!(merged.resume_session);
    }

    #[test]
    fn merge_graveyard_fills_gaps_when_request_omitted_fields() {
        // Reconciliation recovered thinking=High and a system prompt from
        // the on-disk transcript header. The next send_message didn't
        // supply system_prompt/tool_set/thinking — merge must fall back
        // to the graveyard for those.
        let graveyard_options = PiSessionOptions::for_model("anthropic/claude-sonnet-4")
            .with_system_prompt("RECOVERED FROM DISK")
            .with_thinking_level(ThinkingLevel::High)
            .with_tool_set(ToolSet::CodingTools);
        let ge = GraveyardEntry {
            options: graveyard_options,
        };
        let request_options = PiSessionOptions::for_model("anthropic/claude-sonnet-4");
        let merged = merge_graveyard_into(request_options, Some(&ge), false, false, false);
        assert_eq!(merged.system_prompt.as_deref(), Some("RECOVERED FROM DISK"));
        assert!(matches!(merged.thinking_level, ThinkingLevel::High));
        assert!(matches!(merged.tool_set, ToolSet::CodingTools));
        assert!(merged.resume_session);
    }

    /// Regression: the original bug. Reconciliation built a bare entry
    /// (no system prompt because the on-disk header doesn't carry one),
    /// then the merge cloned the entry wholesale and stripped the
    /// frontend-supplied system prompt. The merged options must NOT lose
    /// the request's system prompt just because the graveyard's is None.
    #[test]
    fn merge_graveyard_does_not_strip_request_system_prompt_when_entry_lacks_one() {
        let graveyard_options = PiSessionOptions::for_model("anthropic/claude-opus-4-7");
        assert!(graveyard_options.system_prompt.is_none());
        let ge = GraveyardEntry {
            options: graveyard_options,
        };
        let request_options = PiSessionOptions::for_model("anthropic/claude-opus-4-7")
            .with_system_prompt("FRONTEND SYSTEM PROMPT");
        let merged = merge_graveyard_into(request_options, Some(&ge), true, false, false);
        assert_eq!(
            merged.system_prompt.as_deref(),
            Some("FRONTEND SYSTEM PROMPT"),
            "request system prompt MUST survive merge with a bare graveyard entry"
        );
        assert!(merged.resume_session);
    }

    #[test]
    fn message_size_check_constant_is_sane() {
        // Sanity check that the cap exists and is a reasonable size.
        // This guards against accidental shrinkage that would break normal use,
        // and verifies the constant is wired in (the size check in
        // `send_message` references it directly).
        assert_eq!(MAX_MESSAGE_LEN, 16 * 1024 * 1024);
        assert!(MAX_MESSAGE_LEN > 1024);
    }

    /// Audit 2.12: ensure the supervise wrapper used by the streaming
    /// `tokio::spawn` body emits a structured `chat-event` of type `error`
    /// when the body panics, instead of leaving a phantom spinner. This
    /// mirrors the exact cleanup shape used at the live call site
    /// (`commands/chat.rs` send_message → tokio::spawn(supervise!(...))).
    #[tokio::test]
    async fn chat_stream_panic_emits_error_chat_event() {
        use tauri::Listener;
        let app = tauri::test::mock_app();
        let app_handle = app.handle().clone();
        let sid = "test-session-2_12".to_string();

        let received = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let received_clone = received.clone();
        let target_sid = sid.clone();
        let _listener = app_handle.listen("chat-event", move |event| {
            // The payload is JSON; only collect events for our session whose
            // type is "error" so we don't fight unrelated test traffic.
            let payload = event.payload();
            if payload.contains(&target_sid) && payload.contains("\"error\"") {
                received_clone.lock().unwrap().push(payload.to_string());
            }
        });

        let panic_app = app_handle.clone();
        let panic_sid = sid.clone();
        let supervised = crate::supervise!(
            context = format!("chat session_id={} component=stream", panic_sid),
            on_panic = move |panic_msg: String| {
                let _ = panic_app.emit(
                    "chat-event",
                    ChatEvent {
                        session_id: panic_sid.clone(),
                        event_type: "error".to_string(),
                        content: format!("internal task panicked: {panic_msg}"),
                    },
                );
            },
            async move {
                crate::util::supervise::panic_for_test("chat_stream_test");
            }
        );

        tokio::spawn(supervised)
            .await
            .expect("supervisor must absorb the panic");

        // Allow the Tauri event loop to deliver the emit before we check.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let got = received.lock().unwrap().clone();
        assert!(
            !got.is_empty(),
            "expected at least one chat-event of type error after panic, got: {:?}",
            got
        );
        assert!(
            got.iter().any(|p| p.contains("internal task panicked")),
            "expected panic message in error event, got: {:?}",
            got
        );
    }
}
