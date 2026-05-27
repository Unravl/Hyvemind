//! Nurse system prompt accessor.
//!
//! The prompt source-of-truth is `app/src-tauri/prompts/nurse_system.md`,
//! compiled into the binary via `include_str!` so a corrupted or missing
//! prompt at runtime is a build failure, not a runtime soft-fault.

const NURSE_SYSTEM_PROMPT: &str = include_str!("../../prompts/nurse_system.md");

pub fn default_system_prompt() -> &'static str {
    NURSE_SYSTEM_PROMPT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_is_non_empty() {
        assert!(!default_system_prompt().is_empty());
    }
}
