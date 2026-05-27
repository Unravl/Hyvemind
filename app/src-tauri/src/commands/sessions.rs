//! Admin / observability commands for the Pi session pool.
//!
//! Exposed to the renderer so the Dashboard panel can list, kill, and
//! reconcile active sessions.

use serde::Serialize;
use std::collections::HashSet;

use crate::commands::util::{validate_id, validate_session_id};
use crate::pi::manager::SessionStatSnapshot;
use crate::pi::session::SessionOwner;
use crate::state::app_state::AppState;
use crate::state::ipc_error::IpcError;

/// One row in the active-session list returned to the frontend.
#[derive(Debug, Clone, Serialize)]
pub struct ActiveSession {
    pub id: String,
    /// Discriminator tag — "task" / "review" / "merge" / "swarm" / "unknown".
    pub owner_kind: String,
    /// Owner key (task_id / job_id / swarm_id) where applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_key: Option<String>,
    pub is_alive: bool,
    pub is_busy: bool,
    pub is_pinned: bool,
    pub event_count: u64,
    pub turn_count: u64,
    pub last_activity_ms: u64,
}

fn owner_kind_and_key(o: &SessionOwner) -> (&'static str, Option<String>) {
    match o {
        SessionOwner::Task { task_id } => ("task", Some(task_id.clone())),
        SessionOwner::Review { job_id } => ("review", Some(job_id.clone())),
        SessionOwner::Merge { job_id, round, .. } => {
            ("merge", Some(format!("{}#{}", job_id, round)))
        }
        SessionOwner::Swarm { swarm_id, role, .. } => {
            ("swarm", Some(format!("{}/{}", swarm_id, role)))
        }
        SessionOwner::Unknown => ("unknown", None),
    }
}

