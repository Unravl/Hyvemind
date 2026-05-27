//! Tool-args reader for the merge orchestrator's `submit_verdicts` payload.
//!
//! The merge agent calls `submit_verdicts({verdicts: [...]})` with one entry
//! per reviewer suggestion; this module deserialises that args object into
//! `Vec<RoundVerdict>` ready for `HivemindStore::save_round_verdicts`.
//!
//! The returned `RoundVerdict` values have empty `id` / `job_id` / `created_at`
//! and `round_number = 0`; the caller is responsible for filling those in.

use serde_json::Value;

use super::store::RoundVerdict;

const ALLOWED_VERDICTS: [&str; 3] = ["accepted", "rejected", "modified"];

/// Deserialise a `submit_verdicts` tool-args object into a list of typed
/// verdicts. Returns an empty `Vec` when the args don't carry a usable
/// `verdicts` (or legacy `decisions`) array.
pub fn verdicts_from_tool_args(args: &Value) -> Vec<RoundVerdict> {
    let Some(arr) = verdicts_array(args) else {
        return Vec::new();
    };
    let mut out: Vec<RoundVerdict> = arr.iter().filter_map(decision_to_verdict).collect();
    enforce_single_best_find(&mut out);
    out
}

fn verdicts_array(parsed: &Value) -> Option<&Vec<Value>> {
    if let Value::Array(arr) = parsed {
        return Some(arr);
    }
    let obj = parsed.as_object()?;
    for key in ["verdicts", "decisions", "results"] {
        if let Some(Value::Array(arr)) = obj.get(key) {
            return Some(arr);
        }
    }
    None
}

fn decision_to_verdict(d: &Value) -> Option<RoundVerdict> {
    let obj = d.as_object()?;

    let reviewer_model = first_string(obj, &["reviewer_model", "reviewer", "model", "model_id"])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())?;

    let suggestion = first_string(obj, &["suggestion", "issue", "finding", "summary"])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())?;

    let raw_verdict = obj
        .get("verdict")
        .and_then(Value::as_str)
        .map(|s| s.to_ascii_lowercase().trim().to_string())
        .unwrap_or_default();
    let verdict = if ALLOWED_VERDICTS.contains(&raw_verdict.as_str()) {
        raw_verdict
    } else {
        "rejected".to_string()
    };

    let severity = obj.get("severity").and_then(|v| match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    });
    let severity: Option<i64> = severity.map(|n| n.round().clamp(1.0, 5.0) as i64);

    let reason = obj
        .get("reason")
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let raw_best = obj.get("best_find").or_else(|| obj.get("bestFind"));
    let best_find = match raw_best {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_i64() == Some(1),
        Some(Value::String(s)) => s.trim().eq_ignore_ascii_case("true"),
        _ => false,
    };

    let co_reviewers: Option<Vec<String>> = obj
        .get("co_reviewers")
        .or_else(|| obj.get("coReviewers"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(|s| s.trim().to_string()))
                .filter(|s| !s.is_empty())
                .collect::<Vec<String>>()
        })
        .filter(|v| !v.is_empty());

    Some(RoundVerdict {
        id: String::new(),
        job_id: String::new(),
        round_number: 0,
        reviewer_model,
        suggestion,
        verdict,
        severity,
        reason,
        created_at: String::new(),
        best_find,
        co_reviewers,
    })
}

fn first_string<'a>(obj: &'a serde_json::Map<String, Value>, keys: &[&str]) -> Option<&'a str> {
    for k in keys {
        if let Some(Value::String(s)) = obj.get(*k) {
            return Some(s.as_str());
        }
    }
    None
}

