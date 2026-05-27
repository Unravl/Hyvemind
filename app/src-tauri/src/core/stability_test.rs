//! In-app stability test orchestrator (Tests screen).
//!
//! Drives a Task through the full Hyvemind pipeline (planning → questions →
//! plan → Hivemind review → implementation), then runs programmatic gates
//! and an AI verifier to decide pass/fail. Reuses production code paths
//! (`commands::chat::send_message`, `commands::hivemind::start_review`) so
//! the test actually exercises the real bug surface.
//!
//! See `commands::tests` for the Tauri IPC surface and `app/src/screens/Tests.tsx`
//! for the UI that drives it.

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

/// Handle for an in-flight stability test run, stored on `AppState`
/// so a second invocation can short-circuit and the UI can request
/// cancellation.
#[derive(Debug, Clone)]
pub struct ActiveTestRun {
    pub run_id: String,
    pub cancel_token: CancellationToken,
    pub started_at_ms: u64,
    /// Last `phase` value emitted via `test-progress`. Kept on the active
    /// run so a late subscriber (e.g. the frontend rehydrating after an
    /// app restart) can see the current pipeline position without waiting
    /// for the next event.
    pub last_phase: Option<String>,
    /// Last `status` value emitted via `test-progress`.
    pub last_status: Option<String>,
    /// Last human-readable `message` emitted via `test-progress`.
    pub last_message: Option<String>,
}

/// One programmatic gate result. Mirrors the JSON shape persisted in
/// `~/.hyvemind/test-runs/{run_id}.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateResult {
    pub name: String,
    pub passed: bool,
    pub detail: String,
}

/// Verdict returned by the AI verifier session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifierVerdict {
    pub passed: bool,
    /// `0.0`–`1.0`. Model-reported confidence in the verdict; not used
    /// as a pass/fail input but shown in the UI for context.
    #[serde(default)]
    pub confidence: f64,
    #[serde(default)]
    pub issues: Vec<String>,
    #[serde(default)]
    pub summary: String,
}

/// Persisted record of one test run. Written to
/// `~/.hyvemind/test-runs/{run_id}.json` via the existing
/// `state::store::atomic_write` helper.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestRunRecord {
    pub run_id: String,
    /// One of: `running` · `passed` · `failed` · `cancelled` · `error`.
    pub status: String,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub duration_ms: u64,
    pub task_id: String,
    pub session_id: Option<String>,
    #[serde(default)]
    pub plan_session_id: Option<String>,
    pub hivemind_job_id: Option<String>,
    pub sandbox_dir: String,
    pub total_cost: f64,
    pub gates: Vec<GateResult>,
    pub verdict: Option<VerifierVerdict>,
    pub error: Option<String>,
}

// The full orchestrator implementation lives in
// `core::stability_test::runner` (added in the next pass — this stub
// only declares the persisted types so the rest of the codebase can
// reference them).
pub mod runner;

pub use runner::run_stability_test_inner;
