use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use super::rpc::{PiRpcClient, PiRpcError, PiSessionOptions, ThinkingLevel};
use super::session::{PiSession, SessionOwner};
use super::transport::PiTransport;
use crate::state::sync::{AsyncMutex, AsyncRwLock};
use crate::tunables;

/// Inputs the transport factory receives so test factories can introspect
/// the request even though they ignore most of it. Production builds use
/// every field; mock factories typically discard everything except the
/// permit (which it must hold).
pub struct TransportSpawnRequest<'a> {
    pub binary_path: &'a Path,
    pub working_dir: &'a Path,
    pub options: &'a PiSessionOptions,
    pub env_vars: &'a HashMap<String, String>,
    pub extension_dir: Option<&'a Path>,
    /// Process-pool permit. Default production factory threads this into
    /// `PiRpcClient::spawn` so it drops with the subprocess (audit 2.9).
    /// Mock factories should also hold the permit — dropping it before
    /// the transport itself would break the pool-bookkeeping invariant
    /// the production path relies on.
    pub process_permit: OwnedSemaphorePermit,
}

/// Async factory that produces a [`PiTransport`] from a spawn request.
/// Boxed because the result is `async` (the production path spawns a
/// subprocess) and the closure itself must be `Send + Sync` so the
/// manager can call it from any task.
pub type TransportFactory = Arc<
    dyn for<'a> Fn(
            TransportSpawnRequest<'a>,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<Arc<dyn PiTransport>, PiRpcError>>
                    + Send
                    + 'a,
            >,
        > + Send
        + Sync,
>;

/// Production factory: spawn a real `PiRpcClient`.
///
/// Extracted into a free function so the unit tests for
/// `PiManager::with_transport_factory` can compare-against the prod path
/// without duplicating the boxing dance.
pub fn default_transport_factory() -> TransportFactory {
    Arc::new(|req: TransportSpawnRequest<'_>| {
        Box::pin(async move {
            let rpc = PiRpcClient::spawn(
                req.binary_path,
                req.working_dir,
                req.options,
                req.env_vars,
                req.extension_dir,
                req.process_permit,
            )
            .await?;
            Ok(Arc::new(rpc) as Arc<dyn PiTransport>)
        })
    })
}

// Default ceiling for concurrent Pi processes lives in
// [`tunables::pi_max_processes`] so power users can override it via the
// `HYVEMIND_PI_MAX_PROCESSES` env var without rebuilding. Default is 30.

/// Time budget for a single session's force-kill during `shutdown_all`.
const SHUTDOWN_PER_SESSION_TIMEOUT: Duration = Duration::from_secs(2);

/// Error type for the Pi manager layer.
#[derive(Debug, thiserror::Error)]
pub enum PiManagerError {
    #[error("pi binary not found at {path}")]
    BinaryNotFound { path: PathBuf },

    #[error("session {session_id} not found")]
    SessionNotFound { session_id: String },

    #[error("session {session_id} already exists")]
    SessionExists { session_id: String },

