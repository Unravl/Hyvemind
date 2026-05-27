//! Tauri-managed root state.
//!
//! # Lock-poison policy
//!
//! Any `std::sync::Mutex::lock()` in this module follows the
//! project-wide standard of `unwrap_or_else(|e| e.into_inner())`.
//! Poisoning means an earlier holder panicked — but the protected
//! data (e.g. `pending_interrupted_emits`, a `Vec` of recovery
//! notifications) is still semantically usable. We prefer to deliver
//! whatever recovery emits we have rather than escalate a single
//! prior panic into an app-wide hard-fail.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::info;

use crate::extensions::{
    register_builtin_extensions, ExtensionContext, ExtensionRegistry, SnapshotEntry, SnapshotStatus,
};
use crate::hivemind::cache::ResponseCache;
use crate::hivemind::merge_capture::MergeCapture;
use crate::hivemind::review_log::ReviewLogger;
use crate::hivemind::store::{HivemindStore, InterruptedJobInfo, MergeRunInfo};
use crate::pi::manager::PiManager;
use crate::providers::ProviderRegistry;
use crate::state::config::Config;
use crate::state::store::SwarmStore;
use crate::state::swarm_registry::SwarmRegistry;
use crate::state::sync::{AsyncMutex, AsyncRwLock, SyncMutex, SyncRwLock};
use crate::state::usage_store::UsageStore;
use tokio_util::sync::CancellationToken;

/// Global application state managed by Tauri.
///
/// All fields are wrapped in `Arc` (and `RwLock` where mutation is needed) so
/// that the struct is cheaply clonable and safe to share across async tasks
/// and Tauri command handlers.
pub struct AppState {
    /// Application configuration (mutable -- settings may be changed at runtime).
    pub config: Arc<AsyncRwLock<Config>>,

    /// File-based persistence layer for swarm state, features, plans and handoffs.
    pub swarm_store: Arc<SwarmStore>,

    /// In-memory registry of currently running swarm tasks.
    pub swarm_registry: Arc<SwarmRegistry>,

    /// Hivemind (code review) data store backed by SQLite.
    pub hivemind_store: Arc<HivemindStore>,

    /// Manager for PI (process isolation) sessions.
    pub pi_manager: Arc<PiManager>,

    /// Directory for persisting chat session files.
    pub chat_sessions_dir: PathBuf,

    /// Directory for persisting task message files.
    pub task_messages_dir: PathBuf,

    /// Usage tracking store (shares SQLite pool with hivemind_store).
    pub usage_store: Arc<UsageStore>,

    /// Directory for per-review JSONL log files.
    pub reviews_dir: PathBuf,

    /// Provider registry for hivemind model dispatch.
    pub provider_registry: Arc<AsyncRwLock<ProviderRegistry>>,

    /// Process-wide LLM response cache shared by every hivemind review and
    /// scout-plan review. Constructed once in [`AppState::new`] so cache hits
    /// actually accumulate across reviews — previously each call site built
    /// its own throwaway cache, driving the effective hit rate to zero.
    pub response_cache: Arc<ResponseCache>,

    /// Active review loggers keyed by review ID.
    pub review_loggers: Arc<AsyncMutex<HashMap<String, Arc<ReviewLogger>>>>,

    /// Cancellation tokens for in-flight review jobs, keyed by job_id.
    /// Inserted by `start_review` before spawning the engine task and removed
    /// when the task completes (Ok or Err). `cancel_review` looks up the token
    /// here to signal cancellation to a running engine.
    pub running_reviews: Arc<AsyncRwLock<HashMap<String, tokio_util::sync::CancellationToken>>>,

    /// Per-directory locks for serializing git operations (auto-commit).
    ///
    /// The outer map is guarded by a `SyncMutex` (touched only synchronously
    /// to insert / look up the per-path entry). The inner `AsyncMutex` is the
    /// one held across the actual `git` invocation, which involves async I/O.
    pub auto_commit_locks: Arc<SyncMutex<HashMap<PathBuf, Arc<AsyncMutex<()>>>>>,

