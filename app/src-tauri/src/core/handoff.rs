//! Worker handoff parsing.
//!
//! Workers emit their structured handoff by calling the `submit_handoff` Pi
//! extension tool. The args are captured into `PiSession::captured_tool_args`
//! by the rpc layer; this module deserialises them into [`WorkerHandoff`].

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::pi::session::PiSession;

/// The outcome state reported by a worker.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SuccessState {
    Success,
    Failure,
    Partial,
}

/// Severity level for a `DiscoveredIssue` surfaced by a Worker.
///
/// Workers MAY tag issues so the UI can colour them (info=blue, warn=amber,
/// error=red). Severity is informational only — it never gates execution.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IssueSeverity {
    Info,
    Warn,
    Error,
}

impl std::fmt::Display for IssueSeverity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IssueSeverity::Info => write!(f, "info"),
            IssueSeverity::Warn => write!(f, "warn"),
            IssueSeverity::Error => write!(f, "error"),
        }
    }
}

/// A single issue a Worker discovered while implementing a feature that the
/// user should know about but which is **not** a hard failure of the
/// feature itself. Examples: deprecated dependency spotted in passing,
/// pre-existing test flake, mismatched lockfile, etc.
///
/// Surfaced to the frontend as a [`ProgressEventType::DiscoveredIssue`]
/// progress event so the user can acknowledge/dismiss them async — they
/// never block the swarm.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiscoveredIssue {
    pub severity: IssueSeverity,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested_fix: Option<String>,
}

impl<'de> Deserialize<'de> for SuccessState {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        match s.trim().to_ascii_lowercase().as_str() {
            "success" => Ok(SuccessState::Success),
            "failure" | "failed" | "fail" => Ok(SuccessState::Failure),
            "partial" => Ok(SuccessState::Partial),
            other => Err(serde::de::Error::custom(format!(
                "invalid success_state '{}': expected one of success, failure, partial",
                other
            ))),
        }
    }
}

impl std::fmt::Display for SuccessState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SuccessState::Success => write!(f, "success"),
            SuccessState::Failure => write!(f, "failure"),
            SuccessState::Partial => write!(f, "partial"),
        }
    }
}

/// Structured handoff data emitted by a worker at the end of execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerHandoff {
    pub feature_id: String,
    pub run_id: String,
    pub salient_summary: String,
    pub what_was_implemented: String,
    pub verification: String,
    pub success_state: SuccessState,
    /// Optional list of issues the Worker noticed but which are not a
    /// hard failure of the feature. Surfaced to the frontend as
    /// non-blocking progress events.
    #[serde(default)]
    pub discovered_issues: Vec<DiscoveredIssue>,
}

impl WorkerHandoff {
    /// Validate that required fields are non-empty.
    pub(crate) fn validate(&self) -> Result<()> {
        if self.feature_id.trim().is_empty() {
            return Err(anyhow!("handoff feature_id is empty"));
        }
        if self.run_id.trim().is_empty() {
            return Err(anyhow!("handoff run_id is empty"));
        }
        Ok(())
    }
}

/// Typed marker error attached when handoff parsing fails. The Queen
/// loop downcasts on this so it can synthesize a visible Nurse
/// intervention before the feature is marked failed.
#[derive(Debug, thiserror::Error)]
#[error("handoff parse failed: {reason}")]
pub struct HandoffParseFailed {
    pub reason: String,
}

/// Deserialise a `submit_handoff` tool-args JSON value into a [`WorkerHandoff`].
///
/// Returns `None` when the args do not match the expected shape or the
/// `feature_id` in the args doesn't equal `expected_feature_id` (the Worker
/// must report against the feature it was given). The caller is expected to
/// surface a `HandoffParseFailed` error on `None`.
pub fn handoff_from_tool_args(
    args: &serde_json::Value,
    expected_feature_id: &str,
) -> Option<WorkerHandoff> {
    let handoff: WorkerHandoff = serde_json::from_value(args.clone()).ok()?;
    handoff.validate().ok()?;
    if handoff.feature_id != expected_feature_id {
        return None;
    }
    Some(handoff)
}

