// Internal name — surfaces as "Tasks" in the UI. See PRODUCT.md §3.
//! Unified session-maintenance background task.
//!
//! Runs every 30s and performs:
//!   1. Idle-eviction: kills idle, non-busy, non-pinned, idle-evictable
//!      sessions older than `IDLE_THRESHOLD`. The killed session is
//!      recorded in the manager's graveyard so the next `send_message`
//!      for that id can lazily respawn with `--continue`.
//!   2. Context-bloat eviction: kills sessions that exceed
//!      `CONTEXT_PERCENT_KILL_THRESHOLD` context usage and graveyards
//!      them for lazy respawn. Sessions that are perpetually busy when
//!      this threshold is hit get `mark_needs_respawn()` and are killed
//!      on the next sweep where they are not busy.
//!   3. Turn-count safety-net eviction: kills sessions that have made
//!      more than `MAX_TURNS_BEFORE_RESPAWN` `send_prompt` calls,
//!      regardless of `get_session_stats` health.
//!   4. `auto_commit_locks` sweep: removes entries with
//!      `Arc::strong_count == 1` while holding the outer
//!      `std::sync::Mutex` (no concurrent clone can interleave).
//!   5. Periodic `pi_pool_stats` log line for observability (always).
//!
//! Hivemind-specific bookkeeping (`merge_capture` stale-entry sweep) used
//! to live here too but has been moved into
//! `crate::hivemind::merge_capture::sweep_idle_captures` (audit 6.2). The
//! lib-level wiring that owns the `MergeCapture` registry is responsible
//! for invoking it on the same cadence; `pi/` no longer depends on
//! anything in `hivemind/`.
//!
//! On eviction, emits a `pi-session-evicted` Tauri event with the session
//! id so the frontend can clean its session-keyed refs. Before emitting
//! `pi-session-evicted` we emit a synthetic `chat-event { event_type:
//! "done" }` so any in-flight frontend handlers complete cleanly.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tauri::Emitter;
use tokio::sync::Mutex as TokioMutex;
use tracing::{debug, info, warn, Instrument};

use crate::pi::manager::PiManager;

/// Set of Arc'd state references the maintenance loop needs. Decoupled
/// from `AppState` so the loop can be unit-tested without constructing a
/// full Tauri state and so we don't need to wrap `AppState` in `Arc`.
///
/// Audit 6.2: this struct no longer references the hivemind merge-capture
/// registry — that sweep was moved to `hivemind/merge_capture.rs` so the
/// `pi/` subtree no longer imports from `hivemind/`.
#[derive(Clone)]
pub struct MaintenanceState {
    pub pi_manager: Arc<PiManager>,
    pub auto_commit_locks: Arc<std::sync::Mutex<HashMap<PathBuf, Arc<TokioMutex<()>>>>>,
}

/// How often the maintenance loop runs.
pub const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(30);

/// A Task-owned session that has been idle (no event, no prompt) for
/// this long is killed and graveyarded.
pub const IDLE_THRESHOLD: Duration = Duration::from_secs(10 * 60);

/// Grace period applied to `last_prompt_sent_at` for idle calculation.
/// Prevents evicting a session waiting for its first model token.
pub const PROMPT_GRACE: Duration = Duration::from_secs(5 * 60);

/// `turn_count` above this triggers a kill+graveyard (safety-net for
/// sessions where `get_session_stats` returns stale or no data).
pub const MAX_TURNS_BEFORE_RESPAWN: u64 = 200;

/// `auto_commit_locks` entry count threshold above which the map is
/// proactively swept (even if the interval timer hasn't fired). Cheap.
pub const AUTO_COMMIT_LOCKS_SWEEP_THRESHOLD: usize = 64;

/// Spawn the maintenance loop. Returns the JoinHandle so the caller may
/// abort it on shutdown, but typically the task lives for the process
/// lifetime.
pub fn spawn_maintenance_loop(
    state: MaintenanceState,
    app_handle: tauri::AppHandle,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(
        async move {
            let mut ticker = tokio::time::interval(MAINTENANCE_INTERVAL);
            // Skip the immediate first tick.
            ticker.tick().await;
            loop {
                ticker.tick().await;
                let started = Instant::now();
                run_one_sweep(&state, &app_handle).await;
                debug!(
                    elapsed_ms = started.elapsed().as_millis(),
                    "pi maintenance sweep complete"
                );
            }
        }
        .instrument(tracing::Span::current()),
    )
}

