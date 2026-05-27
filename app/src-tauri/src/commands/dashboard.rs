//! Dashboard IPC surface — live stats, usage breakdowns, cost summaries,
//! and recent activity for the Dashboard view.
//!
//! Every command is read-only and queries the in-memory [`UsageStore`]
//! (backed by SQLite). The time-range commands accept a `"day"` / `"week"` /
//! `"month"` string and translate it into a UTC timestamp boundary.

use serde::Serialize;
use tracing::info;

use crate::state::app_state::AppState;
use crate::state::ipc_error::IpcError;
use crate::state::usage_store::{
    ActivityEntry, CostSummary, ModelUsageSummary, ProviderUsageSummary,
};

/// Live counters for the Dashboard hero strip.
#[derive(Debug, Clone, Serialize)]
pub struct DashboardStats {
    pub active_tasks: usize,
    pub running_swarms: usize,
    pub paused_swarms: usize,
    pub total_reviews: i64,
    pub cost_today: f64,
}

/// Aggregate live counters from all subsystems.
///
/// Queries the Pi session pool for active task count, the swarm registry
/// for running/paused swarm counts, the hivemind SQLite store for total
/// review count, and the usage store for today's cost.
///
/// Returns `Ok(DashboardStats)` on success (individual subsystem errors
/// are gracefully degraded to 0). Never returns an `Err` under normal
/// operation — the only failure mode is a poisoned internal lock, which
/// is a programming error.
#[tauri::command]
pub async fn get_dashboard_stats(
    state: tauri::State<'_, AppState>,
) -> Result<DashboardStats, IpcError> {
    info!("get_dashboard_stats invoked");

    let active_tasks = state.pi_manager.active_count().await;
    let (running, paused) = state.swarm_registry.counts().await;
    let total_reviews = state.hivemind_store.count_jobs().await.unwrap_or(0);
    let cost_summary = state
        .usage_store
        .get_cost_summary()
        .await
        .unwrap_or_default();

    Ok(DashboardStats {
        active_tasks,
        running_swarms: running,
        paused_swarms: paused,
        total_reviews,
        cost_today: cost_summary.today,
    })
}

/// Per-model usage breakdown for a time range.
///
/// Groups stored usage entries by `model_id` and aggregates token
/// counts (input, output, cache read/write) and cost into one
/// [`ModelUsageSummary`] per distinct model.
///
/// `time_range` is a free-form string: `"day"` / `"today"` → midnight
/// UTC today; `"week"` → 7 days ago; `"month"` → 30 days ago. Any
/// other value returns all-time totals.
///
/// Returns `Ok(Vec<ModelUsageSummary>)` on success. Errors bubble up
/// from the underlying SQLite query.
#[tauri::command]
pub async fn get_model_usage(
    state: tauri::State<'_, AppState>,
    time_range: String,
) -> Result<Vec<ModelUsageSummary>, IpcError> {
    info!(time_range = %time_range, "get_model_usage invoked");
    let since = time_range_to_since(&time_range);
    state
        .usage_store
        .get_model_usage(since.as_deref())
        .await
        .map_err(IpcError::from)
}

/// Per-provider usage breakdown for a time range.
///
/// Groups stored usage entries by `provider` (e.g. `"anthropic"`,
/// `"openai"`, `"openrouter"`) and aggregates token counts and cost
/// into one [`ProviderUsageSummary`] per distinct provider.
///
/// Accepts the same `time_range` values as [`get_model_usage`]:
/// `"day"` / `"today"`, `"week"`, `"month"`, or anything else for
/// all-time.
///
/// Returns `Ok(Vec<ProviderUsageSummary>)` on success. Errors bubble
/// up from the underlying SQLite query.
#[tauri::command]
pub async fn get_provider_usage(
    state: tauri::State<'_, AppState>,
    time_range: String,
) -> Result<Vec<ProviderUsageSummary>, IpcError> {
    info!(time_range = %time_range, "get_provider_usage invoked");
    let since = time_range_to_since(&time_range);
    state
        .usage_store
        .get_provider_usage(since.as_deref())
        .await
        .map_err(IpcError::from)
}

/// Aggregate cost summary across all time buckets.
///
/// Returns a [`CostSummary`] with `today`, `week`, and `month` dollar
/// totals computed from the usage store. The frontend renders these in
/// the Dashboard cost-trend panel.
///
/// Returns `Ok(CostSummary)` on success (zeros if no usage recorded).
/// Errors bubble up from the underlying SQLite query.
#[tauri::command]
pub async fn get_cost_summary(state: tauri::State<'_, AppState>) -> Result<CostSummary, IpcError> {
    info!("get_cost_summary invoked");
    state
        .usage_store
        .get_cost_summary()
        .await
        .map_err(IpcError::from)
}

/// Most-recent usage events, ordered by recency.
///
/// Returns up to `limit` [`ActivityEntry`] rows (default 10, hard cap
/// applied by the underlying query). Each entry carries the source
/// (`"chat"`, `"hivemind"`, `"swarm"`), model, token counts, cost,
/// and a UTC timestamp.
///
/// The Dashboard "Recent Activity" feed polls this command.
///
/// Returns `Ok(Vec<ActivityEntry>)` on success. Errors bubble up from
/// the underlying SQLite query.
#[tauri::command]
pub async fn get_recent_activity(
    state: tauri::State<'_, AppState>,
    limit: Option<u32>,
) -> Result<Vec<ActivityEntry>, IpcError> {
    info!("get_recent_activity invoked");
    state
        .usage_store
        .get_recent_activity(limit.unwrap_or(10))
        .await
        .map_err(IpcError::from)
}

fn time_range_to_since(range: &str) -> Option<String> {
    use chrono::{Duration, Utc};
    let now = Utc::now();
    match range {
        "day" | "today" => Some(
            now.date_naive()
                .and_hms_opt(0, 0, 0)
                .unwrap()
                .format("%Y-%m-%d %H:%M:%S")
                .to_string(),
        ),
        "week" => Some(
            (now - Duration::days(7))
                .format("%Y-%m-%d %H:%M:%S")
                .to_string(),
        ),
        "month" => Some(
            (now - Duration::days(30))
                .format("%Y-%m-%d %H:%M:%S")
                .to_string(),
        ),
        _ => None,
    }
}