    #[error(transparent)]
    Rpc(#[from] PiRpcError),
}

/// Entry in the manager's "graveyard" — sessions that were killed by the
/// idle-eviction loop but can be respawned lazily on next use (with
/// `--continue --session {path}`).
#[derive(Debug, Clone)]
pub struct GraveyardEntry {
    pub options: PiSessionOptions,
}

/// Manages a pool of Pi subprocess sessions, enforcing a maximum concurrency
/// limit via a `tokio::sync::Semaphore`.
pub struct PiManager {
    /// Bounds the number of live Pi processes.
    process_semaphore: Arc<Semaphore>,
    /// Active sessions keyed by session id.
    sessions: Arc<AsyncMutex<HashMap<String, Arc<PiSession>>>>,
    /// Path to the Pi binary on disk.
    pi_binary_path: PathBuf,
    /// Maximum number of concurrent processes (informational).
    max_processes: usize,
    /// Environment variables to inject into Pi subprocesses (API keys, etc.).
    env_vars: Arc<AsyncRwLock<HashMap<String, String>>>,
    /// Path to the Pi extensions directory containing custom provider definitions.
    extension_dir: Option<PathBuf>,
    /// Sessions killed by the idle-eviction loop, queued for lazy respawn
    /// on next `send_message`. Keyed by session id.
    graveyard: Arc<AsyncMutex<HashMap<String, GraveyardEntry>>>,
    /// Audit 7.2: factory used to produce the underlying [`PiTransport`].
    /// Defaults to spawning a real Pi subprocess; tests replace this with
    /// a mock factory so the Queen / Scout / Worker / Guard loop can be
    /// exercised without a child process.
    transport_factory: TransportFactory,
    /// Audit 7.2: when `true`, `spawn_session_with_options` skips the
    /// on-disk Pi binary existence check. Set automatically by
    /// `with_transport_factory` and `new_with_mock_factory`. Production
    /// callers leave this `false` so an empty / missing Pi install is
    /// surfaced as `BinaryNotFound` rather than silently producing a
    /// non-functional session.
    skip_binary_check: bool,
    /// Nurse bus — when present, every spawn/kill publishes a lifecycle
    /// event and every constructed `PiSession` is wired up to publish
    /// from `touch_activity` / `set_owner` / `Drop`. `OnceLock` allows
    /// lib.rs to attach the bus after `PiManager` is wrapped in `Arc`
    /// without changing every existing constructor signature.
    nurse_bus: std::sync::OnceLock<Arc<crate::nurse::bus::NurseBus>>,
}

impl PiManager {
    pub fn new(
        pi_binary_path: PathBuf,
        max_processes: usize,
        env_vars: HashMap<String, String>,
        extension_dir: Option<PathBuf>,
    ) -> Self {
        let max = if max_processes == 0 {
            tunables::pi_max_processes()
        } else {
            max_processes
        };

        Self {
            process_semaphore: Arc::new(Semaphore::new(max)),
            sessions: Arc::new(AsyncMutex::new(HashMap::new())),
            pi_binary_path,
            max_processes: max,
            env_vars: Arc::new(AsyncRwLock::new(env_vars)),
            extension_dir,
            graveyard: Arc::new(AsyncMutex::new(HashMap::new())),
            transport_factory: default_transport_factory(),
            skip_binary_check: false,
            nurse_bus: std::sync::OnceLock::new(),
        }
    }

    /// Construct an empty `PiManager` for tests. Does not spawn any Pi
    /// binary and is not usable for real session work — `list_sessions()`
    /// returns empty, `get_session()` returns `None`, and `kill_session()`
    /// returns `SessionNotFound` for every id.
    #[cfg(test)]
    pub fn new_for_tests() -> Self {
        let max = tunables::pi_max_processes();
        Self {
            process_semaphore: Arc::new(Semaphore::new(max)),
            sessions: Arc::new(AsyncMutex::new(HashMap::new())),
            pi_binary_path: PathBuf::from("/nonexistent/pi-test-binary"),
            max_processes: max,
            env_vars: Arc::new(AsyncRwLock::new(HashMap::new())),
            extension_dir: None,
            graveyard: Arc::new(AsyncMutex::new(HashMap::new())),
            transport_factory: default_transport_factory(),
            skip_binary_check: false,
            nurse_bus: std::sync::OnceLock::new(),
        }
    }

