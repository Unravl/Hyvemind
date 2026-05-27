//! Streaming progress event types emitted on the `"hivemind-progress"`
//! Tauri channel.
//!
//! These types live in `hivemind/` (not `commands/`) so subsystems such as
//! `core/scout_review.rs` and `core/stability_test/runner.rs` can emit /
//! consume them without depending on the `commands/` IPC layer.
//! `commands/hivemind.rs` re-exports them so the existing IPC surface is
//! unchanged.

use serde::{Deserialize, Serialize};

/// Progress event emitted during a hivemind review.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HivemindProgressEvent {
    pub job_id: String,
    pub review_id: Option<String>,
    pub event_type: String,
    pub round: u32,
    pub model_id: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_len: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub swarm_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub feature_id: Option<String>,
    /// Coarse phase tag: `"round"`, `"merge"`, `"context"`, `"completed"`,
    /// `"started"`, `"failed"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    /// Set on origin-from-Tasks reviews so the Tasks-view reducer can
    /// scope events. Absent on Swarm-initiated reviews.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    /// Human-readable origin label (e.g. `"Scout: feat-001"`,
    /// `"Queen master plan"`, `"Task <id>"`). Renders in the UI strip.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_label: Option<String>,
    /// Streamed token slice on `*_chunk` event types.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delta: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
    /// Pi session id for inline `context_*` / `merge_*` structured-chunk
    /// events. Carried on `*_started` so frontend reducers can register
    /// the session as "internal" before the first delta arrives.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Tool call id for `*_tool_start` / `*_tool_update` / `*_tool_end`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Tool name on `*_tool_start`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    /// Tool args (raw JSON) on `*_tool_start`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_args: Option<serde_json::Value>,
    /// Streamed tool output chunk on `*_tool_update`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_output: Option<String>,
    /// Final tool result on `*_tool_end`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_result: Option<serde_json::Value>,
}
