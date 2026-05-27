//! Validation contract — stable `VAL-*` assertion IDs and on-disk state.
//!
//! Phase 2 of the Factory adoption roadmap. Each milestone assertion
//! receives a stable identifier of the form `VAL-<ABBR>-<NNN>` (e.g.
//! `VAL-FND-001`). These IDs are the contract between the planner (which
//! authors `validation-contract.md`), the Guard agent (which verifies them),
//! and the per-swarm `validation-state.json` file that tracks per-assertion
//! pass/fail across runs.
//!
//! See the plan in `i-want-you-to-proud-dream.md` (Part 3 / Phase 2) for the
//! design rationale.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::domain::swarm::Milestone;

/// Status of a single tracked assertion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssertionStatus {
    /// Not yet checked.
    Pending,
    /// Last Guard run reported PASS.
    Passed,
    /// Last Guard run reported FAIL.
    Failed,
}

impl Default for AssertionStatus {
    fn default() -> Self {
        AssertionStatus::Pending
    }
}

/// A single validation assertion, identified by a stable `VAL-*` ID.
///
/// `text` is the runnable claim (e.g. "cargo test passes"); `id` is the
/// stable identifier referenced from `Feature::fulfills` and from Guard
/// per-assertion reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationAssertion {
    /// Stable identifier of the form `VAL-<ABBR>-<3-digit-counter>`.
    pub id: String,
    /// The milestone this assertion belongs to (matches `Milestone::id`).
    pub milestone_id: String,
    /// The runnable claim the assertion asserts.
    pub text: String,
    /// Current verification status.
    #[serde(default)]
    pub status: AssertionStatus,
    /// Last time Guard checked this assertion (RFC3339).
    #[serde(default)]
    pub last_checked_at: Option<DateTime<Utc>>,
    /// Last failure message, if any.
    #[serde(default)]
    pub last_error: Option<String>,
}

/// On-disk persisted state for validation assertions.
///
/// Flat map from `assertion_id` → entry. Serialised to
/// `~/.hyvemind/swarms/<id>/validation-state.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ValidationState {
    /// Assertion ID → entry.
    pub assertions: HashMap<String, ValidationStateEntry>,
}

/// One row in the validation-state map.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationStateEntry {
    #[serde(default)]
    pub status: AssertionStatus,
    #[serde(default)]
    pub last_checked_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_error: Option<String>,
}

impl ValidationState {
    /// Record the outcome of a single Guard check.
    pub fn record(
        &mut self,
        assertion_id: &str,
        status: AssertionStatus,
        last_error: Option<String>,
    ) {
        let entry = self.assertions.entry(assertion_id.to_string()).or_default();
        entry.status = status;
        entry.last_checked_at = Some(Utc::now());
        entry.last_error = last_error;
    }
}

/// Derive a 3-letter SHOUTY abbreviation from a milestone id for use in
/// `VAL-<ABBR>-NNN` ids.
///
/// Rules (applied in order):
/// 1. If the id is purely alphabetic, ≤ 4 chars, and not all digits, use it
///    upper-cased and right-padded to 3 chars with the last char.
/// 2. Otherwise, split by `-` / `_` / digits, take the first letter of each
///    alphabetic segment, upper-cased, until 3 chars are collected.
/// 3. If fewer than 3 letters are found, pad with the alphabetic content
///    of the original (upper-cased) until 3 chars are present.
/// 4. As a final fallback, use the supplied `fallback_index` formatted as
///    `MS<digit>` (e.g. milestone index 3 → `MS3`).
pub fn abbreviate_milestone_id(milestone_id: &str, fallback_index: usize) -> String {
    let trimmed = milestone_id.trim();

    // Rule 1: short pure-alphabetic ids (e.g. "fnd") become "FND"
    if !trimmed.is_empty() && trimmed.len() <= 4 && trimmed.chars().all(|c| c.is_ascii_alphabetic())
    {
        let upper: String = trimmed.to_uppercase();
        return pad_to_three_letters(&upper);
    }

    // Rule 2: take the first letter of each alphabetic word segment
    let mut acc = String::new();
    for segment in trimmed.split(|c: char| c == '-' || c == '_' || c.is_ascii_digit()) {
        let seg = segment.trim();
        if seg.is_empty() {
            continue;
        }
        if let Some(first) = seg.chars().find(|c| c.is_ascii_alphabetic()) {
            acc.push(first.to_ascii_uppercase());
            if acc.len() == 3 {
                break;
            }
        }
    }

    // Rule 3: pad from raw alphabetic content (repetitions allowed).
    if acc.len() < 3 {
        for ch in trimmed
            .chars()
            .filter(|c| c.is_ascii_alphabetic())
            .map(|c| c.to_ascii_uppercase())
        {
            acc.push(ch);
            if acc.len() == 3 {
                break;
            }
        }
    }

    if acc.is_empty() {
        // Rule 4: numeric-only or unparseable id
        return format!("MS{}", fallback_index);
    }

    pad_to_three_letters(&acc)
}

