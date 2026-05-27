//! Structured-output schema for Hivemind reviewers.
//!
//! The engine always asks each provider for a tool call against this schema
//! instead of free-form markdown. The provider response is deserialised into
//! [`StructuredReview`] and rendered to the same markdown shape the merge
//! orchestrator consumes — i.e. the downstream Pi merge agent reads a string
//! regardless of which provider produced it.

use serde::{Deserialize, Serialize};

/// Tool definition payload passed alongside a provider request when the
/// structured-output path is active. Provider-specific request builders
/// inject the tool into `tools` and reference its `name` in `tool_choice`.
///
/// Constructed once at engine startup via [`reviewer_tool_definition`] so
/// the per-round dispatch loop doesn't re-build the schema for each model.
#[derive(Debug, Clone)]
pub struct StructuredOutputConfig {
    /// The tool definitions to expose to the model. Each entry is a
    /// provider-agnostic JSON object — Anthropic wants `{"name", "description",
    /// "input_schema"}` and OpenAI wants `{"type":"function","function":{...}}`;
    /// the provider request builder converts as needed.
    pub tools: Vec<serde_json::Value>,
    /// `tool_choice` value forcing the model to call the named tool. For
    /// Anthropic: `{"type":"tool","name":"submit_review"}`. For OpenAI:
    /// `{"type":"function","function":{"name":"submit_review"}}`. The
    /// provider builder picks the right shape from this opaque value.
    pub tool_choice: serde_json::Value,
}

/// Parsed structured-review payload. Fields mirror the JSON schema in
/// [`reviewer_tool_definition`] one-to-one. Order is preserved so the
/// rendered markdown is deterministic.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StructuredReview {
    pub verdict: String,
    #[serde(default)]
    pub issues: Vec<ReviewIssue>,
    #[serde(default)]
    pub strengths: Vec<String>,
    #[serde(default)]
    pub key_takeaways: Vec<String>,
}

/// Single issue surfaced by a reviewer. `layer` is the conceptual category
/// (1: architecture, 2: design, 3: implementation detail, 4: nit) — its
/// exact meaning is owned by the reviewer prompt, not the schema.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReviewIssue {
    pub layer: u8,
    pub title: String,
    pub file_path: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested_fix: Option<String>,
}

impl StructuredReview {
    /// Render the structured payload back to the markdown shape the merge
    /// orchestrator already understands. Stable section ordering and
    /// punctuation; trims trailing whitespace.
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();

        out.push_str("## Verdict\n\n");
        out.push_str(self.verdict.trim());
        out.push_str("\n");

        if !self.issues.is_empty() {
            out.push_str("\n## Issues\n\n");
            for (i, issue) in self.issues.iter().enumerate() {
                out.push_str(&format!(
                    "{}. **[L{}] {}** (`{}`)\n   {}\n",
                    i + 1,
                    issue.layer,
                    issue.title.trim(),
                    issue.file_path.trim(),
                    issue.description.trim(),
                ));
                if let Some(fix) = &issue.suggested_fix {
                    let f = fix.trim();
                    if !f.is_empty() {
                        out.push_str(&format!("   _Suggested fix:_ {}\n", f));
                    }
                }
            }
        }

        if !self.strengths.is_empty() {
            out.push_str("\n## Strengths\n\n");
            for s in &self.strengths {
                out.push_str(&format!("- {}\n", s.trim()));
            }
        }

        if !self.key_takeaways.is_empty() {
            out.push_str("\n## Key takeaways\n\n");
            for k in &self.key_takeaways {
                out.push_str(&format!("- {}\n", k.trim()));
            }
        }

        out.trim_end().to_string()
    }
}

/// The canonical JSON schema for the reviewer tool. Both Anthropic and
/// OpenAI-compatible providers accept JSON-Schema-shaped `input_schema` /
/// `parameters` so this single literal serves both — the per-provider
/// request builder wraps it in the right outer envelope.
pub fn reviewer_input_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "required": ["verdict"],
        "properties": {
            "verdict": { "type": "string" },
            "issues": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["layer", "title", "file_path", "description"],
                    "properties": {
                        "layer": { "type": "integer", "minimum": 1, "maximum": 4 },
                        "title": { "type": "string" },
                        "file_path": { "type": "string" },
                        "description": { "type": "string" },
                        "suggested_fix": { "type": "string" }
                    }
                }
            },
            "strengths": { "type": "array", "items": { "type": "string" } },
            "key_takeaways": { "type": "array", "items": { "type": "string" } }
        }
    })
}

/// Reviewer tool name. Matched against the model's `tool_use` /
/// `tool_calls` response shapes by the provider response parsers.
pub const REVIEWER_TOOL_NAME: &str = "submit_review";

/// Reviewer tool description. Surfaced to the model alongside the schema
/// so it knows what shape to produce.
pub const REVIEWER_TOOL_DESCRIPTION: &str =
    "Submit your structured review of the plan-under-review. \
     The Hivemind merge orchestrator consumes the args directly — \
     do not also emit a markdown-only response in the same turn.";

