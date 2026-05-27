use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::domain::swarm::{SwarmState, SwarmStatus, SwarmUsageAccumulator};
use crate::pi::manager::PiManager;
use crate::state::sync::AsyncMutex;

/// How long `stop()` waits for the queen task to finish after the
/// cancellation token is tripped before aborting the handle and
/// force-killing the swarm's Pi sessions.
pub const STOP_AWAIT_TIMEOUT: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------
// RunningSwarm -- per-swarm runtime handles
// ---------------------------------------------------------------------------

/// Runtime handles for a single running swarm task.
pub struct RunningSwarm {
    /// The spawned task driving swarm execution.
    pub handle: Option<JoinHandle<anyhow::Result<()>>>,
    /// Token used to request graceful cancellation.
    pub cancellation_token: CancellationToken,
    /// Notify used to wake a paused swarm.
    pub pause_token: Arc<Notify>,
    /// Whether the swarm is currently paused.
    pub paused: Arc<AtomicBool>,
    /// When this swarm was registered.
    pub started_at: Instant,
    /// The swarm identifier (duplicated here for logging convenience).
    pub swarm_id: String,
    /// Snapshot of the swarm state at registration time (kept up to date by
    /// the registry for queries that do not need the full store).
    pub state: SwarmState,
}

impl std::fmt::Debug for RunningSwarm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunningSwarm")
            .field("swarm_id", &self.swarm_id)
            .field("paused", &self.paused.load(Ordering::Relaxed))
            .field("started_at", &self.started_at)
            .finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------
// SwarmRegistry -- thread-safe registry of running swarms
// ---------------------------------------------------------------------------

/// Thread-safe registry of currently running swarm tasks.
///
/// Provides pause / resume / stop control and state queries over active
/// swarms.
#[derive(Debug)]
pub struct SwarmRegistry {
    swarms: AsyncMutex<HashMap<String, RunningSwarm>>,
    /// Per-swarm mutexes that serialise `start_swarm` calls.
    /// Acquired at the top of `start_swarm` and released on return.
    /// Prevents concurrent start attempts for the same swarm_id
    /// from spawning duplicate queen tasks.
    start_locks: AsyncMutex<HashMap<String, Arc<AsyncMutex<()>>>>,
    /// In-memory usage accumulators for real-time token tracking.
    /// Each active swarm has one accumulator that agents write to during
    /// live execution. The accumulator feeds into `get_swarm_usage`.
    usage_accumulators: AsyncMutex<HashMap<String, SwarmUsageAccumulator>>,
}

impl SwarmRegistry {
    pub fn new() -> Self {
        Self {
            swarms: AsyncMutex::new(HashMap::new()),
            start_locks: AsyncMutex::new(HashMap::new()),
            usage_accumulators: AsyncMutex::new(HashMap::new()),
        }
    }

    /// Register a new running swarm.
    ///
    /// Accepts the task handle and cancellation token produced by the caller.
    /// Internally creates pause handles.
    pub async fn register(
        &self,
        swarm_id: String,
        state: SwarmState,
        cancel_token: CancellationToken,
    ) {
        let mut swarms = self.swarms.lock().await;
        let entry = RunningSwarm {
            handle: None,
            cancellation_token: cancel_token,
            pause_token: Arc::new(Notify::new()),
            paused: Arc::new(AtomicBool::new(false)),
            started_at: Instant::now(),
            swarm_id: swarm_id.clone(),
            state,
        };
        swarms.insert(swarm_id.clone(), entry);
        info!("registered swarm '{}'", swarm_id);
    }

    /// Attach (or replace) the queen `JoinHandle` for a swarm that has
    /// already been registered with `register`.
    ///
    /// Returns an error if the swarm is not present in the registry.
    /// Used by `start_swarm`, which needs to register before spawning so the
    /// queen can fetch pause handles, then wire the spawned task's
    /// `JoinHandle` back into the registry so `stop()` can await it cleanly.
    /// If a handle was previously attached it is dropped (detached); callers
    /// should ensure they're not clobbering an in-flight task.
    pub async fn set_handle(
        &self,
        swarm_id: &str,
        handle: JoinHandle<anyhow::Result<()>>,
    ) -> Result<()> {
        let mut swarms = self.swarms.lock().await;
        let entry = swarms
            .get_mut(swarm_id)
            .ok_or_else(|| anyhow!("swarm '{}' is not registered", swarm_id))?;
        if entry.handle.is_some() {
            warn!(
                swarm_id = %swarm_id,
                "set_handle: replacing existing JoinHandle (the previous handle will be detached)"
            );
        }
        entry.handle = Some(handle);
        info!(swarm_id = %swarm_id, "attached queen JoinHandle to registry entry");
        Ok(())
    }

