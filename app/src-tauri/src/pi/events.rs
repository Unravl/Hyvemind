use serde::{Deserialize, Serialize};

/// Real token usage stats from a Pi session (via `get_session_stats` command).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PiSessionStats {
    pub input: u64,
    pub output: u64,
    pub reasoning_tokens: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub total_tokens: u64,
    pub cost: f64,
    pub context_tokens: u64,
    pub context_window: u64,
    pub context_percent: f64,
}

/// Events emitted by Pi sessions over the IPC boundary.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
pub enum PiEvent {
    TextDelta(String),
    ThinkingDelta(String),
    ToolExecutionStart {
        tool_call_id: String,
        name: String,
        /// Raw `args` payload the model passed to the tool call. Pi's RPC
        /// `tool_execution_start` event carries this; for built-in tools
        /// (`read`, `bash`, etc.) callers ignore it and lean on the
        /// `tool_execution_end` `result`. For Hyvemind-registered
        /// extension tools (`submit_handoff`, `submit_plan`, etc.) the
        /// model's input IS the payload we care about — `take_tool_args`
        /// on `PiSession` returns it so consumers can deserialise without
        /// touching delimiter parsing.
        #[serde(default)]
        args: serde_json::Value,
    },
    ToolExecutionUpdate {
        tool_call_id: String,
        output: String,
    },
    ToolExecutionEnd {
        tool_call_id: String,
        result: serde_json::Value,
    },
    MessageStart,
    MessageEnd,
    TurnStart,
    TurnComplete,
    AgentStart,
    AgentEnd,
    Error(String),
    Heartbeat,
    /// Legacy tool use event.
    ToolUse {
        name: String,
        input: serde_json::Value,
    },
    /// Legacy tool result event.
    ToolResult {
        output: serde_json::Value,
    },
    /// Queue state update (emitted after steer/follow_up).
    QueueUpdate {
        steering: Vec<String>,
        follow_up: Vec<String>,
    },
    /// Real token usage from `get_session_stats` response.
    SessionStats(PiSessionStats),
    /// Pi is auto-retrying after a transient provider error (e.g. Anthropic
    /// `overloaded_error`). Emitted right after Pi's premature `agent_end`,
    /// before the retry delay. The session must NOT be torn down between
    /// `AutoRetryStart` and `AutoRetryEnd` — Pi will resume streaming on
    /// success.
    AutoRetryStart {
        attempt: u32,
        max_attempts: u32,
        delay_ms: u64,
        error_message: String,
    },
    /// Pi finished an auto-retry attempt. `success: true` means streaming
    /// will continue; `success: false` means retries are exhausted and the
    /// stream should be treated as terminally failed.
    AutoRetryEnd {
        success: bool,
        attempt: u32,
    },
    /// Pi emitted a `thinking_level_change` event reporting the effective
    /// level for this session. If the value differs from what Hyvemind
    /// requested at spawn, Pi clamped it (usually because the provider
    /// extension didn't declare reasoning capability for the chosen model).
    ThinkingLevelChange {
        level: String,
    },
}

impl PiEvent {
    /// Approximate in-memory byte size of this event's payload. Used by
    /// the session transcript trimmer to enforce a byte cap in addition
    /// to the event-count cap. Counts heap-allocated string and JSON
    /// content; ignores the enum discriminant and Copy fields.
    pub fn estimated_size(&self) -> usize {
        fn json_size(v: &serde_json::Value) -> usize {
            match v {
                serde_json::Value::Null => 0,
                serde_json::Value::Bool(_) => 1,
                serde_json::Value::Number(_) => 16,
                serde_json::Value::String(s) => s.len(),
                serde_json::Value::Array(arr) => arr.iter().map(json_size).sum(),
                serde_json::Value::Object(map) => {
                    map.iter().map(|(k, v)| k.len() + json_size(v)).sum()
                }
            }
        }
        match self {
            PiEvent::TextDelta(s) | PiEvent::ThinkingDelta(s) => s.len(),
            PiEvent::ToolExecutionStart {
                tool_call_id,
                name,
                args,
            } => tool_call_id.len() + name.len() + json_size(args),
            PiEvent::ToolExecutionUpdate {
                tool_call_id,
                output,
            } => tool_call_id.len() + output.len(),
            PiEvent::ToolExecutionEnd {
                tool_call_id,
                result,
            } => tool_call_id.len() + json_size(result),
            PiEvent::Error(s) => s.len(),
            PiEvent::ToolUse { name, input } => name.len() + json_size(input),
            PiEvent::ToolResult { output } => json_size(output),
            PiEvent::QueueUpdate {
                steering,
                follow_up,
            } => {
                steering.iter().map(|s| s.len()).sum::<usize>()
                    + follow_up.iter().map(|s| s.len()).sum::<usize>()
            }
            PiEvent::SessionStats(_) => 96, // fixed-size struct of u64/f64
            PiEvent::AutoRetryStart { error_message, .. } => error_message.len() + 24,
            PiEvent::AutoRetryEnd { .. } => 8,
            PiEvent::ThinkingLevelChange { level } => level.len(),
            // Markers (MessageStart/End, TurnStart/Complete, AgentStart/End, Heartbeat)
            _ => 0,
        }
    }

    /// Return a clone of this event with large string fields truncated
    /// to `max_bytes`. Used by the nurse to keep diagnostic prompts
    /// within LLM context limits.
    pub fn truncated(&self, max_bytes: usize) -> PiEvent {
        fn trunc(s: &str, max: usize) -> String {
            if s.len() <= max {
                return s.to_string();
            }
            let mut end = max;
            while end > 0 && !s.is_char_boundary(end) {
                end -= 1;
            }
            format!("{}[truncated]", &s[..end])
        }

        fn trunc_json(v: &serde_json::Value, max: usize) -> serde_json::Value {
            match v {
                serde_json::Value::String(s) => serde_json::Value::String(trunc(s, max)),
                other => other.clone(),
            }
        }

        match self {
            PiEvent::TextDelta(s) => PiEvent::TextDelta(trunc(s, max_bytes)),
            PiEvent::ThinkingDelta(s) => PiEvent::ThinkingDelta(trunc(s, max_bytes)),
            PiEvent::ToolExecutionUpdate {
                tool_call_id,
                output,
            } => PiEvent::ToolExecutionUpdate {
                tool_call_id: tool_call_id.clone(),
                output: trunc(output, max_bytes),
            },
            PiEvent::ToolExecutionEnd {
                tool_call_id,
                result,
            } => PiEvent::ToolExecutionEnd {
                tool_call_id: tool_call_id.clone(),
                result: trunc_json(result, max_bytes),
            },
            PiEvent::Error(s) => PiEvent::Error(trunc(s, max_bytes)),
            PiEvent::ToolResult { output } => PiEvent::ToolResult {
                output: trunc_json(output, max_bytes),
            },
            // All other variants are either small or don't contain
            // user-content strings — clone as-is.
            other => other.clone(),
        }
    }
}