/// Single pass of the maintenance loop. Public for testing.
pub async fn run_one_sweep(state: &MaintenanceState, app_handle: &tauri::AppHandle) {
    let pi_manager = Arc::clone(&state.pi_manager);

    // ── 1+2+3. Session-level eviction ──
    let sessions = pi_manager.list_sessions().await;
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    let mut evicted: Vec<String> = Vec::new();

    for (id, session) in sessions.iter() {
        if session.is_pinned() {
            continue;
        }
        if !session.is_alive() {
            // Dead sessions get cleaned up via kill_session — they're
            // already not contributing to the process pool.
            continue;
        }
        if !session.owner().is_idle_evictable() {
            // Review / Merge / Swarm sessions managed by their engines.
            continue;
        }

        // Compute effective last-activity timestamp.
        let last_event = session.last_activity_ms();
        let last_prompt = session.last_prompt_sent_ms();
        let effective = last_event.max(last_prompt.saturating_add(PROMPT_GRACE.as_millis() as u64));
        let idle_ms = now_ms.saturating_sub(effective);

        let context_pct_kill = session.needs_respawn();
        let turn_count_kill = session.turn_count() > MAX_TURNS_BEFORE_RESPAWN;
        let idle_kill = idle_ms >= IDLE_THRESHOLD.as_millis() as u64;

        // Sessions mid-stream are not eligible for context_pct/turn kills
        // (we already marked `needs_respawn` when they become idle).
        let busy = session.is_busy();

        let should_evict = if busy {
            // Mark for deferred respawn if context bloat detected mid-stream.
            if turn_count_kill {
                session.mark_needs_respawn();
            }
            false
        } else {
            idle_kill || context_pct_kill || turn_count_kill
        };

        if should_evict {
            evicted.push(id.clone());
        }
    }

    for id in evicted {
        // Emit synthetic done before eviction so the frontend's in-flight
        // chat-event handler can finalize.
        let _ = app_handle.emit(
            "chat-event",
            serde_json::json!({
                "session_id": id,
                "event_type": "done",
                "content": "",
            }),
        );

        info!(session_id = %id, "pi maintenance: evicting idle/bloated session");
        if let Err(e) = pi_manager.kill_session(&id).await {
            warn!(session_id = %id, error = %e, "eviction kill_session failed");
            continue;
        }

        let _ = app_handle.emit(
            "pi-session-evicted",
            serde_json::json!({
                "session_id": id,
            }),
        );
    }

    // ── 4. auto_commit_locks sweep ──
    sweep_auto_commit_locks(state);

    // ── 5. pi_pool_stats observability log ──
    let stats = pi_manager.list_session_stats().await;
    let graveyard = pi_manager.graveyard_size().await;
    info!(
        target = "pi_pool_stats",
        active_count = stats.len(),
        max = pi_manager.max_processes(),
        available_permits = pi_manager.available_permits(),
        graveyard_size = graveyard,
        sessions = ?stats,
        "pi_pool_stats"
    );

    // Breadcrumb if we exceed 0.75 × max for downstream operators.
    let max = pi_manager.max_processes();
    if max > 0 && stats.len() * 4 > max * 3 {
        warn!(
            active_count = stats.len(),
            max = max,
            "pi pool at high utilization (> 0.75 × max)"
        );
    }
}

fn sweep_auto_commit_locks(state: &MaintenanceState) {
    let mut locks = match state.auto_commit_locks.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    let pre = locks.len();
    if pre < AUTO_COMMIT_LOCKS_SWEEP_THRESHOLD {
        // Below the watermark — skip the strong_count scan to avoid
        // pointless work on the cold path.
        return;
    }
    locks.retain(|_, m| Arc::strong_count(m) > 1);
    let post = locks.len();
    if pre != post {
        debug!(pre, post, "auto_commit_locks swept");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thresholds_are_reasonable() {
        assert!(IDLE_THRESHOLD >= Duration::from_secs(60));
        assert!(PROMPT_GRACE >= Duration::from_secs(60));
        assert!(MAX_TURNS_BEFORE_RESPAWN >= 50);
    }
}
