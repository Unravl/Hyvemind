//! Batched periodic Nurse review across all active sessions.
//!
//! Every `nurse_batch_interval_secs` the [`BatchReviewer::tick`] task:
//!
//! 1. Snapshots every active streaming Pi session known to the engine.
//! 2. Pulls the most recent `nurse_batch_events_per_session` `PiEvent`s
//!    out of each session's bounded transcript ring.
//! 3. Renders each transcript into a compact `[think] / [text] /
//!    [tool→name] / [tool←name]` log capped at
//!    `nurse_batch_chars_per_session`.
//! 4. Builds ONE batched LLM prompt covering all sessions and sends it
//!    through `ProviderRegistry` using the engine-wide
//!    `nurse_model` / `nurse_provider`.
//! 5. Parses the model's per-session JSON decisions and dispatches each
//!    one through [`crate::nurse::dispatcher::Dispatcher::dispatch_batch_decision`].
//!
//! Designed to be cheap: single LLM call per tick regardless of how many
//! sessions are active. Catches looping / stuck / silently-broken sessions
//! that the heuristic detectors miss — they keyed on text patterns we
//! can't always predict, the LLM keys on the content directly.

use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Arc, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use tauri::Emitter;

use crate::nurse::config::NurseProfile;
use crate::nurse::dispatcher::Dispatcher;
use crate::nurse::engine::NurseEngine;
use crate::nurse::snapshot::NurseDecision;
use crate::pi::events::PiEvent;
use crate::pi::session::SessionOwner;
use crate::providers::provider_trait::CallRequest;
use crate::providers::ProviderRegistry;
use crate::state::sync::AsyncRwLock;

/// Live status of the batched-review ticker. Surfaced through
/// `NurseStatusSnapshot` so the topbar countdown bar can render an
/// accurate progress indicator without polling.
#[derive(Debug, Default)]
pub struct BatchTickStatus {
    /// Unix-millis timestamp of the previous tick's completion (or 0 if
    /// none yet). The frontend uses this to compute elapsed-since.
    pub last_tick_at_unix_ms: AtomicU64,
    /// Wall-clock duration of the most recent tick in ms.
    pub last_tick_duration_ms: AtomicU64,
    /// Unix-millis the next tick is scheduled to fire. Updated at the end
    /// of each tick.
    pub next_tick_at_unix_ms: AtomicU64,
    /// Number of sessions included in the most recent tick.
    pub last_tick_session_count: AtomicU64,
}

impl BatchTickStatus {
    pub fn snapshot(&self) -> BatchTickSnapshot {
        BatchTickSnapshot {
            last_tick_at_unix_ms: self.last_tick_at_unix_ms.load(AtomicOrdering::Relaxed),
            last_tick_duration_ms: self.last_tick_duration_ms.load(AtomicOrdering::Relaxed),
            next_tick_at_unix_ms: self.next_tick_at_unix_ms.load(AtomicOrdering::Relaxed),
            last_tick_session_count: self.last_tick_session_count.load(AtomicOrdering::Relaxed),
        }
    }
}

/// Wire-shape DTO emitted as part of the Nurse status snapshot. The
/// frontend renders the topbar progress bar from these fields.
#[derive(Debug, Clone, Serialize)]
pub struct BatchTickSnapshot {
    pub last_tick_at_unix_ms: u64,
    pub last_tick_duration_ms: u64,
    pub next_tick_at_unix_ms: u64,
    pub last_tick_session_count: u64,
}

pub struct BatchReviewer {
    engine: Weak<NurseEngine>,
    registry: Arc<AsyncRwLock<ProviderRegistry>>,
    dispatcher: Weak<Dispatcher>,
    pub status: Arc<BatchTickStatus>,
}

impl BatchReviewer {
    pub fn new(
        engine: Weak<NurseEngine>,
        registry: Arc<AsyncRwLock<ProviderRegistry>>,
        dispatcher: Weak<Dispatcher>,
    ) -> Self {
        Self {
            engine,
            registry,
            dispatcher,
            status: Arc::new(BatchTickStatus::default()),
        }
    }

