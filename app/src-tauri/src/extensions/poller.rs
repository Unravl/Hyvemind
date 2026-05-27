//! Per-extension polling tasks.
//!
//! Each `UsageProvider`-capable extension gets its own `tokio::spawn`
//! task with an independent interval, timeout, and backoff. A shared
//! `CancellationToken` coordinates graceful shutdown and registry
//! refresh.
//!
//! Startup behaviour: `spawn_pollers` performs an explicit
//! **startup-refresh fan-out** — one `tokio::spawn` per extension that
//! calls `perform_fetch_once` immediately — *before* spawning the
//! periodic loops. The periodic loops then sleep one full
//! `interval_secs` before their first tick. This keeps the
//! "fetch immediately on startup" guarantee verifiable in the logs
//! (`startup initial refresh: dispatching/complete`) instead of
//! relying on the implicit `next_sleep = 0` trick.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tauri::Emitter;
use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn, Instrument};

use crate::hivemind::backoff::BackoffCalculator;

use super::context::ExtensionContext;
use super::registry::ExtensionRegistry;
use super::traits::ProviderExtension;
use super::types::{
    ExtensionError, ExtensionUserSettings, SnapshotEntry, SnapshotStatus, UsageSnapshot,
};

/// Minimum allowed `refresh_interval_secs()` — clamps misconfigured
/// extensions to avoid hammering provider APIs.
pub const MIN_REFRESH_INTERVAL_SECS: u64 = 30;

/// Per-fetch timeout. Wraps every `fetch()` call so a hung HTTP
/// request can't block the per-extension task indefinitely.
pub const FETCH_TIMEOUT_SECS: u64 = 30;

/// Cap on the JSON-serialized size of `UsageSnapshot.raw`.
pub const RAW_PAYLOAD_CAP_BYTES: usize = 64 * 1024;

/// Per-extension manual-refresh in-flight mutex map. Keyed by
/// `extension_id`. Used by `refresh_usage_snapshot` IPC and the
/// startup-refresh kickoff to serialise against the poller's own
/// fetches.
pub type RefreshLocks = Arc<RwLock<HashMap<String, Arc<Mutex<()>>>>>;

/// Internal outcome of a single fetch attempt. Used by
/// `run_extension_loop` to decide whether to reset / increment the
/// backoff counter for the next iteration.
#[derive(Debug)]
pub(crate) enum Outcome {
    /// Fetch succeeded — reset backoff, sleep one interval.
    Ok,
    /// Extension is disabled in user settings — sleep one interval and
    /// check again next tick.
    Disabled,
    /// Extension reported `Unsupported` (terminal). The periodic loop
    /// should exit.
    Unsupported,
    /// Transient failure (Auth/Network/Parse/Internal/timeout). The
    /// periodic loop should bump `consecutive_errors` and back off.
    Err,
}