    /// Construct a `PiManager` for tests that returns a custom transport
    /// for every `spawn_session_with_options` call. Bypasses the real
    /// binary-existence check so the mock can substitute for a missing
    /// Pi install.
    #[cfg(test)]
    pub fn new_with_mock_factory(factory: TransportFactory) -> Self {
        let max = tunables::pi_max_processes();
        Self {
            process_semaphore: Arc::new(Semaphore::new(max)),
            sessions: Arc::new(AsyncMutex::new(HashMap::new())),
            pi_binary_path: PathBuf::from("/mock-pi"),
            max_processes: max,
            env_vars: Arc::new(AsyncRwLock::new(HashMap::new())),
            extension_dir: None,
            graveyard: Arc::new(AsyncMutex::new(HashMap::new())),
            transport_factory: factory,
            skip_binary_check: true,
            nurse_bus: std::sync::OnceLock::new(),
        }
    }

    /// Attach the Nurse bus. Must be called once at startup, before any
    /// session is spawned, so detectors observe every event from cold
    /// boot. A second call is a no-op (the binding is `OnceLock`).
    pub fn set_nurse_bus(&self, bus: Arc<crate::nurse::bus::NurseBus>) {
        let _ = self.nurse_bus.set(bus);
    }

    /// Read-only accessor used by the engine startup reconciliation.
    pub fn nurse_bus(&self) -> Option<Arc<crate::nurse::bus::NurseBus>> {
        self.nurse_bus.get().cloned()
    }

    /// Spawns a new Pi session with full configuration options.
    ///
    /// Acquires a semaphore permit (blocking if the pool is full), spawns
    /// the subprocess with the provided `PiSessionOptions`, and registers
    /// the session.
    #[tracing::instrument(skip_all, fields(session_id = %session_id))]
    pub async fn spawn_session_with_options(
        &self,
        session_id: &str,
        options: &PiSessionOptions,
        working_dir: &Path,
    ) -> Result<Arc<PiSession>, PiManagerError> {
        tracing::info!(
            available_permits = self.process_semaphore.available_permits(),
            "spawn_session_with_options entered"
        );

        if !self.skip_binary_check && !self.pi_binary_path.exists() {
            return Err(PiManagerError::BinaryNotFound {
                path: self.pi_binary_path.clone(),
            });
        }

        {
            let sessions = self.sessions.lock().await;
            if sessions.contains_key(session_id) {
                return Err(PiManagerError::SessionExists {
                    session_id: session_id.to_string(),
                });
            }
        }

        // If we'd block on the semaphore and there are idle, evictable
        // sessions in the pool, free one before waiting. This avoids a
        // 30-second eviction-loop wait when the user opens a new task
        // with the pool already full of stale sessions.
        if self.process_semaphore.available_permits() == 0 {
            let _ = self.try_evict_one_idle().await;
        }

        tracing::info!(
            available_permits = self.process_semaphore.available_permits(),
            "awaiting process semaphore permit"
        );
        let permit = Arc::clone(&self.process_semaphore)
            .acquire_owned()
            .await
            .map_err(|_| PiRpcError::IoError(std::io::Error::other("process semaphore closed")))?;
        tracing::info!("process semaphore permit acquired");

        let active_count = self.sessions.lock().await.len();
        tracing::debug!(
            session_id = %session_id,
            active_sessions = active_count,
            available_permits = self.process_semaphore.available_permits(),
            "spawning session: permit acquired"
        );

        let env_snapshot = self.env_vars.read().await.clone();
        tracing::info!("calling transport factory");
        // Audit 7.2: spawn through the transport factory so tests can
        // substitute a `MockRpcClient`. Default factory still wraps
        // `PiRpcClient::spawn`. Audit 2.9 invariant still holds — the
        // factory threads `permit` into the transport so the permit
        // drops with the underlying subprocess (or mock).
        let req = TransportSpawnRequest {
            binary_path: &self.pi_binary_path,
            working_dir,
            options,
            env_vars: &env_snapshot,
            extension_dir: self.extension_dir.as_deref(),
            process_permit: permit,
        };
        let transport = (self.transport_factory)(req).await?;

        let session = Arc::new(PiSession::new_with_bus(
            session_id.to_string(),
            transport,
            self.nurse_bus.get().cloned(),
        ));

        {
            let mut sessions = self.sessions.lock().await;
            sessions.insert(session_id.to_string(), Arc::clone(&session));
        }
        // Successful respawn — clear any graveyard entry for this id.
        {
            let mut g = self.graveyard.lock().await;
            g.remove(session_id);
        }

        // Publish SessionSpawned AFTER the session is registered so the
        // engine can `Weak::upgrade()` the session immediately on receive.
        if let Some(bus) = self.nurse_bus.get() {
            // Provider is derived from the model id prefix on a best-effort
            // basis ("anthropic/", "openai/", etc.). `None` is fine — the
            // engine resolves it lazily through `ProviderHealthDetector`.
            let provider = options.model.split_once('/').map(|(p, _)| p.to_string());
            bus.publish_owned(crate::nurse::bus::NurseBusEvent::SessionSpawned {
                session_id: session_id.to_string(),
                provider,
                model_id: Some(options.model.clone()),
                spawned_at: std::time::Instant::now(),
                session: Arc::downgrade(&session),
            });
        }

        tracing::info!(
            session_id = %session_id,
            model = %options.model,
            thinking = %options.thinking_level,
            "spawned pi session"
        );
        Ok(session)
    }