    /// One tick of the batched review. Returns `Ok(())` even when no
    /// sessions are active or no decisions are dispatched — only
    /// configuration/provider errors propagate.
    pub async fn tick(&self) -> Result<()> {
        let started = std::time::Instant::now();
        let started_unix_ms = unix_millis_now();

        // 1. Upgrade engine + dispatcher. If either is gone the engine is
        //    shutting down — silently no-op.
        let engine = match self.engine.upgrade() {
            Some(e) => e,
            None => return Ok(()),
        };
        let dispatcher = match self.dispatcher.upgrade() {
            Some(d) => d,
            None => return Ok(()),
        };

        // 2. Snapshot engine config. If nurse is disabled or batch review
        //    is disabled, skip without touching providers or sessions.
        let nurse_cfg = engine.config.read().await.clone();
        let interval_secs = nurse_cfg.effective_batch_interval_secs();
        if !nurse_cfg.enabled || !nurse_cfg.nurse_batch_enabled {
            self.record_idle_tick(started_unix_ms, started, &engine, interval_secs);
            return Ok(());
        }

        // 3. Snapshot active sessions under a brief read lock — never hold
        //    the engine.sessions lock across an `.await`. (Hidden invariant
        //    from CLAUDE.md: config → sessions; never sessions across await.)
        let max_events = crate::tunables::nurse_batch_events_per_session();
        let max_chars = crate::tunables::nurse_batch_chars_per_session();
        let swarms_only = nurse_cfg.swarms_only;
        let candidates: Vec<BatchCandidate> = {
            let sessions_guard = engine.sessions.read().unwrap_or_else(|p| p.into_inner());
            sessions_guard
                .iter()
                .filter_map(|(sid, state)| {
                    if should_skip_for_swarms_only(swarms_only, &state.owner) {
                        return None;
                    }
                    let session = state.session.upgrade()?;
                    if !session.is_busy() {
                        return None;
                    }
                    let activity_count = session.nurse_activity_count();
                    if activity_count == 0
                        || activity_count <= state.last_batch_reviewed_activity_count
                    {
                        return None;
                    }
                    let profile = NurseProfile::for_owner(&state.owner);
                    Some(BatchCandidate {
                        session_id: sid.clone(),
                        owner_label: format!("{:?}", state.owner),
                        profile,
                        session,
                        activity_count,
                    })
                })
                .collect()
        };

        if candidates.is_empty() {
            self.record_idle_tick(started_unix_ms, started, &engine, interval_secs);
            return Ok(());
        }

        // 4. Build per-session inputs (fetch transcripts off-lock).
        let mut inputs = Vec::with_capacity(candidates.len());
        for c in candidates {
            let events = c.session.get_recent_transcript(max_events).await;
            if events.is_empty() {
                continue;
            }
            let rendered = render_transcript_for_prompt(&events, max_chars);
            if rendered.trim().is_empty() {
                continue;
            }
            inputs.push(BatchSessionInput {
                session_id: c.session_id,
                owner_label: c.owner_label,
                profile: c.profile,
                transcript: rendered,
                activity_count: c.activity_count,
            });
        }
        if inputs.is_empty() {
            self.record_idle_tick(started_unix_ms, started, &engine, interval_secs);
            return Ok(());
        }

        // 5. Resolve provider/model. Engine-wide settings only — batch
        //    review is profile-agnostic by design (one call covers all
        //    sessions across all profiles). If no model is configured we
        //    skip silently rather than spamming errors every tick.
        let model_full = match nurse_cfg.nurse_model.as_deref() {
            Some(m) if !m.is_empty() && !m.eq_ignore_ascii_case("none") => m.to_string(),
            _ => {
                self.record_idle_tick(started_unix_ms, started, &engine, interval_secs);
                return Ok(());
            }
        };
        let provider_name = match nurse_cfg.nurse_provider.as_deref() {
            Some(p) if !p.trim().is_empty() => p.to_string(),
            _ => match model_full.split_once('/') {
                Some((p, _)) => p.to_string(),
                None if model_full.starts_with("claude-") => "anthropic".to_string(),
                None => "openrouter".to_string(),
            },
        };
        let api_model = if provider_name == "openrouter" {
            model_full.clone()
        } else {
            model_full
                .split_once('/')
                .map(|(_, m)| m.to_string())
                .unwrap_or_else(|| model_full.clone())
        };

        // 6. Build prompt + dispatch one provider call.
        let prompt = build_batch_prompt(&inputs);
        let system = batch_system_prompt();
        // `with_cache_static_prefix(true)` mirrors the per-session classifier
        // path so Anthropic prompt caching turns on for the static system
        // prompt; DeepSeek and other OpenAI-compatible backends ignore the
        // flag and cache automatically on byte-stable prefixes.
        let req = CallRequest::new(api_model, system, prompt)
            .with_timeout(Some(Duration::from_secs(
                crate::tunables::nurse_provider_timeout_secs(),
            )))
            .with_cache_static_prefix(true);
        let provider_arc = {
            let reg = self.registry.read().await;
            reg.get(&provider_name)
                .ok_or_else(|| {
                    anyhow!(
                        "nurse batch-review provider '{}' not registered",
                        provider_name
                    )
                })?
                .clone()
        };

        {
            let mut sessions = engine.sessions.write().unwrap_or_else(|p| p.into_inner());
            for input in &inputs {
                if let Some(state) = sessions.get_mut(&input.session_id) {
                    state.last_batch_reviewed_activity_count = input.activity_count;
                }
            }
        }

        // Bump the engine-wide nurse-LLM counter before issuing the call
        // so the topbar reflects intent (a mid-call crash still counts).
        // Engine may be gone if shutting down — fall through silently.
        if let Some(engine) = self.engine.upgrade() {
            engine
                .health
                .llm_calls_total
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        let call_started = std::time::Instant::now();
        let resp = provider_arc.call(req).await?;
        let call_duration_ms = call_started.elapsed().as_millis() as u64;
        let cache_hit_tokens = resp.cache_hit_tokens;
        let cache_write_tokens = resp.cache_write_tokens;
        let input_tokens = resp.input_tokens;
        let output_tokens = resp.output_tokens;
        let raw = resp.output;
        let decisions = parse_batch_decisions(&raw)?;

        // Emit a single-row `batch_classifier_returned` event so the
        // user-facing decisions.jsonl carries the per-tick provider-cache
        // stats. One call → many per-session decisions, so this row sits
        // alongside (not inside) each per-session chain that follows; the
        // synthetic decision_id keeps them distinguishable.
        let batch_decision_id = format!("batch-{}", uuid::Uuid::new_v4().simple());
        engine.observability.decisions.write(
            crate::nurse::observability::decision_log::events::batch_classifier_returned(
                &batch_decision_id,
                &provider_name,
                &model_full,
                input_tokens,
                output_tokens,
                cache_hit_tokens,
                cache_write_tokens,
                call_duration_ms,
                inputs.len(),
                decisions.len(),
            ),
        );

        // 7. Dispatch each per-session decision. Errors per session are
        //    logged but do not abort the rest of the batch.
        for d in decisions {
            // Look up the matching input to recover the profile (the model
            // is not asked to repeat the profile name back at us).
            let input = inputs.iter().find(|i| i.session_id == d.session_id);
            let profile = input.map(|i| i.profile).unwrap_or(NurseProfile::Default);
            let reasoning_str = Some(d.reasoning().to_string());
            dispatcher
                .dispatch_batch_decision(&d.session_id, profile, d.decision, reasoning_str)
                .await;
        }

        // 8. Record tick completion for the topbar progress bar.
        let finished_unix_ms = unix_millis_now();
        let duration_ms = started.elapsed().as_millis() as u64;
        let interval_ms = interval_secs * 1000;
        self.status
            .last_tick_at_unix_ms
            .store(finished_unix_ms, AtomicOrdering::Relaxed);
        self.status
            .last_tick_duration_ms
            .store(duration_ms, AtomicOrdering::Relaxed);
        self.status
            .next_tick_at_unix_ms
            .store(finished_unix_ms + interval_ms, AtomicOrdering::Relaxed);
        self.status
            .last_tick_session_count
            .store(inputs.len() as u64, AtomicOrdering::Relaxed);
        emit_status_update(&engine);
        Ok(())
    }

    /// Updates only the timing fields without touching session-count — used
    /// when a tick early-exits (no sessions / no model). Keeps the topbar
    /// progress bar advancing even when no work happened.
    fn record_idle_tick(
        &self,
        started_unix_ms: u64,
        started: std::time::Instant,
        engine: &NurseEngine,
        interval_secs: u64,
    ) {
        let now_unix_ms = unix_millis_now();
        let interval_ms = interval_secs * 1000;
        let _ = started_unix_ms;
        self.status
            .last_tick_at_unix_ms
            .store(now_unix_ms, AtomicOrdering::Relaxed);
        self.status.last_tick_duration_ms.store(
            started.elapsed().as_millis() as u64,
            AtomicOrdering::Relaxed,
        );
        self.status
            .next_tick_at_unix_ms
            .store(now_unix_ms + interval_ms, AtomicOrdering::Relaxed);
        self.status
            .last_tick_session_count
            .store(0, AtomicOrdering::Relaxed);
        emit_status_update(engine);
    }
}

fn emit_status_update(engine: &NurseEngine) {
    let Some(ctx) = engine.intervention_ctx.get() else {
        return;
    };
    if let Err(e) = ctx.app.emit(
        "nurse-event",
        crate::nurse::snapshot::NurseEvent::StatusUpdate(engine.snapshot_status()),
    ) {
        tracing::warn!(error = %e, "nurse batch-review: failed to emit StatusUpdate");
    }
}

struct BatchCandidate {
    session_id: String,
    owner_label: String,
    profile: NurseProfile,
    session: Arc<crate::pi::session::PiSession>,
    activity_count: u64,
}

#[derive(Debug, Clone)]
pub struct BatchSessionInput {
    pub session_id: String,
    pub owner_label: String,
    pub profile: NurseProfile,
    pub transcript: String,
    pub activity_count: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BatchDecisionRow {
    pub session_id: String,
    /// The chosen action. Note: `NurseDecision` itself is tagged with
    /// `#[serde(tag = "decision")]` and carries `reasoning` inline, so
    /// the wire shape lays flat at the row level:
    /// `{"session_id":"...", "decision":"steer", "message":"...", "reasoning":"..."}`.
    #[serde(flatten)]
    pub decision: NurseDecision,
}

impl BatchDecisionRow {
    pub fn reasoning(&self) -> &str {
        self.decision.reasoning()
    }
}

/// Render a slice of `PiEvent`s into a compact, role-tagged transcript
/// suitable for stuffing into a classifier prompt. Adjacent
/// thinking-only or text-only deltas are coalesced into a single block
/// (LLMs stream token-by-token, so per-delta lines would be useless
/// noise). Tool calls and messages get explicit boundaries so the model
/// can see structure. Output is hard-capped at `max_chars`; once the cap
/// is reached, additional events are dropped silently (the most recent
/// content matters most for loop detection, so we render newest-first
/// and reverse at the end).
///
/// Format:
/// ```text
/// [think] The user is asking me to ... (coalesced)
/// [text] Sure, here is ... (coalesced)
/// [tool→read] {"path":"foo.rs"}
/// [tool←read]
/// --- message end ---
/// ```
pub fn render_transcript_for_prompt(events: &[PiEvent], max_chars: usize) -> String {
    // Newest-first rendering: prefer recent content when capping. We
    // build forward into a Vec<String> of lines, count chars, then drop
    // older lines once the budget is exhausted, and finally re-join.
    let mut lines: Vec<String> = Vec::new();

    enum Pending {
        None,
        Think(String),
        Text(String),
    }
    let mut pending = Pending::None;

    let flush = |pending: &mut Pending, lines: &mut Vec<String>| match std::mem::replace(
        pending,
        Pending::None,
    ) {
        Pending::None => {}
        Pending::Think(s) => {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                lines.push(format!("[think] {}", clip_line(trimmed, 800)));
            }
        }
        Pending::Text(s) => {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                lines.push(format!("[text] {}", clip_line(trimmed, 800)));
            }
        }
    };

