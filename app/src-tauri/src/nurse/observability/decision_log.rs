//! Always-on per-decision JSONL log.
//!
//! Each Nurse decision — whether dispatched, gated, or noop — is keyed by
//! a `decision_id` and produces an ordered chain of events recorded as
//! one row per event. Daily rotation: `decisions.jsonl.YYYY-MM-DD`.
//!
//! See `nurse/README.md` (TODO) and the §Observability section of the
//! rewrite plan for the full envelope shape and per-event payloads.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::writer::{today_yyyy_mm_dd, JsonlWriter, PathResolver};

/// One row in `decisions.jsonl`. Wraps the per-event payload in a stable
/// envelope so post-hoc readers can filter by `decision_id` / `session_id`
/// without touching the per-event payload shape.
///
/// `event` is `Cow<'static, str>` so the engine can pass static strings
/// without allocation while deserialisers still produce owned values.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionLogRow {
    pub ts: chrono::DateTime<chrono::Utc>,
    pub ts_unix_ms: u64,
    pub decision_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<crate::nurse::snapshot::SessionOwnerDto>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    pub event: std::borrow::Cow<'static, str>,
    /// Monotonic per-`decision_id` counter so a reader can tell whether
    /// the chain is complete (and detect mid-write truncation).
    pub event_seq: u32,
    pub data: serde_json::Value,
}

#[derive(Debug)]
pub struct DecisionLogger {
    writer: JsonlWriter,
    root: PathBuf,
}

impl DecisionLogger {
    pub fn new(root: PathBuf, dropped_counter: Arc<AtomicU64>) -> Self {
        let root_for_resolver = root.clone();
        let resolver: PathResolver = Arc::new(move |_line| {
            root_for_resolver.join(format!("decisions.jsonl.{}", today_yyyy_mm_dd()))
        });
        let writer = JsonlWriter::spawn("decisions", resolver, dropped_counter);
        Self { writer, root }
    }

    pub fn root(&self) -> &std::path::Path {
        &self.root
    }

    pub fn write(&self, row: DecisionLogRow) {
        match serde_json::to_value(&row) {
            Ok(v) => self.writer.write(v),
            Err(e) => tracing::warn!(
                error = %e,
                event = %row.event,
                "failed to serialize DecisionLogRow"
            ),
        }
    }

    pub fn shutdown(&self) {
        self.writer.shutdown();
    }
}

/// Convenience builder used at every pipeline point.
pub fn row(
    decision_id: &str,
    session_id: Option<String>,
    owner: Option<crate::nurse::snapshot::SessionOwnerDto>,
    profile: Option<String>,
    event: &'static str,
    event_seq: u32,
    data: serde_json::Value,
) -> DecisionLogRow {
    let ts = chrono::Utc::now();
    DecisionLogRow {
        ts,
        ts_unix_ms: ts.timestamp_millis().max(0) as u64,
        decision_id: decision_id.to_string(),
        session_id,
        owner,
        profile,
        event: std::borrow::Cow::Borrowed(event),
        event_seq,
        data,
    }
}

/// Typed builders for every event the dispatcher emits. Each takes the
/// shared envelope (`decision_id` / `session_id` / `owner` / `profile` /
/// `event_seq`) plus typed per-event fields so dispatcher call sites never
/// type event names or field names as raw strings.
///
/// The envelope fields are accepted by reference where possible to avoid
/// cloning on call sites that already own the values inside a
/// `DispatchInput` snapshot. `session_id` / `owner` / `profile` are taken
/// by ref-of-Option so callers pass the cached envelope unchanged across
/// every event in the chain.
pub mod events {
    use super::{row, DecisionLogRow};
    use crate::nurse::health::Severity;
    use crate::nurse::snapshot::SessionOwnerDto;
    use serde_json::json;

