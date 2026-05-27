//! Tauri IPC handlers for Provider Extensions.
//!
//! All commands return `Result<T, String>` consistent with the rest of
//! the project. Extension IDs are validated against the registry — IDs
//! that don't exist are rejected.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tauri::Emitter;
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::commands::util::validate_extension_id;
use crate::extensions::poller;
use crate::extensions::types::{ExtensionManifest, SnapshotEntry, SnapshotStatus};
use crate::state::app_state::AppState;
use crate::state::ipc_error::IpcError;

/// Manual-refresh cooldown — rapid clicks beyond this floor are rejected.
const REFRESH_COOLDOWN_SECS: u64 = 5;

/// List manifests of all registered extensions.
#[tracing::instrument(skip(state))]
#[tauri::command]
pub async fn list_extensions(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<ExtensionManifest>, IpcError> {
    let reg = state.extension_registry.read().await;
    Ok(reg.manifests())
}

/// Get the current snapshot map (one entry per registered extension).
///
/// Entries are sorted by `manifest.id` for deterministic UI ordering.
#[tracing::instrument(skip(state))]
#[tauri::command]
pub async fn get_usage_snapshots(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<SnapshotEntry>, IpcError> {
    let map = state.usage_snapshots.read().await;
    let mut out: Vec<SnapshotEntry> = map.values().cloned().collect();
    out.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
    Ok(out)
}

/// Immediately fetch one extension's snapshot, bypassing the poller's
/// independent timer. Last-write-wins with the poller; best-effort.
///
/// Enforces a 5-second cooldown per extension and a per-extension
/// in-flight mutex to prevent concurrent fetches with the poller.
#[tracing::instrument(skip(state, app))]
#[tauri::command]
pub async fn refresh_usage_snapshot(
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
    extension_id: String,
) -> Result<SnapshotEntry, IpcError> {
    validate_extension_id(&extension_id).map_err(IpcError::validation)?;
    info!(extension_id = %extension_id, "refresh_usage_snapshot invoked");

    // Look up the extension. We resolve it now so the rest of the
    // function can drop the registry lock before performing I/O.
    let extension = {
        let reg = state.extension_registry.read().await;
        reg.get(&extension_id)
            .ok_or_else(|| IpcError::not_found("extension", extension_id.clone()))?
    };

    // Cooldown check.
    {
        let mut cooldowns = state.extension_refresh_cooldowns.write().await;
        if let Some(last) = cooldowns.get(&extension_id) {
            let elapsed = last.elapsed();
            if elapsed < Duration::from_secs(REFRESH_COOLDOWN_SECS) {
                return Err(IpcError::validation(format!(
                    "manual refresh cooldown active ({:.1}s remaining)",
                    (REFRESH_COOLDOWN_SECS as f64) - elapsed.as_secs_f64()
                ))
                .with_id(extension_id.clone()));
            }
        }
        cooldowns.insert(extension_id.clone(), Instant::now());
    }

    // Per-extension in-flight mutex.
    let lock = {
        let mut locks = state.extension_refresh_locks.write().await;
        Arc::clone(
            locks
                .entry(extension_id.clone())
                .or_insert_with(|| Arc::new(Mutex::new(()))),
        )
    };
    // Wait briefly for the per-extension lock rather than immediately
    // rejecting on contention. The startup-refresh kickoff in
    // `spawn_pollers` holds this lock briefly, so an unlucky user
    // clicking refresh during the first ~1-2 s of launch would
    // otherwise see a spurious "already in flight" error.
    let _guard = match tokio::time::timeout(Duration::from_secs(2), lock.lock()).await {
        Ok(g) => g,
        Err(_) => {
            return Err(
                IpcError::validation("a fetch is already in flight for this extension")
                    .with_id(extension_id.clone()),
            )
        }
    };

    // Resolve the usage capability.
    let usage = match extension.usage_provider() {
        Some(u) => u,
        None => {
            return Err(IpcError::validation(format!(
                "extension {} has no Usage capability",
                extension_id
            ))
            .with_id(extension_id.clone()))
        }
    };

    let manifest = extension.manifest();
    let user_settings = state
        .extension_context
        .extension_settings(&extension_id)
        .await;

    // Run the fetch with a timeout.
    let fetched = tokio::time::timeout(
        Duration::from_secs(poller::FETCH_TIMEOUT_SECS),
        usage.fetch(state.extension_context.as_ref()),
    )
    .await;

    let entry: SnapshotEntry = match fetched {
        Ok(Ok(mut snapshot)) => {
            // Cap raw payload.
            if let Some(raw) = &snapshot.raw {
                if let Ok(bytes) = serde_json::to_vec(raw) {
                    if bytes.len() > poller::RAW_PAYLOAD_CAP_BYTES {
                        warn!(
                            extension_id = %extension_id,
                            original_size = bytes.len(),
                            "manual refresh: raw payload exceeded cap — truncating"
                        );
                        snapshot.raw = None;
                    }
                }
            }
            SnapshotEntry {
                manifest,
                snapshot: Some(snapshot.clone()),
                last_error: None,
                last_fetched_at: Some(snapshot.fetched_at),
                status: SnapshotStatus::Ok,
                user_settings,
            }
        }
        Ok(Err(crate::extensions::types::ExtensionError::Unsupported(msg))) => SnapshotEntry {
            manifest,
            snapshot: None,
            last_error: Some(msg),
            last_fetched_at: Some(chrono::Utc::now().timestamp()),
            status: SnapshotStatus::Unsupported,
            user_settings,
        },
        Ok(Err(err)) => SnapshotEntry {
            manifest,
            snapshot: None,
            last_error: Some(err.user_message()),
            last_fetched_at: Some(chrono::Utc::now().timestamp()),
            status: SnapshotStatus::Error,
            user_settings,
        },
        Err(_elapsed) => SnapshotEntry {
            manifest,
            snapshot: None,
            last_error: Some(format!(
                "manual refresh timed out after {}s",
                poller::FETCH_TIMEOUT_SECS
            )),
            last_fetched_at: Some(chrono::Utc::now().timestamp()),
            status: SnapshotStatus::Error,
            user_settings,
        },
    };

    // Persist into shared map.
    {
        let mut snaps = state.usage_snapshots.write().await;
        snaps.insert(extension_id.clone(), entry.clone());
    }

    // Emit event (no raw to keep IPC payload small).
    let snapshot_payload = entry.snapshot.as_ref().map(|s| {
        serde_json::json!({
            "extension_id": s.extension_id,
            "provider_id": s.provider_id,
            "fetched_at": s.fetched_at,
            "headline": s.headline,
            "metrics": s.metrics,
        })
    });
    let _ = app.emit(
        "usage-snapshot-updated",
        serde_json::json!({
            "extension_id": extension_id,
            "status": entry.status,
            "snapshot": snapshot_payload,
            "error": entry.last_error,
        }),
    );

    Ok(entry)
}

/// Update per-extension user settings (enabled / show_in_topbar).
///
/// Either field is optional; missing fields are left unchanged.
/// Validates that `extension_id` exists in the registry.
#[tracing::instrument(skip(state, app))]
#[tauri::command]
pub async fn update_extension_settings(
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
    extension_id: String,
    enabled: Option<bool>,
    show_in_topbar: Option<bool>,
    preferences: Option<std::collections::HashMap<String, String>>,
) -> Result<(), IpcError> {
    validate_extension_id(&extension_id).map_err(IpcError::validation)?;
    info!(
        extension_id = %extension_id,
        enabled = ?enabled,
        show_in_topbar = ?show_in_topbar,
        "update_extension_settings invoked"
    );

    // Validate against registry.
    {
        let reg = state.extension_registry.read().await;
        if reg.get(&extension_id).is_none() {
            return Err(IpcError::not_found("extension", extension_id.clone()));
        }
    }

    let mut transitioned_to_disabled = false;
    let new_settings;
    let (data_dir, bytes) = {
        let mut cfg = state.config.write().await;
        let entry = cfg
            .extension_settings
            .entry(extension_id.clone())
            .or_default();
        let was_enabled = entry.enabled;
        if let Some(e) = enabled {
            entry.enabled = e;
        }
        if let Some(s) = show_in_topbar {
            entry.show_in_topbar = s;
        }
        if let Some(p) = preferences {
            entry.preferences = p;
        }
        new_settings = entry.clone();
        if was_enabled && !new_settings.enabled {
            transitioned_to_disabled = true;
        }
        let bytes = cfg
            .snapshot_to_bytes()
            .map_err(|e| IpcError::internal(format!("failed to serialize config: {}", e)))?;
        (cfg.data_dir.clone(), bytes)
    };
    crate::state::config::Config::write_bytes(data_dir, bytes)
        .await
        .map_err(|e| IpcError::internal(format!("failed to save config: {}", e)))?;

    // Reflect into the snapshot map.
    {
        let mut snaps = state.usage_snapshots.write().await;
        if let Some(entry) = snaps.get_mut(&extension_id) {
            entry.user_settings = new_settings.clone();
            if transitioned_to_disabled {
                entry.status = SnapshotStatus::Disabled;
                entry.snapshot = None;
                entry.last_error = None;
            } else if !new_settings.enabled {
                // Already disabled — keep it Disabled.
                entry.status = SnapshotStatus::Disabled;
                entry.snapshot = None;
            } else if matches!(entry.status, SnapshotStatus::Disabled) {
                // Re-enabling — flip back to Loading so the poller
                // will refresh on its next tick.
                entry.status = SnapshotStatus::Loading;
            }
        }
    }

    if transitioned_to_disabled {
        let _ = app.emit(
            "usage-snapshot-updated",
            serde_json::json!({
                "extension_id": extension_id,
                "status": SnapshotStatus::Disabled,
                "snapshot": serde_json::Value::Null,
                "error": serde_json::Value::Null,
            }),
        );
    } else {
        // Emit a state-change event so the UI updates immediately.
        let status = {
            let snaps = state.usage_snapshots.read().await;
            snaps
                .get(&extension_id)
                .map(|e| e.status.clone())
                .unwrap_or(SnapshotStatus::Loading)
        };
        let _ = app.emit(
            "usage-snapshot-updated",
            serde_json::json!({
                "extension_id": extension_id,
                "status": status,
                "snapshot": serde_json::Value::Null,
                "error": serde_json::Value::Null,
            }),
        );
    }

    Ok(())
}