/// List every Pi session currently in the pool, with metadata.
#[tauri::command]
pub async fn list_active_pi_sessions(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<ActiveSession>, IpcError> {
    let mut out = Vec::new();
    for (id, session) in state.pi_manager.list_sessions().await {
        let (kind, key) = owner_kind_and_key(&session.owner());
        out.push(ActiveSession {
            id,
            owner_kind: kind.to_string(),
            owner_key: key,
            is_alive: session.is_alive(),
            is_busy: session.is_busy(),
            is_pinned: session.is_pinned(),
            event_count: session.event_count(),
            turn_count: session.turn_count(),
            last_activity_ms: session.last_activity_ms(),
        });
    }
    Ok(out)
}

/// Kill a specific Pi session by id. Idempotent — returns Ok even if the
/// session is already gone.
#[tauri::command]
pub async fn kill_pi_session(
    state: tauri::State<'_, AppState>,
    session_id: String,
) -> Result<(), IpcError> {
    validate_session_id(&session_id).map_err(IpcError::validation)?;
    match state.pi_manager.kill_session(&session_id).await {
        Ok(()) => Ok(()),
        Err(crate::pi::manager::PiManagerError::SessionNotFound { .. }) => Ok(()),
        Err(e) => Err(IpcError::internal(format!("failed to kill session: {}", e))
            .with_id(session_id.clone())),
    }
}

/// Raw-SIGKILL a Pi subprocess by its session id WITHOUT going through the
/// orderly shutdown path or removing the session from the manager. Used by
/// the `Test Nurse → Process crash` scenario to simulate a real crash so
/// `ProcessHealthDetector` can observe `!is_alive()` on its next slow tick.
/// The orderly `kill_pi_session` IPC removes the session from the engine
/// before the detector has a chance to see it, defeating the test.
///
/// Unix-only. On Windows this returns `internal` — the scenario is a
/// developer-only debugging affordance and Hyvemind is Mac-primary.
#[tauri::command]
pub async fn sigkill_pi_session(
    state: tauri::State<'_, AppState>,
    session_id: String,
) -> Result<(), IpcError> {
    validate_session_id(&session_id).map_err(IpcError::validation)?;
    let session = state
        .pi_manager
        .get_session(&session_id)
        .await
        .ok_or_else(|| IpcError::not_found("pi_session", &session_id))?;
    let pid = session
        .pid()
        .ok_or_else(|| IpcError::internal("session has no captured pid").with_id(&session_id))?;

    #[cfg(unix)]
    {
        let output = std::process::Command::new("kill")
            .arg("-9")
            .arg(pid.to_string())
            .output()
            .map_err(|e| {
                IpcError::internal(format!("failed to invoke kill(1): {}", e)).with_id(&session_id)
            })?;
        if !output.status.success() {
            return Err(IpcError::internal(format!(
                "kill -9 {} exited with status {:?}: {}",
                pid,
                output.status.code(),
                String::from_utf8_lossy(&output.stderr),
            ))
            .with_id(&session_id));
        }
        Ok(())
    }

    #[cfg(not(unix))]
    {
        let _ = pid;
        Err(
            IpcError::internal("sigkill_pi_session is unsupported on this platform (Unix-only)")
                .with_id(&session_id),
        )
    }
}

/// Reconcile the active session set against the frontend's known
/// session ids. After a webview reload the renderer's session-keyed
/// refs are reset, but the backend may still hold orphan sessions for
/// tasks the user can no longer reference. This command kills every
/// Task-owned session whose id is not in `known_ids`, leaving
/// Review / Merge / Swarm sessions untouched (they have their own
/// lifecycle).
#[tauri::command]
pub async fn reconcile_active_sessions(
    state: tauri::State<'_, AppState>,
    known_ids: Vec<String>,
) -> Result<Vec<String>, IpcError> {
    // Each id is checked against live session ids only (HashSet membership),
    // not used as a path component — but we still validate so a malformed
    // payload can't pollute downstream comparisons or logs.
    for id in &known_ids {
        validate_id(id).map_err(IpcError::validation)?;
    }
    let known: HashSet<String> = known_ids.into_iter().collect();
    let mut killed = Vec::new();
    for (id, session) in state.pi_manager.list_sessions().await {
        if !session.owner().is_reconcile_evictable() {
            continue;
        }
        if session.is_busy() || session.is_pinned() {
            continue;
        }
        if known.contains(&id) {
            continue;
        }
        if let Err(e) = state.pi_manager.kill_session(&id).await {
            tracing::warn!(session_id = %id, error = %e, "reconcile: kill failed");
            continue;
        }
        killed.push(id);
    }
    if !killed.is_empty() {
        tracing::info!(killed_count = killed.len(), killed = ?killed, "reconcile_active_sessions complete");
    }
    Ok(killed)
}

/// Lightweight stats payload for the Pi pool panel.
#[derive(Debug, Clone, Serialize)]
pub struct PiPoolStats {
    pub active_count: usize,
    pub available_permits: usize,
    pub max_processes: usize,
    pub graveyard_size: usize,
    pub sessions: Vec<SessionStatSnapshot>,
}

#[tauri::command]
pub async fn pi_pool_stats(state: tauri::State<'_, AppState>) -> Result<PiPoolStats, IpcError> {
    let sessions = state.pi_manager.list_session_stats().await;
    Ok(PiPoolStats {
        active_count: sessions.len(),
        available_permits: state.pi_manager.available_permits(),
        max_processes: state.pi_manager.max_processes(),
        graveyard_size: state.pi_manager.graveyard_size().await,
        sessions,
    })
}

/// Live per-session token usage for one running Pi session. Returns `None` when
/// the session is no longer in the pool (already evicted or killed), so the
/// frontend can clear its bottom-bar stats on agent transitions instead of
/// showing stale numbers. Errors from the underlying RPC bubble up as `Err`.
///
/// Used by `SwarmControl`'s `ContextStatusBar` to render the *active agent's*
/// in/out tokens and context%, polled every ~2s while a swarm runs.
#[tauri::command]
pub async fn get_pi_session_stats(
    state: tauri::State<'_, AppState>,
    session_id: String,
) -> Result<Option<crate::pi::events::PiSessionStats>, IpcError> {
    validate_session_id(&session_id).map_err(IpcError::validation)?;
    let Some(session) = state.pi_manager.get_session(&session_id).await else {
        return Ok(None);
    };
    match session.get_session_stats().await {
        Ok(stats) => Ok(Some(stats)),
        Err(e) => Err(IpcError::from_provider_error(e.to_string()).with_id(session_id.clone())),
    }
}
