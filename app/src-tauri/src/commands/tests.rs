//! IPC commands for the Tests screen (in-app stability test).
//!
//! Surface:
//! - `run_stability_test` \u{2014} kick off a new run (rejects if one is already in flight)
//! - `cancel_test_run` \u{2014} signal the running test's cancellation token
//! - `list_test_runs` \u{2014} read recent persisted records from `~/.hyvemind/test-runs/`
//! - `get_test_run` \u{2014} read one persisted record
//! - `get_stability_test_config` / `set_stability_test_config` \u{2014} Tests screen
//!   keeps its own model selection independent of the app's `default_model`

use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::commands::util::validate_id;
use crate::core::stability_test::{run_stability_test_inner, ActiveTestRun, TestRunRecord};
use crate::state::app_state::AppState;
use crate::state::config::StabilityTestConfig;
use crate::state::ipc_error::IpcError;

/// Response payload for `run_stability_test`. Returns the run id immediately
/// so the frontend can register listeners before the first `test-progress`
/// event arrives.
#[derive(Debug, Clone, Serialize)]
pub struct RunStabilityTestResponse {
    pub run_id: String,
}

#[tracing::instrument(skip(app, state))]
#[tauri::command]
pub async fn run_stability_test(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<RunStabilityTestResponse, IpcError> {
    // Reject if another run is already in flight.
    {
        let guard = state.active_test_run.read().await;
        if guard.is_some() {
            return Err(IpcError::validation("a stability test is already running"));
        }
    }

    let run_id = format!(
        "{}-{}",
        chrono::Utc::now().format("%Y%m%d-%H%M%S"),
        &uuid::Uuid::new_v4().to_string()[..8]
    );
    let cancel_token = tokio_util::sync::CancellationToken::new();
    let started_at_ms = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    {
        let mut guard = state.active_test_run.write().await;
        *guard = Some(ActiveTestRun {
            run_id: run_id.clone(),
            cancel_token: cancel_token.clone(),
            started_at_ms,
            last_phase: None,
            last_status: None,
            last_message: None,
        });
    }

    info!(run_id = %run_id, "stability test starting");

    let app_for_task = app.clone();
    let run_id_for_task = run_id.clone();
    let token_for_task = cancel_token.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(e) =
            run_stability_test_inner(app_for_task, run_id_for_task.clone(), token_for_task).await
        {
            warn!(
                run_id = %run_id_for_task,
                error = %e,
                "stability test inner task returned error"
            );
        }
    });

    Ok(RunStabilityTestResponse { run_id })
}

/// Snapshot of the currently-running stability test, used by the frontend
/// to rehydrate the Active-run panel after an app restart (or in the gap
/// between subscribing and the next `test-progress` event).
#[derive(Debug, Clone, Serialize)]
pub struct ActiveTestRunDto {
    pub run_id: String,
    pub started_at_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_phase: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_message: Option<String>,
}

#[tauri::command]
pub async fn get_active_test_run(
    state: tauri::State<'_, AppState>,
) -> Result<Option<ActiveTestRunDto>, IpcError> {
    let guard = state.active_test_run.read().await;
    Ok(guard.as_ref().map(|r| ActiveTestRunDto {
        run_id: r.run_id.clone(),
        started_at_ms: r.started_at_ms,
        last_phase: r.last_phase.clone(),
        last_status: r.last_status.clone(),
        last_message: r.last_message.clone(),
    }))
}