    /// On-demand single-session idle eviction. Used by
    /// `spawn_session_with_options` when the semaphore is exhausted to
    /// proactively reclaim an idle, non-pinned, non-busy, idle-evictable
    /// session before blocking on `acquire_owned`.
    async fn try_evict_one_idle(&self) -> Option<String> {
        let candidate = {
            let sessions = self.sessions.lock().await;
            sessions
                .iter()
                .find(|(_, s)| {
                    !s.is_busy() && !s.is_pinned() && s.owner().is_idle_evictable() && s.is_alive()
                })
                .map(|(k, _)| k.clone())
        };
        if let Some(id) = candidate {
            tracing::info!(session_id = %id, "on-demand eviction: reclaiming idle session");
            if let Err(e) = self.kill_session(&id).await {
                tracing::warn!(
                    session_id = %id,
                    error = %e,
                    "on-demand eviction: kill_session failed"
                );
                return None;
            }
            return Some(id);
        }
        None
    }

    /// Returns a snapshot of all active session IDs and their Arc<PiSession> handles.
    pub async fn list_sessions(&self) -> Vec<(String, Arc<PiSession>)> {
        let sessions = self.sessions.lock().await;
        sessions
            .iter()
            .map(|(k, v)| (k.clone(), Arc::clone(v)))
            .collect()
    }

    /// Returns the session with the given id, if it exists.
    pub async fn get_session(&self, session_id: &str) -> Option<Arc<PiSession>> {
        let sessions = self.sessions.lock().await;
        sessions.get(session_id).cloned()
    }

    /// Removes and shuts down the session with the given id.
    ///
    /// Calls `force_kill` on the session before removing it from the map,
    /// so the child process gets SIGKILL (via `kill_on_drop`) even if
    /// other `Arc<PiSession>` holders exist — those holders see the
    /// session token cancelled and bail.
    pub async fn kill_session(&self, session_id: &str) -> Result<(), PiManagerError> {
        let session = {
            let mut sessions = self.sessions.lock().await;
            sessions
                .remove(session_id)
                .ok_or_else(|| PiManagerError::SessionNotFound {
                    session_id: session_id.to_string(),
                })?
        };

        tracing::info!(session_id = %session_id, "killing pi session");
        tracing::debug!(
            session_id = %session_id,
            is_alive = session.is_alive(),
            event_count = session.event_count(),
            "killing session"
        );

        // Best-effort force_kill: ignore the error path because the
        // process may already be dead. Either way the cancel token is
        // signalled and the rpc.shutdown() best-effort.
        let _ = session.force_kill().await;

        // Publish SessionEnded BEFORE dropping the Arc — `Drop` will see
        // `ended_published == true` from the CAS and silently skip.
        session.publish_session_ended(crate::nurse::bus::SessionEndReason::Killed);
        drop(session);

        Ok(())
    }