    /// Shared `&Option<…>` references — saves callers from cloning the
    /// envelope between successive rows in the same decision chain.
    fn clone_envelope(
        session_id: &Option<String>,
        owner: &Option<SessionOwnerDto>,
        profile: &Option<String>,
    ) -> (Option<String>, Option<SessionOwnerDto>, Option<String>) {
        (session_id.clone(), owner.clone(), profile.clone())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn decision_started(
        decision_id: &str,
        session_id: &Option<String>,
        owner: &Option<SessionOwnerDto>,
        profile: &Option<String>,
        event_seq: u32,
        origin: &str,
        tier_at_birth: &str,
        detector: &str,
        severity: Severity,
        dedup_key: &str,
    ) -> DecisionLogRow {
        let (sid, own, prof) = clone_envelope(session_id, owner, profile);
        row(
            decision_id,
            sid,
            own,
            prof,
            "decision_started",
            event_seq,
            json!({
                "origin": origin,
                "tier_at_birth": tier_at_birth,
                "detector": detector,
                "severity": severity,
                "dedup_key": dedup_key,
            }),
        )
    }

    /// `outcome` is one of `"bypassed_critical"` / `"bypassed_tier1"` /
    /// `"gated"` / `"passed"`. `recent_count` and `skip_until_unix_ms`
    /// are populated only on `"gated"`.
    #[allow(clippy::too_many_arguments)]
    pub fn storm_guard_evaluated(
        decision_id: &str,
        session_id: &Option<String>,
        owner: &Option<SessionOwnerDto>,
        profile: &Option<String>,
        event_seq: u32,
        outcome: &str,
        recent_count: Option<usize>,
        skip_until_unix_ms: Option<u64>,
    ) -> DecisionLogRow {
        let (sid, own, prof) = clone_envelope(session_id, owner, profile);
        let mut data = json!({ "outcome": outcome });
        if let Some(rc) = recent_count {
            data["recent_count"] = json!(rc);
        }
        if let Some(s) = skip_until_unix_ms {
            data["skip_until_unix_ms"] = json!(s);
        }
        row(
            decision_id,
            sid,
            own,
            prof,
            "storm_guard_evaluated",
            event_seq,
            data,
        )
    }

    /// `outcome` is one of `"skipped_leave_it"` / `"gated"` / `"allowed"`.
    /// `reason` is set when gated; the counter fields are set when allowed.
    #[allow(clippy::too_many_arguments)]
    pub fn budget_evaluated(
        decision_id: &str,
        session_id: &Option<String>,
        owner: &Option<SessionOwnerDto>,
        profile: &Option<String>,
        event_seq: u32,
        outcome: &str,
        reason: Option<&str>,
        lifetime_used: Option<u32>,
        lifetime_cap: Option<u32>,
        per_detector_used: Option<u32>,
        per_detector_cap: Option<u32>,
    ) -> DecisionLogRow {
        let (sid, own, prof) = clone_envelope(session_id, owner, profile);
        let mut data = json!({ "outcome": outcome });
        if let Some(r) = reason {
            data["reason"] = json!(r);
        }
        if let Some(v) = lifetime_used {
            data["lifetime_used"] = json!(v);
        }
        if let Some(v) = lifetime_cap {
            data["lifetime_cap"] = json!(v);
        }
        if let Some(v) = per_detector_used {
            data["per_detector_used"] = json!(v);
        }
        if let Some(v) = per_detector_cap {
            data["per_detector_cap"] = json!(v);
        }
        row(
            decision_id,
            sid,
            own,
            prof,
            "budget_evaluated",
            event_seq,
            data,
        )
    }

    /// `action` is the snake-case name of the matched action variant; absent
    /// on misses. `entry_id` is the stable identifier for the table row.
    /// `downgrade_reason` is set when the lookup matched but the action was
    /// downgraded (e.g. `Restart` → `Cancel` for Review/Merge owners).
    #[allow(clippy::too_many_arguments)]
    pub fn tier1_evaluated(
        decision_id: &str,
        session_id: &Option<String>,
        owner: &Option<SessionOwnerDto>,
        profile: &Option<String>,
        event_seq: u32,
        matched: bool,
        action: Option<&str>,
        entry_id: Option<&str>,
        downgrade_reason: Option<&str>,
    ) -> DecisionLogRow {
        let (sid, own, prof) = clone_envelope(session_id, owner, profile);
        let mut data = json!({ "matched": matched });
        if let Some(a) = action {
            data["action"] = json!(a);
        }
        if let Some(id) = entry_id {
            data["entry_id"] = json!(id);
        }
        if let Some(r) = downgrade_reason {
            data["downgrade_reason"] = json!(r);
        }
        row(
            decision_id,
            sid,
            own,
            prof,
            "tier1_evaluated",
            event_seq,
            data,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn playbook_evaluated(
        decision_id: &str,
        session_id: &Option<String>,
        owner: &Option<SessionOwnerDto>,
        profile: &Option<String>,
        event_seq: u32,
        matched: bool,
        entry_id: Option<&str>,
        rationale: Option<&str>,
    ) -> DecisionLogRow {
        let (sid, own, prof) = clone_envelope(session_id, owner, profile);
        let mut data = json!({ "matched": matched });
        if let Some(id) = entry_id {
            data["entry_id"] = json!(id);
        }
        if let Some(r) = rationale {
            data["rationale"] = json!(r);
        }
        row(
            decision_id,
            sid,
            own,
            prof,
            "playbook_evaluated",
            event_seq,
            data,
        )
    }

    pub fn classifier_invoked(
        decision_id: &str,
        session_id: &Option<String>,
        owner: &Option<SessionOwnerDto>,
        profile: &Option<String>,
        event_seq: u32,
        provider: &str,
        model: &str,
    ) -> DecisionLogRow {
        let (sid, own, prof) = clone_envelope(session_id, owner, profile);
        row(
            decision_id,
            sid,
            own,
            prof,
            "classifier_invoked",
            event_seq,
            json!({
                "provider": provider,
                "model": model,
            }),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn classifier_returned(
        decision_id: &str,
        session_id: &Option<String>,
        owner: &Option<SessionOwnerDto>,
        profile: &Option<String>,
        event_seq: u32,
        decision: &str,
        input_tokens: Option<u64>,
        output_tokens: Option<u64>,
        cost_usd: Option<f64>,
        duration_ms: u64,
        provider: &str,
        model: &str,
        cache_hit_tokens: u64,
        cache_write_tokens: u64,
    ) -> DecisionLogRow {
        let (sid, own, prof) = clone_envelope(session_id, owner, profile);
        let mut data = json!({
            "decision": decision,
            "duration_ms": duration_ms,
            "provider": provider,
            "model": model,
            "cache_hit_tokens": cache_hit_tokens,
            "cache_write_tokens": cache_write_tokens,
        });
        if let Some(t) = input_tokens {
            data["input_tokens"] = json!(t);
        }
        if let Some(t) = output_tokens {
            data["output_tokens"] = json!(t);
        }
        if let Some(c) = cost_usd {
            data["cost_usd"] = json!(c);
        }
        // Derive a hit ratio when input_tokens is known (DeepSeek prefix-cache
        // is a fraction of prompt_tokens; Anthropic ephemeral cache is reported
        // alongside input_tokens). Null when we have no input_tokens to
        // normalise against.
        if let Some(input_total) = input_tokens {
            let denom = input_total + cache_hit_tokens;
            if denom > 0 {
                let ratio = cache_hit_tokens as f64 / denom as f64;
                data["cache_hit_ratio"] = json!(ratio);
            }
        }
        row(
            decision_id,
            sid,
            own,
            prof,
            "classifier_returned",
            event_seq,
            data,
        )
    }

    pub fn classifier_decision_downgraded(
        decision_id: &str,
        session_id: &Option<String>,
        owner: &Option<SessionOwnerDto>,
        profile: &Option<String>,
        event_seq: u32,
        reason: &str,
    ) -> DecisionLogRow {
        let (sid, own, prof) = clone_envelope(session_id, owner, profile);
        row(
            decision_id,
            sid,
            own,
            prof,
            "classifier_decision_downgraded",
            event_seq,
            json!({ "reason": reason }),
        )
    }

    /// Single-row event written once per batch-review tick. Carries the
    /// provider, model, token usage, cache-hit/-write counts, and number
    /// of per-session decisions parsed out of the response. `session_id` is
    /// always `None` (one call covers many sessions); `decision_id` is a
    /// synthetic `batch-{uuid}` so it doesn't collide with the per-session
    /// dispatch chains that follow.
    #[allow(clippy::too_many_arguments)]
    pub fn batch_classifier_returned(
        batch_decision_id: &str,
        provider: &str,
        model: &str,
        input_tokens: u32,
        output_tokens: u32,
        cache_hit_tokens: u64,
        cache_write_tokens: u64,
        duration_ms: u64,
        session_count: usize,
        parsed_decision_count: usize,
    ) -> DecisionLogRow {
        let mut data = json!({
            "provider": provider,
            "model": model,
            "input_tokens": input_tokens,
            "output_tokens": output_tokens,
            "cache_hit_tokens": cache_hit_tokens,
            "cache_write_tokens": cache_write_tokens,
            "duration_ms": duration_ms,
            "session_count": session_count,
            "parsed_decision_count": parsed_decision_count,
        });
        let denom = input_tokens as u64 + cache_hit_tokens;
        if denom > 0 {
            data["cache_hit_ratio"] = json!(cache_hit_tokens as f64 / denom as f64);
        }
        row(
            batch_decision_id,
            None,
            None,
            None,
            "batch_classifier_returned",
            0,
            data,
        )
    }

    /// Event written when the Tier-3 classifier returned a response but
    /// `parse_first_decision` couldn't decode it. Preserves the provider /
    /// model / token usage / cache stats from the raw response so a parse
    /// failure doesn't lose visibility — the user still sees whether the
    /// call hit the cache and how much it cost.
    #[allow(clippy::too_many_arguments)]
    pub fn classifier_returned_unparseable(
        decision_id: &str,
        session_id: &Option<String>,
        owner: &Option<SessionOwnerDto>,
        profile: &Option<String>,
        event_seq: u32,
        provider: &str,
        model: &str,
        duration_ms: u64,
        raw_len: usize,
        parse_error: &str,
        cache_hit_tokens: u64,
        cache_write_tokens: u64,
    ) -> DecisionLogRow {
        let (sid, own, prof) = clone_envelope(session_id, owner, profile);
        row(
            decision_id,
            sid,
            own,
            prof,
            "classifier_returned_unparseable",
            event_seq,
            json!({
                "provider": provider,
                "model": model,
                "duration_ms": duration_ms,
                "raw_len": raw_len,
                "parse_error": parse_error,
                "cache_hit_tokens": cache_hit_tokens,
                "cache_write_tokens": cache_write_tokens,
            }),
        )
    }

    pub fn intervention_dispatched(
        decision_id: &str,
        session_id: &Option<String>,
        owner: &Option<SessionOwnerDto>,
        profile: &Option<String>,
        event_seq: u32,
        tier_used: &str,
        action_kind: &str,
        outcome_summary: &str,
    ) -> DecisionLogRow {
        let (sid, own, prof) = clone_envelope(session_id, owner, profile);
        row(
            decision_id,
            sid,
            own,
            prof,
            "intervention_dispatched",
            event_seq,
            json!({
                "tier_used": tier_used,
                "action_kind": action_kind,
                "outcome_summary": outcome_summary,
            }),
        )
    }

    /// `stage` is one of `"abort_sent"` / `"liveness_check_at_t3s"` /
    /// `"force_kill_sent"` / `"dead_at"` / `"double_fail_giving_up"`.
    /// `still_alive` is populated for liveness checks; `reason` carries
    /// the canonical `"already_killed"` marker on the `dead_at` row that
    /// fires after a `PiManagerError::SessionNotFound`.
    #[allow(clippy::too_many_arguments)]
    pub fn kill_verification(
        decision_id: &str,
        session_id: &Option<String>,
        owner: &Option<SessionOwnerDto>,
        profile: &Option<String>,
        event_seq: u32,
        stage: &str,
        at_unix_ms: u64,
        still_alive: Option<bool>,
        reason: Option<&str>,
    ) -> DecisionLogRow {
        let (sid, own, prof) = clone_envelope(session_id, owner, profile);
        let mut data = json!({
            "stage": stage,
            "at_unix_ms": at_unix_ms,
        });
        if let Some(alive) = still_alive {
            data["still_alive"] = json!(alive);
        }
        if let Some(r) = reason {
            data["reason"] = json!(r);
        }
        row(
            decision_id,
            sid,
            own,
            prof,
            "kill_verification",
            event_seq,
            data,
        )
    }

    pub fn intervention_outcome(
        decision_id: &str,
        session_id: &Option<String>,
        owner: &Option<SessionOwnerDto>,
        profile: &Option<String>,
        event_seq: u32,
        result: &str,
        error: Option<&str>,
    ) -> DecisionLogRow {
        let (sid, own, prof) = clone_envelope(session_id, owner, profile);
        let mut data = json!({ "result": result });
        if let Some(e) = error {
            data["error"] = json!(e);
        }
        row(
            decision_id,
            sid,
            own,
            prof,
            "intervention_outcome",
            event_seq,
            data,
        )
    }

    /// `status` is one of the canonical finalisation tags
    /// (`"dispatched"` / `"gated_severity"` / `"gated_in_flight"` /
    /// `"gated_storm_guard"` / `"gated_budget"` / `"gated_post_lag"` /
    /// `"gated_self_kill_grace"` / `"gated_disabled"` /
    /// `"classifier_skipped_no_model"` / `"classifier_failed"` /
    /// `"fast_path_awaiting_model"` / `"fast_path_healthy_streaming"` /
    /// `"no_session"` / `"dispatched_synthesized"` /
    /// `"dispatcher_unattached"` / `"panic"`). `extra` is merged into
    /// the row's `data` object so callers can supply status-specific
    /// fields (e.g. `min_required`, `existing_decision_id`, `error`)
    /// without forcing a new typed builder per status.
    #[allow(clippy::too_many_arguments)]
    pub fn decision_finalised(
        decision_id: &str,
        session_id: &Option<String>,
        owner: &Option<SessionOwnerDto>,
        profile: &Option<String>,
        event_seq: u32,
        status: &str,
        total_duration_ms: u64,
        num_events_in_chain: u32,
        extra: serde_json::Value,
    ) -> DecisionLogRow {
        let (sid, own, prof) = clone_envelope(session_id, owner, profile);
        let mut data = json!({
            "status": status,
            "total_duration_ms": total_duration_ms,
            "num_events_in_chain": num_events_in_chain,
        });
        if let serde_json::Value::Object(map) = extra {
            if let serde_json::Value::Object(ref mut base) = data {
                for (k, v) in map {
                    base.insert(k, v);
                }
            }
        }
        row(
            decision_id,
            sid,
            own,
            prof,
            "decision_finalised",
            event_seq,
            data,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn write_then_disk_round_trip() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dropped = Arc::new(AtomicU64::new(0));
        let logger = DecisionLogger::new(tmp.path().to_path_buf(), Arc::clone(&dropped));
        logger.write(row(
            "d1",
            Some("s1".into()),
            None,
            None,
            "decision_started",
            0,
            serde_json::json!({"origin": "test"}),
        ));
        logger.write(row(
            "d1",
            Some("s1".into()),
            None,
            None,
            "decision_finalised",
            1,
            serde_json::json!({"status": "noop_below_threshold"}),
        ));
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let path = tmp
            .path()
            .join(format!("decisions.jsonl.{}", today_yyyy_mm_dd()));
        let txt = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<String> = txt.lines().map(|s| s.to_string()).collect();
        assert_eq!(lines.len(), 2);
        let r0: DecisionLogRow = serde_json::from_str(&lines[0]).unwrap();
        let r1: DecisionLogRow = serde_json::from_str(&lines[1]).unwrap();
        assert_eq!(r0.event, "decision_started");
        assert_eq!(r0.event_seq, 0);
        assert_eq!(r1.event, "decision_finalised");
        assert_eq!(r1.event_seq, 1);
        logger.shutdown();
    }
}
