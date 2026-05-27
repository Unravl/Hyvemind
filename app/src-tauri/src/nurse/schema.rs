//! Provider-native tools/tool_choice JSON for `nurse_decisions`.
//!
//! Copied verbatim from `core::nurse_schema` so the LLM contract is
//! identical across the cutover. Both Anthropic and OpenAI-compatible
//! providers accept JSON-Schema-shaped `input_schema` / `parameters`, so
//! this single literal serves both — the per-provider request builder
//! wraps it in the right outer envelope.

use crate::hivemind::review_schema::StructuredOutputConfig;

/// Tool name used in `tool_choice`. Matches the function/tool name the
/// model is forced to call. Surfaced to the provider parsers so the
/// extractors match the right block when the model honoured the call.
pub const NURSE_TOOL_NAME: &str = "nurse_decisions";

/// Tool description shown to the model.
pub const NURSE_TOOL_DESCRIPTION: &str =
    "Emit Nurse decisions for the sessions in the input. The Hyvemind \
     runtime reads the args directly. For the single-error-event path, \
     return a `decisions` array with exactly one element.";

/// Canonical JSON schema for the Nurse tool.
pub fn nurse_input_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "required": ["decisions"],
        "properties": {
            "decisions": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["session_id", "decision", "reasoning"],
                    "properties": {
                        "session_id": { "type": "string" },
                        "decision": {
                            "type": "string",
                            "enum": ["leave_it", "steer", "restart", "cancel"]
                        },
                        "reasoning": { "type": "string" },
                        "message": { "type": "string" },
                        "observation": { "type": "string" },
                        "action": { "type": "string" },
                        "check_back_secs": { "type": "integer", "minimum": 1, "maximum": 1800 }
                    }
                }
            }
        }
    })
}

pub fn anthropic_tool_definition() -> serde_json::Value {
    serde_json::json!({
        "name": NURSE_TOOL_NAME,
        "description": NURSE_TOOL_DESCRIPTION,
        "input_schema": nurse_input_schema(),
    })
}

pub fn openai_tool_definition() -> serde_json::Value {
    serde_json::json!({
        "type": "function",
        "function": {
            "name": NURSE_TOOL_NAME,
            "description": NURSE_TOOL_DESCRIPTION,
            "parameters": nurse_input_schema(),
        }
    })
}

pub fn anthropic_tool_choice() -> serde_json::Value {
    serde_json::json!({ "type": "tool", "name": NURSE_TOOL_NAME })
}

pub fn openai_tool_choice() -> serde_json::Value {
    serde_json::json!({ "type": "function", "function": { "name": NURSE_TOOL_NAME } })
}

pub fn anthropic_structured_config() -> StructuredOutputConfig {
    StructuredOutputConfig {
        tools: vec![anthropic_tool_definition()],
        tool_choice: anthropic_tool_choice(),
    }
}

pub fn openai_structured_config() -> StructuredOutputConfig {
    StructuredOutputConfig {
        tools: vec![openai_tool_definition()],
        tool_choice: openai_tool_choice(),
    }
}

pub fn structured_config_for_provider(provider_name: &str) -> StructuredOutputConfig {
    match provider_name {
        "anthropic" => anthropic_structured_config(),
        _ => openai_structured_config(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anthropic_and_openai_definitions_agree_on_schema_body() {
        let anth = anthropic_tool_definition();
        let oai = openai_tool_definition();
        assert_eq!(anth["name"], NURSE_TOOL_NAME);
        assert_eq!(oai["function"]["name"], NURSE_TOOL_NAME);
        assert_eq!(anth["input_schema"], oai["function"]["parameters"]);
    }

    #[test]
    fn schema_has_required_decision_enum() {
        let schema = nurse_input_schema();
        let decisions = &schema["properties"]["decisions"]["items"];
        let enum_values = decisions["properties"]["decision"]["enum"]
            .as_array()
            .unwrap();
        let strings: Vec<&str> = enum_values.iter().filter_map(|v| v.as_str()).collect();
        for k in ["leave_it", "steer", "restart", "cancel"] {
            assert!(strings.contains(&k));
        }
    }
}