/// Spawn polling infrastructure for every `UsageProvider`-capable
/// extension in the registry.
///
/// Two phases:
///
/// 1. **Startup-refresh fan-out** — one `tokio::spawn` per extension
///    that calls `perform_fetch_once` exactly once. Runs in parallel
///    so a slow provider can't delay app startup. Respects
///    `cancel_token`.
///
/// 2. **Periodic loops** — one `tokio::spawn` per extension running
///    `run_extension_loop`, which sleeps `interval_secs` before its
///    first tick (the kickoff has already done the first fetch).
///
/// Tasks live until the supplied `CancellationToken` is cancelled.
/// In-flight fetches are also cancellable via `tokio::select!`.
pub async fn spawn_pollers(
    registry: Arc<RwLock<ExtensionRegistry>>,
    snapshots: Arc<RwLock<HashMap<String, SnapshotEntry>>>,
    context: Arc<ExtensionContext>,
    refresh_locks: RefreshLocks,
    cancel_token: CancellationToken,
    app_handle: tauri::AppHandle,
) {
    // Snapshot the (id, extension) pairs so the spawn loop doesn't need
    // to hold the read lock across `tokio::spawn`.
    let pairs = {
        let reg = registry.read().await;
        reg.iter_sorted()
    };

    // ── Phase 1: explicit startup-refresh fan-out ─────────────
    //
    // Dispatch one initial fetch per extension. Each task is
    // independent and cancellable. The kickoff acquires the
    // per-extension refresh mutex so it serialises cleanly against
    // any concurrent manual `refresh_usage_snapshot` call.
    let mut kickoff_count = 0usize;
    for (id, ext) in pairs.iter() {
        if ext.usage_provider().is_none() {
            continue;
        }
        kickoff_count += 1;

        let id = id.clone();
        let ext = Arc::clone(ext);
        let snapshots_k = Arc::clone(&snapshots);
        let context_k = Arc::clone(&context);
        let refresh_locks_k = Arc::clone(&refresh_locks);
        let cancel_token_k = cancel_token.clone();
        let app_handle_k = app_handle.clone();
        let span_id = id.clone();

        tokio::spawn(
            async move {
                debug!(
                    extension_id = %id,
                    "startup initial refresh: dispatching fetch"
                );

                // Bail early if the generation has already been cancelled
                // (e.g. a fast back-to-back `refresh_extension_registry`).
                if cancel_token_k.is_cancelled() {
                    debug!(
                        extension_id = %id,
                        "startup initial refresh: cancelled before lock"
                    );
                    return;
                }

                // Acquire (or create) the per-extension refresh mutex.
                let lock = {
                    let mut locks = refresh_locks_k.write().await;
                    Arc::clone(
                        locks
                            .entry(id.clone())
                            .or_insert_with(|| Arc::new(Mutex::new(()))),
                    )
                };

                // Race the lock acquisition + fetch against cancellation.
                let outcome = tokio::select! {
                    _ = cancel_token_k.cancelled() => {
                        debug!(
                            extension_id = %id,
                            "startup initial refresh: cancelled before fetch"
                        );
                        return;
                    }
                    outcome = async {
                        let _guard = lock.lock().await;
                        perform_fetch_once_with_cancel(
                            &id,
                            &ext,
                            &snapshots_k,
                            &context_k,
                            &app_handle_k,
                            &cancel_token_k,
                        )
                        .await
                    } => outcome,
                };

                debug!(
                    extension_id = %id,
                    outcome = ?outcome,
                    "startup initial refresh: complete"
                );
            }
            .instrument(tracing::info_span!("ext_poller", extension = %span_id)),
        );
    }

    info!(
        spawned_initial = kickoff_count,
        "dispatched startup refresh for provider extensions"
    );

    // ── Phase 2: periodic loops ──────────────────────────────
    let mut spawn_count = 0usize;
    for (id, ext) in pairs {
        if ext.usage_provider().is_none() {
            continue;
        }
        spawn_count += 1;

        let snapshots = Arc::clone(&snapshots);
        let context = Arc::clone(&context);
        let cancel_token = cancel_token.clone();
        let app_handle = app_handle.clone();
        let span_id = id.clone();

        tokio::spawn(
            async move {
                run_extension_loop(id, ext, snapshots, context, cancel_token, app_handle).await;
            }
            .instrument(tracing::info_span!("ext_poller", extension = %span_id)),
        );
    }

    info!(spawned = spawn_count, "spawned provider-extension pollers");
}

/// Body of one extension's poll loop. Sleeps first, then fetches,
/// repeating until the cancellation token fires or the extension
/// reports `Unsupported` (terminal).
///
/// Global override: reads `context.poll_interval_secs()` before each
/// sleep iteration so the user can tune the polling cadence from the
/// Settings UI without restarting pollers. If the global override does
/// not apply (per-extension), the per-extension `refresh_interval_secs()`
/// return value is used directly. The per-extension trait method is
/// retained with `#[allow(dead_code)]` for backward compatibility.
async fn run_extension_loop(
    id: String,
    ext: Arc<dyn ProviderExtension>,
    snapshots: Arc<RwLock<HashMap<String, SnapshotEntry>>>,
    context: Arc<ExtensionContext>,
    cancel_token: CancellationToken,
    app_handle: tauri::AppHandle,
) {
    let manifest = ext.manifest();
    let provider_id = manifest.provider_id.clone();
    let backoff = BackoffCalculator::default();
    let mut consecutive_errors: u32 = 0;

    if ext.usage_provider().is_none() {
        return;
    }

    info!(
        extension_id = %id,
        provider_id = %provider_id,
        "extension poller starting"
    );

    // Initial fetch is dispatched separately by `spawn_pollers`; this
    // loop only handles periodic refreshes, so always sleep one full
    // interval before the first tick. Read the global interval from
    // context so the user's Settings-panel value is picked up.
    let global_interval = context.poll_interval_secs().await;
    let mut next_sleep = Duration::from_secs(global_interval);

    loop {
        // Sleep with cancellation.
        if next_sleep > Duration::ZERO {
            tokio::select! {
                _ = cancel_token.cancelled() => {
                    debug!(extension_id = %id, "poller cancelled during sleep");
                    return;
                }
                _ = tokio::time::sleep(next_sleep) => {}
            }
        }

        // Re-read the global poll interval from context so the user can
        // change it in Settings without restarting pollers. Re-read
        // happens *before* the fetch, so the new interval governs the
        // *next* sleep duration.
        let current_global_interval = context.poll_interval_secs().await;

        // Run the fetch via the shared helper, racing it against
        // shutdown cancellation.
        let outcome = tokio::select! {
            _ = cancel_token.cancelled() => {
                debug!(extension_id = %id, "poller cancelled mid-fetch");
                return;
            }
            o = perform_fetch_once_with_cancel(
                &id, &ext, &snapshots, &context, &app_handle, &cancel_token,
            ) => o,
        };

        match outcome {
            Outcome::Ok | Outcome::Disabled => {
                consecutive_errors = 0;
                next_sleep = Duration::from_secs(current_global_interval);
            }
            Outcome::Unsupported => {
                // Terminal — stop the loop.
                return;
            }
            Outcome::Err => {
                consecutive_errors = consecutive_errors.saturating_add(1);
                // Off-by-one fix: pass the raw consecutive_errors so the
                // first failure gets a real backoff (the previous
                // `saturating_sub(1)` produced a zero-duration sleep on
                // the first error, immediately re-firing the fetch).
                next_sleep = backoff.calculate(consecutive_errors);
                warn!(
                    extension_id = %id,
                    consecutive_errors,
                    next_sleep_secs = next_sleep.as_secs(),
                    "extension fetch failed — backing off"
                );
            }
        }
    }
}

