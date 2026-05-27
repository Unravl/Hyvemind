//! Scout agent for feature analysis and planning.
//!
//! The scout examines a feature request and the working directory to produce
//! a detailed implementation plan, complexity estimate, and risk assessment.
//! Results are delivered exclusively via the `submit_scout_result` Pi
//! extension tool — there is no transcript-scraping fallback.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;

use crate::domain::swarm::Feature;
use crate::pi::session::PiSession;

/// System prompt for the scout agent, loaded at compile time.
const SCOUT_SYSTEM_PROMPT: &str = include_str!("../../prompts/scout_system.md");

/// Result of a scout analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoutResult {
    /// The implementation plan produced by the scout.
    pub plan: String,
    /// Estimated complexity: "low", "medium", or "high".
    pub estimated_complexity: String,
    /// Identified risks and concerns.
    pub risks: Vec<String>,
    /// The feature ID this scout result pertains to.
    pub feature_id: String,
}

/// Tool-args payload shape emitted by the Scout via `submit_scout_result`.
#[derive(Debug, Deserialize)]
struct ScoutToolArgs {
    plan: String,
    estimated_complexity: String,
    #[serde(default)]
    risks: Vec<String>,
}

/// Returns the default system prompt for the scout role.
///
/// This is intended to be passed to `PiSessionOptions::for_scout()` when
/// spawning the session, so that the Pi SDK delivers it via its
/// `systemPromptOverride` mechanism rather than being hacked into the
/// user message.
pub fn default_system_prompt() -> &'static str {
    SCOUT_SYSTEM_PROMPT
}

/// Run the scout agent on a feature.
///
/// The Pi session MUST have been spawned with the scout system prompt
/// already configured via `PiSessionOptions::for_scout()`. The Scout MUST
/// end the run by calling `submit_scout_result` — if it doesn't, this
/// function errors.
#[tracing::instrument(
    skip_all,
    fields(agent = "scout", feature_id = %feature.id)
)]
pub async fn run_scout(
    session: &Arc<PiSession>,
    feature: &Feature,
    working_dir: &Path,
    _system_prompt: &str,
) -> Result<ScoutResult> {
    let user_message = build_scout_prompt(feature, working_dir);

    tracing::debug!(
        feature_id = %feature.id,
        prompt_len = user_message.len(),
        "scout prompt built"
    );
    tracing::trace!(
        feature_id = %feature.id,
        full_prompt = %user_message,
        "scout full prompt"
    );

    tracing::info!(
        feature_id = %feature.id,
        feature_name = %feature.name,
        "running scout analysis"
    );

    session
        .send_prompt(&user_message, None)
        .await
        .map_err(|e| anyhow::anyhow!(e))
        .context(format!(
            "scout send_prompt failed for feature '{}'",
            feature.id
        ))?;

    let _response = session
        .collect_response()
        .await
        .map_err(|e| anyhow::anyhow!(e))
        .context(format!(
            "scout collect_response failed for feature '{}'",
            feature.id
        ))?;

    let args = session
        .take_tool_args("submit_scout_result")
        .ok_or_else(|| {
            anyhow!(
                "scout for feature '{}' did not call submit_scout_result",
                feature.id
            )
        })?;

    let parsed: ScoutToolArgs = serde_json::from_value(args).with_context(|| {
        format!(
            "scout for feature '{}' submit_scout_result args did not deserialise",
            feature.id
        )
    })?;

    let estimated_complexity = normalize_complexity(&parsed.estimated_complexity);

    let result = ScoutResult {
        plan: parsed.plan,
        estimated_complexity,
        risks: parsed.risks,
        feature_id: feature.id.clone(),
    };

    tracing::info!(
        feature_id = %feature.id,
        complexity = %result.estimated_complexity,
        risk_count = result.risks.len(),
        "scout analysis complete"
    );

    Ok(result)
}

/// Build the user prompt for the scout agent.
fn build_scout_prompt(feature: &Feature, working_dir: &Path) -> String {
    let deps_section = if feature.dependencies.is_empty() {
        "None".to_string()
    } else {
        feature.dependencies.join(", ")
    };

    let milestone_section = feature
        .milestone
        .as_deref()
        .unwrap_or("No milestone assigned");

    format!(
        r#"## Feature to Analyze

**ID:** {id}
**Name:** {name}
**Description:** {description}

**Dependencies:** {deps}
**Milestone:** {milestone}

**Working Directory:** {working_dir}

Please analyze this feature and call `submit_scout_result` with:
1. `plan` — a detailed step-by-step implementation plan (markdown)
2. `estimated_complexity` — exactly one of `"low"`, `"medium"`, or `"high"`
3. `risks` — an array of identified risks/concerns (empty array if none)
"#,
        id = feature.id,
        name = feature.name,
        description = feature.description,
        deps = deps_section,
        milestone = milestone_section,
        working_dir = working_dir.display(),
    )
}