    /// In-flight merge captures keyed by Pi merge `session_id`.
    ///
    /// The chunk-emit hook in `commands/chat.rs` looks up a capture by the
    /// streaming session id and appends each chunk to the capture's file.
    /// Registered by `start_merge_run`, removed by `complete_merge_run`.
    ///
    /// Uses a `SyncRwLock` so the streaming callback in
    /// `send_message` (a sync `FnMut`) can do a fast read-then-write inline
    /// in arrival order — the previous tokio-based version was awaited
    /// inside a `tokio::spawn` per chunk, which let the scheduler reorder
    /// adjacent writes and corrupt the on-disk capture.
    pub merge_capture: Arc<SyncRwLock<HashMap<String, Arc<MergeCapture>>>>,

    /// Push-mode Nurse engine — the sole nurse dispatcher. Attached during
    /// `lib.rs::setup` after `NurseBus` is wired into `PiManager`. Held in
    /// a `OnceLock` so the engine can be late-attached without changing
    /// `AppState::new`'s signature. Callers use `state.nurse_engine()` to
    /// access; `None` only during the very first frames of startup before
    /// `attach_nurse_engine` runs.
    pub nurse: std::sync::OnceLock<Arc<crate::nurse::engine::NurseEngine>>,

    /// Provider-extension registry (Provider Extensions, distinct from
    /// Pi Extensions npm packages).
    pub extension_registry: Arc<AsyncRwLock<ExtensionRegistry>>,

    /// Shared snapshot map keyed by composite `extension_id`.
    pub usage_snapshots: Arc<AsyncRwLock<HashMap<String, SnapshotEntry>>>,

    /// Per-call context passed to extension `fetch()` implementations.
    pub extension_context: Arc<ExtensionContext>,

    /// CancellationToken that drives the current generation of extension
    /// pollers. Replaced (and the old token cancelled) by
    /// `refresh_extension_registry`.
    pub extension_poller_cancel: Arc<AsyncMutex<CancellationToken>>,

    /// Per-extension manual-refresh in-flight locks.
    pub extension_refresh_locks: crate::extensions::poller::RefreshLocks,

    /// Per-extension cooldown markers for manual refresh (5-second floor).
    pub extension_refresh_cooldowns: Arc<AsyncRwLock<HashMap<String, std::time::Instant>>>,

    /// Rows the startup sweep marked as `interrupted` — drained once by
    /// `take_pending_interrupted_emits()` so `lib.rs` can emit
    /// `hivemind-progress` notifications after the frontend has had a
    /// chance to attach listeners. Mixes both merge-run and job-level sweeps
    /// so the consumer can emit a unified `merge_interrupted` /
    /// `review_interrupted` event stream in order.
    pending_interrupted_emits: SyncMutex<Vec<PendingInterruptedEmit>>,

    /// Swarms the startup `reconcile_orphaned_swarms_with_replay` sweep
    /// (audit 2.2) found in an in-flight state on disk and marked
    /// `Interrupted` with a non-empty list of failed-by-interruption
    /// features. Drained once by
    /// [`AppState::take_pending_swarm_reconciled_emits`] so `lib.rs` can
    /// emit a `swarm_reconciled` Tauri event per swarm after the frontend
    /// has attached its listeners. Frontend uses the payload to render a
    /// "Resume" badge.
    pending_swarm_reconciled_emits: SyncMutex<Vec<crate::core::recovery::ReconciledSwarm>>,

    /// Directory for persisted stability test run records
    /// (`~/.hyvemind/test-runs/`).
    pub test_runs_dir: PathBuf,

    /// Root directory for per-run stability test sandboxes
    /// (`~/.hyvemind/test-sandbox/`). Each run scaffolds a fresh
    /// `{run_id}/` subdirectory under here.
    pub test_sandbox_dir: PathBuf,