    for ev in events {
        match ev {
            PiEvent::ThinkingDelta(s) => match &mut pending {
                Pending::Think(buf) => buf.push_str(s),
                _ => {
                    flush(&mut pending, &mut lines);
                    pending = Pending::Think(s.clone());
                }
            },
            PiEvent::TextDelta(s) => match &mut pending {
                Pending::Text(buf) => buf.push_str(s),
                _ => {
                    flush(&mut pending, &mut lines);
                    pending = Pending::Text(s.clone());
                }
            },
            PiEvent::ToolExecutionStart { name, args, .. } => {
                flush(&mut pending, &mut lines);
                let args_preview = args.to_string();
                lines.push(format!(
                    "[tool\u{2192}{}] {}",
                    name,
                    clip_line(args_preview.trim(), 200)
                ));
            }
            PiEvent::ToolExecutionEnd { result, .. } => {
                flush(&mut pending, &mut lines);
                let preview = result.to_string();
                if preview.len() <= 80 {
                    lines.push(format!("[tool\u{2190}] {}", preview));
                } else {
                    lines.push("[tool\u{2190}]".to_string());
                }
            }
            PiEvent::MessageEnd => {
                flush(&mut pending, &mut lines);
                lines.push("--- message end ---".to_string());
            }
            PiEvent::Error(s) => {
                flush(&mut pending, &mut lines);
                lines.push(format!("[error] {}", clip_line(s.trim(), 400)));
            }
            PiEvent::AutoRetryStart { error_message, .. } => {
                flush(&mut pending, &mut lines);
                lines.push(format!("[retry] {}", clip_line(error_message.trim(), 200)));
            }
            _ => {}
        }
    }
    flush(&mut pending, &mut lines);