/// Enforce at most one `best_find=true` per parsed batch (one merge = one round).
/// If the orchestrator marked multiple, keep the highest severity; tiebreak by
/// first occurrence.
fn enforce_single_best_find(out: &mut [RoundVerdict]) {
    let best_idxs: Vec<usize> = out
        .iter()
        .enumerate()
        .filter_map(|(i, v)| if v.best_find { Some(i) } else { None })
        .collect();
    if best_idxs.len() <= 1 {
        return;
    }

    let mut keep_idx = best_idxs[0];
    let mut keep_sev = out[keep_idx].severity.unwrap_or(-1);
    for &i in &best_idxs[1..] {
        let sev = out[i].severity.unwrap_or(-1);
        if sev > keep_sev {
            keep_idx = i;
            keep_sev = sev;
        }
    }
    for i in best_idxs {
        if i != keep_idx {
            out[i].best_find = false;
        }
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn happy_path_from_tool_args() {
        let args = serde_json::json!({
            "verdicts": [
                {
                    "reviewer_model": "anthropic/claude-sonnet-4",
                    "suggestion": "Add SELECT FOR UPDATE on family-id lookup",
                    "verdict": "accepted",
                    "severity": 4,
                    "reason": "Real race; agreed by 2/3 reviewers.",
                    "co_reviewers": ["openai/gpt-5"],
                    "best_find": true
                },
                {
                    "reviewer_model": "openai/gpt-5",
                    "suggestion": "Scope creep on rotate_session helper",
                    "verdict": "rejected",
                    "severity": 1
                }
            ]
        });
        let verdicts = verdicts_from_tool_args(&args);
        assert_eq!(verdicts.len(), 2);
        assert_eq!(verdicts[0].reviewer_model, "anthropic/claude-sonnet-4");
        assert_eq!(verdicts[0].verdict, "accepted");
        assert_eq!(verdicts[0].severity, Some(4));
        assert!(verdicts[0].best_find);
        assert_eq!(
            verdicts[0].co_reviewers.as_deref(),
            Some(&["openai/gpt-5".to_string()][..])
        );
        assert_eq!(verdicts[1].verdict, "rejected");
        assert!(!verdicts[1].best_find);
    }

    #[test]
    fn accepts_legacy_decisions_key() {
        let args = serde_json::json!({
            "decisions": [
                {"reviewer": "a/b", "suggestion": "x", "verdict": "modified"}
            ]
        });
        let verdicts = verdicts_from_tool_args(&args);
        assert_eq!(verdicts.len(), 1);
        assert_eq!(verdicts[0].verdict, "modified");
    }

    #[test]
    fn dedup_best_find_keeps_highest_severity() {
        let args = serde_json::json!({
            "verdicts": [
                {"reviewer_model": "a/b", "suggestion": "s1", "verdict": "accepted", "severity": 2, "best_find": true},
                {"reviewer_model": "c/d", "suggestion": "s2", "verdict": "accepted", "severity": 5, "best_find": true},
                {"reviewer_model": "e/f", "suggestion": "s3", "verdict": "accepted", "severity": 3, "best_find": true}
            ]
        });
        let verdicts = verdicts_from_tool_args(&args);
        assert_eq!(verdicts.len(), 3);
        let best: Vec<bool> = verdicts.iter().map(|v| v.best_find).collect();
        assert_eq!(best, vec![false, true, false]);
    }

    #[test]
    fn invalid_verdict_defaults_to_rejected() {
        let args = serde_json::json!({
            "verdicts": [{"reviewer_model": "a/b", "suggestion": "x", "verdict": "maybe"}]
        });
        let verdicts = verdicts_from_tool_args(&args);
        assert_eq!(verdicts.len(), 1);
        assert_eq!(verdicts[0].verdict, "rejected");
    }

    #[test]
    fn empty_or_missing_args_returns_empty_vec() {
        assert!(verdicts_from_tool_args(&serde_json::json!({})).is_empty());
        assert!(verdicts_from_tool_args(&serde_json::json!({"verdicts": []})).is_empty());
        assert!(verdicts_from_tool_args(&serde_json::json!("string")).is_empty());
    }

    #[test]
    fn alternate_field_names_still_parse() {
        let args = serde_json::json!({
            "verdicts": [{"model_id": "x/y", "finding": "bug", "verdict": "accepted", "severity": "4"}]
        });
        let verdicts = verdicts_from_tool_args(&args);
        assert_eq!(verdicts.len(), 1);
        assert_eq!(verdicts[0].reviewer_model, "x/y");
        assert_eq!(verdicts[0].suggestion, "bug");
        assert_eq!(verdicts[0].severity, Some(4));
    }

    #[test]
    fn severity_clamped_to_range() {
        let args = serde_json::json!({
            "verdicts": [
                {"reviewer_model": "a/b", "suggestion": "x", "verdict": "accepted", "severity": 99},
                {"reviewer_model": "c/d", "suggestion": "y", "verdict": "accepted", "severity": 0}
            ]
        });
        let verdicts = verdicts_from_tool_args(&args);
        assert_eq!(verdicts[0].severity, Some(5));
        assert_eq!(verdicts[1].severity, Some(1));
    }
}