/// Normalize a complexity string to one of: low, medium, high.
fn normalize_complexity(raw: &str) -> String {
    let lower = raw.trim().to_lowercase();
    if lower.contains("low") {
        "low".to_string()
    } else if lower.contains("high") {
        "high".to_string()
    } else {
        "medium".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_complexity() {
        assert_eq!(normalize_complexity("Low"), "low");
        assert_eq!(normalize_complexity("HIGH complexity"), "high");
        assert_eq!(normalize_complexity("Medium effort"), "medium");
        assert_eq!(normalize_complexity("something else"), "medium");
    }

    #[test]
    fn test_build_scout_prompt() {
        let feature = Feature::new(
            "feat-1".to_string(),
            "Login Page".to_string(),
            "Build the login page".to_string(),
        );
        let prompt = build_scout_prompt(&feature, Path::new("/tmp/project"));
        assert!(prompt.contains("feat-1"));
        assert!(prompt.contains("Login Page"));
        assert!(prompt.contains("/tmp/project"));
        assert!(prompt.contains("submit_scout_result"));
    }

    // -- E2E against MockRpcClient ------------------------------------------

    use crate::pi::events::PiEvent;
    use crate::pi::mock::mock_session;
    use crate::pi::rpc::PiCommand;

    /// Success path: the model calls `submit_scout_result` with a valid
    /// payload. `run_scout` deserialises the args and returns a typed
    /// `ScoutResult`.
    #[tokio::test]
    async fn scout_e2e_success_against_mock_transport() {
        let (session, mock) = mock_session("scout-success");
        mock.emit_text_chunk("Analyzing the feature...\n");
        mock.emit(PiEvent::ToolExecutionStart {
            tool_call_id: "tc-1".to_string(),
            name: "submit_scout_result".to_string(),
            args: serde_json::json!({
                "plan": "Step 1: foo\nStep 2: bar",
                "estimated_complexity": "low",
                "risks": ["only one"],
            }),
        });
        mock.emit_agent_end();

        let feature = Feature::new(
            "feat-mock".into(),
            "Mock Feature".into(),
            "exercise the transport".into(),
        );

        let result = run_scout(&session, &feature, Path::new("/tmp/mock"), "")
            .await
            .expect("scout should succeed against mock transport");

        assert_eq!(result.feature_id, "feat-mock");
        assert_eq!(result.estimated_complexity, "low");
        assert_eq!(result.risks.len(), 1);
        assert!(result.plan.contains("Step 1: foo"));

        // The scout sent exactly one prompt.
        let log = mock.sent_commands().await;
        assert_eq!(log.len(), 1, "expected single Prompt; got {:?}", log);
        match &log[0] {
            PiCommand::Prompt { message, .. } => {
                assert!(message.contains("feat-mock"));
                assert!(message.contains("Mock Feature"));
            }
            other => panic!("expected Prompt, got {:?}", other),
        }
    }

    /// Missing tool call: the model streams text but never calls
    /// `submit_scout_result`. `run_scout` must surface a clean error.
    #[tokio::test]
    async fn scout_e2e_missing_tool_call_against_mock_transport() {
        let (session, mock) = mock_session("scout-missing");
        mock.emit_text_chunk("Here's my analysis, no tool call though.\n");
        mock.emit_agent_end();

        let feature = Feature::new("feat-x".into(), "x".into(), "y".into());
        let err = run_scout(&session, &feature, Path::new("/tmp/mock"), "")
            .await
            .expect_err("scout should error when submit_scout_result is missing");
        let msg = format!("{:#}", err);
        assert!(
            msg.contains("submit_scout_result"),
            "error should mention the missing tool: {msg}"
        );
    }

    /// Failure path: the underlying transport dies mid-stream. The
    /// scout's `collect_response` must surface `PiRpcError::ProcessCrashed`
    /// which `run_scout` wraps with context.
    #[tokio::test]
    async fn scout_e2e_failure_on_crash_against_mock_transport() {
        let (session, mock) = mock_session("scout-crash");
        mock.crash("simulated provider 401");

        let feature = Feature::new("feat-x".into(), "x".into(), "y".into());
        let err = run_scout(&session, &feature, Path::new("/tmp/mock"), "")
            .await
            .expect_err("scout should propagate the transport crash");
        let msg = format!("{:#}", err);
        assert!(
            msg.contains("scout collect_response failed"),
            "missing collect_response context: {msg}"
        );
        assert!(
            msg.contains("simulated provider 401"),
            "stderr should propagate: {msg}"
        );
    }
}
