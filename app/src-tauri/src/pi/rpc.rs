use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{broadcast, oneshot, Mutex, OwnedSemaphorePermit};
use tracing::Instrument;

use super::events::PiEvent;

// ---------------------------------------------------------------------------
// Reader hard caps
// ---------------------------------------------------------------------------

/// Hard cap on a single line read from Pi's stdout/stderr before we treat it
/// as hostile and drop it. Pi normally emits compact JSONL events; any line
/// pushing past a few MiB is either pathologically large tool output or a
/// misbehaving extension. Without a cap, a 1 GiB single-line payload would
/// be buffered in full by `lines()` and OOM the host before parsing — a
/// trivial DoS path. 4 MiB is well clear of any legitimate Pi event and
/// keeps the worst-case allocation bounded.
const MAX_PI_LINE_BYTES: usize = 4 * 1024 * 1024;

/// Chunk size used when draining the remainder of an oversized line so we
/// don't reallocate the discard buffer unboundedly while throwing bytes
/// away.
const PI_LINE_DRAIN_CHUNK_BYTES: usize = 64 * 1024;

/// Outcome of a single capped line read.
#[derive(Debug, PartialEq, Eq)]
enum ReadLineOutcome {
    /// A complete line (≤ cap) is in the output buffer, including any
    /// trailing `\n`.
    Line,
    /// The line exceeded the cap. The output buffer holds at most
    /// `MAX_PI_LINE_BYTES` of it; `dropped_bytes` were discarded while
    /// draining the reader to end-of-line (or EOF).
    Overflow { dropped_bytes: u64 },
    /// Stream closed with no remaining bytes.
    Eof,
}

/// Reads one line from `reader` (delimited by `\n`, inclusive) into `out`,
/// capping the kept bytes at `MAX_PI_LINE_BYTES`. If the line exceeds the
/// cap, the function continues consuming the reader (in chunks bounded by
/// `PI_LINE_DRAIN_CHUNK_BYTES`) until it reaches the next newline or EOF —
/// so the *next* call resumes on a fresh line boundary — and reports the
/// number of dropped bytes.
///
/// This is what makes the stdout/stderr readers OOM-safe against a
/// misbehaving Pi or extension emitting an arbitrarily large single line.
async fn read_capped_line<R>(reader: &mut R, out: &mut Vec<u8>) -> std::io::Result<ReadLineOutcome>
where
    R: AsyncBufRead + Unpin,
{
    out.clear();

    let mut overflowed = false;
    let mut dropped: u64 = 0;
    let mut saw_any_byte = false;
    let mut saw_newline = false;

    // Phase 1: pull bytes from the BufReader, copying up to the cap into
    // `out` and counting the excess into `dropped`. We use fill_buf/consume
    // directly so we can stop *appending* without stopping *reading*.
    loop {
        let available = match reader.fill_buf().await {
            Ok(b) if b.is_empty() => {
                // EOF.
                if !saw_any_byte {
                    return Ok(ReadLineOutcome::Eof);
                }
                break;
            }
            Ok(b) => b,
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        saw_any_byte = true;

        if let Some(nl_pos) = available.iter().position(|&b| b == b'\n') {
            let line_part = &available[..=nl_pos]; // include the \n
            if !overflowed {
                let space = MAX_PI_LINE_BYTES.saturating_sub(out.len());
                if line_part.len() <= space {
                    out.extend_from_slice(line_part);
                } else {
                    out.extend_from_slice(&line_part[..space]);
                    dropped = (line_part.len() - space) as u64;
                    overflowed = true;
                }
            } else {
                dropped += line_part.len() as u64;
            }
            let consume_len = line_part.len();
            reader.consume(consume_len);
            saw_newline = true;
            break;
        }

        if !overflowed {
            let space = MAX_PI_LINE_BYTES.saturating_sub(out.len());
            if available.len() <= space {
                out.extend_from_slice(available);
            } else {
                out.extend_from_slice(&available[..space]);
                dropped = (available.len() - space) as u64;
                overflowed = true;
            }
        } else {
            dropped += available.len() as u64;
        }
        let consume_len = available.len();
        reader.consume(consume_len);
    }

    // Phase 2: if we overflowed but never reached a newline, drain forward
    // in chunks so the next call starts on a clean line boundary.
    if overflowed && !saw_newline {
        let mut discard: Vec<u8> = Vec::with_capacity(PI_LINE_DRAIN_CHUNK_BYTES);
        loop {
            discard.clear();
            // read_until returns 0 only at EOF; otherwise it returns the
            // bytes consumed, with `\n` at the end if a newline was found.
            let n = reader.read_until(b'\n', &mut discard).await?;
            if n == 0 {
                break;
            }
            dropped += n as u64;
            if discard.last() == Some(&b'\n') {
                break;
            }
            // No newline and read_until returned >0 only at EOF in tokio
            // 1.x; defensive break in case behaviour changes.
            if discard.is_empty() {
                break;
            }
        }
    }

    if overflowed {
        Ok(ReadLineOutcome::Overflow {
            dropped_bytes: dropped,
        })
    } else {
        Ok(ReadLineOutcome::Line)
    }
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors specific to the Pi RPC transport layer.
#[derive(Debug, thiserror::Error)]
pub enum PiRpcError {
    #[error("pi process crashed (exit_code={exit_code:?}): {stderr}")]
    ProcessCrashed {
        exit_code: Option<i32>,
        stderr: String,
    },

    #[error("pi RPC timeout")]
    Timeout,

    #[error("stdin closed")]
    StdinClosed,

    #[error("pi process is not available: {stderr}")]
    ProcessUnavailable { stderr: String },

    #[error(transparent)]
    IoError(#[from] std::io::Error),

    #[error(transparent)]
    JsonError(#[from] serde_json::Error),
}

// ---------------------------------------------------------------------------
// Thinking level
// ---------------------------------------------------------------------------

/// Thinking level configuration for Pi sessions.
///
/// Maps to Pi's `--thinking` CLI flag and `set_thinking_level` RPC command.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingLevel {
    Off,
    Low,
    Medium,
    High,
}

impl Default for ThinkingLevel {
    fn default() -> Self {
        Self::Medium
    }
}

impl std::str::FromStr for ThinkingLevel {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "off" => Ok(Self::Off),
            "low" => Ok(Self::Low),
            "medium" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            other => Err(format!("unknown thinking level: '{}'", other)),
        }
    }
}

impl std::fmt::Display for ThinkingLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ThinkingLevel::Off => write!(f, "off"),
            ThinkingLevel::Low => write!(f, "low"),
            ThinkingLevel::Medium => write!(f, "medium"),
            ThinkingLevel::High => write!(f, "high"),
        }
    }
}

// ---------------------------------------------------------------------------
// Tool sets
// ---------------------------------------------------------------------------

/// Tool set configuration for Pi sessions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolSet {
    /// Full coding tools: read, bash, edit, write, grep, find, ls
    CodingTools,
    /// Read-only tools: built-in (read, grep, find, ls) plus the bundled
    /// extension tools that don't mutate project state (web research, code
    /// search, subagent dispatch, MCP). Hyvemind-registered structured-output
    /// tools (`HYVEMIND_EXTENSION_TOOLS`) are appended automatically.
    ReadOnlyTools,
    /// Explicit list of tool names
    Custom(Vec<String>),
}

/// Names of the Hyvemind-registered Pi extension tools. The `hyvemind-handoff`
/// local extension registers these via `pi.registerTool(...)`; their
/// `execute` bodies are no-op echoes — the Rust backend captures the
/// model's tool-call `args` off `PiEvent::ToolExecutionStart` instead of
/// scanning the transcript for delimiters.
///
/// Listed in one place so every `ToolSet` variant agrees on what counts as
/// "the structured-output tools" and so adding a new one is a single-line
/// edit. Phase 2 wires `submit_handoff`; Phase 3 wires the planning trio;
/// Phase 4 wires `submit_context` and the stability-test tools. Phase 5/6
/// schemas go through the provider directly and don't need a Pi tool name.
pub const HYVEMIND_EXTENSION_TOOLS: &[&str] = &[
    "submit_handoff",
    "submit_task_complete",
    "submit_task_meta",
    "submit_questions",
    "submit_plan",
    "submit_features",
    "submit_context",
    "submit_review_prompt",
    "submit_stability_questions",
    "submit_stability_plan",
    "submit_stability_verdict",
    "submit_stability_impl_complete",
    "submit_scout_result",
    "submit_guard_result",
    "submit_verdicts",
    "submit_review",
];

impl Default for ToolSet {
    fn default() -> Self {
        Self::CodingTools
    }
}

impl std::fmt::Display for ToolSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ToolSet::CodingTools => write!(f, "coding_tools"),
            ToolSet::ReadOnlyTools => write!(f, "read_only_tools"),
            ToolSet::Custom(names) => write!(f, "custom({})", names.join(",")),
        }
    }
}

// ---------------------------------------------------------------------------
// Session options
// ---------------------------------------------------------------------------

/// Translate a Hyvemind-internal `provider/model` string into the form Pi
/// expects on its `--provider/--model` argv.
///
/// Hyvemind labels the Pi-managed subscription backends as `chatgpt/<model>`
/// and `claude-sub/<model>`, but Pi itself only knows them as
/// `openai-codex/<model>` and `anthropic/<model>`. Forgetting this mapping
/// at any spawn site causes Pi to error with `Model "claude-sub/..." not
/// found.` — so the mapping is applied inside every `PiSessionOptions::for_*`
/// constructor below. Already-mapped or non-subscription ids pass through
/// unchanged (the function is idempotent).
pub fn map_model_for_pi(model: &str) -> String {
    if let Some((provider, model_id)) = model.split_once('/') {
        let pi_provider = match provider {
            "chatgpt" => "openai-codex",
            "claude-sub" => "anthropic",
            _ => provider,
        };
        format!("{}/{}", pi_provider, model_id)
    } else {
        model.to_string()
    }
}

/// Configuration options for spawning a Pi session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PiSessionOptions {
    /// Model identifier (e.g. "claude-sonnet-4-20250514" or "anthropic/claude-sonnet-4")
    pub model: String,
    #[serde(default)]
    pub thinking_level: ThinkingLevel,
    #[serde(default)]
    pub tool_set: ToolSet,
    #[serde(default)]
    pub system_prompt: Option<String>,
    #[serde(default)]
    pub resume_session: bool,
    #[serde(default)]
    pub session_file: Option<String>,
}

impl PiSessionOptions {
    pub fn for_model(model: &str) -> Self {
        Self {
            model: map_model_for_pi(model),
            thinking_level: ThinkingLevel::default(),
            tool_set: ToolSet::default(),
            system_prompt: None,
            resume_session: false,
            session_file: None,
        }
    }

    pub fn for_scout(model: &str, system_prompt: &str) -> Self {
        Self {
            model: map_model_for_pi(model),
            thinking_level: ThinkingLevel::High,
            tool_set: ToolSet::ReadOnlyTools,
            system_prompt: Some(system_prompt.to_string()),
            resume_session: false,
            session_file: None,
        }
    }

    pub fn for_worker(model: &str, system_prompt: &str) -> Self {
        Self {
            model: map_model_for_pi(model),
            thinking_level: ThinkingLevel::Medium,
            tool_set: ToolSet::CodingTools,
            system_prompt: Some(system_prompt.to_string()),
            resume_session: false,
            session_file: None,
        }
    }

    pub fn for_guard(model: &str, system_prompt: &str) -> Self {
        let mut tools: Vec<String> = vec![
            "read".to_string(),
            "bash".to_string(),
            "grep".to_string(),
            "find".to_string(),
            "ls".to_string(),
            // Subagent delegation + MCP shim. Guard uses `subagent` for focused
            // fact-checks (e.g. "open this test file and tell me which assertions
            // failed") in a fresh child without polluting its own context. `mcp`
            // is included for symmetry — Guard already has `bash`, so refusing the
            // MCP shim is inconsistent. Do NOT strip these in a future refactor:
            // they are intentional, not oversight.
            "subagent".to_string(),
            "mcp".to_string(),
        ];
        // Guard's custom allowlist would otherwise exclude the structured
        // submission tools (and `subagent`/`mcp`, which are added explicitly
        // above for the same reason). Append the extension allowlist so any
        // future Guard-side use of the extension schemas works without a
        // follow-up rollout.
        tools.extend(HYVEMIND_EXTENSION_TOOLS.iter().map(|s| s.to_string()));
        Self {
            model: map_model_for_pi(model),
            thinking_level: ThinkingLevel::Medium,
            tool_set: ToolSet::Custom(tools),
            system_prompt: Some(system_prompt.to_string()),
            resume_session: false,
            session_file: None,
        }
    }