    /// Handle for the currently running stability test run, if any.
    /// `None` means the screen is idle and a new run may be started.
    /// Set by `commands::tests::run_stability_test`; cleared when the
    /// run finishes (success / failure / cancellation).
    pub active_test_run: Arc<AsyncRwLock<Option<crate::core::stability_test::ActiveTestRun>>>,
}

/// One pending startup event the consumer (`lib.rs`) should fan out as a
/// `hivemind-progress` Tauri event.
#[derive(Debug, Clone)]
pub enum PendingInterruptedEmit {
    Merge(MergeRunInfo),
    Job(InterruptedJobInfo),
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState").finish_non_exhaustive()
    }
}

impl AppState {
    /// Initialise all application subsystems and return the composed state.
    ///
    /// Called once during `tauri::Builder::setup`.
    pub async fn new(_handle: &tauri::AppHandle) -> Result<Self> {
        // 1. Load configuration (disk + env vars)
        let init_start = std::time::Instant::now();
        let config = Config::load().context("failed to load application config")?;
        tracing::debug!(
            elapsed_ms = init_start.elapsed().as_millis(),
            "config loaded"
        );
        let data_dir = config.data_dir.clone();
        info!("data directory: {}", data_dir.display());

        // 2. File-based swarm persistence
        let swarm_store = SwarmStore::new(&data_dir);

        // 2a. Reconcile orphaned swarms from prior sessions.
        //
        // The in-memory registry is empty at startup, so any on-disk swarm
        // still marked `Implementing` is a ghost from a previous session
        // (crash, force-quit, ungraceful exit). `reconcile_orphaned_swarms`
        // rewrites those to `Interrupted` with an explanatory error
        // message so the user can click Resume to continue from where the
        // queen left off. Paused swarms are intentionally left alone so
        // user-issued pauses survive a restart.
        let empty_running: std::collections::HashSet<String> = Default::default();
        let n =
            crate::core::recovery::reconcile_orphaned_swarms(&swarm_store, &empty_running).await;
        if n > 0 {
            tracing::info!(
                count = n,
                "reconciled orphaned 'Implementing' swarms to 'Interrupted' at startup"
            );
        }

        // 2a-bis. Audit 2.2: full per-feature reconciliation via
        // progress_log.jsonl replay. The simpler `reconcile_orphaned_swarms`
        // above only touches the swarm-level status; this pass folds the
        // append-only JSONL progress log over `features.json` and marks
        // any feature still in a mid-flight state (Scouting / Implementing /
        // Reviewing / Validating) as `Failed { interrupted: true,
        // resumable: true }`. The returned `ReconciledSwarm` list is
        // stashed in `pending_swarm_reconciled_emits` for `lib.rs` to fan
        // out as `swarm_reconciled` Tauri events after `app.manage(state)`
        // so the frontend can render a Resume badge per swarm.
        let pending_swarm_reconciled =
            crate::core::recovery::reconcile_orphaned_swarms_with_replay(
                &swarm_store,
                &empty_running,
            )
            .await;
        if !pending_swarm_reconciled.is_empty() {
            let interrupted_feature_total: usize = pending_swarm_reconciled
                .iter()
                .map(|r| r.interrupted_features.len())
                .sum();
            tracing::info!(
                swarm_count = pending_swarm_reconciled.len(),
                interrupted_features = interrupted_feature_total,
                "reconciled in-flight features via progress_log replay at startup"
            );
        }

        // 2b. One-shot migration: any swarm written by an older build with
        // `status = Failed` AND the legacy reconcile sentinel message is
        // upgraded to `Interrupted` so it can be Resumed.
        let migrated =
            crate::core::recovery::migrate_legacy_reconciled_failures(&swarm_store).await;
        if migrated > 0 {
            tracing::info!(
                count = migrated,
                "migrated legacy Failed-by-reconcile swarms to 'Interrupted'"
            );
        }

        // 3. Running-swarm registry
        let swarm_registry = SwarmRegistry::new();

        // 4. Hivemind (SQLite) store
        let hivemind_store = HivemindStore::new(&data_dir.join("hivemind"))
            .await
            .context("failed to initialise hivemind store")?;
        tracing::debug!(
            elapsed_ms = init_start.elapsed().as_millis(),
            "hivemind store initialized"
        );

        // 4a. Usage store (shares pool with hivemind)
        let usage_store = UsageStore::new(hivemind_store.pool().clone());

        // 4b. Chat session persistence directory
        // "chat-sessions" is the historical on-disk name — these are Tasks-view conversations. See PRODUCT.md §3.
        let chat_sessions_dir = data_dir.join("chat-sessions");
        std::fs::create_dir_all(&chat_sessions_dir)
            .with_context(|| format!("failed to create {}", chat_sessions_dir.display()))?;

        // 4c. Task message persistence directory
        let task_messages_dir = data_dir.join("task-messages");
        std::fs::create_dir_all(&task_messages_dir)
            .with_context(|| format!("failed to create {}", task_messages_dir.display()))?;

        // 4d. PI session manager (built before the provider registry so
        //     subscription providers — chatgpt / claude-sub — can be
        //     registered against the live Pi pool).
        let pi_binary_path = config
            .pi_binary_path
            .clone()
            .unwrap_or_else(|| std::path::PathBuf::from("pi"));

        // Build initial environment variables for Pi subprocesses from
        // the config (file + env var overrides).
        let pi_env = config.pi_env_vars();
        info!(
            pi_env_count = pi_env.len(),
            "forwarding API keys to Pi sessions"
        );

        // Resolve the Pi extensions directory. Hyvemind ships its extensions
        // (both the bundled npm packages and any checked-in source files) at
        // `<pi-binary-dir>/pi-extensions/`. `scripts/build-pi.sh` is responsible
        // for assembling that directory; if it's missing, no extensions load.
        //
        // `canonicalize_clean` (not `std::fs::canonicalize`) — on Windows the
        // standard version returns `\\?\C:\...` extended-length paths, and
        // Bun 1.x's module loader segfaults when those are passed as
        // `--extension` CLI args. The dunce-backed clean variant strips the
        // prefix when safe, identical to std on macOS/Linux.
        let extension_dir = pi_binary_path
            .parent()
            .map(|p| p.join("pi-extensions"))
            .filter(|p| p.exists())
            .and_then(|p| crate::commands::util::canonicalize_clean(&p).ok());

        if let Some(ref dir) = extension_dir {
            info!(
                extension_dir = %dir.display(),
                "found Pi extension directory"
            );
        }

        // `HYVEMIND_PI_MAX_PROCESSES` (read once at startup) overrides the
        // persisted `config.max_pi_processes` when present. Lets ops cap pool
        // size without touching settings.json — and survives the case where a
        // stale config still carries the old 30-process default.
        let env_pi_max = std::env::var("HYVEMIND_PI_MAX_PROCESSES")
            .ok()
            .and_then(|s| s.trim().parse::<usize>().ok())
            .filter(|n| *n > 0);
        let effective_pi_max = env_pi_max.unwrap_or(config.max_pi_processes);
        if let Some(n) = env_pi_max {
            info!(
                pi_pool_size = n,
                "HYVEMIND_PI_MAX_PROCESSES overriding config.max_pi_processes"
            );
        }

        let pi_manager = Arc::new(PiManager::new(
            pi_binary_path,
            effective_pi_max,
            pi_env,
            extension_dir,
        ));

        // 4d-bis. Shared LLM response cache (process-wide singleton).
        //
        // Tuned for cross-review reuse: 2000 entries, 12h TTL, 512 KB per
        // cached response body. Previously each `start_review` /
        // `run_scout_hivemind_review` call constructed and dropped its own
        // cache, so hits never crossed review boundaries.
        let response_cache = Arc::new(
            ResponseCache::new(
                crate::tunables::response_cache_size(),
                crate::tunables::response_cache_ttl(),
            )
            .with_max_response_size(512 * 1024),
        );

        // 4e. Provider registry for hivemind. Pass the Pi manager so
        //     subscription providers can route through Pi sessions.
        let mut provider_registry = ProviderRegistry::new();
        provider_registry.refresh_from_config_with_pi(
            &config.provider_keys,
            &config.providers,
            Some(Arc::clone(&pi_manager)),
        );

        // 4f. Reviews log directory
        let reviews_dir = data_dir.join("reviews");
        std::fs::create_dir_all(&reviews_dir)
            .with_context(|| format!("failed to create {}", reviews_dir.display()))?;

        // 4f-bis. Stability test directories (Tests screen).
        let test_runs_dir = data_dir.join("test-runs");
        std::fs::create_dir_all(&test_runs_dir)
            .with_context(|| format!("failed to create {}", test_runs_dir.display()))?;
        let test_sandbox_dir = data_dir.join("test-sandbox");
        std::fs::create_dir_all(&test_sandbox_dir)
            .with_context(|| format!("failed to create {}", test_sandbox_dir.display()))?;

        // 4g. Sweep any merge_runs left in `running` AND any jobs left in
        //     `pending`/`running`/`round_*` from a prior process — they
        //     survived a host-process death and should be flagged
        //     `interrupted` so the UI can offer recovery. The returned rows
        //     are stashed for emit after Tauri's setup hook attaches
        //     `app.manage`.
        let interrupted_merges = hivemind_store
            .sweep_interrupted_merges()
            .await
            .context("failed to sweep interrupted merges at startup")?;
        let interrupted_jobs = hivemind_store
            .sweep_interrupted_jobs()
            .await
            .context("failed to sweep interrupted jobs at startup")?;
        tracing::info!(
            merge_count = interrupted_merges.len(),
            job_count = interrupted_jobs.len(),
            "marked interrupted rows from prior session"
        );
        let mut interrupted: Vec<PendingInterruptedEmit> =
            Vec::with_capacity(interrupted_merges.len() + interrupted_jobs.len());
        for m in interrupted_merges {
            interrupted.push(PendingInterruptedEmit::Merge(m));
        }
        for j in interrupted_jobs {
            interrupted.push(PendingInterruptedEmit::Job(j));
        }

        // 4h. Provider-registry Arc — shared with the Nurse engine (attached
        //     later from `lib.rs::setup`) and with every IPC handler that
        //     dispatches through a provider.
        let provider_registry_arc = Arc::new(AsyncRwLock::new(provider_registry));

        // 4i. Provider-extension scaffolding. Build the context, register
        //     built-ins against the live provider map, seed the snapshot
        //     map with `Loading` entries so the first IPC read includes
        //     every registered extension.
        let config_arc = Arc::new(AsyncRwLock::new(config));
        let extension_http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .context("failed to build provider-extensions reqwest client")?;
        let extension_context = Arc::new(ExtensionContext::new(
            Arc::clone(&config_arc),
            extension_http,
            Arc::clone(&pi_manager),
            data_dir.clone(),
        ));
        let mut extension_registry = ExtensionRegistry::new();
        {
            let cfg = config_arc.read().await;
            register_builtin_extensions(&cfg.providers, &mut extension_registry);
        }
        let mut snapshot_seed: HashMap<String, SnapshotEntry> = HashMap::new();
        {
            let cfg = config_arc.read().await;
            for manifest in extension_registry.manifests() {
                let user_settings = cfg
                    .extension_settings
                    .get(&manifest.id)
                    .cloned()
                    .unwrap_or_default();
                let initial_status = if user_settings.enabled {
                    SnapshotStatus::Loading
                } else {
                    SnapshotStatus::Disabled
                };
                snapshot_seed.insert(
                    manifest.id.clone(),
                    SnapshotEntry {
                        manifest,
                        snapshot: None,
                        last_error: None,
                        last_fetched_at: None,
                        status: initial_status,
                        user_settings,
                    },
                );
            }
        }
        let extension_registry_arc = Arc::new(AsyncRwLock::new(extension_registry));
        let usage_snapshots: Arc<AsyncRwLock<HashMap<String, SnapshotEntry>>> =
            Arc::new(AsyncRwLock::new(snapshot_seed));
        let extension_poller_cancel = Arc::new(AsyncMutex::new(CancellationToken::new()));

        tracing::debug!(
            total_elapsed_ms = init_start.elapsed().as_millis(),
            "AppState initialization complete"
        );

        Ok(Self {
            config: config_arc,
            swarm_store: Arc::new(swarm_store),
            swarm_registry: Arc::new(swarm_registry),
            hivemind_store: Arc::new(hivemind_store),
            pi_manager,
            chat_sessions_dir,
            task_messages_dir,
            usage_store: Arc::new(usage_store),
            provider_registry: provider_registry_arc,
            response_cache,
            reviews_dir,
            review_loggers: Arc::new(AsyncMutex::new(HashMap::new())),
            running_reviews: Arc::new(AsyncRwLock::new(HashMap::new())),
            auto_commit_locks: Arc::new(SyncMutex::new(HashMap::new())),
            merge_capture: Arc::new(SyncRwLock::new(HashMap::new())),
            nurse: std::sync::OnceLock::new(),
            extension_registry: extension_registry_arc,
            usage_snapshots,
            extension_context,
            extension_poller_cancel,
            extension_refresh_locks: Arc::new(AsyncRwLock::new(HashMap::new())),
            extension_refresh_cooldowns: Arc::new(AsyncRwLock::new(HashMap::new())),
            pending_interrupted_emits: SyncMutex::new(interrupted),
            pending_swarm_reconciled_emits: SyncMutex::new(pending_swarm_reconciled),
            test_runs_dir,
            test_sandbox_dir,
            active_test_run: Arc::new(AsyncRwLock::new(None)),
        })
    }

