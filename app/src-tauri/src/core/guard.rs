//! Guard validation agent.
//!
//! The guard runs after milestone features are complete to verify that
//! assertions defined in the milestone actually pass. If assertions fail,
//! fix features can be generated for another attempt. Results are delivered
//! exclusively via the `submit_guard_result` Pi extension tool.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;

use crate::core::validation::ValidationAssertion;
use crate::domain::swarm::{Feature, Milestone};
use crate::pi::session::PiSession;

/// System prompt for the guard agent, loaded at compile time.
const GUARD_SYSTEM_PROMPT: &str = include_str!("../../prompts/guard_system.md");

/// Result of a single assertion check.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssertionResult {
    /// The assertion that was checked.
    pub assertion: String,
    /// Whether the assertion passed.
    pub passed: bool,
    /// Output or evidence from running the check.
    pub output: String,
    /// Error message if the assertion failed.
    pub error: Option<String>,
    /// Optional stable `VAL-*` identifier this result corresponds to. Set
    /// when the Guard was invoked via `run_guard_with_assertions` (Phase 2
    /// validator features); `None` for legacy milestone-wide runs.
    #[serde(default)]
    pub assertion_id: Option<String>,
}

/// Result of guard validation for a feature.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationResult {
    /// Whether all assertions passed.
    pub passed: bool,
    /// Individual assertion results.
    pub assertion_results: Vec<AssertionResult>,
    /// The feature ID that was validated.
    pub feature_id: String,
}

impl ValidationResult {
    /// Count how many assertions failed.
    pub fn failure_count(&self) -> usize {
        self.assertion_results.iter().filter(|r| !r.passed).count()
    }

    /// Get the failed assertion results.
    pub fn failures(&self) -> Vec<&AssertionResult> {
        self.assertion_results
            .iter()
            .filter(|r| !r.passed)
            .collect()
    }
}

/// Tool-args payload shape emitted by the Guard via `submit_guard_result`.
#[derive(Debug, Deserialize)]
struct GuardToolArgs {
    assertions: Vec<GuardAssertionItem>,
}

#[derive(Debug, Deserialize)]
struct GuardAssertionItem {
    #[serde(default)]
    id: Option<String>,
    status: GuardStatus,
    #[serde(default)]
    evidence: String,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum GuardStatus {
    Pass,
    Fail,
}

/// Returns the default system prompt for the guard role.
pub fn default_system_prompt() -> &'static str {
    GUARD_SYSTEM_PROMPT
}

/// Run guard validation against an explicit list of `ValidationAssertion`
/// rather than the full milestone's `assertions` slice.
///
/// Used by the Phase 2 auto-injected validator features: the Queen looks up
/// the assertions referenced in `feature.fulfills`, passes them here, and
/// receives back a `ValidationResult` whose `assertion_id` field on each
/// result is populated with the matching `VAL-*` id.
///
/// When `to_check` is empty, this auto-passes (mirroring legacy `run_guard`).
#[tracing::instrument(
    skip_all,
    fields(agent = "guard", feature_id = %feature.id, validator = true)
)]
pub async fn run_guard_with_assertions(
    session: &Arc<PiSession>,
    feature: &Feature,
    milestone: &Milestone,
    to_check: &[&ValidationAssertion],
    working_dir: &Path,
    _system_prompt: &str,
) -> Result<ValidationResult> {
    if to_check.is_empty() {
        tracing::info!(
            feature_id = %feature.id,
            milestone_id = %milestone.id,
            "no assertions to validate (Phase 2); auto-passing"
        );
        return Ok(ValidationResult {
            passed: true,
            assertion_results: Vec::new(),
            feature_id: feature.id.clone(),
        });
    }

    let user_message = build_guard_prompt_for_assertions(feature, milestone, working_dir, to_check);

    tracing::debug!(
        feature_id = %feature.id,
        assertion_count = to_check.len(),
        prompt_len = user_message.len(),
        "guard (Phase 2) prompt built"
    );
    tracing::trace!(
        feature_id = %feature.id,
        full_prompt = %user_message,
        "guard (Phase 2) full prompt"
    );

    tracing::info!(
        feature_id = %feature.id,
        milestone_id = %milestone.id,
        assertion_count = to_check.len(),
        "running guard validation against VAL-* assertion subset"
    );

    session
        .send_prompt(&user_message, None)
        .await
        .map_err(|e| anyhow::anyhow!(e))
        .context(format!(
            "guard send_prompt failed for validator feature '{}' / milestone '{}'",
            feature.id, milestone.id
        ))?;

    let _response = session
        .collect_response()
        .await
        .map_err(|e| anyhow::anyhow!(e))
        .context(format!(
            "guard collect_response failed for validator feature '{}' / milestone '{}'",
            feature.id, milestone.id
        ))?;

    let args = session
        .take_tool_args("submit_guard_result")
        .ok_or_else(|| {
            anyhow!(
                "guard for validator feature '{}' did not call submit_guard_result",
                feature.id
            )
        })?;

    let mut result = guard_result_from_args(&args, &feature.id, to_check)?;
    // Phase 2: back-fill VAL-* id on each row when the tool args omitted it.
    for (r, a) in result.assertion_results.iter_mut().zip(to_check.iter()) {
        if r.assertion_id.is_none() {
            r.assertion_id = Some(a.id.clone());
        }
    }

    tracing::debug!(
        feature_id = %feature.id,
        passed = result.passed,
        assertion_results = ?result.assertion_results.iter().map(|r| (r.assertion_id.clone(), r.passed)).collect::<Vec<_>>(),
        "guard (Phase 2) per-assertion results"
    );

    tracing::info!(
        feature_id = %feature.id,
        passed = result.passed,
        total = result.assertion_results.len(),
        failed = result.failure_count(),
        "guard validation (Phase 2) complete"
    );

    Ok(result)
}