    pub fn with_session_file(mut self, path: impl Into<String>) -> Self {
        self.session_file = Some(path.into());
        self
    }

    pub fn with_resume(mut self) -> Self {
        self.resume_session = true;
        self
    }

    pub fn with_thinking_level(mut self, level: ThinkingLevel) -> Self {
        self.thinking_level = level;
        self
    }

    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(prompt.into());
        self
    }

    pub fn with_tool_set(mut self, tool_set: ToolSet) -> Self {
        self.tool_set = tool_set;
        self
    }

    /// Build CLI arguments for spawning the Pi process.
    pub fn to_cli_args(&self) -> Vec<String> {
        let mut args = Vec::new();

        args.push("--mode".to_string());
        args.push("rpc".to_string());

        if !self.resume_session && self.session_file.is_none() {
            args.push("--no-session".to_string());
        }

        args.push("--model".to_string());
        args.push(self.model.clone());

        args.push("--thinking".to_string());
        args.push(self.thinking_level.to_string());

        match &self.tool_set {
            // CodingTools doesn't pass `--tools`, so Pi defaults to allowing
            // every registered tool — including the Hyvemind extension tools
            // (`submit_handoff`, `submit_plan`, etc.). No allowlist edit
            // needed for this branch.
            ToolSet::CodingTools => {}
            ToolSet::ReadOnlyTools => {
                args.push("--tools".to_string());
                let mut allowlist = String::from(
                    "read,grep,find,ls,web_search,fetch_content,code_search,get_search_content,subagent,mcp",
                );
                for name in HYVEMIND_EXTENSION_TOOLS {
                    allowlist.push(',');
                    allowlist.push_str(name);
                }
                args.push(allowlist);
            }
            ToolSet::Custom(names) => {
                args.push("--tools".to_string());
                args.push(names.join(","));
            }
        }

        if let Some(ref prompt) = self.system_prompt {
            args.push("--system-prompt".to_string());
            args.push(prompt.clone());
        }

        if self.resume_session {
            args.push("--continue".to_string());
        }

        if let Some(ref file) = self.session_file {
            args.push("--session".to_string());
            args.push(file.clone());
        }

        args
    }
}

// ---------------------------------------------------------------------------
// Image types (for multimodal prompts)
// ---------------------------------------------------------------------------

/// An image content block matching the Pi SDK's `ImageContent` RPC format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PiImage {
    #[serde(rename = "type")]
    pub kind: String, // "image"
    pub data: String, // raw base64, no data-URL prefix
    #[serde(rename = "mimeType")]
    pub mime_type: String, // "image/png", "image/jpeg", etc.
}

// ---------------------------------------------------------------------------
// Commands sent to Pi (Pi RPC protocol)
// ---------------------------------------------------------------------------

/// Commands sent to Pi subprocess over stdin as JSONL.
///
/// Serialized with `{"type": "<command_name>", ...fields}` to match the
/// Pi RPC protocol specification.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum PiCommand {
    /// Send a prompt to the session.
    #[serde(rename = "prompt")]
    Prompt {
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        images: Option<Vec<PiImage>>,
    },

    /// Queue a steering message during an active turn.
    #[serde(rename = "steer")]
    Steer {
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        images: Option<Vec<PiImage>>,
    },

    /// Queue a follow-up message.
    #[serde(rename = "follow_up")]
    FollowUp {
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        images: Option<Vec<PiImage>>,
    },

    /// Abort the current operation.
    #[serde(rename = "abort")]
    Abort {},

    /// Change the model at runtime.
    #[serde(rename = "set_model")]
    SetModel {
        provider: String,
        #[serde(rename = "modelId")]
        model_id: String,
    },

    /// Change thinking level at runtime.
    #[serde(rename = "set_thinking_level")]
    SetThinkingLevel { level: ThinkingLevel },

    /// Request current session state.
    #[serde(rename = "get_state")]
    GetState {},

    /// Request token usage and context stats for the session.
    #[serde(rename = "get_session_stats")]
    GetSessionStats {},
}

// ---------------------------------------------------------------------------
// Pi RPC event parsing
// ---------------------------------------------------------------------------