    /// Reconcile the extension registry against the current provider config
    /// and respawn pollers. Mirrors `refresh_provider_registry`. Call this
    /// whenever providers or API keys change.
    ///
    /// Steps:
    /// 1. Cancel the current poller generation.
    /// 2. Read current `config.providers` and `config.extension_settings`.
    /// 3. Build a fresh `ExtensionRegistry` via `register_builtin_extensions`.
    /// 4. Seed `Loading` entries for any new extensions; mark removed
    ///    extensions as `Disabled` (snapshot wiped).
    /// 5. Replace the cancellation token and respawn pollers.
    pub async fn refresh_extension_registry(&self, app_handle: &tauri::AppHandle) {
        // 1. Cancel the existing generation.
        {
            let mut guard = self.extension_poller_cancel.lock().await;
            guard.cancel();
            // Replace with a fresh token for the new generation.
            *guard = CancellationToken::new();
        }

        // 2 + 3. Rebuild registry.
        let (providers, ext_settings) = {
            let cfg = self.config.read().await;
            (cfg.providers.clone(), cfg.extension_settings.clone())
        };
        let mut new_registry = ExtensionRegistry::new();
        register_builtin_extensions(&providers, &mut new_registry);

        let new_manifests = new_registry.manifests();
        let new_ids: std::collections::HashSet<String> =
            new_manifests.iter().map(|m| m.id.clone()).collect();

        // 4. Reconcile snapshot map.
        {
            let mut snaps = self.usage_snapshots.write().await;
            // Mark removed extensions as Disabled (wipe snapshot).
            let removed: Vec<String> = snaps
                .keys()
                .filter(|k| !new_ids.contains(*k))
                .cloned()
                .collect();
            for k in &removed {
                if let Some(entry) = snaps.get_mut(k) {
                    entry.status = SnapshotStatus::Disabled;
                    entry.snapshot = None;
                }
            }
            // Seed new extensions in Loading state.
            for manifest in &new_manifests {
                if snaps.contains_key(&manifest.id) {
                    continue;
                }
                let user_settings = ext_settings.get(&manifest.id).cloned().unwrap_or_default();
                let initial_status = if user_settings.enabled {
                    SnapshotStatus::Loading
                } else {
                    SnapshotStatus::Disabled
                };
                snaps.insert(
                    manifest.id.clone(),
                    SnapshotEntry {
                        manifest: manifest.clone(),
                        snapshot: None,
                        last_error: None,
                        last_fetched_at: None,
                        status: initial_status,
                        user_settings,
                    },
                );
            }
        }

        // Swap in the new registry.
        {
            let mut reg = self.extension_registry.write().await;
            *reg = new_registry;
        }

        // 5. Respawn pollers with the new cancellation token.
        let token = {
            let guard = self.extension_poller_cancel.lock().await;
            guard.clone()
        };
        crate::extensions::poller::spawn_pollers(
            Arc::clone(&self.extension_registry),
            Arc::clone(&self.usage_snapshots),
            Arc::clone(&self.extension_context),
            Arc::clone(&self.extension_refresh_locks),
            token,
            app_handle.clone(),
        )
        .await;
    }