    /// Returns the number of active sessions.
    pub async fn active_count(&self) -> usize {
        let sessions = self.sessions.lock().await;
        sessions.len()
    }

    /// Shuts down all active sessions.
    ///
    /// Kills all sessions concurrently, each wrapped in a per-session
    /// timeout, so worst-case shutdown time is bounded by the per-session
    /// timeout (not N × timeout).
    pub async fn shutdown_all(&self) -> Result<(), PiManagerError> {
        // Snapshot session IDs + Arcs under the lock, then release the
        // lock before issuing kills (kill_session re-acquires it).
        let sessions: Vec<(String, Arc<PiSession>)> = {
            let map = self.sessions.lock().await;
            map.iter()
                .map(|(k, v)| (k.clone(), Arc::clone(v)))
                .collect()
        };

        let kills = sessions.into_iter().map(|(id, _session)| {
            let id_for_log = id.clone();
            let sessions_arc = Arc::clone(&self.sessions);
            async move {
                // Remove from map under lock, then force_kill the Arc we
                // pulled. Mirrors `kill_session` but lets us drive all
                // kills concurrently.
                let session_opt = {
                    let mut map = sessions_arc.lock().await;
                    map.remove(&id)
                };
                if let Some(session) = session_opt {
                    if let Err(e) =
                        tokio::time::timeout(SHUTDOWN_PER_SESSION_TIMEOUT, session.force_kill())
                            .await
                    {
                        tracing::warn!(
                            session_id = %id_for_log,
                            elapsed_ms = SHUTDOWN_PER_SESSION_TIMEOUT.as_millis(),
                            error = %e,
                            "force_kill timed out during shutdown_all; relying on kill_on_drop"
                        );
                    }
                    drop(session);
                }
            }
        });

        futures::future::join_all(kills).await;

        tracing::info!("all pi sessions shut down");
        Ok(())
    }

    /// Force-kill every session whose `SessionOwner::Swarm.swarm_id` matches.
    ///
    /// Used by `stop_swarm` so the Pi subprocesses backing Scout/Worker/Guard
    /// die immediately rather than running their current LLM call to
    /// completion. Returns the number of sessions killed.
    pub async fn kill_sessions_for_swarm(&self, swarm_id: &str) -> usize {
        let targets: Vec<(String, Arc<PiSession>)> = {
            let map = self.sessions.lock().await;
            map.iter()
                .filter(|(_, s)| {
                    matches!(s.owner(), SessionOwner::Swarm { swarm_id: ref sid, .. } if sid == swarm_id)
                })
                .map(|(k, v)| (k.clone(), Arc::clone(v)))
                .collect()
        };
        let count = targets.len();
        let kills = targets.into_iter().map(|(id, _)| {
            let sessions_arc = Arc::clone(&self.sessions);
            let id_for_log = id.clone();
            async move {
                let session_opt = {
                    let mut map = sessions_arc.lock().await;
                    map.remove(&id)
                };
                if let Some(session) = session_opt {
                    if let Err(e) = tokio::time::timeout(
                        SHUTDOWN_PER_SESSION_TIMEOUT,
                        session.force_kill(),
                    )
                    .await
                    {
                        tracing::warn!(
                            session_id = %id_for_log,
                            error = %e,
                            "force_kill timed out during kill_sessions_for_swarm; relying on kill_on_drop"
                        );
                    }
                    drop(session);
                }
            }
        });
        futures::future::join_all(kills).await;
        tracing::info!(swarm_id = %swarm_id, killed = count, "killed swarm-owned pi sessions");
        count
    }

    /// Returns the configured maximum number of concurrent processes.
    pub fn max_processes(&self) -> usize {
        self.max_processes
    }