/// Parse a raw JSONL line from Pi's stdout into our internal PiEvent.
///
/// Pi RPC events have a top-level `"type"` field. The structure varies by type:
/// - `message_update` contains `assistantMessageEvent` with sub-type
/// - `tool_execution_*` contains tool metadata
/// - Lifecycle events (`turn_start`, `agent_end`, etc.) are simple markers
/// - `response` acknowledges commands (errors surfaced as PiEvent::Error)
fn parse_pi_event(line: &str) -> Option<PiEvent> {
    let raw: serde_json::Value = serde_json::from_str(line).ok()?;
    let event_type = raw.get("type")?.as_str()?;

    match event_type {
        "message_update" => {
            let ae = raw.get("assistantMessageEvent")?;
            let sub_type = ae.get("type")?.as_str()?;
            // Pi has emitted at least four shapes in the wild for assistant
            // message updates: `text_delta`, `text`, `text_done`, and the
            // thinking equivalents. RC4 of the broken-handoff fix: previously
            // only `text_delta`/`thinking_delta` were accepted, and any other
            // sub-type returned None. That left `done` events sometimes
            // missing the final assistant text, and (worse) a silent path
            // where Pi's terminator hint was discarded entirely.
            //
            // Strategy: keep the explicit fast paths first, then fall through
            // to a permissive matcher that pulls `delta` (preferred) or `text`
            // out of any sub-type whose name contains "text" or "thinking".
            let pull_payload = || -> Option<String> {
                ae.get("delta")
                    .and_then(|d| d.as_str())
                    .or_else(|| ae.get("text").and_then(|t| t.as_str()))
                    .map(|s| s.to_string())
            };
            match sub_type {
                "text_delta" => {
                    let delta = ae.get("delta")?.as_str()?;
                    Some(PiEvent::TextDelta(delta.to_string()))
                }
                "thinking_delta" => {
                    let delta = ae.get("delta")?.as_str()?;
                    Some(PiEvent::ThinkingDelta(delta.to_string()))
                }
                other if other.contains("thinking") => {
                    let payload = pull_payload()?;
                    Some(PiEvent::ThinkingDelta(payload))
                }
                other if other.contains("text") => {
                    let payload = pull_payload()?;
                    Some(PiEvent::TextDelta(payload))
                }
                _ => None,
            }
        }
        "tool_execution_start" => {
            let tool_call_id = raw.get("toolCallId")?.as_str()?.to_string();
            let name = raw.get("toolName")?.as_str()?.to_string();
            // Pi's RPC `tool_execution_start` carries an `args` field with
            // the model's tool-call input. Hyvemind used to drop this;
            // capturing it is what lets `submit_*` extension tools relay
            // structured payloads from the model back to the Rust backend
            // without any delimiter parsing on the transcript.
            let args = raw.get("args").cloned().unwrap_or(serde_json::Value::Null);
            Some(PiEvent::ToolExecutionStart {
                tool_call_id,
                name,
                args,
            })
        }
        "tool_execution_update" => {
            let tool_call_id = raw.get("toolCallId")?.as_str()?.to_string();
            let output = raw
                .get("partialResult")
                .and_then(|r| r.get("content"))
                .and_then(|c| c.get(0))
                .and_then(|item| item.get("text"))
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string();
            Some(PiEvent::ToolExecutionUpdate {
                tool_call_id,
                output,
            })
        }
        "tool_execution_end" => {
            let tool_call_id = raw.get("toolCallId")?.as_str()?.to_string();
            let result = raw
                .get("result")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            Some(PiEvent::ToolExecutionEnd {
                tool_call_id,
                result,
            })
        }
        "message_start" => Some(PiEvent::MessageStart),
        "message_end" => Some(PiEvent::MessageEnd),
        "turn_start" => Some(PiEvent::TurnStart),
        "turn_end" => Some(PiEvent::TurnComplete),
        "agent_start" => Some(PiEvent::AgentStart),
        "agent_end" => Some(PiEvent::AgentEnd),
        "response" => {
            let success = raw.get("success").and_then(|s| s.as_bool()).unwrap_or(true);
            if !success {
                let error = raw
                    .get("error")
                    .and_then(|e| e.as_str())
                    .unwrap_or("unknown RPC error");
                Some(PiEvent::Error(error.to_string()))
            } else {
                // Pi nests get_session_stats data under "data", check both locations.
                let data = raw.get("data").unwrap_or(&raw);
                let tokens = data.get("tokens").or_else(|| raw.get("tokens"));
                let ctx = data.get("contextUsage").or_else(|| raw.get("contextUsage"));
                if tokens.is_some() || ctx.is_some() {
                    let cost = data
                        .get("cost")
                        .and_then(|c| c.as_f64())
                        .or_else(|| raw.get("cost").and_then(|c| c.as_f64()))
                        .unwrap_or(0.0);
                    Some(PiEvent::SessionStats(super::events::PiSessionStats {
                        input: tokens.and_then(|t| t["input"].as_u64()).unwrap_or(0),
                        output: tokens.and_then(|t| t["output"].as_u64()).unwrap_or(0),
                        reasoning_tokens: tokens
                            .and_then(|t| t["reasoningTokens"].as_u64())
                            .unwrap_or(0),
                        cache_read: tokens.and_then(|t| t["cacheRead"].as_u64()).unwrap_or(0),
                        cache_write: tokens.and_then(|t| t["cacheWrite"].as_u64()).unwrap_or(0),
                        total_tokens: tokens.and_then(|t| t["total"].as_u64()).unwrap_or(0),
                        cost,
                        context_tokens: ctx.and_then(|c| c["tokens"].as_u64()).unwrap_or(0),
                        context_window: ctx.and_then(|c| c["contextWindow"].as_u64()).unwrap_or(0),
                        context_percent: ctx.and_then(|c| c["percent"].as_f64()).unwrap_or(0.0),
                    }))
                } else {
                    None
                }
            }
        }
        "queue_update" => {
            let steering = raw
                .get("steering")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            let follow_up = raw
                .get("followUp")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            Some(PiEvent::QueueUpdate {
                steering,
                follow_up,
            })
        }
        "auto_retry_start" => {
            // Pi emits `agent_end` just BEFORE this event, then waits
            // `delay_ms` and replays the turn. `errorMessage` is a JSON
            // string containing the provider's raw error envelope (e.g.
            // Anthropic's `overloaded_error`). Surface enough metadata for
            // the UI to render a "retrying in Xs (attempt N/M)" banner.
            let attempt = raw.get("attempt").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            let max_attempts = raw.get("maxAttempts").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            let delay_ms = raw.get("delayMs").and_then(|v| v.as_u64()).unwrap_or(0);
            let error_message = raw
                .get("errorMessage")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Some(PiEvent::AutoRetryStart {
                attempt,
                max_attempts,
                delay_ms,
                error_message,
            })
        }
        "auto_retry_end" => {
            let success = raw
                .get("success")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let attempt = raw.get("attempt").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            Some(PiEvent::AutoRetryEnd { success, attempt })
        }
        // Pi emits this on session start with the effective thinking level
        // after clamping to the model's declared capabilities. The field is
        // `thinkingLevel` (camelCase) in the JSONL payload. Both
        // `thinking_level_change` and `thinking_level_changed` shapes have
        // been observed; accept either to be defensive against Pi schema
        // drift.
        "thinking_level_change" | "thinking_level_changed" => {
            let level = raw
                .get("thinkingLevel")
                .or_else(|| raw.get("level"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Some(PiEvent::ThinkingLevelChange { level })
        }
        "message" => {
            // Pi emits the authoritative assistant-message envelope at turn
            // end. Normal completions land at our streaming layer via
            // message_update + text_done; we only care about the *failure*
            // case here, where Pi sets `message.stopReason = "error"` and
            // stashes the provider's raw payload in `errorMessage`. Without
            // this branch the error is silently recorded to the session
            // JSONL and never reaches the UI — the spinner just hangs.
            let msg = raw.get("message")?;
            let stop_reason = msg.get("stopReason").and_then(|v| v.as_str()).unwrap_or("");
            if stop_reason != "error" {
                return None;
            }
            let error_message = msg
                .get("errorMessage")
                .and_then(|v| v.as_str())
                .unwrap_or("model turn ended with stopReason=error")
                .to_string();
            Some(PiEvent::Error(error_message))
        }
        _ => {
            // Promoted from TRACE→WARN (RC4): future Pi schema drift now
            // surfaces in normal operations logs instead of being invisible.
            tracing::warn!(event_type, "unhandled pi event type");
            None
        }
    }
}

/// True for `message_update` sub-types we intentionally drop in
/// `parse_pi_event`. The streaming layer reads tool calls via the
/// authoritative `tool_execution_*` family and text/thinking via
/// `text_delta`/`thinking_delta`; the sub-types listed here are
/// either duplicate streaming deltas (`toolcall_delta`) or payload-less
/// lifecycle markers (`*_start` / `*_end`). Suppressing them on the
/// "unrecognized" log path keeps the per-session debug file bounded —
/// each `toolcall_delta` carries the cumulative `partial` message and
/// re-shipping every one of them at full body grew the log to ~750 MB
/// on read-heavy sessions.
fn is_known_ignored_message_update(line: &str) -> bool {
    let v: serde_json::Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => return false,
    };
    if v.get("type").and_then(|t| t.as_str()) != Some("message_update") {
        return false;
    }
    let sub = v
        .get("assistantMessageEvent")
        .and_then(|ae| ae.get("type"))
        .and_then(|t| t.as_str());
    matches!(
        sub,
        Some(
            "toolcall_start"
                | "toolcall_delta"
                | "toolcall_end"
                | "thinking_start"
                | "thinking_end"
                | "text_start"
                | "text_end"
        )
    )
}

// ---------------------------------------------------------------------------
// RPC Client
// ---------------------------------------------------------------------------

/// Request envelope sent from `force_kill_child` to the monitor task.
/// The monitor task drives the kill/wait sequence (it owns the `Child`)
/// and sends `()` on `reply_tx` when the process is reaped or the
/// deadline expires.
struct ForceKillRequest {
    reply_tx: oneshot::Sender<()>,
}

/// Manages a single Pi subprocess, providing typed send/subscribe/shutdown.
///
/// Audit 7.2: implements `std::fmt::Debug` (manually — most fields are
/// non-Debug task handles or runtime primitives) so it satisfies the
/// `PiTransport: Debug` bound. The Debug output is intentionally minimal
/// — just `pid` and `alive` — to avoid logging anything that might race
/// with the monitor task on a contended mutex.
pub struct PiRpcClient {
    stdin: Arc<Mutex<tokio::process::ChildStdin>>,
    event_tx: broadcast::Sender<PiEvent>,
    alive: Arc<AtomicBool>,
    stderr_buf: Arc<Mutex<String>>,
    /// Shared owner of the spawned `tokio::process::Child`.
    ///
    /// Audit 2.9: the monitor task no longer owns the `Child` outright.
    /// At task start it locks this slot, takes the `Child` out, drives the
    /// wait/force-kill state machine on it, and leaves the slot as `None`
    /// when finished. The slot is shared with `Drop for PiRpcClient` so the
    /// last-resort kill path can synchronously fire `start_kill` on the
    /// process *without* relying on the monitor task to still be alive —
    /// previously, dropping the client merely aborted the monitor task
    /// handle and left `kill_on_drop` to fire only when the monitor's
    /// local `Child` binding finally got dropped on the next runtime tick.
    /// With this slot the `Drop` impl can also bypass the monitor entirely
    /// (e.g. when no async runtime is running at drop time).
    child: Arc<Mutex<Option<Child>>>,
    /// Process-pool semaphore permit. Held for the lifetime of the client
    /// so the permit is returned to the pool the moment the underlying
    /// subprocess is torn down — *not* whenever the last `Arc<PiSession>`
    /// clone finally drops (audit 2.9). Without this move, orphan
    /// forwarder Arcs would pin permits past the process's lifetime.
    ///
    /// `Option` so the test-only `spawn_for_test` constructor can omit it
    /// (its assertions don't exercise the pool semaphore).
    _process_permit: Option<OwnedSemaphorePermit>,
    /// One-shot channel used by `force_kill_child` to instruct the monitor
    /// task to take the shutdown path (Abort → wait 1s → SIGKILL → wait 2s).
    /// `Mutex<Option<...>>` so the first `force_kill_child` call wins; a
    /// second concurrent call returns immediately (process is already
    /// being killed by the monitor task).
    force_kill_tx: Mutex<Option<oneshot::Sender<ForceKillRequest>>>,
    /// Audit 2.3: the spawned subprocess's OS PID, captured at spawn time.
    /// Used by `PiSession::pid()` so progress events (and stat snapshots)
    /// can report the PID without locking the `child` slot — that mutex is
    /// often contended by the monitor task during shutdown.
    pid: Option<u32>,
    _stdout_handle: tokio::task::JoinHandle<()>,
    _stderr_handle: tokio::task::JoinHandle<()>,
    _monitor_handle: tokio::task::JoinHandle<()>,
}

impl PiRpcClient {
    /// Spawns a Pi subprocess and returns a connected `PiRpcClient`.
    ///
    /// `process_permit` is the `OwnedSemaphorePermit` reserving this
    /// process's slot in the pool semaphore. It is held by the returned
    /// client and dropped only when the client itself drops (audit 2.9 —
    /// previously held by `PiSession`, which let orphan forwarder Arcs
    /// pin the permit past the subprocess's lifetime).
    pub async fn spawn(
        binary_path: &Path,
        working_dir: &Path,
        options: &PiSessionOptions,
        env_vars: &HashMap<String, String>,
        extension_dir: Option<&Path>,
        process_permit: OwnedSemaphorePermit,
    ) -> Result<Self, PiRpcError> {
        tracing::info!(binary = %binary_path.display(), "PiRpcClient::spawn entered");
        let cli_args = options.to_cli_args();

        let mut cmd = Command::new(binary_path);
        cmd.current_dir(working_dir)
            .args(&cli_args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);

        // Forward API keys and other environment variables.
        for (key, value) in env_vars {
            cmd.env(key, value);
        }

        // Hyvemind owns the Pi distribution end-to-end: we ship the binary
        // and its extensions inside the app bundle, and explicitly load each
        // one via --extension below. Disable Pi's auto-discovery so a user's
        // globally-installed (e.g. `pi install npm:pi-web-access`) packages
        // don't double-load and trip Pi's tool-name conflict check.
        cmd.arg("--no-extensions");

        // Load extensions via `--extension` flag.
        //
        // Two layouts supported:
        //   (a) New bundled layout — each subdirectory is an npm package with
        //       a `package.json` whose `.pi.extensions` array lists relative
        //       entry-point paths (e.g. `"./index.ts"`).
        //   (b) Legacy flat layout — bare extension files dropped directly
        //       into the directory. Kept for backwards compatibility with
        //       older bundles and ad-hoc dev setups.
        if let Some(ext_dir) = extension_dir {
            if ext_dir.exists() {
                match std::fs::read_dir(ext_dir) {
                    Ok(entries) => {
                        for entry in entries.flatten() {
                            let path = entry.path();
                            if path.is_file() {
                                // (b) Legacy flat layout — pass the file directly.
                                cmd.arg("--extension").arg(&path);
                                tracing::debug!(
                                    extension = %path.display(),
                                    "loading Pi extension file (legacy flat layout)"
                                );
                            } else if path.is_dir() {
                                // (a) New layout — read package.json for entry points.
                                let pkg_json_path = path.join("package.json");
                                let pkg_data = match std::fs::read_to_string(&pkg_json_path) {
                                    Ok(s) => s,
                                    Err(e) => {
                                        // Missing package.json is fine (silent skip).
                                        // Other I/O errors get a warn.
                                        if e.kind() == std::io::ErrorKind::NotFound {
                                            tracing::trace!(
                                                dir = %path.display(),
                                                "extension dir has no package.json; skipping"
                                            );
                                        } else {
                                            tracing::warn!(
                                                error = %e,
                                                path = %pkg_json_path.display(),
                                                "failed to read extension package.json; skipping"
                                            );
                                        }
                                        continue;
                                    }
                                };
                                let parsed: serde_json::Value =
                                    match serde_json::from_str(&pkg_data) {
                                        Ok(v) => v,
                                        Err(e) => {
                                            tracing::warn!(
                                                error = %e,
                                                path = %pkg_json_path.display(),
                                                "failed to parse extension package.json; skipping"
                                            );
                                            continue;
                                        }
                                    };
                                let pkg_name = parsed
                                    .get("name")
                                    .and_then(|v| v.as_str())
                                    .map(|s| s.to_string());
                                let entries = parsed
                                    .get("pi")
                                    .and_then(|v| v.get("extensions"))
                                    .and_then(|v| v.as_array());
                                let Some(entries) = entries else {
                                    tracing::trace!(
                                        dir = %path.display(),
                                        "extension package.json has no .pi.extensions array; skipping"
                                    );
                                    continue;
                                };
                                if entries.is_empty() {
                                    tracing::trace!(
                                        dir = %path.display(),
                                        "extension package.json .pi.extensions is empty; skipping"
                                    );
                                    continue;
                                }
                                for entry in entries {
                                    let Some(rel) = entry.as_str() else { continue };
                                    let abs = path.join(rel);
                                    if !abs.exists() {
                                        tracing::warn!(
                                            package = ?pkg_name,
                                            entry = rel,
                                            resolved = %abs.display(),
                                            "Pi extension entry point not found; skipping"
                                        );
                                        continue;
                                    }
                                    cmd.arg("--extension").arg(&abs);
                                    tracing::debug!(
                                        package = ?pkg_name,
                                        extension = %abs.display(),
                                        "loading Pi extension"
                                    );
                                }
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            dir = %ext_dir.display(),
                            "failed to read Pi extension directory"
                        );
                    }
                }
            }
        }

        tracing::info!(
            binary = %binary_path.display(),
            binary_exists = binary_path.exists(),
            cwd = %working_dir.display(),
            cwd_exists = working_dir.exists(),
            model = %options.model,
            thinking = %options.thinking_level,
            tools = %options.tool_set,
            resume = options.resume_session,
            session_file = ?options.session_file,
            "spawning Pi subprocess"
        );
        tracing::debug!(
            cli_args = ?cli_args,
            env_keys = ?env_vars.keys().collect::<Vec<_>>(),
            system_prompt_preview = ?options.system_prompt.as_deref().map(|s| crate::pi::preview(s, 200)),
            "pi spawn details"
        );

        let mut child: Child = cmd.spawn()?;

        let child_stdin = child
            .stdin
            .take()
            .ok_or_else(|| PiRpcError::IoError(std::io::Error::other("failed to capture stdin")))?;
        let child_stdout = child.stdout.take().ok_or_else(|| {
            PiRpcError::IoError(std::io::Error::other("failed to capture stdout"))
        })?;
        let child_stderr = child.stderr.take().ok_or_else(|| {
            PiRpcError::IoError(std::io::Error::other("failed to capture stderr"))
        })?;

        // Capacity must comfortably exceed the burst of TextDelta/Tool events
        // a single Pi turn can produce. 256 was undersized: a multi-thousand-
        // character assistant response emits hundreds of small TextDelta events
        // in rapid succession, and any short-lived subscriber (e.g. the
        // 2.5s periodic `get_session_stats` poller in chat.rs) keeps those
        // events live in its receiver until it next polls. With many tasks
        // streaming in parallel the Tauri IPC emit can briefly stall the
        // primary `collect_response_streaming` receiver as well — at 256 it
        // started lagging mid-response, dropping TextDeltas, and the user saw
        // "missing plan" / truncated review-prompt symptoms with no error.
        // 4096 gives ~16× headroom while keeping per-session memory bounded
        // (each slot is a small enum + boxed strings; ~100KB worst case).
        let (event_tx, _) = broadcast::channel::<PiEvent>(4096);
        let alive = Arc::new(AtomicBool::new(true));
        let stderr_buf = Arc::new(Mutex::new(String::new()));

        // Shared flag: set by stdout reader when AgentEnd/TurnComplete is seen,
        // checked by the monitor task to suppress spurious error events.
        let completed = Arc::new(AtomicBool::new(false));

        // Capture the caller's session-scoped span so the three long-lived
        // background tasks below route their tracing events to the right
        // per-session log file (see state/log_routing.rs).
        let session_span = tracing::Span::current();

        // ---- stdout reader task ----
        let tx_stdout = event_tx.clone();
        let alive_stdout = alive.clone();
        let completed_stdout = completed.clone();
        let requested_thinking = options.thinking_level.to_string();
        let model_for_log = options.model.clone();
        let clamp_warning_logged = Arc::new(AtomicBool::new(false));
        let clamp_warning_logged_stdout = clamp_warning_logged.clone();
        // Audit 2.12: panic-safe wrapper. On panic, surface a `PiEvent::Error`
        // through the broadcast channel so the chat-event consumer flips the
        // session from spinning to "error" instead of hanging silently.
        let panic_tx_stdout = event_tx.clone();
        let panic_alive_stdout = alive.clone();
        let session_span_stdout = session_span.clone();
        let _stdout_handle = tokio::spawn(crate::supervise!(
            context = "pi rpc component=stdout_reader",
            on_panic = move |panic_msg: String| {
                panic_alive_stdout.store(false, Ordering::SeqCst);
                let _ = panic_tx_stdout.send(crate::pi::events::PiEvent::Error(format!(
                    "pi stdout reader PANICKED: {panic_msg}"
                )));
            },
            async move {
                #[cfg(test)]
                crate::util::supervise::maybe_panic_for_test("pi_rpc_stdout");
                let mut reader = BufReader::new(child_stdout);
                let mut line_buf: Vec<u8> = Vec::with_capacity(16 * 1024);
                let mut saw_terminal_event = false;
                loop {
                    let outcome = match read_capped_line(&mut reader, &mut line_buf).await {
                        Ok(o) => o,
                        Err(e) => {
                            tracing::warn!(error = %e, "pi stdout read error");
                            break;
                        }
                    };
                    match outcome {
                        ReadLineOutcome::Eof => break,
                        ReadLineOutcome::Overflow { dropped_bytes } => {
                            // Hostile or runaway line — log, surface a
                            // synthetic error to the upstream consumer, and
                            // resume reading the next line. The reader has
                            // already advanced past the trailing newline (or
                            // EOF).
                            let kept = line_buf.len();
                            let total = kept as u64 + dropped_bytes;
                            tracing::warn!(
                                cap = MAX_PI_LINE_BYTES,
                                kept_bytes = kept,
                                dropped_bytes,
                                total_bytes = total,
                                "pi stdout line exceeded cap; dropped tail and continuing"
                            );
                            let _ = tx_stdout.send(PiEvent::Error(format!(
                                "pi stdout line exceeded {}B cap (total ~{}B, dropped {}B); line discarded",
                                MAX_PI_LINE_BYTES, total, dropped_bytes
                            )));
                            continue;
                        }
                        ReadLineOutcome::Line => {}
                    }
                    // Convert to &str lossily — Pi emits UTF-8 JSONL so
                    // non-UTF-8 here is itself a protocol violation we
                    // want logged, but we shouldn't crash the reader.
                    let line = match std::str::from_utf8(&line_buf) {
                        Ok(s) => s,
                        Err(e) => {
                            tracing::warn!(error = %e, "pi stdout: non-UTF8 line; skipping");
                            continue;
                        }
                    };
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    match parse_pi_event(trimmed) {
                        Some(event) => {
                            tracing::trace!(
                                raw_line_len = trimmed.len(),
                                raw_line_preview = %crate::pi::preview(trimmed, 500),
                                "pi stdout: raw JSONL"
                            );
                            tracing::debug!(event = ?event, "pi stdout: parsed event");
                            // RC4: TurnComplete (`turn_end`) is the per-turn
                            // terminator and is reliably emitted by Pi at the
                            // end of every request/response cycle. Treat it
                            // as a terminal-event hint so a clean Pi exit
                            // post-TurnComplete doesn't synthesize a spurious
                            // error event from the stdout monitor task.
                            //
                            // Do NOT include MessageEnd here — it fires per
                            // intermediate message within a turn (between
                            // tool calls), so treating it as terminal would
                            // mask real mid-turn process crashes.
                            if matches!(event, PiEvent::TurnComplete | PiEvent::AgentEnd) {
                                saw_terminal_event = true;
                                completed_stdout.store(true, Ordering::SeqCst);
                            }
                            if let PiEvent::ThinkingLevelChange { level } = &event {
                                if !level.is_empty()
                                    && level != &requested_thinking
                                    && !clamp_warning_logged_stdout.swap(true, Ordering::SeqCst)
                                {
                                    tracing::warn!(
                                        requested = %requested_thinking,
                                        reported = %level,
                                        model = %model_for_log,
                                        "pi clamped thinking level — the model is registered without reasoning capability in hyvemind-providers; --thinking will be ignored and output may land entirely in a thinking content block"
                                    );
                                }
                            }
                            let _ = tx_stdout.send(event);
                        }
                        None => {
                            if !is_known_ignored_message_update(trimmed) {
                                tracing::debug!(
                                    line = %crate::pi::preview(trimmed, 500),
                                    line_len = trimmed.len(),
                                    "skipping unrecognized pi stdout line"
                                );
                            }
                        }
                    }
                }
                alive_stdout.store(false, Ordering::SeqCst);
                tracing::info!(saw_terminal_event, "pi stdout reader finished");
                // If stdout closed without a terminal event, send an error so
                // collect_response_streaming doesn't hang forever.
                if !saw_terminal_event {
                    tracing::warn!("pi stdout closed without terminal event — sending error event");
                    let _ = tx_stdout.send(PiEvent::Error(
                        "pi process stdout closed unexpectedly".to_string(),
                    ));
                }
            }
            .instrument(session_span_stdout)
        ));

        // ---- stderr capture task ----
        //
        // Append-only buffer with an amortised cap so a talkative Pi
        // (warnings every turn) doesn't slowly grow the in-memory string
        // unboundedly. We only trim when the buffer exceeds 2 × the cap
        // (rare path) and we snap the trim point forward to the next
        // valid UTF-8 char boundary so we never split a multibyte
        // codepoint.
        const MAX_STDERR_BYTES: usize = 64 * 1024;
        let stderr_buf_clone = stderr_buf.clone();
        // Audit 2.12: panic-safe wrapper. Stderr capture is best-effort by
        // design (used only for error context on monitor exit) — on panic
        // just log; the chat stream itself is owned by the stdout task
        // wrapper, which has its own panic handler.
        let session_span_stderr = session_span.clone();
        let tx_stderr = event_tx.clone();
        let _stderr_handle = tokio::spawn(crate::supervise!(
            context = "pi rpc component=stderr_reader",
            async move {
                #[cfg(test)]
                crate::util::supervise::maybe_panic_for_test("pi_rpc_stderr");
                let mut reader = BufReader::new(child_stderr);
                let mut line_buf: Vec<u8> = Vec::with_capacity(8 * 1024);
                loop {
                    let outcome = match read_capped_line(&mut reader, &mut line_buf).await {
                        Ok(o) => o,
                        Err(e) => {
                            tracing::warn!(error = %e, "pi stderr read error");
                            break;
                        }
                    };
                    match outcome {
                        ReadLineOutcome::Eof => break,
                        ReadLineOutcome::Overflow { dropped_bytes } => {
                            let kept = line_buf.len();
                            let total = kept as u64 + dropped_bytes;
                            tracing::warn!(
                                cap = MAX_PI_LINE_BYTES,
                                kept_bytes = kept,
                                dropped_bytes,
                                total_bytes = total,
                                "pi stderr line exceeded cap; dropped tail and continuing"
                            );
                            let _ = tx_stderr.send(PiEvent::Error(format!(
                                "pi stderr line exceeded {}B cap (total ~{}B, dropped {}B); line discarded",
                                MAX_PI_LINE_BYTES, total, dropped_bytes
                            )));
                            continue;
                        }
                        ReadLineOutcome::Line => {}
                    }
                    // Strip trailing \n / \r so the log line and the
                    // captured snapshot are clean.
                    let trimmed_end = {
                        let mut end = line_buf.len();
                        while end > 0
                            && (line_buf[end - 1] == b'\n' || line_buf[end - 1] == b'\r')
                        {
                            end -= 1;
                        }
                        end
                    };
                    let line_str = match std::str::from_utf8(&line_buf[..trimmed_end]) {
                        Ok(s) => s.to_string(),
                        Err(_) => String::from_utf8_lossy(&line_buf[..trimmed_end]).into_owned(),
                    };
                    tracing::info!(stderr = %line_str, "pi stderr");
                    let mut buf = stderr_buf_clone.lock().await;
                    if !buf.is_empty() {
                        buf.push('\n');
                    }
                    buf.push_str(&line_str);
                    if buf.len() > 2 * MAX_STDERR_BYTES {
                        let mut keep_from = buf.len() - MAX_STDERR_BYTES;
                        // Snap to next char boundary so we don't split a
                        // multibyte codepoint. `is_char_boundary` is safe
                        // for indices in `0..=buf.len()`.
                        while keep_from < buf.len() && !buf.is_char_boundary(keep_from) {
                            keep_from += 1;
                        }
                        let trimmed = buf[keep_from..].to_string();
                        *buf = trimmed;
                    }
                }
            }
            .instrument(session_span_stderr)
        ));

        // ---- process monitor task ----
        //
        // Audit 2.9: `Child` ownership now lives in an `Arc<Mutex<Option<Child>>>`
        // shared between the monitor task and `Drop for PiRpcClient`. At task
        // start, the monitor takes the `Child` out of the slot, drives the
        // wait/force-kill state machine, and leaves the slot `None` on
        // completion. `Drop` can then synchronously try-lock the slot, take
        // any leftover `Child`, and fire `start_kill` immediately — without
        // depending on the monitor task still being alive on the runtime.
        //
        // The monitor task handles:
        //   (a) normal exit via `child.wait()`, OR
        //   (b) a force-kill request that drives the proper shutdown
        //       sequence (Abort already sent over stdin by the caller,
        //       wait up to 1s for graceful exit, SIGKILL via `start_kill`,
        //       wait up to 2s for the reap).
        //
        // Sharing `Child` across tasks is awkward (`wait()` takes `&mut`)
        // but holding the lock for the *whole* wait() is fine here because
        // `Drop` uses `try_lock` and falls back to aborting the monitor
        // handle (which drops the local `Child` and triggers `kill_on_drop`).
        // Audit 2.3: capture the OS PID before moving the Child so `pid()`
        // can answer cheaply without locking the (often contended) slot.
        let child_pid = child.id();
        let child_slot: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(Some(child)));
        let child_slot_monitor = child_slot.clone();
        let (force_kill_tx_oneshot, force_kill_rx) = oneshot::channel::<ForceKillRequest>();
        let force_kill_tx = Mutex::new(Some(force_kill_tx_oneshot));
        let tx_monitor = event_tx.clone();
        let alive_monitor = alive.clone();
        let stderr_monitor = stderr_buf.clone();
        let completed_monitor = completed;
        // Audit 2.12: panic-safe wrapper. The monitor is the last line of
        // defence telling consumers Pi died — if IT panics, no Error event
        // ever reaches the chat-event stream. Make sure something does.
        let panic_tx_monitor = event_tx.clone();
        let panic_alive_monitor = alive.clone();
        let _monitor_handle = tokio::spawn(crate::supervise!(
            context = "pi rpc component=process_monitor",
            on_panic = move |panic_msg: String| {
                panic_alive_monitor.store(false, Ordering::SeqCst);
                let _ = panic_tx_monitor.send(crate::pi::events::PiEvent::Error(format!(
                    "pi process monitor PANICKED: {panic_msg}"
                )));
            },
            async move {
                #[cfg(test)]
                crate::util::supervise::maybe_panic_for_test("pi_rpc_monitor");
                // Take the Child out of the shared slot. If `Drop` already
                // raced ahead and took it (vanishingly unlikely — the slot
                // is populated synchronously before this task spawns and
                // `Drop` only fires after the client is constructed), bail
                // out cleanly.
                let mut child = match child_slot_monitor.lock().await.take() {
                    Some(c) => c,
                    None => {
                        tracing::debug!("pi monitor: child slot already empty on entry; exiting");
                        alive_monitor.store(false, Ordering::SeqCst);
                        return;
                    }
                };
                let mut force_kill_reply: Option<oneshot::Sender<()>> = None;
                let status: std::io::Result<std::process::ExitStatus> = tokio::select! {
                    s = child.wait() => s,
                    req = force_kill_rx => {
                        // Caller invoked force_kill. They've already sent
                        // Abort over stdin (or it failed — either way the
                        // process must come down now). Run the bounded
                        // shutdown sequence:
                        //   1. Up to 1s for graceful exit.
                        //   2. SIGKILL via start_kill().
                        //   3. Up to 2s for the OS to reap.
                        // If still alive after step 3, log a warn — the
                        // child's `kill_on_drop(true)` is the last resort.
                        match req {
                            Ok(r) => force_kill_reply = Some(r.reply_tx),
                            Err(_) => {
                                // Sender dropped without firing — shouldn't
                                // happen since we hold it in PiRpcClient,
                                // but tolerate it gracefully.
                                tracing::debug!("force_kill_tx dropped without firing");
                            }
                        }
                        tracing::info!("force_kill received — running bounded shutdown");
                        let graceful = tokio::time::timeout(
                            Duration::from_secs(1),
                            child.wait(),
                        )
                        .await;
                        match graceful {
                            Ok(s) => {
                                tracing::info!("force_kill: process exited gracefully after Abort");
                                s
                            }
                            Err(_) => {
                                tracing::warn!("force_kill: graceful exit timed out after 1s — sending SIGKILL");
                                // start_kill is non-blocking; sends SIGKILL
                                // on Unix and TerminateProcess on Windows.
                                if let Err(e) = child.start_kill() {
                                    tracing::warn!(error = %e, "force_kill: start_kill failed");
                                }
                                match tokio::time::timeout(
                                    Duration::from_secs(2),
                                    child.wait(),
                                )
                                .await
                                {
                                    Ok(s) => {
                                        tracing::info!("force_kill: process reaped after SIGKILL");
                                        s
                                    }
                                    Err(_) => {
                                        tracing::warn!("force_kill: process still alive after SIGKILL + 2s wait — relying on kill_on_drop");
                                        // Synthesize a "killed by signal" exit status by
                                        // attempting another wait without timeout — but
                                        // bail to keep the task bounded. We return an
                                        // io::Error to make the downstream code treat
                                        // this as a non-success exit.
                                        Err(std::io::Error::other(
                                            "pi process did not exit within force_kill timeout",
                                        ))
                                    }
                                }
                            }
                        }
                    }
                };
                alive_monitor.store(false, Ordering::SeqCst);

                // Notify the caller of force_kill that the kill sequence has
                // completed (or timed out). Drop the Child explicitly so its
                // resources release before the reply fires. The shared slot
                // already holds `None` (we took the Child out at task start),
                // so there's nothing to put back.
                drop(child);
                if let Some(reply_tx) = force_kill_reply.take() {
                    let _ = reply_tx.send(());
                }

                let exit_code = status.as_ref().ok().and_then(|s| s.code());

                // If a terminal event was already delivered, this is a normal
                // post-response exit — no need to send an error.
                if completed_monitor.load(Ordering::SeqCst) {
                    tracing::info!(exit_code = ?exit_code, "pi process exited after completing response");
                    return;
                }
                let success = status.as_ref().map(|s| s.success()).unwrap_or(false);

                let stderr_snapshot = stderr_monitor.lock().await.clone();
                if !success {
                    let _ = tx_monitor.send(PiEvent::Error(format!(
                        "pi process exited unexpectedly (code={exit_code:?}): {stderr_snapshot}"
                    )));
                } else {
                    tracing::warn!(exit_code = ?exit_code, "pi process exited without completing response");
                    let msg = if stderr_snapshot.is_empty() {
                        "pi process exited without responding".to_string()
                    } else {
                        format!("pi process exited without responding: {stderr_snapshot}")
                    };
                    let _ = tx_monitor.send(PiEvent::Error(msg));
                }
            }
            .instrument(session_span)
        ));

        Ok(Self {
            stdin: Arc::new(Mutex::new(child_stdin)),
            event_tx,
            alive,
            stderr_buf,
            child: child_slot,
            _process_permit: Some(process_permit),
            force_kill_tx,
            pid: child_pid,
            _stdout_handle,
            _stderr_handle,
            _monitor_handle,
        })
    }

    /// Audit 2.3: returns the OS PID of the spawned Pi subprocess captured
    /// at spawn time. `None` for test-only constructors that did not
    /// capture a PID. Stable across the client's lifetime — the PID does
    /// not change when the process exits; it only stops being mapped.
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// Sends a command to the Pi subprocess (JSONL over stdin).
    pub async fn send_command(&self, cmd: &PiCommand) -> Result<(), PiRpcError> {
        if !self.is_alive() {
            let stderr = self.stderr_snapshot().await;
            if stderr.trim().is_empty() {
                return Err(PiRpcError::StdinClosed);
            }
            return Err(PiRpcError::ProcessUnavailable { stderr });
        }

        let mut line = serde_json::to_string(cmd)?;
        tracing::debug!(command_len = line.len(), "pi stdin: sending command");
        tracing::trace!(command_json = %line.trim(), "pi stdin: command body");
        line.push('\n');

        let mut stdin = self.stdin.lock().await;
        if let Err(e) = stdin.write_all(line.as_bytes()).await {
            let stderr = self.stderr_snapshot().await;
            if stderr.trim().is_empty() {
                return Err(PiRpcError::IoError(e));
            }
            return Err(PiRpcError::ProcessUnavailable { stderr });
        }
        if let Err(e) = stdin.flush().await {
            let stderr = self.stderr_snapshot().await;
            if stderr.trim().is_empty() {
                return Err(PiRpcError::IoError(e));
            }
            return Err(PiRpcError::ProcessUnavailable { stderr });
        }

        Ok(())
    }

    /// Returns a new broadcast receiver for Pi events.
    pub fn subscribe(&self) -> broadcast::Receiver<PiEvent> {
        self.event_tx.subscribe()
    }

    /// Bounded force-kill: send Abort, then drive the monitor task through
    /// a Abort → wait 1s → SIGKILL → wait 2s sequence, returning only when
    /// the OS has reaped the process (or the 3s ceiling is hit).
    ///
    /// This is the correct teardown for stop/kill paths because plain
    /// `shutdown()` returns before the kernel reaps the child, leaving
    /// brief zombie state and racing the caller's expectation that the
    /// PID is gone. After this method returns the process is no longer
    /// running (barring the documented 3s timeout, in which case
    /// `kill_on_drop(true)` on the underlying `Child` is the last resort).
    ///
    /// `cfg(unix)`: SIGKILL is sent via `Child::start_kill`.
    /// `cfg(windows)`: `start_kill` calls `TerminateProcess` — same
    /// guarantee that the process will not run user code afterward.
    /// Either way the wait is bounded by `tokio::time::timeout`.
    pub async fn force_kill_child(&self) -> Result<(), PiRpcError> {
        // (1) Best-effort Abort over stdin — the monitor task interprets the
        // force_kill signal regardless of whether Abort was acknowledged.
        let _ = self.send_command(&PiCommand::Abort {}).await;
        self.alive.store(false, Ordering::SeqCst);

        // (2) Take the force_kill sender. First caller wins; subsequent
        // callers see `None` and assume the monitor is already running
        // (or has finished) the shutdown sequence.
        let tx_opt = {
            let mut guard = self.force_kill_tx.lock().await;
            guard.take()
        };

        if let Some(tx) = tx_opt {
            let (reply_tx, reply_rx) = oneshot::channel();
            // If the monitor task already exited normally (child.wait
            // path), the send will fail — that's fine, process is gone.
            if tx.send(ForceKillRequest { reply_tx }).is_err() {
                tracing::debug!("force_kill_child: monitor task already exited");
            } else {
                // Wait up to 4s for the monitor to complete its shutdown
                // sequence (1s graceful + 2s after SIGKILL + 1s margin).
                // If the monitor itself wedges (shouldn't happen — every
                // wait inside it is bounded), don't block the caller
                // forever.
                match tokio::time::timeout(Duration::from_secs(4), reply_rx).await {
                    Ok(Ok(())) => {
                        tracing::debug!("force_kill_child: monitor signalled completion");
                    }
                    Ok(Err(_)) => {
                        tracing::debug!("force_kill_child: monitor dropped reply channel");
                    }
                    Err(_) => {
                        tracing::warn!(
                            "force_kill_child: monitor did not signal completion within 4s"
                        );
                    }
                }
            }
        } else {
            tracing::debug!("force_kill_child: shutdown already in flight or completed");
        }

        // (3) Close stdin for good measure (idempotent — the process is
        // already gone or going).
        let mut stdin = self.stdin.lock().await;
        let _ = stdin.shutdown().await;

        Ok(())
    }

    /// Returns `true` if the underlying process is still running.
    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    /// Returns a snapshot of everything captured on stderr so far.
    pub async fn stderr_snapshot(&self) -> String {
        self.stderr_buf.lock().await.clone()
    }

    /// Test-only constructor that wraps an arbitrary `Command` (e.g.
    /// `sleep 60`) in the same monitor/force-kill machinery used by the
    /// real `spawn` path. The `Command` must use piped stdin/stdout/stderr
    /// and `kill_on_drop(true)`. The OS PID of the spawned child is
    /// returned alongside the client so the test can verify reaping via
    /// `kill(pid, None)`.
    ///
    /// `process_permit` is forwarded into the client and dropped when the
    /// client itself drops — pass `Some(...)` to assert pool-permit
    /// release semantics (audit 2.9), or `None` when the test doesn't
    /// care about the pool semaphore.
    ///
    /// Used by `force_kill_child_reaps_process` to exercise the full
    /// shutdown sequence end-to-end without a real Pi binary.
    #[cfg(test)]
    pub(crate) async fn spawn_for_test(cmd: Command) -> Result<(Self, u32), PiRpcError> {
        Self::spawn_for_test_with_permit(cmd, None).await
    }

    /// Variant of `spawn_for_test` that also accepts a process-pool permit.
    /// Used by `drop_releases_semaphore_permit_promptly` (audit 2.9) to
    /// assert the permit returns to the pool when the client drops.
    #[cfg(test)]
    pub(crate) async fn spawn_for_test_with_permit(
        mut cmd: Command,
        process_permit: Option<OwnedSemaphorePermit>,
    ) -> Result<(Self, u32), PiRpcError> {
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let mut child: Child = cmd.spawn()?;
        let pid = child.id().expect("spawned child should have a PID");

        let child_stdin = child
            .stdin
            .take()
            .ok_or_else(|| PiRpcError::IoError(std::io::Error::other("failed to capture stdin")))?;
        let child_stdout = child.stdout.take().ok_or_else(|| {
            PiRpcError::IoError(std::io::Error::other("failed to capture stdout"))
        })?;
        let child_stderr = child.stderr.take().ok_or_else(|| {
            PiRpcError::IoError(std::io::Error::other("failed to capture stderr"))
        })?;

        let (event_tx, _) = broadcast::channel::<PiEvent>(64);
        let alive = Arc::new(AtomicBool::new(true));
        let stderr_buf = Arc::new(Mutex::new(String::new()));
        let completed = Arc::new(AtomicBool::new(false));

        // Drain stdout/stderr in the background so the child doesn't block
        // on a full pipe buffer.
        let alive_stdout = alive.clone();
        let _stdout_handle = tokio::spawn(async move {
            let reader = BufReader::new(child_stdout);
            let mut lines = reader.lines();
            while let Ok(Some(_)) = lines.next_line().await {}
            alive_stdout.store(false, Ordering::SeqCst);
        });

        let stderr_buf_clone = stderr_buf.clone();
        let _stderr_handle = tokio::spawn(async move {
            let reader = BufReader::new(child_stderr);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let mut buf = stderr_buf_clone.lock().await;
                if !buf.is_empty() {
                    buf.push('\n');
                }
                buf.push_str(&line);
            }
        });

        // Reuse the exact monitor logic used by the production spawn path
        // (force-kill request handling, bounded waits, SIGKILL fallback).
        // Audit 2.9: Child ownership is shared with `Drop for PiRpcClient`
        // via `Arc<Mutex<Option<Child>>>` (same as the production path).
        let child_slot: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(Some(child)));
        let child_slot_monitor = child_slot.clone();
        let (force_kill_tx_oneshot, force_kill_rx) = oneshot::channel::<ForceKillRequest>();
        let force_kill_tx = Mutex::new(Some(force_kill_tx_oneshot));
        let alive_monitor = alive.clone();
        let _monitor_handle = tokio::spawn(async move {
            let mut child = match child_slot_monitor.lock().await.take() {
                Some(c) => c,
                None => {
                    alive_monitor.store(false, Ordering::SeqCst);
                    return;
                }
            };
            let mut force_kill_reply: Option<oneshot::Sender<()>> = None;
            let _status: std::io::Result<std::process::ExitStatus> = tokio::select! {
                s = child.wait() => s,
                req = force_kill_rx => {
                    if let Ok(r) = req {
                        force_kill_reply = Some(r.reply_tx);
                    }
                    let graceful = tokio::time::timeout(
                        Duration::from_secs(1),
                        child.wait(),
                    )
                    .await;
                    match graceful {
                        Ok(s) => s,
                        Err(_) => {
                            let _ = child.start_kill();
                            match tokio::time::timeout(
                                Duration::from_secs(2),
                                child.wait(),
                            )
                            .await
                            {
                                Ok(s) => s,
                                Err(_) => Err(std::io::Error::other(
                                    "test child did not exit within force_kill timeout",
                                )),
                            }
                        }
                    }
                }
            };
            alive_monitor.store(false, Ordering::SeqCst);
            drop(child);
            if let Some(reply_tx) = force_kill_reply.take() {
                let _ = reply_tx.send(());
            }
            // Completed flag is not relevant for the test harness — drop
            // unused so the compiler doesn't complain.
            drop(completed);
        });

        Ok((
            Self {
                stdin: Arc::new(Mutex::new(child_stdin)),
                event_tx,
                alive,
                stderr_buf,
                child: child_slot,
                _process_permit: process_permit,
                force_kill_tx,
                pid: Some(pid),
                _stdout_handle,
                _stderr_handle,
                _monitor_handle,
            },
            pid,
        ))
    }
}