    /// Rebuild the hivemind provider registry from current config.
    ///
    /// Call this after any mutation of `config.providers` or
    /// `config.provider_keys` so that subsequent hivemind/nurse dispatches
    /// see the change without requiring an app restart.
    ///
    /// REMEMBER: any new command that mutates provider config or API keys
    /// (e.g. a future `update_provider` / `delete_provider`) must call
    /// this helper, otherwise hivemind/nurse will keep using the stale
    /// `ProviderRegistry` until the app restarts.
    ///
    /// Lock ordering: this method takes a short read on `self.config`,
    /// drops it, then takes a write on `self.provider_registry`. Callers
    /// must therefore not hold a `self.config.write()` guard across this
    /// call — drop the config guard first (e.g. by ending the `{ ... }`
    /// block that held it).
    ///
    /// Note on contention: the write lock on `provider_registry` will
    /// block until any in-flight read guards drop, and the hivemind
    /// engine + nurse hold a read guard across the entire model call
    /// (bounded by their timeouts). A key-save action mid-review may
    /// therefore wait up to that timeout before completing — acceptable
    /// given that keys are rarely changed mid-flight.
    pub async fn refresh_provider_registry(&self) {
        // Snapshot the small config maps under the read lock and release it
        // before taking the registry write lock. This keeps lock ordering
        // simple (config first, then registry) and avoids holding two locks
        // simultaneously for the entire refresh.
        let (api_keys, providers) = {
            let cfg = self.config.read().await;
            (cfg.provider_keys.clone(), cfg.providers.clone())
        };
        let mut registry = self.provider_registry.write().await;
        registry.refresh_from_config_with_pi(
            &api_keys,
            &providers,
            Some(Arc::clone(&self.pi_manager)),
        );
    }