    // Enforce the global char cap by dropping OLDEST lines until we fit.
    // (Recent content matters most for loop detection.)
    let mut total: usize = lines.iter().map(|l| l.len() + 1).sum();
    while total > max_chars && !lines.is_empty() {
        let drop_len = lines.remove(0).len() + 1;
        total = total.saturating_sub(drop_len);
    }

    lines.join("\n")
}

fn clip_line(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    // Cut on a char boundary close to `max`.
    let mut end = max;
    while !s.is_char_boundary(end) && end > 0 {
        end -= 1;
    }
    format!("{}\u{2026}", &s[..end])
}

/// Build the user-prompt body sent to the batch reviewer. Wrapped in
/// numbered sections so the LLM can correlate decisions back to a
/// session id without ambiguity.
fn build_batch_prompt(inputs: &[BatchSessionInput]) -> String {
    let mut out = String::new();
    out.push_str(
        "You are reviewing the recent activity of the following Pi agent sessions. \
For each session, decide ONE action and return a single JSON object on the \
LAST line of your response with the shape:\n\n",
    );
    out.push_str(
        r#"{"decisions":[{"session_id":"...","decision":"leave_it","check_back_secs":120,"reasoning":"..."},{"session_id":"...","decision":"steer","message":"...","reasoning":"..."},{"session_id":"...","decision":"cancel","message":"...","reasoning":"..."},{"session_id":"...","decision":"restart","reasoning":"..."}]}"#,
    );
    out.push_str(
        "\n\nValid `decision` values: \"leave_it\" | \"steer\" | \"cancel\" | \"restart\".\n",
    );
    out.push_str(
        "- `leave_it` — the session is making real progress; specify `check_back_secs` (60-1800).\n",
    );
    out.push_str(
        "- `steer { message }` — aborts the current turn AND sends `message` as a redirect. \
Pick this when you see clear repetition or the agent is going down the wrong path. \
The message should be concrete: tell it what to do instead.\n",
    );
    out.push_str(
        "- `cancel { message }` — kill the in-flight turn entirely. The user sees `message`. \
Use sparingly; prefer `steer` if a redirect could rescue it.\n",
    );
    out.push_str(
        "- `restart` — kill and respawn the session. Use only when the agent is unrecoverable.\n\n",
    );
    out.push_str(
        "Do NOT return decisions for sessions you don't see below. Do NOT invent session ids.\n\n",
    );
    for (i, input) in inputs.iter().enumerate() {
        out.push_str(&format!(
            "## Session {} of {}\nsession_id: {}\nowner: {}\nprofile: {:?}\n\n```\n{}\n```\n\n",
            i + 1,
            inputs.len(),
            input.session_id,
            input.owner_label,
            input.profile,
            input.transcript,
        ));
    }
    out
}