/// Run one fetch iteration for a single extension and write the result
/// into the shared snapshot map. This is the canonical implementation
/// of "fetch one extension once" — shared between the startup-refresh
/// kickoff and the periodic loop.
///
/// Returns an `Outcome` describing what happened, so the caller can
/// decide how to schedule the next attempt (or whether to stop, in
/// the `Unsupported` case).
///
/// Side effects:
///   * Writes a `SnapshotEntry` for `id` into `snapshots`.
///   * Emits a `usage-snapshot-updated` Tauri event.
/// Cancellation-aware HTTP fetch for a single extension's usage data.
/// The provided `cancel_token` is raced against the HTTP fetch so a fast
/// shutdown or registry refresh can abort an in-flight request without
/// waiting for the full `FETCH_TIMEOUT_SECS`.
pub(crate) async fn perform_fetch_once_with_cancel<R: tauri::Runtime>(
    id: &str,
    ext: &Arc<dyn ProviderExtension>,
    snapshots: &Arc<RwLock<HashMap<String, SnapshotEntry>>>,
    context: &Arc<ExtensionContext>,
    app_handle: &tauri::AppHandle<R>,
    cancel_token: &CancellationToken,
) -> Outcome {
    let manifest = ext.manifest();

    let usage = match ext.usage_provider() {
        Some(u) => u,
        None => return Outcome::Unsupported,
    };

    // Re-read user settings each call — a Settings toggle between
    // ticks should take effect on the very next fetch.
    let settings = context.extension_settings(id).await;
    if !settings.enabled {
        write_disabled_entry(id, snapshots, &manifest, &settings, app_handle).await;
        return Outcome::Disabled;
    }

    // NOTE: contention on this single `RwLock<HashMap<…>>` is acceptable
    // while the extension count is small (<10). Once we exceed that, the
    // map should be migrated to `dashmap::DashMap` or per-extension
    // sharding so writers don't block one another.
    // TODO(extensions): consider dashmap / per-shard locking when the
    // built-in extension count exceeds 10.
    //
    // NOTE: `raw` payload size enforcement (RAW_PAYLOAD_CAP_BYTES) is
    // done by serializing once below. If extensions become hot enough
    // for this to matter, they can supply a `raw_len` hint on the
    // snapshot so we can short-circuit the serialization.
    let fetch_fut = usage.fetch(context.as_ref());
    let fetch_result = tokio::select! {
        biased;
        _ = cancel_token.cancelled() => {
            debug!(extension_id = %id, "fetch cancelled by token");
            return Outcome::Err;
        }
        result = tokio::time::timeout(Duration::from_secs(FETCH_TIMEOUT_SECS), fetch_fut) => result,
    };

    match fetch_result {
        Ok(Ok(mut snapshot)) => {
            // Cap raw payload at RAW_PAYLOAD_CAP_BYTES.
            if let Some(raw) = &snapshot.raw {
                if let Ok(bytes) = serde_json::to_vec(raw) {
                    if bytes.len() > RAW_PAYLOAD_CAP_BYTES {
                        warn!(
                            extension_id = %id,
                            original_size = bytes.len(),
                            cap = RAW_PAYLOAD_CAP_BYTES,
                            "raw payload exceeded cap — truncating to None"
                        );
                        snapshot.raw = None;
                    }
                }
            }

            // Emit first — we keep an owned copy of the snapshot for
            // the IPC event, then move the original into the map.
            let emitted_status = SnapshotStatus::Ok;
            let fetched_at = snapshot.fetched_at;
            emit_event(app_handle, id, &emitted_status, Some(&snapshot), None);
            let entry = SnapshotEntry {
                manifest: manifest.clone(),
                // Move (not clone) the snapshot — the only other user
                // was the already-emitted event payload.
                snapshot: Some(snapshot),
                last_error: None,
                last_fetched_at: Some(fetched_at),
                status: SnapshotStatus::Ok,
                user_settings: settings.clone(),
            };
            {
                let mut map = snapshots.write().await;
                map.insert(id.to_string(), entry);
            }
            Outcome::Ok
        }
        Ok(Err(ExtensionError::Unsupported(msg))) => {
            info!(
                extension_id = %id,
                reason = %msg,
                "extension reported Unsupported — poller stopping"
            );
            let entry = SnapshotEntry {
                manifest: manifest.clone(),
                snapshot: None,
                last_error: Some(msg.clone()),
                last_fetched_at: Some(chrono::Utc::now().timestamp()),
                status: SnapshotStatus::Unsupported,
                user_settings: settings.clone(),
            };
            {
                let mut map = snapshots.write().await;
                map.insert(id.to_string(), entry);
            }
            emit_event(
                app_handle,
                id,
                &SnapshotStatus::Unsupported,
                None,
                Some(&msg),
            );
            Outcome::Unsupported
        }
        Ok(Err(err)) => {
            let msg = err.user_message();
            warn!(
                extension_id = %id,
                error = %msg,
                "extension fetch failed"
            );
            let entry = SnapshotEntry {
                manifest: manifest.clone(),
                snapshot: None,
                last_error: Some(msg.clone()),
                last_fetched_at: Some(chrono::Utc::now().timestamp()),
                status: SnapshotStatus::Error,
                user_settings: settings.clone(),
            };
            {
                let mut map = snapshots.write().await;
                map.insert(id.to_string(), entry);
            }
            emit_event(app_handle, id, &SnapshotStatus::Error, None, Some(&msg));
            Outcome::Err
        }
        Err(_elapsed) => {
            let msg = format!("fetch timed out after {}s", FETCH_TIMEOUT_SECS);
            warn!(
                extension_id = %id,
                "extension fetch timed out"
            );
            let entry = SnapshotEntry {
                manifest: manifest.clone(),
                snapshot: None,
                last_error: Some(msg.clone()),
                last_fetched_at: Some(chrono::Utc::now().timestamp()),
                status: SnapshotStatus::Error,
                user_settings: settings.clone(),
            };
            {
                let mut map = snapshots.write().await;
                map.insert(id.to_string(), entry);
            }
            emit_event(app_handle, id, &SnapshotStatus::Error, None, Some(&msg));
            Outcome::Err
        }
    }
}