    /// Returns the number of currently-available semaphore permits
    /// (i.e. unused process-pool capacity).
    pub fn available_permits(&self) -> usize {
        self.process_semaphore.available_permits()
    }

    /// Update the environment variables that will be injected into future
    /// Pi subprocesses. Existing sessions are unaffected.
    pub async fn update_env_vars(&self, new_vars: HashMap<String, String>) {
        let mut env = self.env_vars.write().await;
        *env = new_vars;
        tracing::info!(count = env.len(), "pi manager env vars updated");
    }

    // -----------------------------------------------------------------------
    // Graveyard (lazy-respawn book-keeping)
    // -----------------------------------------------------------------------

    /// Look up and remove a graveyard entry for `session_id`.
    pub async fn exhume(&self, session_id: &str) -> Option<GraveyardEntry> {
        let mut g = self.graveyard.lock().await;
        g.remove(session_id)
    }

    /// Returns the number of currently-buried sessions (for observability).
    pub async fn graveyard_size(&self) -> usize {
        self.graveyard.lock().await.len()
    }

    /// Reconcile the in-memory graveyard from on-disk session transcripts.
    ///
    /// After a process crash there is no carry-over: the graveyard map only
    /// lives in this struct's memory, so every prior Pi session looks brand
    /// new to the running manager. The Pi session transcripts under
    /// `<home>/chat-sessions/*.jsonl` still exist on disk, though — each
    /// file's stem is the session id Hyvemind originally minted, and the
    /// first JSONL record is a `session` event that carries Pi's own
    /// `cwd` (working directory) and is followed by a `model_change` event
    /// describing the originally-selected provider/model.
    ///
    /// This function enumerates those transcripts and writes a
    /// `GraveyardEntry` for each one so that the next time someone tries to
    /// `send_message` against a recovered session id, the manager already
    /// knows to respawn Pi with `--continue --session <file>` against the
    /// original working directory. Without this, the user would silently
    /// start a brand-new conversation that ignores the prior transcript.
    ///
    /// Best-effort: returns the count of entries inserted. Individual file
    /// parse failures are logged and skipped rather than aborting the sweep.
    /// Pre-existing graveyard entries (e.g. burials from earlier in this
    /// process lifetime) are never overwritten.
    pub async fn reconcile_graveyard_from_disk(&self, home: &Path) -> usize {
        let chat_sessions = home.join("chat-sessions");
        let entries = match std::fs::read_dir(&chat_sessions) {
            Ok(e) => e,
            Err(e) => {
                tracing::debug!(
                    dir = %chat_sessions.display(),
                    error = %e,
                    "graveyard reconciliation: chat-sessions dir not present; nothing to do"
                );
                return 0;
            }
        };

        let mut inserted = 0usize;
        let mut g = self.graveyard.lock().await;

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let session_id = match path.file_stem().and_then(|s| s.to_str()) {
                Some(s) => s.to_string(),
                None => continue,
            };

            // Skip if already buried (e.g. process started, did some work,
            // then this function is called a second time defensively — keep
            // the existing entry rather than clobbering with a stale read).
            if g.contains_key(&session_id) {
                continue;
            }

            let (_, model, thinking) = parse_session_header(&path);
            let mut options = PiSessionOptions::for_model(&model)
                .with_session_file(path.display().to_string())
                .with_resume();
            if let Some(level) = thinking {
                options = options.with_thinking_level(level);
            }
            // Belt-and-suspenders: keep tool_set at the default so the
            // next user prompt's options (which override the graveyard
            // copy when explicitly supplied) take precedence cleanly.
            options.resume_session = true;

            g.insert(session_id.clone(), GraveyardEntry { options });
            inserted += 1;
        }

        tracing::info!(
            inserted,
            total_buried = g.len(),
            dir = %chat_sessions.display(),
            "graveyard reconciliation complete"
        );
        inserted
    }