/// Build the user prompt for the Phase 2 Guard run.
fn build_guard_prompt_for_assertions(
    feature: &Feature,
    milestone: &Milestone,
    working_dir: &Path,
    to_check: &[&ValidationAssertion],
) -> String {
    let assertions_list: String = to_check
        .iter()
        .enumerate()
        .map(|(i, a)| format!("{}. `{}` — {}", i + 1, a.id, a.text))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        r#"## Guard Validation (Validator Feature)

**Feature ID:** {feature_id}
**Feature Name:** {feature_name}
**Milestone:** {milestone_name} (`{milestone_id}`)
**Working Directory:** {working_dir}

## Assertions to Verify

Each assertion below is identified by its stable `VAL-*` ID. Report results
keyed by these IDs.

{assertions}

## Instructions

For each assertion above, verify whether it holds true in the current state
of the working directory. Then call the `submit_guard_result` tool with one
entry per assertion (in the order presented), each carrying `id` (the VAL-*),
`status` ("pass" or "fail"), `evidence`, and `error` (omit or null on pass).
"#,
        feature_id = feature.id,
        feature_name = feature.name,
        milestone_id = milestone.id,
        milestone_name = milestone.name,
        working_dir = working_dir.display(),
        assertions = assertions_list,
    )
}

/// Deserialise `submit_guard_result` tool args into a `ValidationResult`.
fn guard_result_from_args(
    args: &serde_json::Value,
    feature_id: &str,
    to_check: &[&ValidationAssertion],
) -> Result<ValidationResult> {
    let parsed: GuardToolArgs = serde_json::from_value(args.clone())
        .with_context(|| format!("submit_guard_result args did not deserialise"))?;

    if parsed.assertions.len() != to_check.len() {
        return Err(anyhow!(
            "guard returned {} assertion results, expected {}",
            parsed.assertions.len(),
            to_check.len()
        ));
    }

    let assertion_results: Vec<AssertionResult> = parsed
        .assertions
        .into_iter()
        .zip(to_check.iter())
        .map(|(item, expected)| {
            let passed = matches!(item.status, GuardStatus::Pass);
            AssertionResult {
                assertion: expected.text.clone(),
                passed,
                output: item.evidence,
                error: item.error.filter(|e| {
                    let lower = e.trim().to_lowercase();
                    !lower.is_empty() && lower != "none" && lower != "n/a"
                }),
                assertion_id: item.id,
            }
        })
        .collect();

    let passed = assertion_results.iter().all(|r| r.passed);
    Ok(ValidationResult {
        passed,
        assertion_results,
        feature_id: feature_id.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assertion(id: &str, text: &str) -> ValidationAssertion {
        ValidationAssertion {
            id: id.to_string(),
            milestone_id: "m1".to_string(),
            text: text.to_string(),
            status: Default::default(),
            last_checked_at: None,
            last_error: None,
        }
    }

    #[test]
    fn guard_result_from_args_parses_all_pass() {
        let a1 = assertion("VAL-001", "cargo test passes");
        let a2 = assertion("VAL-002", "endpoint /health returns 200");
        let refs: Vec<&ValidationAssertion> = vec![&a1, &a2];

        let args = serde_json::json!({
            "assertions": [
                {"id": "VAL-001", "status": "pass", "evidence": "cargo test ok"},
                {"id": "VAL-002", "status": "pass", "evidence": "curl returned 200"},
            ]
        });
        let r = guard_result_from_args(&args, "f1", &refs).unwrap();
        assert!(r.passed);
        assert_eq!(r.assertion_results.len(), 2);
        assert!(r.assertion_results[0].passed);
        assert_eq!(
            r.assertion_results[0].assertion_id.as_deref(),
            Some("VAL-001")
        );
    }

    #[test]
    fn guard_result_from_args_marks_failures_and_keeps_error() {
        let a1 = assertion("VAL-001", "tests pass");
        let refs: Vec<&ValidationAssertion> = vec![&a1];

        let args = serde_json::json!({
            "assertions": [
                {"id": "VAL-001", "status": "fail", "evidence": "compile error", "error": "missing semicolon"},
            ]
        });
        let r = guard_result_from_args(&args, "f1", &refs).unwrap();
        assert!(!r.passed);
        assert_eq!(r.failure_count(), 1);
        assert!(r.assertion_results[0].error.is_some());
    }

    #[test]
    fn guard_result_from_args_filters_none_and_na_errors() {
        let a1 = assertion("VAL-001", "tests pass");
        let refs: Vec<&ValidationAssertion> = vec![&a1];

        for noisy in &["none", "N/A", " "] {
            let args = serde_json::json!({
                "assertions": [
                    {"id": "VAL-001", "status": "pass", "evidence": "ok", "error": noisy},
                ]
            });
            let r = guard_result_from_args(&args, "f1", &refs).unwrap();
            assert!(
                r.assertion_results[0].error.is_none(),
                "error '{}' should be filtered",
                noisy
            );
        }
    }

    #[test]
    fn guard_result_from_args_errors_on_length_mismatch() {
        let a1 = assertion("VAL-001", "tests pass");
        let a2 = assertion("VAL-002", "lints clean");
        let refs: Vec<&ValidationAssertion> = vec![&a1, &a2];
        let args = serde_json::json!({
            "assertions": [
                {"id": "VAL-001", "status": "pass", "evidence": "ok"}
            ]
        });
        assert!(guard_result_from_args(&args, "f1", &refs).is_err());
    }

    #[test]
    fn guard_result_from_args_errors_on_malformed_payload() {
        let a1 = assertion("VAL-001", "tests pass");
        let refs: Vec<&ValidationAssertion> = vec![&a1];
        let args = serde_json::json!({"not_assertions": []});
        assert!(guard_result_from_args(&args, "f1", &refs).is_err());
    }

    #[test]
    fn test_validation_result_helpers() {
        let result = ValidationResult {
            passed: false,
            assertion_results: vec![
                AssertionResult {
                    assertion: "a1".to_string(),
                    passed: true,
                    output: "ok".to_string(),
                    error: None,
                    assertion_id: None,
                },
                AssertionResult {
                    assertion: "a2".to_string(),
                    passed: false,
                    output: "bad".to_string(),
                    error: Some("err".to_string()),
                    assertion_id: None,
                },
            ],
            feature_id: "f1".to_string(),
        };

        assert_eq!(result.failure_count(), 1);
        assert_eq!(result.failures().len(), 1);
        assert_eq!(result.failures()[0].assertion, "a2");
    }

    #[test]
    fn test_default_system_prompt_non_empty() {
        let prompt = default_system_prompt();
        assert!(!prompt.is_empty());
    }

    // -- E2E against MockRpcClient ------------------------------------------

    use crate::pi::events::PiEvent;
    use crate::pi::mock::mock_session;

    #[tokio::test]
    async fn guard_e2e_success_against_mock_transport() {
        let (session, mock) = mock_session("guard-success");
        let milestone = Milestone {
            id: "m1".to_string(),
            name: "M1".to_string(),
            features: vec!["f1".to_string()],
            assertions: vec!["x".to_string()],
            sealed: false,
        };
        let a1 = assertion("VAL-001", "cargo test passes");
        let refs: Vec<&ValidationAssertion> = vec![&a1];

        mock.emit_text_chunk("Verifying assertion...\n");
        mock.emit(PiEvent::ToolExecutionStart {
            tool_call_id: "tc-1".to_string(),
            name: "submit_guard_result".to_string(),
            args: serde_json::json!({
                "assertions": [
                    {"id": "VAL-001", "status": "pass", "evidence": "cargo test ok"}
                ]
            }),
        });
        mock.emit_agent_end();

        let feature = Feature::new("f1".into(), "F1".into(), "d".into());
        let r =
            run_guard_with_assertions(&session, &feature, &milestone, &refs, Path::new("/tmp"), "")
                .await
                .expect("guard succeeds against mock transport");
        assert!(r.passed);
        assert_eq!(r.assertion_results.len(), 1);
        assert_eq!(
            r.assertion_results[0].assertion_id.as_deref(),
            Some("VAL-001")
        );
    }

    #[tokio::test]
    async fn guard_e2e_missing_tool_call_errors() {
        let (session, mock) = mock_session("guard-missing");
        let milestone = Milestone {
            id: "m1".to_string(),
            name: "M1".to_string(),
            features: vec!["f1".to_string()],
            assertions: vec!["x".to_string()],
            sealed: false,
        };
        let a1 = assertion("VAL-001", "cargo test passes");
        let refs: Vec<&ValidationAssertion> = vec![&a1];

        mock.emit_text_chunk("Forgot to call the tool.\n");
        mock.emit_agent_end();

        let feature = Feature::new("f1".into(), "F1".into(), "d".into());
        let err =
            run_guard_with_assertions(&session, &feature, &milestone, &refs, Path::new("/tmp"), "")
                .await
                .expect_err("guard errors when submit_guard_result missing");
        let msg = format!("{:#}", err);
        assert!(msg.contains("submit_guard_result"));
    }
}