    /// Return an `Arc<Mutex<()>>` for the given swarm_id. The mutex is created
    /// on first access and cached. Only one `start_swarm` call per swarm_id
    /// can hold the lock at a time; the lock is released when the `MutexGuard`
    /// is dropped (when `start_swarm` returns).
    pub async fn get_start_lock(&self, swarm_id: &str) -> Arc<AsyncMutex<()>> {
        let mut locks = self.start_locks.lock().await;
        locks
            .entry(swarm_id.to_string())
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone()
    }

    /// Return a snapshot of the in-memory swarm state.
    pub async fn get_state(&self, swarm_id: &str) -> Option<SwarmState> {
        let swarms = self.swarms.lock().await;
        swarms.get(swarm_id).map(|e| e.state.clone())
    }

    /// Replace the in-memory state for a registered swarm.
    ///
    /// Used after a non-running edit persists to disk so subsequent
    /// `list_swarms` / `get_state` calls see the updated values instead
    /// of a stale registry snapshot. Returns an error if the swarm is not
    /// registered — callers should treat that as "not running, disk write
    /// is sufficient".
    pub async fn replace_state(&self, swarm_id: &str, new_state: SwarmState) -> Result<()> {
        let mut swarms = self.swarms.lock().await;
        let entry = swarms
            .get_mut(swarm_id)
            .ok_or_else(|| anyhow!("swarm '{}' is not registered", swarm_id))?;
        entry.state = new_state;
        Ok(())
    }

    /// Update the status of a registered swarm.
    pub async fn update_status(&self, swarm_id: &str, status: SwarmStatus) {
        let mut swarms = self.swarms.lock().await;
        if let Some(entry) = swarms.get_mut(swarm_id) {
            entry.state.set_status(status);
        }
    }

    /// Mark a registered swarm as `Failed` and record an error message.
    ///
    /// Mirrors [`update_status`] but uses [`SwarmState::set_error`] so the
    /// in-memory state carries both `status = Failed` and the error string
    /// before the next `get_state` / `write_state` cycle. The swarm-fail
    /// join handler in `commands/swarms.rs` calls this so the on-disk
    /// `state.error` actually reflects what went wrong (instead of `null`).
    pub async fn set_error(&self, swarm_id: &str, error: String) {
        let mut swarms = self.swarms.lock().await;
        if let Some(entry) = swarms.get_mut(swarm_id) {
            entry.state.set_error(error);
        }
    }

    /// Pause a running swarm.
    ///
    /// Sets the `paused` flag. The swarm task is expected to check this flag
    /// at safe yield points and await the `pause_token` notification before
    /// continuing.
    pub async fn pause(&self, swarm_id: &str) -> Result<()> {
        let mut swarms = self.swarms.lock().await;
        let entry = swarms
            .get_mut(swarm_id)
            .ok_or_else(|| anyhow!("swarm '{}' is not running", swarm_id))?;

        if entry.paused.load(Ordering::Relaxed) {
            warn!("swarm '{}' is already paused", swarm_id);
            return Ok(());
        }

        entry.paused.store(true, Ordering::Relaxed);
        entry.state.set_status(SwarmStatus::Paused);
        info!("paused swarm '{}'", swarm_id);
        Ok(())
    }

    /// Resume a paused swarm.
    ///
    /// Clears the `paused` flag and notifies the pause token so the swarm
    /// task wakes up.
    pub async fn resume(&self, swarm_id: &str) -> Result<()> {
        let mut swarms = self.swarms.lock().await;
        let entry = swarms
            .get_mut(swarm_id)
            .ok_or_else(|| anyhow!("swarm '{}' is not running", swarm_id))?;

        if !entry.paused.load(Ordering::Relaxed) {
            warn!("swarm '{}' is not paused", swarm_id);
            return Ok(());
        }

        entry.paused.store(false, Ordering::Relaxed);
        entry.state.set_status(SwarmStatus::Implementing);
        entry.pause_token.notify_one();
        info!("resumed swarm '{}'", swarm_id);
        Ok(())
    }

    /// Stop a running swarm by cancelling its token and awaiting the task.
    ///
    /// Lock/await ordering:
    /// 1. Take the registry lock briefly to `remove()` the entry, then drop it
    ///    so concurrent registry queries don't block on the await below.
    /// 2. Clean up the usage accumulator (separate mutex).
    /// 3. Cancel the swarm's `CancellationToken`.
    /// 4. If the swarm is paused, notify the pause token so the queen wakes
    ///    up and observes the cancellation.
    /// 5. If a `JoinHandle` is attached, `await` it under a bounded
    ///    `STOP_AWAIT_TIMEOUT` (30s). On timeout we log a warning, abort the
    ///    handle, and -- if a `PiManager` was provided -- force-kill every
    ///    Pi session still attributed to the swarm so any LLM call in flight
    ///    can't keep running after `stop()` returns.
    ///
    /// `pi_manager` is optional so unit tests that don't have a real Pi
    /// process pool can still exercise the cancellation path; production
    /// callers should always pass `Some(&state.pi_manager)`.
    pub async fn stop(&self, swarm_id: &str, pi_manager: Option<&PiManager>) -> Result<()> {
        self.stop_with_timeout(swarm_id, pi_manager, STOP_AWAIT_TIMEOUT)
            .await
    }