/// Pull the captured `submit_handoff` tool args off a Pi session and
/// deserialise them. Returns an error wrapping [`HandoffParseFailed`] when
/// no tool call was captured or the args are malformed.
pub fn handoff_from_session(session: &Arc<PiSession>, feature_id: &str) -> Result<WorkerHandoff> {
    let args = session.take_tool_args("submit_handoff").ok_or_else(|| {
        anyhow::Error::new(HandoffParseFailed {
            reason: "worker did not call submit_handoff tool".to_string(),
        })
    })?;
    handoff_from_tool_args(&args, feature_id).ok_or_else(|| {
        anyhow::Error::new(HandoffParseFailed {
            reason: format!(
                "submit_handoff args did not validate or feature_id mismatch (expected {})",
                feature_id
            ),
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_handoff_args() -> serde_json::Value {
        serde_json::json!({
            "feature_id": "feat-1",
            "run_id": "run-abc",
            "salient_summary": "Implemented the login page",
            "what_was_implemented": "Added login form with validation",
            "verification": "Run tests with `cargo test`",
            "success_state": "success"
        })
    }

    #[test]
    fn handoff_from_tool_args_parses_complete_payload() {
        let args = sample_handoff_args();
        let h = handoff_from_tool_args(&args, "feat-1").expect("payload parses");
        assert_eq!(h.feature_id, "feat-1");
        assert_eq!(h.run_id, "run-abc");
        assert_eq!(h.success_state, SuccessState::Success);
        assert!(h.discovered_issues.is_empty());
    }

    #[test]
    fn handoff_from_tool_args_returns_none_on_feature_id_mismatch() {
        let args = sample_handoff_args();
        assert!(handoff_from_tool_args(&args, "feat-other").is_none());
    }

    #[test]
    fn handoff_from_tool_args_returns_none_on_missing_required_field() {
        let mut args = sample_handoff_args();
        args.as_object_mut().unwrap().remove("success_state");
        assert!(handoff_from_tool_args(&args, "feat-1").is_none());
    }

    #[test]
    fn handoff_from_tool_args_returns_none_on_empty_feature_id() {
        let args = serde_json::json!({
            "feature_id": "",
            "run_id": "r",
            "salient_summary": "s",
            "what_was_implemented": "w",
            "verification": "v",
            "success_state": "success",
        });
        assert!(handoff_from_tool_args(&args, "").is_none());
    }

    #[test]
    fn handoff_from_tool_args_accepts_discovered_issues() {
        let args = serde_json::json!({
            "feature_id": "feat-7",
            "run_id": "run-z",
            "salient_summary": "implemented login",
            "what_was_implemented": "form + validation",
            "verification": "cargo test",
            "success_state": "success",
            "discovered_issues": [
                { "severity": "warn", "description": "noticed stale fixture" },
                { "severity": "info", "description": "unused import" },
            ],
        });
        let h = handoff_from_tool_args(&args, "feat-7").expect("payload parses");
        assert_eq!(h.discovered_issues.len(), 2);
        assert_eq!(h.discovered_issues[0].severity, IssueSeverity::Warn);
    }

    #[test]
    fn success_state_case_insensitive() {
        for state_str in &[
            "Success",
            "SUCCESS",
            "success",
            " success ",
            "Failed",
            "PARTIAL",
        ] {
            let args = serde_json::json!({
                "feature_id": "f1",
                "run_id": "r1",
                "salient_summary": "s",
                "what_was_implemented": "w",
                "verification": "v",
                "success_state": state_str,
            });
            handoff_from_tool_args(&args, "f1")
                .unwrap_or_else(|| panic!("failed to parse success_state '{}'", state_str));
        }
    }

    #[test]
    fn success_state_display() {
        assert_eq!(SuccessState::Success.to_string(), "success");
        assert_eq!(SuccessState::Failure.to_string(), "failure");
        assert_eq!(SuccessState::Partial.to_string(), "partial");
    }

    #[test]
    fn discovered_issue_severity_display() {
        assert_eq!(IssueSeverity::Info.to_string(), "info");
        assert_eq!(IssueSeverity::Warn.to_string(), "warn");
        assert_eq!(IssueSeverity::Error.to_string(), "error");
    }
}