impl std::fmt::Debug for PiRpcClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PiRpcClient")
            .field("pid", &self.pid)
            .field("alive", &self.alive.load(Ordering::SeqCst))
            .finish()
    }
}

impl Drop for PiRpcClient {
    /// Audit 2.9: synchronous best-effort kill path for the Pi subprocess.
    ///
    /// Previously, dropping a `PiRpcClient` only aborted its task handles
    /// and relied on `Child::kill_on_drop(true)` firing whenever the
    /// monitor task's local `Child` binding finally dropped. With long
    /// `child.wait()` polls (the common case — Pi processes sit idle
    /// waiting for the next prompt) the monitor wasn't released until the
    /// runtime got around to it, so the child could linger on the OS for
    /// an indeterminate window.
    ///
    /// Now we:
    ///   1. Abort the three task handles so they stop holding their
    ///      cloned `Arc`s (stdin, stderr_buf, event_tx subscribers, and
    ///      the monitor's local `Child`).
    ///   2. Try to take the `Child` out of the shared slot ourselves. If
    ///      the monitor task already took it, abort step 1 above has
    ///      already started the kill_on_drop sequence on the monitor's
    ///      local binding.
    ///   3. If we got the `Child`, call `start_kill()` directly (sync —
    ///      sends SIGKILL on Unix / TerminateProcess on Windows without
    ///      awaiting), then schedule the reaping wait on a detached task
    ///      if a runtime is available. If no runtime is available (e.g.
    ///      Drop firing during shutdown), `kill_on_drop(true)` finishes
    ///      the job when the `Child` is finally dropped.
    ///
    /// The `OwnedSemaphorePermit` returns to the pool when `_process_permit`
    /// drops below — that's the whole point of moving the permit here from
    /// `PiSession` (audit 2.9): once the subprocess is being torn down,
    /// the slot is *immediately* freed up for the next spawn, regardless
    /// of how many forwarder Arcs to `PiSession` are still live.
    fn drop(&mut self) {
        // (1) Signal task cancellation. abort() is non-blocking; the
        // tasks will be cancelled on their next yield point.
        self._monitor_handle.abort();
        self._stdout_handle.abort();
        self._stderr_handle.abort();

        // (2) Synchronously try to take the Child out of the shared slot.
        // `try_lock` is sync and never blocks the dropper. If the monitor
        // task is still holding the lock (rare — it only holds during the
        // initial `take()`), the abort above will let it drop the local
        // `Child` and `kill_on_drop(true)` will fire shortly thereafter.
        let taken_child: Option<Child> = match self.child.try_lock() {
            Ok(mut guard) => guard.take(),
            Err(_) => {
                tracing::debug!(
                    "PiRpcClient::drop: child slot busy; relying on monitor abort + kill_on_drop"
                );
                None
            }
        };

        // (3) Fire SIGKILL synchronously and arrange for the reap.
        if let Some(mut child) = taken_child {
            let pid = child.id();
            // start_kill() is non-blocking and sends SIGKILL on Unix.
            // Failing here usually means the process is already gone.
            if let Err(e) = child.start_kill() {
                tracing::debug!(
                    error = %e,
                    pid = ?pid,
                    "PiRpcClient::drop: start_kill failed (process likely already exited)"
                );
            }

            // Schedule the await on a detached task so the OS gets a
            // chance to reap. If we're not in a tokio runtime context
            // (e.g. drop fires during process shutdown), fall through
            // and let `kill_on_drop(true)` reap when `child` drops below.
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    let _ = child.wait().await;
                });
            } else {
                tracing::debug!(
                    pid = ?pid,
                    "PiRpcClient::drop: no tokio runtime available; relying on kill_on_drop"
                );
                // Dropping `child` here fires kill_on_drop's blocking
                // SIGKILL+wait via the OS, which is fine outside a runtime.
                drop(child);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_args_default_has_no_session() {
        let args = PiSessionOptions::for_model("x").to_cli_args();
        assert!(args.contains(&"--no-session".to_string()));
        assert!(!args.contains(&"--session".to_string()));
        assert!(!args.contains(&"--continue".to_string()));
    }

    #[test]
    fn subscription_provider_names_are_remapped_for_pi() {
        // The Pi runtime only knows `anthropic` / `openai-codex`; if any
        // `for_*` constructor stops applying `map_model_for_pi`, swarm Scout/
        // Worker/Guard sessions die with:
        //   Error: Model "claude-sub/claude-opus-4-7" not found.
        // and the feature fails before the first token. Lock the mapping in.
        assert_eq!(
            PiSessionOptions::for_scout("claude-sub/claude-opus-4-7", "sys").model,
            "anthropic/claude-opus-4-7"
        );
        assert_eq!(
            PiSessionOptions::for_worker("chatgpt/gpt-5", "sys").model,
            "openai-codex/gpt-5"
        );
        assert_eq!(
            PiSessionOptions::for_guard("claude-sub/claude-sonnet-4", "sys").model,
            "anthropic/claude-sonnet-4"
        );
        assert_eq!(
            PiSessionOptions::for_model("claude-sub/claude-opus-4-7").model,
            "anthropic/claude-opus-4-7"
        );
        // Non-subscription providers and bare model ids pass through unchanged
        // (and the mapping is idempotent for callers that already remapped).
        assert_eq!(
            PiSessionOptions::for_scout("anthropic/claude-opus-4-7", "sys").model,
            "anthropic/claude-opus-4-7"
        );
        assert_eq!(
            PiSessionOptions::for_scout("openrouter/x-ai/grok-4", "sys").model,
            "openrouter/x-ai/grok-4"
        );
        assert_eq!(
            PiSessionOptions::for_model("claude-sonnet-4-20250514").model,
            "claude-sonnet-4-20250514"
        );
    }

    #[test]
    fn cli_args_with_session_file_no_resume() {
        let args = PiSessionOptions::for_model("x")
            .with_session_file("f")
            .to_cli_args();
        assert!(!args.contains(&"--no-session".to_string()));
        assert!(args.contains(&"--session".to_string()));
        assert!(args.contains(&"f".to_string()));
        assert!(!args.contains(&"--continue".to_string()));
    }

    #[test]
    fn cli_args_with_session_file_and_resume() {
        let args = PiSessionOptions::for_model("x")
            .with_session_file("f")
            .with_resume()
            .to_cli_args();
        assert!(!args.contains(&"--no-session".to_string()));
        assert!(args.contains(&"--continue".to_string()));
        assert!(args.contains(&"--session".to_string()));
        assert!(args.contains(&"f".to_string()));
    }

    #[test]
    fn thinking_level_from_str_valid() {
        assert_eq!("off".parse::<ThinkingLevel>().unwrap(), ThinkingLevel::Off);
        assert_eq!("low".parse::<ThinkingLevel>().unwrap(), ThinkingLevel::Low);
        assert_eq!(
            "medium".parse::<ThinkingLevel>().unwrap(),
            ThinkingLevel::Medium
        );
        assert_eq!(
            "high".parse::<ThinkingLevel>().unwrap(),
            ThinkingLevel::High
        );
        // Case insensitive
        assert_eq!(
            "HIGH".parse::<ThinkingLevel>().unwrap(),
            ThinkingLevel::High
        );
        assert_eq!(
            "Medium".parse::<ThinkingLevel>().unwrap(),
            ThinkingLevel::Medium
        );
    }

    #[test]
    fn thinking_level_from_str_invalid() {
        assert!("xhigh".parse::<ThinkingLevel>().is_err());
        assert!("max".parse::<ThinkingLevel>().is_err());
        assert!("".parse::<ThinkingLevel>().is_err());
    }

    #[test]
    fn with_thinking_level_overrides_default() {
        let opts = PiSessionOptions::for_model("x").with_thinking_level(ThinkingLevel::High);
        let args = opts.to_cli_args();
        assert!(args.contains(&"high".to_string()));
    }

    #[test]
    fn parse_session_stats_with_data_envelope() {
        // Pi nests get_session_stats response under "data"
        let line = r#"{"type":"response","command":"get_session_stats","success":true,"data":{"tokens":{"input":10,"output":180,"reasoningTokens":1200,"cacheRead":0,"cacheWrite":19059,"total":19249},"cost":0.074,"contextUsage":{"tokens":19249,"contextWindow":200000,"percent":9.6245}}}"#;
        let event = parse_pi_event(line).expect("should parse");
        match event {
            PiEvent::SessionStats(stats) => {
                assert_eq!(stats.input, 10);
                assert_eq!(stats.output, 180);
                assert_eq!(stats.reasoning_tokens, 1200);
                assert_eq!(stats.cache_write, 19059);
                assert_eq!(stats.total_tokens, 19249);
                assert!((stats.cost - 0.074).abs() < 0.001);
                assert_eq!(stats.context_window, 200000);
                assert!((stats.context_percent - 9.6245).abs() < 0.001);
            }
            other => panic!("expected SessionStats, got {:?}", other),
        }
    }

    #[test]
    fn parse_message_with_stop_reason_error_surfaces_pi_event_error() {
        // Pi writes the authoritative assistant message envelope at turn end.
        // When the provider call failed (e.g. billing rejection), stopReason
        // is "error" and the raw payload sits in errorMessage. We must turn
        // that into PiEvent::Error so the frontend's red banner fires —
        // otherwise the session JSONL holds the only trace and the UI hangs.
        let line = r#"{"type":"message","id":"x","parentId":"p","timestamp":"t","message":{"role":"assistant","content":[],"stopReason":"error","timestamp":1,"errorMessage":"400 {\"type\":\"error\",\"error\":{\"type\":\"invalid_request_error\",\"message\":\"You're out of extra usage.\"}}"}}"#;
        let event = parse_pi_event(line).expect("should parse");
        match event {
            PiEvent::Error(msg) => assert!(msg.contains("out of extra usage"), "got {msg}"),
            other => panic!("expected PiEvent::Error, got {:?}", other),
        }
    }

    #[test]
    fn parse_message_with_normal_stop_reason_is_ignored() {
        // A successful turn end goes through the streaming layer; the
        // authoritative message envelope here should produce no event.
        let line = r#"{"type":"message","id":"x","parentId":"p","timestamp":"t","message":{"role":"assistant","content":[{"type":"text","text":"done"}],"stopReason":"stop","timestamp":1}}"#;
        assert!(parse_pi_event(line).is_none());
    }

    #[test]
    fn is_known_ignored_message_update_matches_noisy_subtypes() {
        // toolcall_delta is the worst offender — Pi emits one per token of
        // the streamed tool-call args, each containing the cumulative
        // `partial` payload. parse_pi_event drops it (we read tool calls
        // off the authoritative tool_execution_* family instead), and the
        // predicate must also drop it so the stdout reader doesn't dump
        // the full body into the debug log.
        for sub in [
            "toolcall_start",
            "toolcall_delta",
            "toolcall_end",
            "thinking_start",
            "thinking_end",
            "text_start",
            "text_end",
        ] {
            let line = format!(
                r#"{{"type":"message_update","assistantMessageEvent":{{"type":"{sub}","partial":{{"role":"assistant"}}}}}}"#
            );
            assert!(
                is_known_ignored_message_update(&line),
                "sub-type {sub} should be classified as known-ignored"
            );
            // Sanity: parse_pi_event still drops it (predicate exists
            // precisely because parse returns None for these).
            assert!(parse_pi_event(&line).is_none(), "sub-type {sub}");
        }
    }

    #[test]
    fn is_known_ignored_message_update_rejects_other_lines() {
        // Truly novel sub-types must NOT be silenced — we want them to
        // hit the unrecognized-line debug log so Pi schema drift is
        // visible. Same for non-message_update events and malformed JSON.
        let novel = r#"{"type":"message_update","assistantMessageEvent":{"type":"some_new_subtype","delta":"x"}}"#;
        assert!(!is_known_ignored_message_update(novel));

        let non_message_update = r#"{"type":"agent_end"}"#;
        assert!(!is_known_ignored_message_update(non_message_update));

        let missing_subtype = r#"{"type":"message_update","assistantMessageEvent":{}}"#;
        assert!(!is_known_ignored_message_update(missing_subtype));

        let not_json = "not even json";
        assert!(!is_known_ignored_message_update(not_json));

        // text_delta / thinking_delta are handled by parse_pi_event and
        // must NOT be on the ignore list (we'd lose the streamed text).
        let text_delta = r#"{"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"hi"}}"#;
        assert!(!is_known_ignored_message_update(text_delta));
    }

    #[tokio::test]
    async fn read_capped_line_handles_small_lines() {
        use std::io::Cursor;
        use tokio::io::BufReader;
        let data = b"hello\nworld\n".to_vec();
        let mut reader = BufReader::new(Cursor::new(data));
        let mut buf = Vec::new();

        let r = read_capped_line(&mut reader, &mut buf).await.unwrap();
        assert_eq!(r, ReadLineOutcome::Line);
        assert_eq!(&buf[..], b"hello\n");

        let r = read_capped_line(&mut reader, &mut buf).await.unwrap();
        assert_eq!(r, ReadLineOutcome::Line);
        assert_eq!(&buf[..], b"world\n");

        let r = read_capped_line(&mut reader, &mut buf).await.unwrap();
        assert_eq!(r, ReadLineOutcome::Eof);
    }

    #[tokio::test]
    async fn read_capped_line_handles_eof_without_newline() {
        use std::io::Cursor;
        use tokio::io::BufReader;
        let data = b"trailing".to_vec();
        let mut reader = BufReader::new(Cursor::new(data));
        let mut buf = Vec::new();

        let r = read_capped_line(&mut reader, &mut buf).await.unwrap();
        assert_eq!(r, ReadLineOutcome::Line);
        assert_eq!(&buf[..], b"trailing");

        let r = read_capped_line(&mut reader, &mut buf).await.unwrap();
        assert_eq!(r, ReadLineOutcome::Eof);
    }

    /// Integration test for the OOM-DoS fix. Fires a 5 MiB single line at
    /// the capped reader, verifies the kept portion is bounded by the cap,
    /// the overflow is reported with the correct dropped byte count, and
    /// the reader recovers cleanly to read the *next* line after the giant
    /// one — i.e. the process is not "stuck" on the bad line.
    #[tokio::test]
    async fn read_capped_line_recovers_from_5mib_overflow() {
        use std::io::Cursor;
        use tokio::io::BufReader;

        // 5 MiB of `x` followed by `\n`, then a normal next line.
        const OVERSIZE: usize = 5 * 1024 * 1024;
        let mut data = vec![b'x'; OVERSIZE];
        data.push(b'\n');
        data.extend_from_slice(b"next-line-survives\n");

        let mut reader = BufReader::new(Cursor::new(data));
        let mut buf = Vec::new();

        let r = read_capped_line(&mut reader, &mut buf).await.unwrap();
        match r {
            ReadLineOutcome::Overflow { dropped_bytes } => {
                // Kept bytes must not exceed the cap.
                assert!(
                    buf.len() <= MAX_PI_LINE_BYTES,
                    "kept {} bytes exceeded cap {}",
                    buf.len(),
                    MAX_PI_LINE_BYTES
                );
                // Total of kept + dropped should equal the input line
                // length (5 MiB of x + 1 newline).
                let total = buf.len() as u64 + dropped_bytes;
                assert_eq!(
                    total,
                    OVERSIZE as u64 + 1,
                    "kept={} + dropped={} should equal {}",
                    buf.len(),
                    dropped_bytes,
                    OVERSIZE + 1
                );
                // Specifically we should keep exactly the cap.
                assert_eq!(buf.len(), MAX_PI_LINE_BYTES);
                assert_eq!(dropped_bytes, (OVERSIZE + 1 - MAX_PI_LINE_BYTES) as u64);
            }
            other => panic!("expected Overflow, got {:?}", other),
        }

        // Critical: the reader must have advanced PAST the giant line and
        // be ready to read the next one. This proves the recovery path.
        let r = read_capped_line(&mut reader, &mut buf).await.unwrap();
        assert_eq!(r, ReadLineOutcome::Line);
        assert_eq!(&buf[..], b"next-line-survives\n");

        // And then cleanly EOF.
        let r = read_capped_line(&mut reader, &mut buf).await.unwrap();
        assert_eq!(r, ReadLineOutcome::Eof);
    }

    /// Pi normally emits short JSONL lines; verify they pass through
    /// untouched after the giant-line case has happened first (i.e. the
    /// helper is idempotent across overflow → normal transitions).
    #[tokio::test]
    async fn read_capped_line_overflow_followed_by_many_small_lines() {
        use std::io::Cursor;
        use tokio::io::BufReader;

        let oversize_payload = vec![b'A'; MAX_PI_LINE_BYTES + 100];
        let mut data = oversize_payload;
        data.push(b'\n');
        for i in 0..16u32 {
            data.extend_from_slice(format!(r#"{{"type":"turn_end","i":{i}}}"#).as_bytes());
            data.push(b'\n');
        }

        let mut reader = BufReader::new(Cursor::new(data));
        let mut buf = Vec::new();

        // First line overflows.
        let r = read_capped_line(&mut reader, &mut buf).await.unwrap();
        assert!(matches!(r, ReadLineOutcome::Overflow { .. }));

        // Following 16 lines all read cleanly.
        for i in 0..16u32 {
            let r = read_capped_line(&mut reader, &mut buf).await.unwrap();
            assert_eq!(r, ReadLineOutcome::Line);
            let s = std::str::from_utf8(&buf).unwrap();
            assert!(s.contains(&format!(r#""i":{i}"#)), "line {i}: got {s:?}");
        }

        let r = read_capped_line(&mut reader, &mut buf).await.unwrap();
        assert_eq!(r, ReadLineOutcome::Eof);
    }

    /// Cross-platform "is this PID still alive?" check used by the
    /// force-kill test. Shells out to `kill -0 <pid>`, which is the
    /// canonical Unix probe: exit 0 means the process exists; non-zero
    /// means it doesn't (or we lack permission, which can't happen for
    /// our own child). Avoids pulling in `libc` or `nix` as a dev-dep.
    #[cfg(unix)]
    fn pid_is_alive(pid: u32) -> bool {
        std::process::Command::new("kill")
            .arg("-0")
            .arg(pid.to_string())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// End-to-end: spawn `sleep 60`, call force_kill, assert that
    /// `force_kill` returns within its own deadline AND that the OS has
    /// actually reaped the process (no zombie).
    ///
    /// `sleep 60` ignores stdin entirely, so the Abort RPC line we write
    /// has no effect. The graceful-wait path therefore always times out
    /// and the SIGKILL fallback is the path under test. This is exactly
    /// the bug profile we're guarding against: a process that won't
    /// honor Abort must still be reaped before `force_kill` returns.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn force_kill_child_reaps_long_running_process() {
        let mut cmd = Command::new("sleep");
        cmd.arg("60");
        let (client, pid) = PiRpcClient::spawn_for_test(cmd)
            .await
            .expect("spawn sleep 60");
        assert!(
            pid_is_alive(pid),
            "sleep should be alive immediately after spawn"
        );

        let started = std::time::Instant::now();
        client
            .force_kill_child()
            .await
            .expect("force_kill_child should succeed");
        let elapsed = started.elapsed();

        // Must return within 4s ceiling (1s graceful + 2s post-SIGKILL +
        // 1s reply margin). Allow generous slack for slow CI.
        assert!(
            elapsed < Duration::from_secs(6),
            "force_kill_child took {:?}, exceeds bounded deadline",
            elapsed
        );

        // Critical assertion: the OS PID is no longer alive. Allow up to
        // 1s for the kernel to reap (typically immediate).
        let poll_started = std::time::Instant::now();
        let mut alive_after = pid_is_alive(pid);
        while alive_after && poll_started.elapsed() < Duration::from_secs(1) {
            tokio::time::sleep(Duration::from_millis(20)).await;
            alive_after = pid_is_alive(pid);
        }
        assert!(
            !alive_after,
            "process {} still alive after force_kill_child + 1s grace",
            pid
        );
        assert!(
            !client.is_alive(),
            "client.is_alive() should be false after force_kill"
        );
    }

    /// Audit 2.9: dropping a `PiRpcClient` (no explicit `force_kill_child`
    /// call) must still reap the underlying process within ~1s, even when
    /// the child ignores stdin close (e.g. `sleep 60`).
    ///
    /// Pre-2.9, the monitor task owned the `Child` and dropping the client
    /// only `abort()`-ed the task handles. The Child wouldn't actually be
    /// dropped — triggering `kill_on_drop(true)` — until the runtime got
    /// around to actually cancelling the monitor task, which for a
    /// blocking `child.wait()` poll could be arbitrarily delayed.
    ///
    /// Post-2.9, `Drop for PiRpcClient` takes the `Child` out of the
    /// shared slot itself, calls `start_kill()` synchronously, and
    /// schedules the reap on a detached task — so the OS sees SIGKILL
    /// immediately.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn drop_client_kills_hanging_process_within_one_second() {
        let mut cmd = Command::new("sleep");
        cmd.arg("60");
        let (client, pid) = PiRpcClient::spawn_for_test(cmd)
            .await
            .expect("spawn sleep 60");
        assert!(
            pid_is_alive(pid),
            "sleep should be alive immediately after spawn"
        );

        // Drop the client. No explicit force_kill — we are testing the
        // Drop impl's kill path.
        drop(client);

        // Allow up to 1s for the kernel to deliver SIGKILL + reap.
        let poll_started = std::time::Instant::now();
        let mut alive_after = pid_is_alive(pid);
        while alive_after && poll_started.elapsed() < Duration::from_secs(1) {
            tokio::time::sleep(Duration::from_millis(20)).await;
            alive_after = pid_is_alive(pid);
        }
        assert!(
            !alive_after,
            "process {} still alive 1s after PiRpcClient was dropped — Drop kill path regressed",
            pid
        );
    }

    /// Audit 2.9: the process-pool `OwnedSemaphorePermit` now lives on
    /// `PiRpcClient`, not on `PiSession`. Dropping the client must
    /// release the permit back to the semaphore the moment the subprocess
    /// is torn down — *independent* of whatever forwarder `Arc<PiSession>`
    /// clones may still be in flight.
    ///
    /// This test acquires the only permit from a 1-slot semaphore, spawns
    /// a test client holding that permit, asserts `available_permits()`
    /// is zero, drops the client, and then waits up to 1s for the count
    /// to return to capacity.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn drop_client_releases_semaphore_permit_promptly() {
        use tokio::sync::Semaphore;

        let semaphore = Arc::new(Semaphore::new(1));
        assert_eq!(semaphore.available_permits(), 1, "fresh semaphore");
        let permit = Arc::clone(&semaphore)
            .acquire_owned()
            .await
            .expect("acquire test permit");
        assert_eq!(
            semaphore.available_permits(),
            0,
            "permit held: no slots remain"
        );

        let mut cmd = Command::new("sleep");
        cmd.arg("60");
        let (client, pid) = PiRpcClient::spawn_for_test_with_permit(cmd, Some(permit))
            .await
            .expect("spawn sleep 60 with permit");
        assert!(pid_is_alive(pid), "sleep should be alive after spawn");
        assert_eq!(
            semaphore.available_permits(),
            0,
            "permit still held while client is alive"
        );

        drop(client);

        // Allow up to 1s for the permit to return to the pool.
        let poll_started = std::time::Instant::now();
        while semaphore.available_permits() == 0 && poll_started.elapsed() < Duration::from_secs(1)
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(
            semaphore.available_permits(),
            1,
            "permit did not return to the semaphore within 1s of dropping PiRpcClient"
        );

        // Sanity: the underlying process should also be gone by now (see
        // sibling test `drop_client_kills_hanging_process_within_one_second`
        // for the dedicated assertion).
        let poll_started = std::time::Instant::now();
        let mut alive_after = pid_is_alive(pid);
        while alive_after && poll_started.elapsed() < Duration::from_secs(1) {
            tokio::time::sleep(Duration::from_millis(20)).await;
            alive_after = pid_is_alive(pid);
        }
        assert!(
            !alive_after,
            "process {} still alive 1s after drop — Drop kill path regressed",
            pid
        );
    }

    #[test]
    fn parse_session_stats_top_level() {
        // Also support tokens/contextUsage at top level (legacy format)
        let line = r#"{"type":"response","success":true,"tokens":{"input":5,"output":50,"cacheRead":0,"cacheWrite":0,"total":55},"cost":0.01,"contextUsage":{"tokens":55,"contextWindow":100000,"percent":0.055}}"#;
        let event = parse_pi_event(line).expect("should parse");
        match event {
            PiEvent::SessionStats(stats) => {
                assert_eq!(stats.input, 5);
                assert_eq!(stats.output, 50);
                assert_eq!(stats.context_window, 100000);
            }
            other => panic!("expected SessionStats, got {:?}", other),
        }
    }

    // -- Phase 1: ToolExecutionStart carries `args` ---------------------------

    #[test]
    fn parse_tool_execution_start_captures_args_payload() {
        // Synthetic JSONL line mirroring Pi 0.74's tool_execution_start event
        // shape for an extension-registered tool. The model's args MUST be
        // captured verbatim — the Rust backend reads them off this event in
        // place of scanning the transcript for a delimited block.
        let line = r#"{"type":"tool_execution_start","toolCallId":"tc-1","toolName":"submit_handoff","args":{"feature_id":"feat-1","run_id":"run-abc","salient_summary":"impl","what_was_implemented":"x","verification":"y","success_state":"success"}}"#;
        let event = parse_pi_event(line).expect("should parse");
        match event {
            PiEvent::ToolExecutionStart {
                tool_call_id,
                name,
                args,
            } => {
                assert_eq!(tool_call_id, "tc-1");
                assert_eq!(name, "submit_handoff");
                assert_eq!(args["feature_id"], "feat-1");
                assert_eq!(args["success_state"], "success");
            }
            other => panic!("expected ToolExecutionStart, got {:?}", other),
        }
    }

    #[test]
    fn parse_tool_execution_start_defaults_args_to_null_when_missing() {
        // Built-in Pi tools (`read`, `bash`, etc.) may emit the event with
        // no `args` field. The variant must still parse with args=Null so
        // legacy handling keeps working unchanged.
        let line = r#"{"type":"tool_execution_start","toolCallId":"tc-2","toolName":"read"}"#;
        let event = parse_pi_event(line).expect("should parse");
        match event {
            PiEvent::ToolExecutionStart { name, args, .. } => {
                assert_eq!(name, "read");
                assert!(args.is_null());
            }
            other => panic!("expected ToolExecutionStart, got {:?}", other),
        }
    }

    #[test]
    fn hyvemind_extension_tool_list_is_non_empty_and_unique() {
        // Sanity check on the central allowlist so a future edit can't
        // silently drop the structured-output tools from ReadOnlyTools.
        assert!(!HYVEMIND_EXTENSION_TOOLS.is_empty());
        let mut seen = std::collections::HashSet::new();
        for name in HYVEMIND_EXTENSION_TOOLS {
            assert!(seen.insert(*name), "duplicate tool name: {}", name);
        }
        // Must contain the Phase-2/3 surfaces (others added in later phases).
        assert!(HYVEMIND_EXTENSION_TOOLS.contains(&"submit_handoff"));
        assert!(HYVEMIND_EXTENSION_TOOLS.contains(&"submit_task_complete"));
        assert!(HYVEMIND_EXTENSION_TOOLS.contains(&"submit_plan"));
        assert!(HYVEMIND_EXTENSION_TOOLS.contains(&"submit_features"));
        assert!(HYVEMIND_EXTENSION_TOOLS.contains(&"submit_review_prompt"));
    }

    #[test]
    fn read_only_tools_cli_args_include_extension_allowlist() {
        // The ReadOnlyTools branch in `to_cli_args` appends every name in
        // `HYVEMIND_EXTENSION_TOOLS` to the `--tools` argument. Without this
        // the model would get "tool not allowed" rejections when it tried
        // to call the structured-output tools from a read-only session.
        let opts = PiSessionOptions {
            model: "any-model".to_string(),
            thinking_level: ThinkingLevel::High,
            tool_set: ToolSet::ReadOnlyTools,
            system_prompt: Some("sys".to_string()),
            resume_session: false,
            session_file: None,
        };
        let args = opts.to_cli_args();
        let tools_idx = args
            .iter()
            .position(|a| a == "--tools")
            .expect("ReadOnlyTools should emit --tools");
        let allowlist = &args[tools_idx + 1];
        for name in HYVEMIND_EXTENSION_TOOLS {
            assert!(
                allowlist.contains(name),
                "expected ReadOnlyTools --tools to include {}, got {}",
                name,
                allowlist
            );
        }
    }

    #[test]
    fn guard_cli_args_include_subagent_mcp_and_extension_allowlist() {
        // Guard uses a `Custom` tool set with an explicit allowlist. The
        // allowlist must include the structured submission tools
        // (`HYVEMIND_EXTENSION_TOOLS`, e.g. `submit_guard_result`) AND the
        // `subagent` + `mcp` tools that Guard relies on for focused
        // fact-check delegation. This regression-guards both surfaces —
        // dropping either would silently re-introduce "tool not allowed"
        // rejections at runtime.
        let opts = PiSessionOptions::for_guard("any-model", "sys");
        let args = opts.to_cli_args();
        let tools_idx = args
            .iter()
            .position(|a| a == "--tools")
            .expect("Guard should emit --tools");
        let allowlist = &args[tools_idx + 1];
        assert!(
            allowlist.contains("subagent"),
            "expected Guard --tools to include subagent, got {}",
            allowlist
        );
        assert!(
            allowlist.contains("mcp"),
            "expected Guard --tools to include mcp, got {}",
            allowlist
        );
        for name in HYVEMIND_EXTENSION_TOOLS {
            assert!(
                allowlist.contains(name),
                "expected Guard --tools to include {}, got {}",
                name,
                allowlist
            );
        }
    }

    /// Audit 2.12: shared helper exercising the supervise-wrapped cleanup
    /// closure used by the Pi RPC stdout / monitor tasks. On panic the
    /// supervisor must:
    ///   1. flip the `alive` AtomicBool to `false` (so callers see the
    ///      child as dead), and
    ///   2. broadcast a `PiEvent::Error` so the chat-event consumer
    ///      surfaces a structured error in the UI instead of leaving a
    ///      phantom spinner.
    async fn run_pi_rpc_supervise_panic_check(component: &'static str) {
        use crate::pi::events::PiEvent;
        use std::sync::atomic::AtomicBool;

        let (event_tx, mut event_rx) = broadcast::channel::<PiEvent>(16);
        let alive = Arc::new(AtomicBool::new(true));

        let panic_tx = event_tx.clone();
        let panic_alive = alive.clone();
        let supervised = crate::supervise!(
            context = format!("pi rpc component={}", component),
            on_panic = move |panic_msg: String| {
                panic_alive.store(false, Ordering::SeqCst);
                let _ = panic_tx.send(PiEvent::Error(format!(
                    "pi {component} PANICKED: {panic_msg}"
                )));
            },
            async move {
                crate::util::supervise::panic_for_test("pi_rpc_test");
            }
        );

        tokio::spawn(supervised)
            .await
            .expect("supervisor must absorb the panic");

        // alive flipped to false
        assert!(
            !alive.load(Ordering::SeqCst),
            "{component}: alive should be false after panic"
        );

        // PiEvent::Error broadcast
        let evt = event_rx
            .recv()
            .await
            .expect("should have received Error event");
        match evt {
            PiEvent::Error(msg) => assert!(
                msg.contains("PANICKED"),
                "{component}: expected PANICKED in error, got: {msg}"
            ),
            other => panic!("{component}: expected PiEvent::Error, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn pi_rpc_stdout_panic_surfaces_error_event() {
        run_pi_rpc_supervise_panic_check("stdout_reader").await;
    }

    #[tokio::test]
    async fn pi_rpc_monitor_panic_surfaces_error_event() {
        run_pi_rpc_supervise_panic_check("process_monitor").await;
    }

    /// Stderr capture is best-effort; on panic we only log (no event), but
    /// the supervisor must still absorb the panic without killing the
    /// outer task and without bubbling up — this test confirms that.
    #[tokio::test]
    async fn pi_rpc_stderr_panic_is_absorbed_silently() {
        let supervised =
            crate::supervise!(context = "pi rpc component=stderr_reader", async move {
                crate::util::supervise::panic_for_test("pi_rpc_stderr_test");
            });
        tokio::spawn(supervised)
            .await
            .expect("supervisor must absorb the panic without bubbling");
    }
}