    /// Drain the list of rows the startup sweep flagged as `interrupted`
    /// (both merge_runs and jobs). Call this exactly once during Tauri's
    /// `setup` hook to emit `hivemind-progress` events to any frontend
    /// listener.
    pub fn take_pending_interrupted_emits(&self) -> Vec<PendingInterruptedEmit> {
        // Lock-poison policy: recover and continue. See module docs.
        let mut guard = self
            .pending_interrupted_emits
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::mem::take(&mut *guard)
    }

    /// Drain the list of swarms the startup `progress_log` replay reconciler
    /// (audit 2.2) marked `Interrupted` with at least one feature failed-by-
    /// interruption. Call exactly once in Tauri's `setup` hook to emit
    /// `swarm_reconciled` events after `app.manage(state)`.
    pub fn take_pending_swarm_reconciled_emits(
        &self,
    ) -> Vec<crate::core::recovery::ReconciledSwarm> {
        // Lock-poison policy: recover and continue. See module docs.
        let mut guard = self
            .pending_swarm_reconciled_emits
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::mem::take(&mut *guard)
    }

    /// Late-attach the push-mode Nurse engine. Called from `lib.rs::setup`
    /// after the engine is constructed; a second call is a silent no-op
    /// (the binding is `OnceLock`).
    pub fn attach_nurse_engine(&self, engine: Arc<crate::nurse::engine::NurseEngine>) {
        let _ = self.nurse.set(engine);
    }

