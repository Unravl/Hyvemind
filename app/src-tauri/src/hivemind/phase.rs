//! Phase-classification helpers for resumable reviews.
//!
//! The classifier (`compute_review_phase`) is a pure function that maps a
//! `Job` + its `JobStep`s + `MergeRunInfo`s onto a [`ReviewResumePhase`] +
//! round + message. It is used by:
//! * `commands/hivemind.rs::get_resumable_review_for_task` — to build the
//!   full frontend-facing snapshot.
//! * [`derive_phase_for_emit`] — invoked at startup to populate the
//!   `review_interrupted` event payload.
//!
//! Lives in `hivemind/` (not `commands/`) so the startup-time emit code in
//! `lib.rs` can call it without depending on the IPC adapter layer.

use serde::Serialize;

use crate::hivemind::store::{HivemindStore, Job, JobStep, MergeRunInfo};

/// Phase the resume button should land in. See the phase derivation rules in
/// `compute_review_phase`. The lower-cased serialisation matches the contract
/// the frontend `ReviewPhase` union expects.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewResumePhase {
    Context,
    Round,
    Merge,
    BetweenRounds,
    Final,
}

/// Stable string for the `phase` field on a `review_interrupted` event
/// payload. Mirrors the `ReviewResumePhase` serialisation.
pub(crate) fn phase_to_str(p: &ReviewResumePhase) -> &'static str {
    match p {
        ReviewResumePhase::Context => "context",
        ReviewResumePhase::Round => "round",
        ReviewResumePhase::Merge => "merge",
        ReviewResumePhase::BetweenRounds => "between_rounds",
        ReviewResumePhase::Final => "final",
    }
}

/// Returned by [`compute_review_phase`] alongside the round and human-readable
/// status message. The caller decides whether to build a full snapshot
/// (frontend command) or just emit the phase string (startup event).
pub(crate) struct PhaseDecision {
    pub phase: ReviewResumePhase,
    pub round: i64,
    pub message: String,
}

fn parse_round_status(status: &str) -> Option<i64> {
    status
        .strip_prefix("round_")
        .and_then(|s| s.parse::<i64>().ok())
}

/// Pure phase classifier — operates on data the caller has already loaded so
/// it is usable both by the IPC command and by the startup event emitter
/// without re-querying the store inside this function.
pub(crate) fn compute_review_phase(
    job: &Job,
    steps: &[JobStep],
    merge_runs: &[MergeRunInfo],
) -> PhaseDecision {
    let total_rounds = job.num_rounds.max(1);
    let status = job.status.as_str();

    // Latest completed merge round (status='completed'), if any.
    let latest_completed_merge = merge_runs
        .iter()
        .filter(|m| m.status == "completed")
        .map(|m| m.round_number)
        .max();

    // 1. Final: every requested round's merge is completed.
    if let Some(latest) = latest_completed_merge {
        if latest >= total_rounds {
            return PhaseDecision {
                phase: ReviewResumePhase::Final,
                round: latest as i64,
                message: "Final plan ready".to_string(),
            };
        }
    }

    // Identify the "current" round from status. For `interrupted` jobs we
    // pick the highest round that has any rows in either `job_steps` or
    // `merge_runs`; fall back to `current_round` or 1.
    let round_from_status = parse_round_status(status);
    let highest_step_round = steps.iter().map(|s| s.round_number).max();
    let highest_merge_round = merge_runs.iter().map(|m| m.round_number).max();
    let current_round = round_from_status
        .or(highest_step_round.max(highest_merge_round))
        .or(if job.current_round > 0 {
            Some(job.current_round)
        } else {
            None
        })
        .unwrap_or(1);

    // 2. Between-rounds: a prior round's merge has completed, no further
    //    activity for the next round, and another round remains.
    if let Some(latest) = latest_completed_merge {
        if latest < total_rounds {
            let next = latest + 1;
            let steps_for_next = steps.iter().any(|s| s.round_number == next);
            let merge_for_next = merge_runs.iter().any(|m| m.round_number == next);
            if !steps_for_next && !merge_for_next {
                return PhaseDecision {
                    phase: ReviewResumePhase::BetweenRounds,
                    round: next,
                    message: format!("Round {} of {} ready to dispatch", next, total_rounds),
                };
            }
        }
    }

    // 3. Merge: round's steps are all completed and the merge_run for that
    //    round is `running`/`interrupted`/missing (i.e. merge phase hit).
    let steps_for_round: Vec<&JobStep> = steps
        .iter()
        .filter(|s| s.round_number == current_round)
        .collect();
    let all_steps_completed =
        !steps_for_round.is_empty() && steps_for_round.iter().all(|s| s.status == "completed");
    if all_steps_completed {
        let merge_for_round = merge_runs.iter().find(|m| m.round_number == current_round);
        match merge_for_round {
            Some(m) if m.status == "completed" => {
                // Shouldn't reach here — handled by between_rounds / final
                // above, but be defensive.
            }
            _ => {
                return PhaseDecision {
                    phase: ReviewResumePhase::Merge,
                    round: current_round,
                    message: format!("Merge for round {} was interrupted", current_round),
                };
            }
        }
    }

    // 4. Context: pending status with zero steps.
    if status == "pending" && steps.is_empty() {
        return PhaseDecision {
            phase: ReviewResumePhase::Context,
            round: 1,
            message: "Review interrupted at context gather".to_string(),
        };
    }

    // 5. Default: round in progress.
    PhaseDecision {
        phase: ReviewResumePhase::Round,
        round: current_round,
        message: format!(
            "Round {} of {} was interrupted",
            current_round, total_rounds
        ),
    }
}