/// Build an Anthropic-shaped tool definition for the reviewer schema.
pub fn anthropic_tool_definition() -> serde_json::Value {
    serde_json::json!({
        "name": REVIEWER_TOOL_NAME,
        "description": REVIEWER_TOOL_DESCRIPTION,
        "input_schema": reviewer_input_schema(),
    })
}

/// Build an OpenAI-shaped tool definition for the reviewer schema.
pub fn openai_tool_definition() -> serde_json::Value {
    serde_json::json!({
        "type": "function",
        "function": {
            "name": REVIEWER_TOOL_NAME,
            "description": REVIEWER_TOOL_DESCRIPTION,
            "parameters": reviewer_input_schema(),
        }
    })
}

/// `tool_choice` for forcing the reviewer call (Anthropic).
pub fn anthropic_tool_choice() -> serde_json::Value {
    serde_json::json!({ "type": "tool", "name": REVIEWER_TOOL_NAME })
}

/// `tool_choice` for forcing the reviewer call (OpenAI / OpenRouter / Ollama).
pub fn openai_tool_choice() -> serde_json::Value {
    serde_json::json!({ "type": "function", "function": { "name": REVIEWER_TOOL_NAME } })
}

/// Build the Anthropic-flavoured config Hivemind passes to
/// `AnthropicProvider`. Convenience wrapper so engine.rs doesn't need to
/// know the per-provider envelope shape.
pub fn anthropic_structured_config() -> StructuredOutputConfig {
    StructuredOutputConfig {
        tools: vec![anthropic_tool_definition()],
        tool_choice: anthropic_tool_choice(),
    }
}

/// Build the OpenAI-flavoured config (also used for OpenRouter/Ollama).
pub fn openai_structured_config() -> StructuredOutputConfig {
    StructuredOutputConfig {
        tools: vec![openai_tool_definition()],
        tool_choice: openai_tool_choice(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> StructuredReview {
        StructuredReview {
            verdict: "Approve with minor fixes.".to_string(),
            issues: vec![
                ReviewIssue {
                    layer: 2,
                    title: "Hard-coded retry count".to_string(),
                    file_path: "src/retry.rs".to_string(),
                    description: "Should come from config.".to_string(),
                    suggested_fix: Some("Read from Config::retry_max.".to_string()),
                },
                ReviewIssue {
                    layer: 4,
                    title: "Comment typo".to_string(),
                    file_path: "src/lib.rs".to_string(),
                    description: "`recieve` → `receive`.".to_string(),
                    suggested_fix: None,
                },
            ],
            strengths: vec!["Clean separation of concerns.".to_string()],
            key_takeaways: vec!["Ship after fixing L2 retry issue.".to_string()],
        }
    }

    #[test]
    fn structured_review_renders_to_stable_markdown() {
        let md = sample().to_markdown();
        assert!(md.contains("## Verdict\n\nApprove with minor fixes."));
        assert!(md.contains("## Issues\n\n1. **[L2] Hard-coded retry count**"));
        assert!(md.contains("_Suggested fix:_ Read from Config::retry_max."));
        // No suggested_fix line for the second issue.
        let issue_2_idx = md.find("Comment typo").expect("issue 2 present");
        let tail = &md[issue_2_idx..];
        let next_section_idx = tail.find("\n## ").unwrap_or(tail.len());
        let issue_2_block = &tail[..next_section_idx];
        assert!(!issue_2_block.contains("_Suggested fix:_"));
        assert!(md.contains("## Strengths\n\n- Clean separation of concerns."));
        assert!(md.contains("## Key takeaways\n\n- Ship after fixing L2 retry issue."));
    }

    #[test]
    fn structured_review_renders_empty_optional_sections_as_omitted() {
        let minimal = StructuredReview {
            verdict: "All good.".to_string(),
            issues: vec![],
            strengths: vec![],
            key_takeaways: vec![],
        };
        let md = minimal.to_markdown();
        assert!(md.contains("## Verdict"));
        assert!(!md.contains("## Issues"));
        assert!(!md.contains("## Strengths"));
        assert!(!md.contains("## Key takeaways"));
    }

    #[test]
    fn structured_review_round_trip_through_json() {
        let original = sample();
        let json = serde_json::to_value(&original).unwrap();
        let decoded: StructuredReview = serde_json::from_value(json).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn structured_review_accepts_minimal_payload_with_defaults() {
        // Only `verdict` is required — optional arrays default to empty.
        let json = serde_json::json!({ "verdict": "ok" });
        let r: StructuredReview = serde_json::from_value(json).unwrap();
        assert!(r.issues.is_empty());
        assert!(r.strengths.is_empty());
        assert!(r.key_takeaways.is_empty());
    }

    #[test]
    fn anthropic_and_openai_tool_definitions_agree_on_schema_body() {
        let anth = anthropic_tool_definition();
        let oai = openai_tool_definition();
        assert_eq!(anth["name"], REVIEWER_TOOL_NAME);
        assert_eq!(oai["function"]["name"], REVIEWER_TOOL_NAME);
        // Same input_schema shape, just nested differently.
        assert_eq!(anth["input_schema"], oai["function"]["parameters"]);
    }
}