    /// Read-only accessor for the new engine. `None` until
    /// `attach_nurse_engine` has been called (i.e. very early in startup
    /// or in test paths that don't go through `lib.rs::setup`).
    pub fn nurse_engine(&self) -> Option<&Arc<crate::nurse::engine::NurseEngine>> {
        self.nurse.get()
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    /// Simulates: prior process opened a merge_runs row with status='running'
    /// and wrote a partial merge-rN.txt before being killed. On the next
    /// startup, `sweep_interrupted_merges` flips the row to 'interrupted'
    /// while preserving the on-disk partial file. The struct returned
    /// becomes `take_pending_interrupted_emits()` payload for the
    /// `hivemind-progress` notification.
    #[tokio::test]
    async fn test_recovery_emits_for_pre_existing_running_row() {
        let tmp = TempDir::new().expect("tempdir");
        let data_dir = tmp.path();
        let reviews_dir = data_dir.join("reviews");
        let hivemind_dir = data_dir.join("hivemind");
        std::fs::create_dir_all(&reviews_dir).expect("reviews dir");
        std::fs::create_dir_all(&hivemind_dir).expect("hivemind dir");

        // Pre-populate SQLite with a job row + a running merge_run row.
        let store = crate::hivemind::store::HivemindStore::new(&hivemind_dir.join("sqlite.db"))
            .await
            .expect("open store");

        store
            .create_job(
                "job-recover",
                "plan",
                "neutral",
                2,
                300,
                Some("hmr-recover"),
                None,
                None,
                None,
                None,
            )
            .await
            .expect("create job");

        // Pre-write a partial merge file at the canonical location.
        let merge_dir = reviews_dir.join("hmr-recover");
        std::fs::create_dir_all(&merge_dir).expect("merge dir");
        let merge_path = merge_dir.join("merge-r1.txt");
        std::fs::write(&merge_path, b"partial merge text from prior run").expect("write partial");

        store
            .insert_merge_run(
                "mr-recover-1",
                "job-recover",
                1,
                "sess-old",
                "model-x",
                "openrouter",
                "high",
                merge_path.to_str().unwrap(),
            )
            .await
            .expect("insert running merge");

        // Run the sweep — this is what AppState::new does at startup.
        let interrupted = store.sweep_interrupted_merges().await.expect("sweep ok");

        // 1. One row swept, with the right shape for the emit payload.
        assert_eq!(interrupted.len(), 1);
        let info = &interrupted[0];
        assert_eq!(info.job_id, "job-recover");
        assert_eq!(info.review_id.as_deref(), Some("hmr-recover"));
        assert_eq!(info.round_number, 1);
        assert_eq!(info.model_id, "model-x");
        assert_eq!(info.status, "interrupted");

        // 2. The DB row reflects the new status.
        let row = store
            .get_merge_run("job-recover", 1)
            .await
            .expect("get")
            .expect("row exists");
        assert_eq!(row.status, "interrupted");
        assert!(row.failed_at.is_some());

        // 3. The partial file is preserved on disk for the resume path.
        let preserved = std::fs::read_to_string(&merge_path).expect("read partial");
        assert_eq!(preserved, "partial merge text from prior run");

        // 4. Idempotency — second sweep is a no-op.
        let again = store
            .sweep_interrupted_merges()
            .await
            .expect("second sweep");
        assert!(again.is_empty());
    }
}