/// Right-pad a string to 3 chars by repeating its last char. If empty,
/// returns `"MS_"` as a deterministic non-empty fallback.
fn pad_to_three_letters(s: &str) -> String {
    if s.is_empty() {
        return "MS_".to_string();
    }
    let mut out = s.to_string();
    let last = out.chars().last().unwrap();
    while out.len() < 3 {
        out.push(last);
    }
    if out.len() > 3 {
        out.truncate(3);
    }
    out
}

/// Assign stable `VAL-<ABBR>-NNN` IDs to every assertion across `milestones`.
///
/// This is the canonical entry point used by `start_swarm` to produce a
/// flat list of `ValidationAssertion` carrying a stable ID for each
/// assertion text. The function does **not** mutate the milestone's own
/// `assertions: Vec<String>` (which remains the human-readable text); it
/// only produces the parallel `ValidationAssertion` registry.
///
/// Determinism: for a given list of milestones in a given order, the
/// returned IDs are stable across runs — counters reset to `001` per
/// milestone, and abbreviation derivation is pure.
pub fn assign_assertion_ids(milestones: &[Milestone]) -> Vec<ValidationAssertion> {
    let mut out: Vec<ValidationAssertion> = Vec::new();

    for (m_idx, milestone) in milestones.iter().enumerate() {
        let abbr = abbreviate_milestone_id(&milestone.id, m_idx + 1);
        for (a_idx, text) in milestone.assertions.iter().enumerate() {
            let id = format!("VAL-{}-{:03}", abbr, a_idx + 1);
            out.push(ValidationAssertion {
                id,
                milestone_id: milestone.id.clone(),
                text: text.clone(),
                status: AssertionStatus::Pending,
                last_checked_at: None,
                last_error: None,
            });
        }
    }

    out
}