fn batch_system_prompt() -> String {
    String::from(
        "You are Nurse — a supervisor for running LLM agent sessions. Every \
N minutes you receive the recent transcripts of all active sessions. Your job \
is to spot sessions that are stuck (looping, repeating the same thought, \
chasing a dead end, error-spinning) and dispatch an action.\n\n\
Bias hard toward `leave_it`. Real agents emit messy thinking and that is fine \
as long as work is progressing — new tool calls, varied content, forward \
motion. Only steer/cancel when you have *evidence* of a stuck state: the same \
sentence repeating, the same tool failing five times, no tool calls for a \
long stretch with no real progress.\n\n\
When you do steer, write a SHORT concrete redirect — name the loop you saw and \
tell the agent what to do next. Do not lecture.\n\n\
Return ONLY the JSON object on the final line. Any reasoning prose before that \
JSON line will be ignored.",
    )
}

/// Parse the model's response into per-session decisions. Accepts:
/// 1. A bare JSON object `{"decisions":[...]}`.
/// 2. Trailing-JSON form: any text followed by a final line with the JSON.
/// 3. The JSON wrapped in a fenced code block.
fn parse_batch_decisions(raw: &str) -> Result<Vec<BatchDecisionRow>> {
    // Try parsing the raw text as JSON first.
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(raw.trim()) {
        if let Some(arr) = v.get("decisions").and_then(|d| d.as_array()) {
            return parse_decision_array(arr);
        }
    }

    // Strip fenced code blocks, then try.
    if let Some(stripped) = strip_fenced_json(raw) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&stripped) {
            if let Some(arr) = v.get("decisions").and_then(|d| d.as_array()) {
                return parse_decision_array(arr);
            }
        }
    }

    // Fall back: scan for the LAST `{` ... `}` substring and try that.
    if let Some(last_obj) = last_balanced_json_object(raw) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&last_obj) {
            if let Some(arr) = v.get("decisions").and_then(|d| d.as_array()) {
                return parse_decision_array(arr);
            }
        }
    }

    Err(anyhow!(
        "nurse batch-review: could not parse decisions JSON (len={})",
        raw.len()
    ))
}