async fn write_disabled_entry<R: tauri::Runtime>(
    id: &str,
    snapshots: &Arc<RwLock<HashMap<String, SnapshotEntry>>>,
    manifest: &super::types::ExtensionManifest,
    settings: &ExtensionUserSettings,
    app_handle: &tauri::AppHandle<R>,
) {
    let entry = SnapshotEntry {
        manifest: manifest.clone(),
        snapshot: None,
        last_error: None,
        last_fetched_at: None,
        status: SnapshotStatus::Disabled,
        user_settings: settings.clone(),
    };
    {
        let mut map = snapshots.write().await;
        map.insert(id.to_string(), entry);
    }
    emit_event(app_handle, id, &SnapshotStatus::Disabled, None, None);
}

/// Emit a `usage-snapshot-updated` Tauri event. Best-effort — silent
/// discard on closed webview, matching the project pattern.
fn emit_event<R: tauri::Runtime>(
    app_handle: &tauri::AppHandle<R>,
    extension_id: &str,
    status: &SnapshotStatus,
    snapshot: Option<&UsageSnapshot>,
    error: Option<&str>,
) {
    let snapshot_payload = snapshot.map(|s| {
        json!({
            "extension_id": s.extension_id,
            "provider_id": s.provider_id,
            "fetched_at": s.fetched_at,
            "headline": s.headline,
            "metrics": s.metrics,
            // Intentionally omit `raw` to avoid double-serialization
            // over IPC. Frontend can fetch via get_usage_snapshots().
        })
    });
    let payload = json!({
        "extension_id": extension_id,
        "status": status,
        "snapshot": snapshot_payload,
        "error": error,
    });
    if let Err(e) = app_handle.emit("usage-snapshot-updated", payload) {
        warn!(
            extension_id,
            error = ?e,
            "failed to emit usage-snapshot-updated"
        );
    }
}