/// Compute just enough of a phase decision for the startup `review_interrupted`
/// emit payload — loads the job's steps + merge_runs and runs the classifier.
/// Best-effort: failures collapse to a generic `round` phase so the emit code
/// in `lib.rs` can still raise a notification.
pub async fn derive_phase_for_emit(
    store: &HivemindStore,
    job_id: &str,
) -> (&'static str, i64, i64, String) {
    let job = match store.get_job(job_id).await.ok().flatten() {
        Some(j) => j,
        None => {
            return (
                "round",
                0,
                0,
                "Review interrupted by host restart".to_string(),
            );
        }
    };
    let steps = store.get_job_steps(job_id).await.unwrap_or_default();
    let merge_runs = store
        .list_merge_runs_for_job(job_id)
        .await
        .unwrap_or_default();
    let decision = compute_review_phase(&job, &steps, &merge_runs);
    let total = job.num_rounds;
    (
        phase_to_str(&decision.phase),
        decision.round,
        total,
        decision.message,
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    async fn fresh_store() -> (TempDir, HivemindStore) {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let db_path = tmp.path().join("hivemind.db");
        let store = HivemindStore::new(&db_path)
            .await
            .expect("init hivemind store");
        (tmp, store)
    }

    fn fake_job_for_phase(
        status: &str,
        num_rounds: i64,
        task_id: Option<&str>,
        review_id: Option<&str>,
    ) -> Job {
        Job {
            id: "job-x".into(),
            plan: "plan".into(),
            name: "name".into(),
            stance: "against".into(),
            status: status.into(),
            num_rounds,
            current_round: 0,
            timeout_seconds: 300,
            created_at: "2026-01-01 00:00:00".into(),
            updated_at: "2026-01-01 00:00:00".into(),
            completed_at: None,
            error: None,
            final_output: None,
            total_cost: 0.0,
            total_input_tokens: 0,
            total_output_tokens: 0,
            review_id: review_id.map(|s| s.to_string()),
            hivemind_id: None,
            task_id: task_id.map(|s| s.to_string()),
            project_path: None,
        }
    }

    fn fake_step(round: i64, status: &str, model_id: &str) -> JobStep {
        JobStep {
            id: format!("step-{}-{}", round, model_id),
            job_id: "job-x".into(),
            round_number: round,
            sort_order: 0,
            model_id: model_id.into(),
            provider: "openrouter".into(),
            stance: "against".into(),
            status: status.into(),
            started_at: None,
            completed_at: None,
            input_tokens: None,
            output_tokens: None,
            cost: None,
            output: if status == "completed" {
                Some(format!("{} output", model_id))
            } else {
                None
            },
            error: None,
            duration_ms: None,
            prompt: None,
        }
    }

    fn fake_merge(round: i64, status: &str) -> MergeRunInfo {
        MergeRunInfo {
            id: format!("mr-{}", round),
            job_id: "job-x".into(),
            review_id: Some("hmr-x".into()),
            round_number: round,
            session_id: format!("sess-{}", round),
            model_id: "merge-model".into(),
            provider: "anthropic".into(),
            thinking_level: "high".into(),
            status: status.into(),
            started_at: "2026-01-01 00:00:00".into(),
            completed_at: None,
            failed_at: None,
            error: None,
            output_path: format!("/tmp/merge-r{}.txt", round),
            output_len: 0,
        }
    }

    #[test]
    fn phase_context_pending_with_no_steps() {
        let job = fake_job_for_phase("pending", 2, Some("t1"), Some("hmr-x"));
        let decision = compute_review_phase(&job, &[], &[]);
        assert!(matches!(decision.phase, ReviewResumePhase::Context));
        assert_eq!(decision.round, 1);
    }

    #[test]
    fn phase_round_in_progress() {
        let job = fake_job_for_phase("round_1", 2, Some("t1"), Some("hmr-x"));
        let steps = vec![
            fake_step(1, "completed", "m1"),
            fake_step(1, "running", "m2"),
        ];
        let decision = compute_review_phase(&job, &steps, &[]);
        assert!(matches!(decision.phase, ReviewResumePhase::Round));
        assert_eq!(decision.round, 1);
    }

    #[test]
    fn phase_merge_steps_done_merge_interrupted() {
        let job = fake_job_for_phase("round_1", 2, Some("t1"), Some("hmr-x"));
        let steps = vec![
            fake_step(1, "completed", "m1"),
            fake_step(1, "completed", "m2"),
        ];
        let merges = vec![fake_merge(1, "interrupted")];
        let decision = compute_review_phase(&job, &steps, &merges);
        assert!(matches!(decision.phase, ReviewResumePhase::Merge));
        assert_eq!(decision.round, 1);
    }

    #[test]
    fn phase_between_rounds_merge_done_no_next_round() {
        let job = fake_job_for_phase("round_1", 2, Some("t1"), Some("hmr-x"));
        let steps = vec![fake_step(1, "completed", "m1")];
        let merges = vec![fake_merge(1, "completed")];
        let decision = compute_review_phase(&job, &steps, &merges);
        assert!(matches!(decision.phase, ReviewResumePhase::BetweenRounds));
        assert_eq!(decision.round, 2);
    }

    #[test]
    fn phase_final_when_final_round_merge_completed() {
        let job = fake_job_for_phase("round_2", 2, Some("t1"), Some("hmr-x"));
        let steps = vec![fake_step(2, "completed", "m1")];
        let merges = vec![fake_merge(1, "completed"), fake_merge(2, "completed")];
        let decision = compute_review_phase(&job, &steps, &merges);
        assert!(matches!(decision.phase, ReviewResumePhase::Final));
        assert_eq!(decision.round, 2);
    }

    #[test]
    fn phase_round_handles_interrupted_status() {
        // `interrupted` jobs derive their round from the highest step or
        // merge round seen, not from the status itself.
        let job = fake_job_for_phase("interrupted", 2, Some("t1"), Some("hmr-x"));
        let steps = vec![
            fake_step(1, "completed", "m1"),
            fake_step(1, "completed", "m2"),
        ];
        let merges = vec![fake_merge(1, "interrupted")];
        let decision = compute_review_phase(&job, &steps, &merges);
        assert!(matches!(decision.phase, ReviewResumePhase::Merge));
        assert_eq!(decision.round, 1);
    }

    #[tokio::test]
    async fn derive_phase_for_emit_returns_round_for_unknown_job() {
        let (_tmp, store) = fresh_store().await;
        let (phase, round, total, _msg) = derive_phase_for_emit(&store, "missing-job").await;
        assert_eq!(phase, "round");
        assert_eq!(round, 0);
        assert_eq!(total, 0);
    }

    #[tokio::test]
    async fn derive_phase_for_emit_reads_steps_and_merges() {
        let (_tmp, store) = fresh_store().await;
        // Create a job + a completed step + an interrupted merge run for r1.
        store
            .create_job(
                "phase-job",
                "plan",
                "against",
                2,
                300,
                Some("hmr-phase"),
                None,
                None,
                Some("task-phase"),
                None,
            )
            .await
            .unwrap();
        store
            .create_job_step(
                "step-1",
                "phase-job",
                1,
                0,
                "m1",
                "openrouter",
                "against",
                "prompt",
            )
            .await
            .unwrap();
        store
            .complete_job_step("step-1", "out", 10, 20, 0.0, 100)
            .await
            .unwrap();
        store
            .insert_merge_run(
                "mr-phase",
                "phase-job",
                1,
                "sess-1",
                "merge-model",
                "anthropic",
                "high",
                "/tmp/p1.txt",
            )
            .await
            .unwrap();
        // Simulate the sweep flipping merge_runs to interrupted.
        store.sweep_interrupted_merges().await.unwrap();
        store
            .update_job_status("phase-job", "round_1")
            .await
            .unwrap();

        let (phase, round, total, _msg) = derive_phase_for_emit(&store, "phase-job").await;
        assert_eq!(phase, "merge");
        assert_eq!(round, 1);
        assert_eq!(total, 2);
    }
}