fn parse_decision_array(arr: &[serde_json::Value]) -> Result<Vec<BatchDecisionRow>> {
    let mut out = Vec::with_capacity(arr.len());
    for el in arr {
        match serde_json::from_value::<BatchDecisionRow>(el.clone()) {
            Ok(row) => out.push(row),
            Err(e) => {
                tracing::warn!(error = %e, raw = %el, "nurse batch-review: skipping malformed decision row");
            }
        }
    }
    Ok(out)
}

fn strip_fenced_json(raw: &str) -> Option<String> {
    // Match ```json ... ``` or ``` ... ``` blocks.
    let trimmed = raw.trim();
    if !trimmed.contains("```") {
        return None;
    }
    let after_open = trimmed.find("```")?;
    let body_start = trimmed[after_open + 3..]
        .find('\n')
        .map(|n| after_open + 3 + n + 1)
        .unwrap_or(after_open + 3);
    let after_body = &trimmed[body_start..];
    let close = after_body.find("```")?;
    Some(after_body[..close].trim().to_string())
}

fn last_balanced_json_object(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    let mut last_close: Option<usize> = None;
    for (i, b) in bytes.iter().enumerate() {
        if *b == b'}' {
            last_close = Some(i);
        }
    }
    let close = last_close?;
    // Walk backwards counting braces to find the matching `{`.
    let mut depth: i32 = 0;
    let mut start: Option<usize> = None;
    for i in (0..=close).rev() {
        match bytes[i] {
            b'}' => depth += 1,
            b'{' => {
                depth -= 1;
                if depth == 0 {
                    start = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    let s = start?;
    Some(raw[s..=close].to_string())
}

/// Pure predicate: when `swarms_only` is true, skip every owner that
/// isn't a `SessionOwner::Swarm`. Extracted so tests can exercise it
/// without standing up the engine.
pub fn should_skip_for_swarms_only(swarms_only: bool, owner: &SessionOwner) -> bool {
    swarms_only && !matches!(owner, SessionOwner::Swarm { .. })
}

fn unix_millis_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn render_coalesces_adjacent_thinking_deltas() {
        let events = vec![
            PiEvent::ThinkingDelta("Hello ".into()),
            PiEvent::ThinkingDelta("world, ".into()),
            PiEvent::ThinkingDelta("how are you?".into()),
        ];
        let out = render_transcript_for_prompt(&events, 10_000);
        // One [think] line, joined.
        assert_eq!(out, "[think] Hello world, how are you?");
    }

    #[test]
    fn render_separates_text_from_thinking() {
        let events = vec![
            PiEvent::ThinkingDelta("Thinking about it.".into()),
            PiEvent::TextDelta("Answer: 42.".into()),
            PiEvent::ThinkingDelta("More thinking.".into()),
        ];
        let out = render_transcript_for_prompt(&events, 10_000);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("[think] Thinking about it."));
        assert!(lines[1].starts_with("[text] Answer: 42."));
        assert!(lines[2].starts_with("[think] More thinking."));
    }

    #[test]
    fn render_emits_tool_boundaries() {
        let events = vec![
            PiEvent::ThinkingDelta("Reading config.".into()),
            PiEvent::ToolExecutionStart {
                tool_call_id: "tc1".into(),
                name: "read".into(),
                args: json!({"path": "config.json"}),
            },
            PiEvent::ToolExecutionEnd {
                tool_call_id: "tc1".into(),
                result: json!({"content": "..."}),
            },
        ];
        let out = render_transcript_for_prompt(&events, 10_000);
        assert!(out.contains("[think] Reading config."));
        assert!(out.contains("[tool\u{2192}read]"));
        assert!(out.contains("[tool\u{2190}]"));
    }

    #[test]
    fn render_caps_oldest_first_when_over_budget() {
        let events: Vec<PiEvent> = (0..50)
            .map(|i| PiEvent::TextDelta(format!("line {} content is here\n", i)))
            .collect();
        // 64 chars budget should keep only the last line(s).
        let out = render_transcript_for_prompt(&events, 64);
        assert!(out.len() <= 64);
        // The output must end with a recent line, never with `line 0`.
        assert!(!out.contains("line 0 "));
    }

    #[test]
    fn render_emits_message_end_marker() {
        let events = vec![PiEvent::TextDelta("Done.".into()), PiEvent::MessageEnd];
        let out = render_transcript_for_prompt(&events, 10_000);
        assert!(out.contains("--- message end ---"));
    }

    #[test]
    fn parse_batch_decisions_accepts_bare_json() {
        let raw = r#"{"decisions":[{"session_id":"s1","decision":"leave_it","check_back_secs":120,"reasoning":"healthy"}]}"#;
        let out = parse_batch_decisions(raw).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].session_id, "s1");
        match &out[0].decision {
            NurseDecision::LeaveIt {
                check_back_secs, ..
            } => assert_eq!(*check_back_secs, 120),
            other => panic!("expected LeaveIt, got {:?}", other),
        }
    }

    #[test]
    fn parse_batch_decisions_accepts_fenced_json() {
        let raw = r#"Here is my answer:

```json
{
  "decisions": [
    {"session_id":"s1","decision":"steer","message":"stop repeating","reasoning":"3 identical lines"}
  ]
}
```
"#;
        let out = parse_batch_decisions(raw).unwrap();
        assert_eq!(out.len(), 1);
        match &out[0].decision {
            NurseDecision::Steer { message, .. } => assert_eq!(message, "stop repeating"),
            other => panic!("expected Steer, got {:?}", other),
        }
    }

    #[test]
    fn parse_batch_decisions_accepts_trailing_json() {
        let raw = r#"Some reasoning prose explaining what I think...

{"decisions":[{"session_id":"s2","decision":"cancel","message":"unsalvageable","reasoning":"dead"}]}"#;
        let out = parse_batch_decisions(raw).unwrap();
        assert_eq!(out.len(), 1);
        match &out[0].decision {
            NurseDecision::Cancel { message, .. } => assert_eq!(message, "unsalvageable"),
            other => panic!("expected Cancel, got {:?}", other),
        }
    }

    #[test]
    fn parse_batch_decisions_skips_malformed_rows() {
        let raw = r#"{"decisions":[
            {"session_id":"s1","decision":"leave_it","check_back_secs":120,"reasoning":"ok"},
            {"session_id":"s2","decision":"made_up_action"}
        ]}"#;
        let out = parse_batch_decisions(raw).unwrap();
        // Malformed s2 is skipped; s1 still parses.
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].session_id, "s1");
    }

    #[test]
    fn parse_batch_decisions_errors_on_garbage() {
        let err = parse_batch_decisions("not json at all").unwrap_err();
        assert!(err.to_string().contains("could not parse"));
    }

    #[test]
    fn should_skip_for_swarms_only_matrix() {
        let task = SessionOwner::Task {
            task_id: "t".into(),
        };
        let swarm = SessionOwner::Swarm {
            swarm_id: "s".into(),
            role: "worker".into(),
        };
        let review = SessionOwner::Review { job_id: "j".into() };
        let merge = SessionOwner::Merge {
            job_id: "j".into(),
            round: 1,
            swarm_id: None,
        };
        // swarms_only = true: only Swarm passes.
        assert!(should_skip_for_swarms_only(true, &task));
        assert!(!should_skip_for_swarms_only(true, &swarm));
        assert!(should_skip_for_swarms_only(true, &review));
        assert!(should_skip_for_swarms_only(true, &merge));
        // swarms_only = false: nothing is skipped.
        assert!(!should_skip_for_swarms_only(false, &task));
        assert!(!should_skip_for_swarms_only(false, &swarm));
        assert!(!should_skip_for_swarms_only(false, &review));
        assert!(!should_skip_for_swarms_only(false, &merge));
    }

    #[test]
    fn build_batch_prompt_includes_all_sessions() {
        let inputs = vec![
            BatchSessionInput {
                session_id: "s1".into(),
                owner_label: "Task { task_id: \"abc\" }".into(),
                profile: NurseProfile::Tasks,
                transcript: "[text] hello".into(),
                activity_count: 1,
            },
            BatchSessionInput {
                session_id: "s2".into(),
                owner_label: "Swarm { swarm_id: \"def\" }".into(),
                profile: NurseProfile::Swarm,
                transcript: "[think] working".into(),
                activity_count: 1,
            },
        ];
        let prompt = build_batch_prompt(&inputs);
        assert!(prompt.contains("session_id: s1"));
        assert!(prompt.contains("session_id: s2"));
        assert!(prompt.contains("Session 1 of 2"));
        assert!(prompt.contains("Session 2 of 2"));
    }
}