    /// Like `stop()` but with an explicit timeout. Exposed so callers (and
    /// tests) can supply a shorter wait when 30s is undesirable. Production
    /// code should call `stop()` and inherit `STOP_AWAIT_TIMEOUT`.
    pub async fn stop_with_timeout(
        &self,
        swarm_id: &str,
        pi_manager: Option<&PiManager>,
        await_timeout: Duration,
    ) -> Result<()> {
        let entry = {
            let mut swarms = self.swarms.lock().await;
            swarms
                .remove(swarm_id)
                .ok_or_else(|| anyhow!("swarm '{}' is not running", swarm_id))?
        };

        // Clean up usage accumulator
        self.remove_usage_accumulator(swarm_id).await;

        info!("stopping swarm '{}'", swarm_id);
        entry.cancellation_token.cancel();

        // If paused, wake it so it can observe cancellation.
        if entry.paused.load(Ordering::Relaxed) {
            entry.paused.store(false, Ordering::Relaxed);
            entry.pause_token.notify_one();
        }

        if let Some(handle) = entry.handle {
            // Grab an `abort_handle` so the original `JoinHandle` can be
            // moved into the timeout future while we still retain the
            // ability to forcibly cancel the task on timeout.
            let abort_handle = handle.abort_handle();
            match tokio::time::timeout(await_timeout, handle).await {
                Ok(Ok(Ok(()))) => info!("swarm '{}' stopped cleanly", swarm_id),
                Ok(Ok(Err(e))) => warn!("swarm '{}' stopped with error: {}", swarm_id, e),
                Ok(Err(e)) => warn!("swarm '{}' task panicked: {}", swarm_id, e),
                Err(_elapsed) => {
                    warn!(
                        swarm_id = %swarm_id,
                        timeout_secs = await_timeout.as_secs(),
                        "swarm queen task did not exit within timeout; aborting handle and force-killing Pi sessions"
                    );
                    abort_handle.abort();
                    if let Some(pm) = pi_manager {
                        let killed = pm.kill_sessions_for_swarm(swarm_id).await;
                        info!(
                            swarm_id = %swarm_id,
                            killed,
                            "force-killed Pi sessions after queen await timeout"
                        );
                    } else {
                        warn!(
                            swarm_id = %swarm_id,
                            "no PiManager available; queen task may continue running until its Pi sessions exit on their own"
                        );
                    }
                }
            }
        }

        Ok(())
    }

    /// Check whether a swarm is currently registered and running.
    pub async fn is_running(&self, swarm_id: &str) -> bool {
        let swarms = self.swarms.lock().await;
        swarms.contains_key(swarm_id)
    }

    /// Check whether a swarm has an actively executing queen task.
    ///
    /// `create_swarm` pre-registers swarms in `Planning` so the registry
    /// always has an entry from the moment a swarm exists — so registry
    /// presence alone is not enough to tell "has a queen task been spawned?"
    /// from "swarm just exists." This method returns true only when the
    /// state's status is `Implementing` or `Paused`.
    pub async fn is_active(&self, swarm_id: &str) -> bool {
        let swarms = self.swarms.lock().await;
        match swarms.get(swarm_id) {
            Some(entry) => matches!(
                entry.state.status,
                SwarmStatus::Implementing | SwarmStatus::Paused
            ),
            None => false,
        }
    }

    /// Return (running_count, paused_count) of registered swarms.
    pub async fn counts(&self) -> (usize, usize) {
        let swarms = self.swarms.lock().await;
        let paused = swarms
            .values()
            .filter(|e| e.paused.load(std::sync::atomic::Ordering::Relaxed))
            .count();
        let running = swarms.len() - paused;
        (running, paused)
    }

    /// Return snapshots of all registered swarm states.
    pub async fn list_all(&self) -> Vec<SwarmState> {
        let swarms = self.swarms.lock().await;
        swarms.values().map(|e| e.state.clone()).collect()
    }

    /// Remove a swarm entry without stopping it (e.g. after it completes
    /// on its own). Returns the last-known state if found.
    pub async fn remove(&self, swarm_id: &str) -> Option<SwarmState> {
        let mut swarms = self.swarms.lock().await;
        swarms.remove(swarm_id).map(|e| e.state)
    }