/// Render a markdown document grouping assertions by milestone.
///
/// Used by `SwarmStore::write_validation_contract` to produce a
/// human-readable contract file. Format:
///
/// ```text
/// # Validation Contract
///
/// ## Milestone: <name> (<id>)
///
/// - `VAL-FND-001` — cargo check passes cleanly
/// - `VAL-FND-002` — all unit tests green
/// ```
pub fn render_validation_contract(
    milestones: &[Milestone],
    assertions: &[ValidationAssertion],
) -> String {
    let mut out = String::from("# Validation Contract\n\n");
    out.push_str(
        "This file lists every validation assertion the swarm must satisfy before a \
milestone can be sealed. The Guard agent verifies each assertion by its stable \
`VAL-*` identifier; the per-assertion state lives in `validation-state.json`.\n\n",
    );

    // Index assertions by milestone for O(1) lookup.
    let mut by_milestone: HashMap<&str, Vec<&ValidationAssertion>> = HashMap::new();
    for a in assertions {
        by_milestone
            .entry(a.milestone_id.as_str())
            .or_default()
            .push(a);
    }

    for milestone in milestones {
        out.push_str(&format!(
            "## Milestone: {} (`{}`)\n\n",
            milestone.name, milestone.id
        ));
        match by_milestone.get(milestone.id.as_str()) {
            Some(rows) if !rows.is_empty() => {
                for a in rows {
                    out.push_str(&format!("- `{}` — {}\n", a.id, a.text));
                }
            }
            _ => {
                out.push_str("_No assertions defined for this milestone._\n");
            }
        }
        out.push('\n');
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_milestone(id: &str, name: &str, assertions: Vec<&str>) -> Milestone {
        Milestone {
            id: id.to_string(),
            name: name.to_string(),
            features: Vec::new(),
            assertions: assertions.into_iter().map(String::from).collect(),
            sealed: false,
        }
    }

    // ----------------------------------------------------------------
    // abbreviate_milestone_id
    // ----------------------------------------------------------------

    #[test]
    fn test_abbr_short_alphabetic_id() {
        // "fnd" -> "FND"
        assert_eq!(abbreviate_milestone_id("fnd", 0), "FND");
        // "abc" -> "ABC"
        assert_eq!(abbreviate_milestone_id("abc", 0), "ABC");
    }

    #[test]
    fn test_abbr_hyphenated_id() {
        // "m1-foundations" -> first letter of each alpha segment: "F" + pad
        // segments after splitting on - and digits: "m" "foundations"
        // first letters: M, F → need 3, pad with next alpha char
        let v = abbreviate_milestone_id("m1-foundations", 1);
        assert_eq!(v.len(), 3);
        assert!(v.chars().all(|c| c.is_ascii_alphabetic()));
        assert!(v.starts_with('M') || v.starts_with('F'));
    }

    #[test]
    fn test_abbr_multi_word_id() {
        // "user-auth-flow" -> "UAF"
        assert_eq!(abbreviate_milestone_id("user-auth-flow", 0), "UAF");
        // "data_storage" -> first letters D, S, then pad from raw alphabetic
        // content → next alphabetic char is D (from "data") again → "DSD"
        assert_eq!(abbreviate_milestone_id("data_storage", 0), "DSD");
    }

    #[test]
    fn test_abbr_pure_numeric_uses_fallback() {
        // Pure numeric id falls back to MS<index>
        assert_eq!(abbreviate_milestone_id("123", 4), "MS4");
        assert_eq!(abbreviate_milestone_id("", 7), "MS7");
    }

    #[test]
    fn test_abbr_padding_for_one_letter() {
        // Single-letter id → pad to 3 by repeating
        assert_eq!(abbreviate_milestone_id("a", 0), "AAA");
        assert_eq!(abbreviate_milestone_id("x", 0), "XXX");
    }

    #[test]
    fn test_abbr_deterministic() {
        // Same input → same output, every time.
        // "polish-pass" → first letters P, P → pad from raw alpha → "PPP"
        for _ in 0..5 {
            assert_eq!(abbreviate_milestone_id("polish-pass", 0), "PPP");
        }
    }

    // ----------------------------------------------------------------
    // assign_assertion_ids
    // ----------------------------------------------------------------

    #[test]
    fn test_assign_ids_single_milestone() {
        let milestones = vec![make_milestone(
            "fnd",
            "Foundations",
            vec!["cargo check passes", "tests green"],
        )];
        let assigned = assign_assertion_ids(&milestones);
        assert_eq!(assigned.len(), 2);
        assert_eq!(assigned[0].id, "VAL-FND-001");
        assert_eq!(assigned[0].milestone_id, "fnd");
        assert_eq!(assigned[0].text, "cargo check passes");
        assert_eq!(assigned[0].status, AssertionStatus::Pending);
        assert_eq!(assigned[1].id, "VAL-FND-002");
        assert_eq!(assigned[1].text, "tests green");
    }

    #[test]
    fn test_assign_ids_multiple_milestones_counter_resets() {
        let milestones = vec![
            make_milestone("fnd", "Foundations", vec!["a", "b", "c"]),
            make_milestone("plsh", "Polish", vec!["d", "e"]),
        ];
        let assigned = assign_assertion_ids(&milestones);
        assert_eq!(assigned.len(), 5);
        // Foundations: counter starts at 001
        assert_eq!(assigned[0].id, "VAL-FND-001");
        assert_eq!(assigned[1].id, "VAL-FND-002");
        assert_eq!(assigned[2].id, "VAL-FND-003");
        // Polish: counter RESETS to 001
        // "plsh" abbreviation: short alphabetic 4 chars → first 3 letters
        // Actually: it's 4 chars purely alphabetic, so Rule 1 takes it as "PLSH"
        // then truncates to "PLS"
        assert_eq!(assigned[3].id, "VAL-PLS-001");
        assert_eq!(assigned[4].id, "VAL-PLS-002");
    }

    #[test]
    fn test_assign_ids_empty_milestone_list() {
        let milestones: Vec<Milestone> = Vec::new();
        assert!(assign_assertion_ids(&milestones).is_empty());
    }

    #[test]
    fn test_assign_ids_milestone_with_no_assertions() {
        let milestones = vec![make_milestone("empty", "Empty", vec![])];
        assert!(assign_assertion_ids(&milestones).is_empty());
    }

    #[test]
    fn test_assign_ids_stable_across_calls() {
        let milestones = vec![
            make_milestone("user-auth", "Auth", vec!["login works", "logout works"]),
            make_milestone("data", "Data", vec!["db connects"]),
        ];
        let first = assign_assertion_ids(&milestones);
        let second = assign_assertion_ids(&milestones);
        assert_eq!(first, second);
    }

    #[test]
    fn test_assign_ids_numeric_milestone_uses_fallback_abbr() {
        // Milestone with a purely numeric id → MS<index>
        let milestones = vec![make_milestone("42", "Forty-Two", vec!["something passes"])];
        let assigned = assign_assertion_ids(&milestones);
        assert_eq!(assigned.len(), 1);
        assert_eq!(assigned[0].id, "VAL-MS1-001");
    }

    // ----------------------------------------------------------------
    // ValidationState
    // ----------------------------------------------------------------

    #[test]
    fn test_validation_state_default_empty() {
        let s = ValidationState::default();
        assert!(s.assertions.is_empty());
    }

    #[test]
    fn test_validation_state_record_creates_entry() {
        let mut s = ValidationState::default();
        s.record("VAL-FND-001", AssertionStatus::Passed, None);
        let entry = s.assertions.get("VAL-FND-001").expect("entry present");
        assert_eq!(entry.status, AssertionStatus::Passed);
        assert!(entry.last_checked_at.is_some());
        assert!(entry.last_error.is_none());
    }

    #[test]
    fn test_validation_state_record_updates_existing() {
        let mut s = ValidationState::default();
        s.record("VAL-FND-001", AssertionStatus::Failed, Some("boom".into()));
        s.record("VAL-FND-001", AssertionStatus::Passed, None);
        let entry = s.assertions.get("VAL-FND-001").expect("entry present");
        assert_eq!(entry.status, AssertionStatus::Passed);
        assert!(entry.last_error.is_none());
    }

    #[test]
    fn test_validation_state_roundtrip_json() {
        let mut s = ValidationState::default();
        s.record("VAL-FND-001", AssertionStatus::Passed, None);
        s.record("VAL-FND-002", AssertionStatus::Failed, Some("err".into()));
        let json = serde_json::to_string(&s).expect("serialise");
        let back: ValidationState = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(back.assertions.len(), 2);
        assert_eq!(
            back.assertions.get("VAL-FND-001").unwrap().status,
            AssertionStatus::Passed
        );
        assert_eq!(
            back.assertions.get("VAL-FND-002").unwrap().status,
            AssertionStatus::Failed
        );
    }

    // ----------------------------------------------------------------
    // render_validation_contract
    // ----------------------------------------------------------------

    #[test]
    fn test_render_contract_groups_by_milestone() {
        let milestones = vec![
            make_milestone("fnd", "Foundations", vec!["cargo check", "tests pass"]),
            make_milestone("plsh", "Polish", vec!["lint clean"]),
        ];
        let assigned = assign_assertion_ids(&milestones);
        let rendered = render_validation_contract(&milestones, &assigned);
        assert!(rendered.contains("# Validation Contract"));
        assert!(rendered.contains("## Milestone: Foundations"));
        assert!(rendered.contains("## Milestone: Polish"));
        assert!(rendered.contains("VAL-FND-001"));
        assert!(rendered.contains("VAL-FND-002"));
        assert!(rendered.contains("VAL-PLS-001"));
        assert!(rendered.contains("cargo check"));
        assert!(rendered.contains("lint clean"));
    }

    #[test]
    fn test_render_contract_empty_milestone() {
        let milestones = vec![make_milestone("fnd", "Foundations", vec![])];
        let assigned = assign_assertion_ids(&milestones);
        let rendered = render_validation_contract(&milestones, &assigned);
        assert!(rendered.contains("## Milestone: Foundations"));
        assert!(rendered.contains("No assertions defined"));
    }
}