#[tauri::command]
pub async fn cancel_test_run(state: tauri::State<'_, AppState>) -> Result<bool, IpcError> {
    let guard = state.active_test_run.read().await;
    if let Some(active) = guard.as_ref() {
        active.cancel_token.cancel();
        Ok(true)
    } else {
        Ok(false)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct TestRunSummary {
    pub run_id: String,
    pub status: String,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub duration_ms: u64,
    pub total_cost: f64,
    pub pass_count: usize,
    pub fail_count: usize,
    pub verdict_passed: Option<bool>,
    pub verdict_summary: Option<String>,
    pub error: Option<String>,
}

impl From<&TestRunRecord> for TestRunSummary {
    fn from(r: &TestRunRecord) -> Self {
        let pass_count = r.gates.iter().filter(|g| g.passed).count();
        let fail_count = r.gates.iter().filter(|g| !g.passed).count();
        Self {
            run_id: r.run_id.clone(),
            status: r.status.clone(),
            started_at: r.started_at.clone(),
            completed_at: r.completed_at.clone(),
            duration_ms: r.duration_ms,
            total_cost: r.total_cost,
            pass_count,
            fail_count,
            verdict_passed: r.verdict.as_ref().map(|v| v.passed),
            verdict_summary: r.verdict.as_ref().map(|v| v.summary.clone()),
            error: r.error.clone(),
        }
    }
}

#[tauri::command]
pub async fn list_test_runs(
    state: tauri::State<'_, AppState>,
    limit: Option<u32>,
) -> Result<Vec<TestRunSummary>, IpcError> {
    let dir = state.test_runs_dir.clone();
    let limit = limit.unwrap_or(50).min(500) as usize;
    let summaries = tokio::task::spawn_blocking(move || -> Vec<TestRunSummary> {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => return Vec::new(),
        };
        let mut records: Vec<(SystemTime, TestRunRecord)> = entries
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path()
                    .extension()
                    .and_then(|x| x.to_str())
                    .map(|s| s == "json")
                    .unwrap_or(false)
            })
            .filter_map(|e| {
                let mtime = e.metadata().and_then(|m| m.modified()).ok()?;
                let data = std::fs::read_to_string(e.path()).ok()?;
                let rec: TestRunRecord = serde_json::from_str(&data).ok()?;
                Some((mtime, rec))
            })
            .collect();
        records.sort_by(|a, b| b.0.cmp(&a.0));
        records
            .iter()
            .take(limit)
            .map(|(_, r)| TestRunSummary::from(r))
            .collect()
    })
    .await
    .map_err(|e| IpcError::internal(format!("list_test_runs join failed: {}", e)))?;
    Ok(summaries)
}

#[tauri::command]
pub async fn get_test_run(
    state: tauri::State<'_, AppState>,
    run_id: String,
) -> Result<Option<TestRunRecord>, IpcError> {
    // run_id is `YYYYMMDD-HHMMSS-<8 hex>` (see `run_stability_test` above).
    // That format is a strict subset of validate_id's allowlist (digits,
    // hex, `-`); the shared validator catches `..`, `/`, `\`, leading `.`,
    // null bytes, and anything outside `[A-Za-z0-9_-]` — a superset of the
    // hand-rolled checks that used to live here.
    validate_id(&run_id).map_err(IpcError::validation)?;
    let path = state.test_runs_dir.join(format!("{}.json", run_id));
    if !path.exists() {
        return Ok(None);
    }
    let data = std::fs::read_to_string(&path)
        .map_err(|e| IpcError::internal(e.to_string()).with_id(run_id.clone()))?;
    let rec: TestRunRecord = serde_json::from_str(&data)
        .map_err(|e| IpcError::internal(format!("parse failed: {}", e)).with_id(run_id.clone()))?;
    Ok(Some(rec))
}

/// Frontend-friendly mirror of `StabilityTestConfig`. Mirrors field names
/// 1:1 so the UI form can edit it directly.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StabilityTestConfigDto {
    pub task_model: String,
    pub verifier_model: String,
    #[serde(default)]
    pub hivemind_id: Option<String>,
}

impl From<&StabilityTestConfig> for StabilityTestConfigDto {
    fn from(c: &StabilityTestConfig) -> Self {
        Self {
            task_model: c.task_model.clone(),
            verifier_model: c.verifier_model.clone(),
            hivemind_id: c.hivemind_id.clone(),
        }
    }
}

#[tauri::command]
pub async fn get_stability_test_config(
    state: tauri::State<'_, AppState>,
) -> Result<StabilityTestConfigDto, IpcError> {
    let cfg = state.config.read().await;
    Ok(StabilityTestConfigDto::from(&cfg.stability_test))
}

#[tauri::command]
pub async fn set_stability_test_config(
    state: tauri::State<'_, AppState>,
    config: StabilityTestConfigDto,
) -> Result<StabilityTestConfigDto, IpcError> {
    let hivemind_id = config
        .hivemind_id
        .as_ref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    // hivemind_id flows into the persisted Config and is later used as a
    // lookup / path component by the hivemind subsystem. Reject malformed
    // ids at the IPC boundary so a bad payload can't poison the config.
    if let Some(ref hid) = hivemind_id {
        validate_id(hid).map_err(IpcError::validation)?;
    }
    let (response, data_dir, bytes) = {
        let mut cfg = state.config.write().await;
        cfg.stability_test = StabilityTestConfig {
            task_model: config.task_model.trim().to_string(),
            hivemind_id,
            verifier_model: config.verifier_model.trim().to_string(),
        };
        let bytes = cfg
            .snapshot_to_bytes()
            .map_err(|e| IpcError::internal(format!("serialize config failed: {}", e)))?;
        let response = StabilityTestConfigDto::from(&cfg.stability_test);
        (response, cfg.data_dir.clone(), bytes)
    };
    crate::state::config::Config::write_bytes(data_dir, bytes)
        .await
        .map_err(|e| IpcError::internal(format!("save config failed: {}", e)))?;
    Ok(response)
}