    /// Get the pause token and paused flag for a swarm so the executor can
    /// check / await pause state without holding the registry lock.
    pub async fn get_pause_handles(
        &self,
        swarm_id: &str,
    ) -> Option<(Arc<Notify>, Arc<AtomicBool>)> {
        let swarms = self.swarms.lock().await;
        swarms
            .get(swarm_id)
            .map(|e| (Arc::clone(&e.pause_token), Arc::clone(&e.paused)))
    }

    // ---- Usage Accumulator API ----

    /// Register an in-memory usage accumulator for the given swarm.
    /// The accumulator will be merged with the DB total by `get_swarm_usage`.
    pub async fn register_usage_accumulator(&self, swarm_id: &str) -> SwarmUsageAccumulator {
        let mut accs = self.usage_accumulators.lock().await;
        let acc = SwarmUsageAccumulator::new();
        accs.insert(swarm_id.to_string(), acc.clone());
        acc
    }

    /// Retrieve the usage accumulator for a swarm, if one exists.
    pub async fn get_usage_accumulator(&self, swarm_id: &str) -> Option<SwarmUsageAccumulator> {
        let accs = self.usage_accumulators.lock().await;
        accs.get(swarm_id).cloned()
    }

    /// Remove the usage accumulator for a swarm (called on swarm stop/cleanup).
    pub async fn remove_usage_accumulator(&self, swarm_id: &str) {
        let mut accs = self.usage_accumulators.lock().await;
        accs.remove(swarm_id);
    }
}

impl Default for SwarmRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::swarm::{ModelSettings, SwarmConfig, SwarmState};
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;
    use tokio::time::sleep;

    fn sample_state(name: &str) -> SwarmState {
        let config = SwarmConfig {
            name: name.into(),
            description: "".into(),
            working_directory: "/tmp".into(),
            model_settings: ModelSettings::default(),
            features: vec![],
            milestones: vec![],
        };
        SwarmState::from_config(&config)
    }

    /// Verifies the `register` + `set_handle` pattern used by `start_swarm`:
    /// the swarm is registered first (so the queen can fetch pause handles),
    /// then the spawned handle is attached, and `stop()` awaits it.
    #[tokio::test]
    async fn set_handle_attaches_handle_after_register() {
        let registry = SwarmRegistry::new();
        let state = sample_state("swarm-attach");
        let id = state.id.clone();
        let cancel = CancellationToken::new();

        // Mirror the production sequence: bare register first.
        registry
            .register(id.clone(), state.clone(), cancel.clone())
            .await;

        let done = Arc::new(AtomicBool::new(false));
        let cancel_for_task = cancel.clone();
        let done_for_task = Arc::clone(&done);
        let handle: JoinHandle<anyhow::Result<()>> = tokio::spawn(async move {
            cancel_for_task.cancelled().await;
            sleep(Duration::from_millis(100)).await;
            done_for_task.store(true, Ordering::Relaxed);
            Ok(())
        });
        registry
            .set_handle(&id, handle)
            .await
            .expect("set_handle should succeed for a registered swarm");

        // Now stop must await the freshly-attached handle to completion.
        registry.stop(&id, None).await.expect("stop ok");
        assert!(
            done.load(Ordering::Relaxed),
            "set_handle-attached task was not awaited by stop()"
        );
    }

    /// `set_handle` must error on an unknown swarm_id.
    #[tokio::test]
    async fn set_handle_errors_for_unknown_swarm() {
        let registry = SwarmRegistry::new();
        let handle: JoinHandle<anyhow::Result<()>> = tokio::spawn(async { Ok(()) });
        let err = registry
            .set_handle("nope", handle)
            .await
            .expect_err("set_handle for unknown id must fail");
        assert!(
            err.to_string().contains("not registered"),
            "unexpected error: {err}"
        );
    }

    /// `set_error` must populate both `state.error` and flip status to
    /// `Failed` atomically, so the swarm-fail join handler in
    /// `commands/swarms.rs` can write a non-null error to disk.
    #[tokio::test]
    async fn set_error_records_message_and_flips_to_failed() {
        let registry = SwarmRegistry::new();
        let state = sample_state("swarm-set-error");
        let id = state.id.clone();
        registry
            .register(id.clone(), state, CancellationToken::new())
            .await;

        registry
            .set_error(&id, "dependency cycle detected: a -> b -> a".into())
            .await;

        let after = registry.get_state(&id).await.expect("registered");
        assert_eq!(after.status, SwarmStatus::Failed);
        assert_eq!(
            after.error.as_deref(),
            Some("dependency cycle detected: a -> b -> a")
        );
    }

    /// `set_error` on an unknown swarm_id is a silent no-op (the join handler
    /// races crash-recovery and shouldn't panic if the registry has already
    /// been cleared).
    #[tokio::test]
    async fn set_error_unknown_swarm_is_noop() {
        let registry = SwarmRegistry::new();
        // Must not panic.
        registry.set_error("nope", "anything".into()).await;
    }
}