    // -----------------------------------------------------------------------
    // Stats / observability
    // -----------------------------------------------------------------------

    /// Lightweight snapshot of every active session for logging/UI.
    pub async fn list_session_stats(&self) -> Vec<SessionStatSnapshot> {
        let sessions = self.sessions.lock().await;
        sessions
            .iter()
            .map(|(id, s)| SessionStatSnapshot {
                id: id.clone(),
                owner: format!("{:?}", s.owner()),
                is_alive: s.is_alive(),
                is_busy: s.is_busy(),
                is_pinned: s.is_pinned(),
                event_count: s.event_count(),
                turn_count: s.turn_count(),
                last_activity_ms: s.last_activity_ms(),
                last_prompt_sent_ms: s.last_prompt_sent_ms(),
            })
            .collect()
    }
}

/// Lightweight snapshot of a session for logging / UI display.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionStatSnapshot {
    pub id: String,
    pub owner: String,
    pub is_alive: bool,
    pub is_busy: bool,
    pub is_pinned: bool,
    pub event_count: u64,
    pub turn_count: u64,
    pub last_activity_ms: u64,
    pub last_prompt_sent_ms: u64,
}

/// Parse the first few lines of a Pi session transcript to recover the
/// working directory, originally-selected model, and (best-effort)
/// thinking level.
///
/// Pi writes a `session` event as the very first line carrying the `cwd`
/// it was launched against, followed by a `model_change` event with the
/// active `provider`/`modelId`, and often a `thinking_level_change` event
/// recording the effective reasoning level. Any of these may be missing
/// on corrupt or truncated files — in which case we fall back to
/// sensible defaults so graveyard reconciliation never panics on
/// malformed input. The next `send_message` overrides any field the user
/// explicitly re-supplies, so a slightly-wrong recovered value is
/// harmless.
fn parse_session_header(path: &Path) -> (PathBuf, String, Option<ThinkingLevel>) {
    use std::io::{BufRead, BufReader};

    let mut working_dir = PathBuf::from(".");
    let mut model = String::from("unknown");
    let mut thinking: Option<ThinkingLevel> = None;

    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "graveyard reconciliation: failed to open session file"
            );
            return (working_dir, model, thinking);
        }
    };
    let reader = BufReader::new(file);
    // Only inspect a small prefix — the leading metadata events are always
    // at the top of the file. Reading the whole transcript for every
    // graveyard entry would be wasteful on session files that have grown
    // into the multi-megabyte range.
    let mut have_model = false;
    let mut have_thinking = false;
    for line in reader.lines().take(16).flatten() {
        let val: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        match val.get("type").and_then(|t| t.as_str()) {
            Some("session") => {
                if let Some(cwd) = val.get("cwd").and_then(|c| c.as_str()) {
                    working_dir = PathBuf::from(cwd);
                }
            }
            Some("model_change") => {
                if let Some(m) = val.get("modelId").and_then(|m| m.as_str()) {
                    let provider = val.get("provider").and_then(|p| p.as_str()).unwrap_or("");
                    // Use `provider/model` when both are present so the
                    // graveyard entry round-trips cleanly to providers
                    // like OpenRouter that prefix-route by namespace.
                    model = if provider.is_empty() {
                        m.to_string()
                    } else {
                        format!("{}/{}", provider, m)
                    };
                    have_model = true;
                }
            }
            Some("thinking_level_change") | Some("thinking_level_changed") => {
                let raw = val
                    .get("thinkingLevel")
                    .or_else(|| val.get("level"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if let Ok(level) = raw.parse::<ThinkingLevel>() {
                    thinking = Some(level);
                    have_thinking = true;
                }
            }
            _ => {}
        }
        if have_model && have_thinking {
            break;
        }
    }

    (working_dir, model, thinking)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes three fake Pi session transcripts to a tempdir and asserts
    /// `reconcile_graveyard_from_disk` populates the graveyard map with one
    /// entry per file, parsing `cwd` + `modelId` from the leading session
    /// header where present and falling back to defaults where not.
    #[tokio::test]
    async fn reconcile_graveyard_from_disk_populates_map() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let chat_dir = tmp.path().join("chat-sessions");
        std::fs::create_dir_all(&chat_dir).expect("mkdir");

        // Well-formed: session + model_change + thinking_level_change header.
        let happy = chat_dir.join("11111111-1111-1111-1111-111111111111.jsonl");
        std::fs::write(
            &happy,
            r#"{"type":"session","version":3,"id":"abc","timestamp":"2026-05-15T13:15:15.215Z","cwd":"/tmp/proj-a"}
{"type":"model_change","id":"x","parentId":null,"timestamp":"2026-05-15T13:15:17.053Z","provider":"anthropic","modelId":"claude-sonnet-4"}
{"type":"thinking_level_change","id":"z","parentId":"x","timestamp":"2026-05-15T13:15:17.100Z","thinkingLevel":"high"}
{"type":"message","id":"y","message":{"role":"user","content":[{"type":"text","text":"hi"}]}}
"#,
        )
        .expect("write happy");

        // Only a session header, no model_change — `model` should fall back.
        let no_model = chat_dir.join("22222222-2222-2222-2222-222222222222.jsonl");
        std::fs::write(
            &no_model,
            r#"{"type":"session","version":3,"id":"def","timestamp":"2026-05-15T13:15:15.215Z","cwd":"/tmp/proj-b"}
"#,
        )
        .expect("write no_model");

        // Garbage (not valid JSON) — function must not panic and must still
        // record an entry keyed by file stem.
        let garbage = chat_dir.join("33333333-3333-3333-3333-333333333333.jsonl");
        std::fs::write(&garbage, "this is not json\n").expect("write garbage");

        // Non-jsonl file in the directory — must be ignored.
        std::fs::write(chat_dir.join("README.txt"), "ignore me").expect("write readme");

        let manager = PiManager::new_for_tests();
        let inserted = manager.reconcile_graveyard_from_disk(tmp.path()).await;
        assert_eq!(inserted, 3, "should insert one entry per jsonl file");
        assert_eq!(manager.graveyard_size().await, 3);

        let happy_entry = manager
            .exhume("11111111-1111-1111-1111-111111111111")
            .await
            .expect("happy entry");
        assert_eq!(happy_entry.options.model, "anthropic/claude-sonnet-4");
        assert!(happy_entry.options.resume_session);
        assert_eq!(
            happy_entry.options.session_file.as_deref(),
            Some(happy.display().to_string().as_str())
        );
        // thinking_level_change was present — should be recovered.
        assert!(matches!(
            happy_entry.options.thinking_level,
            ThinkingLevel::High
        ));

        let no_model_entry = manager
            .exhume("22222222-2222-2222-2222-222222222222")
            .await
            .expect("no_model entry");
        // `model_change` was missing — falls back to the parser default.
        assert_eq!(no_model_entry.options.model, "unknown");
        assert!(no_model_entry.options.resume_session);

        let garbage_entry = manager
            .exhume("33333333-3333-3333-3333-333333333333")
            .await
            .expect("garbage entry");
        // Both fields fall back to defaults — the entry exists and is safe
        // to respawn with; a subsequent send_message will supply the real
        // model.
        assert_eq!(garbage_entry.options.model, "unknown");
        assert!(garbage_entry.options.resume_session);
    }

    /// Missing chat-sessions dir is a no-op (e.g. fresh install on a clean
    /// machine).
    #[tokio::test]
    async fn reconcile_graveyard_missing_dir_is_noop() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let manager = PiManager::new_for_tests();
        let inserted = manager.reconcile_graveyard_from_disk(tmp.path()).await;
        assert_eq!(inserted, 0);
        assert_eq!(manager.graveyard_size().await, 0);
    }
}
