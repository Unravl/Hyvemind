// Internal name — surfaces as "Tasks" in the UI. See PRODUCT.md §3.
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use super::chunk_sink::ChunkSink;
use super::events::PiEvent;
use super::rpc::{PiCommand, PiImage, PiRpcError};
use super::transport::PiTransport;
use crate::state::sync::{AsyncMutex, SyncMutex, SyncRwLock};

/// Append a streaming chunk to `sink`. Called from the chat-event streaming
/// closure and from the engine-driven merge phase so both paths share the
/// exact same durable write semantics.
///
/// This is a thin wrapper around [`ChunkSink::write_chunk`]; it exists so
/// callers can pass either an owned sink reference (`&dyn ChunkSink`)
/// without `pi/` knowing anything about hivemind. Returns silently — the
/// sink is responsible for any I/O error handling.
pub fn forward_chunk_to_sink(sink: &dyn ChunkSink, chunk: &str) {
    sink.write_chunk(chunk);
}

fn is_nurse_relevant_event(event: &PiEvent) -> bool {
    !matches!(event, PiEvent::SessionStats(_) | PiEvent::Heartbeat)
}

/// Maximum events retained per session transcript. The transcript is a
/// sliding window; events older than this cap are evicted (O(1) via
/// `VecDeque::pop_front`).
const MAX_TRANSCRIPT_EVENTS: usize = 2_000;

/// Maximum cumulative byte size of all events in the transcript.
/// Evicts oldest events until the byte budget is satisfied. 64 MB is the
/// soft cap; a single large tool result (multi-MB) will still fit but
/// won't drag the running total above the budget for long.
const MAX_TRANSCRIPT_BYTES: usize = 64 * 1024 * 1024;

/// Ownership tag for a Pi session.
///
/// Used by the eviction loop and renderer-reconcile path to make safe
/// lifecycle decisions: Task-owned sessions are evictable when idle,
/// while Review / Merge / Swarm sessions are managed by their owning
/// engines and never killed by idle sweeps or renderer reconciliation.
#[derive(Debug, Clone)]
pub enum SessionOwner {
    /// Tasks-view conversation owned by the frontend.
    Task { task_id: String },
    /// Hivemind review (context, model dispatch).
    Review { job_id: String },
    /// Hivemind merge orchestrator.
    Merge {
        job_id: String,
        round: u32,
        /// Parent swarm id when this merge runs on behalf of a swarm-triggered
        /// Hivemind review. `None` for Tasks-view reviews. Used by
        /// `get_swarm_usage` to attribute live merge tokens to the parent swarm.
        swarm_id: Option<String>,
    },
    /// Swarm role (queen / scout / worker / guard / nurse).
    Swarm { swarm_id: String, role: String },
    /// Unspecified / legacy. Treated like Task for eviction safety.
    Unknown,
}

impl SessionOwner {
    /// True if idle eviction may kill this session.
    pub fn is_idle_evictable(&self) -> bool {
        matches!(self, SessionOwner::Task { .. } | SessionOwner::Unknown)
    }

    /// True if the renderer reconcile sweep may kill this session.
    pub fn is_reconcile_evictable(&self) -> bool {
        matches!(self, SessionOwner::Task { .. })
    }
}

/// High-level session wrapper around a `PiRpcClient`.
///
/// Maintains an authoritative transcript of all events received during the
/// session. The process-pool semaphore permit lives on the underlying
/// `PiRpcClient` (audit 2.9) so it drops the instant the subprocess is
/// torn down — independent of how many forwarder `Arc<PiSession>` clones
/// are still outstanding.
///
/// Methods map to the Pi SDK's session API:
/// - `send_prompt()` / `send_chat()` -> `session.prompt(message)`
/// - `steer()` -> `session.steer(message)`
/// - `follow_up()` -> `session.followUp(message)`
/// - `abort()` -> `session.abort()`
/// - `set_model()` -> `session.setModel(model)`
/// - `set_thinking_level()` -> `session.setThinkingLevel(level)`
/// - `subscribe_events()` -> `session.subscribe(callback)`
pub struct PiSession {
    /// Unique session identifier.
    pub id: String,
    /// The underlying RPC transport. Production wires this to a
    /// [`super::rpc::PiRpcClient`] (which holds the
    /// `OwnedSemaphorePermit` from the Pi process pool internally —
    /// audit 2.9); tests substitute [`super::mock::MockRpcClient`] to
    /// drive the agent loop without spawning a subprocess (audit 7.2).
    rpc: Arc<dyn PiTransport>,
    /// Bounded transcript of recent events. The transcript is a sliding
    /// window capped at `MAX_TRANSCRIPT_EVENTS` and `MAX_TRANSCRIPT_BYTES`.
    /// Older events are evicted to keep memory bounded.
    transcript: Arc<AsyncMutex<VecDeque<PiEvent>>>,
    /// Running byte total of `transcript`. Kept under the same mutex as
    /// `transcript` (callers must lock the deque before reading/writing).
    transcript_bytes: Arc<AsyncMutex<usize>>,
    /// Unix-millis timestamp of the most recent event.
    last_event_at: Arc<AtomicU64>,
    /// Unix-millis timestamp of the most recent prompt sent. Used by the
    /// eviction loop in tandem with `last_event_at` so a session blocked
    /// waiting for its first model token isn't mistaken for idle.
    last_prompt_sent_at: Arc<AtomicU64>,
    /// Total number of events received.
    event_counter: Arc<AtomicU64>,
    /// Count of text/thinking delta events only. Distinct from
    /// `event_counter`, which is polluted by stats polls and heartbeats and
    /// so misses the "stats poll, frozen tokens" stall pattern.
    text_event_counter: Arc<AtomicU64>,
    /// Unix-millis timestamp of the most recent text/thinking delta. 0 if
    /// none yet.
    last_text_event_at: Arc<AtomicU64>,
    /// Count of `ToolExecutionStart` / `ToolExecutionUpdate` /
    /// `ToolExecutionEnd` events. Used by Nurse's multi-signal stall
    /// classifier to distinguish "model is busy in a long-running tool"
    /// (no text deltas, but tool activity) from "model is silently spinning".
    tool_event_counter: Arc<AtomicU64>,
    /// Unix-millis timestamp of the most recent tool-execution event. 0 if
    /// none yet.
    last_tool_event_at: Arc<AtomicU64>,
    /// Count of Pi events that are meaningful to Nurse LLM review. This
    /// intentionally excludes host-side instrumentation (`SessionStats`)
    /// and transport keepalives (`Heartbeat`) so usage polling cannot
    /// make an idle transcript look fresh.
    nurse_activity_counter: Arc<AtomicU64>,
    /// Unix-millis timestamp of the most recent `MessageStart`. Paired with
    /// `messages_in_flight` to give the Nurse an "API call in flight"
    /// signal: when `messages_in_flight > 0`, Pi is mid-message and the
    /// duration is `now - last_message_start_at`. Opus-class models with
    /// high thinking can legitimately take 3-5 min to compose a large
    /// tool-call argument with no streaming output in between, so the
    /// Nurse should not classify this as a stall.
    last_message_start_at: Arc<AtomicU64>,
    /// `MessageStart` count minus `MessageEnd` count. Signed because a
    /// stray `MessageEnd` (provider quirk, partial transcript replay)
    /// briefly underflowing into negative is preferable to wrapping at
    /// `u64::MAX` and reporting "in flight" forever. The counter approach
    /// is robust against same-millisecond `MessageStart`/`MessageEnd`
    /// pairs (tool-result messages close in microseconds), unlike a
    /// timestamp-comparison scheme.
    messages_in_flight: Arc<std::sync::atomic::AtomicI64>,
    /// Number of `send_prompt()` calls made on this session. Secondary
    /// safety-net threshold for context-bloat detection.
    turn_count: Arc<AtomicU64>,
    /// Whether the session is currently processing a prompt (busy).
    busy: Arc<AtomicBool>,
    /// Pinned sessions are skipped by the eviction loop. Set by
    /// `PiManager::get_and_pin_session()` (under the manager's outer
    /// mutex) and cleared by `SessionPinGuard::Drop`.
    pinned: Arc<AtomicBool>,
    /// Flag set when the context window passes the deferred-respawn
    /// threshold while the session is busy. Checked on the
    /// `busy -> !busy` transition; on true, the session is graveyarded.
    needs_respawn: Arc<AtomicBool>,
    /// Cancellation token signalled when this session is force-killed.
    /// Background tasks holding an `Arc<PiSession>` (usage poll, etc.)
    /// should select on `token.cancelled()` so they drop their Arc
    /// promptly and release the semaphore permit.
    cancel_token: CancellationToken,
    /// Owner tag (Task / Review / Merge / Swarm / Unknown). Used by the
    /// eviction loop and renderer reconcile to scope its actions.
    owner: Arc<SyncRwLock<SessionOwner>>,
    /// Per-tool-name capture of the most recent `tool_execution_start`
    /// `args` payload observed during `collect_response*`. Populated for
    /// any `ToolExecutionStart` event whose `name` matches a Hyvemind
    /// extension tool (see `pi::rpc::HYVEMIND_EXTENSION_TOOLS`). Drained
    /// via `take_tool_args`. Every Pi-backed agent consumes its structured
    /// payload from this map.
    captured_tool_args: Arc<SyncMutex<std::collections::HashMap<String, serde_json::Value>>>,
    /// Nurse bus — fed by `touch_activity`, `set_owner`, and `Drop`.
    /// `None` when the session was constructed before the Nurse engine
    /// existed (e.g. early-boot test paths); detectors silently skip those.
    /// Field is immutable for the session's lifetime — no setter exists.
    bus: Option<Arc<crate::nurse::bus::NurseBus>>,
    /// CAS guard so `kill_session` + the subsequent `Drop` produce
    /// exactly one `SessionEnded` event, not two.
    ended_published: AtomicBool,
}

impl PiSession {
    /// Creates a new session wrapping `rpc`. The Pi process-pool semaphore
    /// permit (if any) must already be held by `rpc` itself (audit 2.9).
    ///
    /// Production callers pass an
    /// `Arc<crate::pi::rpc::PiRpcClient>` (which auto-coerces to
    /// `Arc<dyn PiTransport>`); tests pass an
    /// `Arc<crate::pi::mock::MockRpcClient>` (audit 7.2).
    pub fn new(id: String, rpc: Arc<dyn PiTransport>) -> Self {
        Self::new_with_bus(id, rpc, None)
    }

    /// Construct a session bound to a [`NurseBus`](crate::nurse::bus::NurseBus).
    /// Production callers (`PiManager::spawn_session_with_options`) thread the
    /// engine's bus in here; test paths typically use [`Self::new`] which
    /// passes `None` so detectors silently skip.
    pub fn new_with_bus(
        id: String,
        rpc: Arc<dyn PiTransport>,
        bus: Option<Arc<crate::nurse::bus::NurseBus>>,
    ) -> Self {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        Self {
            id,
            rpc,
            transcript: Arc::new(AsyncMutex::new(VecDeque::new())),
            transcript_bytes: Arc::new(AsyncMutex::new(0)),
            last_event_at: Arc::new(AtomicU64::new(now_ms)),
            last_prompt_sent_at: Arc::new(AtomicU64::new(0)),
            event_counter: Arc::new(AtomicU64::new(0)),
            text_event_counter: Arc::new(AtomicU64::new(0)),
            last_text_event_at: Arc::new(AtomicU64::new(0)),
            tool_event_counter: Arc::new(AtomicU64::new(0)),
            last_tool_event_at: Arc::new(AtomicU64::new(0)),
            nurse_activity_counter: Arc::new(AtomicU64::new(0)),
            last_message_start_at: Arc::new(AtomicU64::new(0)),
            messages_in_flight: Arc::new(std::sync::atomic::AtomicI64::new(0)),
            turn_count: Arc::new(AtomicU64::new(0)),
            busy: Arc::new(AtomicBool::new(false)),
            pinned: Arc::new(AtomicBool::new(false)),
            needs_respawn: Arc::new(AtomicBool::new(false)),
            cancel_token: CancellationToken::new(),
            owner: Arc::new(SyncRwLock::new(SessionOwner::Unknown)),
            captured_tool_args: Arc::new(SyncMutex::new(std::collections::HashMap::new())),
            bus,
            ended_published: AtomicBool::new(false),
        }
    }

    /// Take (and remove) the captured `args` payload for `tool_name`, if
    /// any was observed during the most recent `collect_response*` cycle.
    ///
    /// Returns `Some(value)` when the model called the named extension tool
    /// at least once and the `tool_execution_start` event carried the
    /// expected payload; `None` otherwise. Callers that get `None` MUST
    /// fall back to delimiter parsing on the transcript so the Phase-0
    /// hardened parsers remain the safety net.
    ///
    /// Side-effect: the entry is removed so a subsequent call for the same
    /// tool name returns `None` until the next prompt observes it again.
    /// This matches the per-turn lifecycle of the planning/handoff tools.
    pub fn take_tool_args(&self, tool_name: &str) -> Option<serde_json::Value> {
        self.captured_tool_args
            .lock()
            .ok()
            .and_then(|mut map| map.remove(tool_name))
    }

    /// Record a `ToolExecutionStart` event's `args` payload when the tool
    /// name matches one of the Hyvemind extension tools. Called from
    /// `collect_response` and `collect_response_streaming` so both paths
    /// expose the same `take_tool_args` API.
    fn maybe_capture_tool_args(&self, event: &PiEvent) {
        capture_extension_tool_args(&self.captured_tool_args, event);
    }

    // -----------------------------------------------------------------------
    // Primary interaction methods (map to Pi SDK session API)
    // -----------------------------------------------------------------------

    /// Sends a prompt to the Pi session (maps to `session.prompt(message)`).
    ///
    /// This is the primary method for sending user messages. The Pi agent
    /// will process the prompt and emit events via the event stream.
    /// Optionally include images for multimodal prompts.
    #[tracing::instrument(skip_all, fields(session_id = %self.id))]
    pub async fn send_prompt(
        &self,
        message: &str,
        images: Option<Vec<PiImage>>,
    ) -> Result<(), PiRpcError> {
        tracing::debug!(
            session_id = %self.id,
            message_len = message.len(),
            message_preview = %crate::pi::preview(message, 2000),
            image_count = images.as_ref().map(|v| v.len()).unwrap_or(0),
            "pi session: sending prompt"
        );
        // Record activity bookkeeping before sending so the eviction loop
        // does not consider the session idle while it waits for its first
        // response token.
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        self.last_prompt_sent_at.store(now_ms, Ordering::Relaxed);
        self.turn_count.fetch_add(1, Ordering::Relaxed);
        self.busy.store(true, Ordering::SeqCst);
        self.rpc
            .send_command(PiCommand::Prompt {
                message: message.to_string(),
                images,
            })
            .await
    }

    /// Queue a steering message during an active turn (maps to `session.steer()`).
    ///
    /// Use this to redirect the agent while it is streaming. The message
    /// is queued and processed at the next safe yield point.
    /// Used by the Nurse agent to redirect stalled workers.
    #[tracing::instrument(skip_all, fields(session_id = %self.id))]
    pub async fn steer(
        &self,
        message: &str,
        images: Option<Vec<PiImage>>,
    ) -> Result<(), PiRpcError> {
        tracing::debug!(
            session_id = %self.id,
            message_len = message.len(),
            message_preview = %crate::pi::preview(message, 2000),
            image_count = images.as_ref().map(|v| v.len()).unwrap_or(0),
            "pi session: steering"
        );
        self.rpc
            .send_command(PiCommand::Steer {
                message: message.to_string(),
                images,
            })
            .await
    }

    /// Abort the current operation (maps to `session.abort()`).
    ///
    /// This is preferred over killing the process for graceful interruption.
    /// The agent will stop its current work and emit a TurnComplete event.
    #[tracing::instrument(skip_all, fields(session_id = %self.id))]
    pub async fn abort(&self) -> Result<(), PiRpcError> {
        self.rpc.send_command(PiCommand::Abort {}).await
    }

    /// Request real token usage and context stats from the Pi session.
    ///
    /// Sends `get_session_stats` and waits up to 5 seconds for the response.
    ///
    /// `broadcast::RecvError::Lagged` is treated as transient and the loop
    /// continues — a busy session (Scout doing 10+ tool reads in a burst,
    /// Guard tailing build output, Worker streaming tokens) routinely fills
    /// the per-receiver broadcast buffer. Returning `StdinClosed` here would
    /// surface as an IPC error to `get_pi_session_stats`, which the
    /// SwarmControl frontend silently catches; the bottom bar would then
    /// stay at `↑0 / ↓0 / 0%` for the lifetime of any agent whose stream
    /// outpaces the 2 s polling cadence (most observably Scout/Guard, whose
    /// short bursty runs have less headroom than Worker's longer cadence).
    /// Matches the lag-handling already used by `collect_response`
    /// (`session.rs:426`) and `collect_response_streaming`
    /// (`session.rs:528`). Only `Closed` is terminal here.
    #[tracing::instrument(skip_all, fields(session_id = %self.id))]
    pub async fn get_session_stats(&self) -> Result<super::events::PiSessionStats, PiRpcError> {
        let mut rx = self.rpc.subscribe();
        self.rpc.send_command(PiCommand::GetSessionStats {}).await?;
        let timeout = Duration::from_secs(5);
        loop {
            match tokio::time::timeout(timeout, rx.recv()).await {
                Ok(Ok(PiEvent::SessionStats(stats))) => return Ok(stats),
                Ok(Ok(PiEvent::Error(msg))) => {
                    return Err(PiRpcError::ProcessCrashed {
                        exit_code: None,
                        stderr: msg,
                    });
                }
                Ok(Ok(_)) => continue, // skip other events
                Ok(Err(broadcast::error::RecvError::Lagged(n))) => {
                    tracing::warn!(
                        session_id = %self.id,
                        dropped = n,
                        "get_session_stats broadcast receiver lagged \u{2014} continuing"
                    );
                    continue;
                }
                Ok(Err(broadcast::error::RecvError::Closed)) => {
                    return Err(PiRpcError::StdinClosed);
                }
                Err(_) => return Err(PiRpcError::Timeout),
            }
        }
    }

    /// Change thinking level at runtime.
    #[tracing::instrument(skip_all, fields(session_id = %self.id))]
    pub async fn set_thinking_level(
        &self,
        level: super::rpc::ThinkingLevel,
    ) -> Result<(), PiRpcError> {
        self.rpc
            .send_command(PiCommand::SetThinkingLevel { level })
            .await
    }

    // -----------------------------------------------------------------------
    // Event streaming
    // -----------------------------------------------------------------------

    /// Subscribe to the raw event stream (maps to `session.subscribe()`).
    ///
    /// Returns a broadcast receiver that emits all `PiEvent`s from the
    /// underlying Pi process. Callers can use this for real-time streaming
    /// to the frontend instead of waiting for `collect_response()`.
    pub fn subscribe_events(&self) -> broadcast::Receiver<PiEvent> {
        self.rpc.subscribe()
    }

    /// Push an event onto the bounded transcript, evicting oldest entries
    /// while the event count or byte total exceeds the configured caps.
    /// Caller must already have called `touch_activity`.
    async fn push_transcript(&self, event: PiEvent) {
        let size = event.estimated_size();
        let mut transcript = self.transcript.lock().await;
        let mut bytes = self.transcript_bytes.lock().await;
        transcript.push_back(event);
        *bytes = bytes.saturating_add(size);
        while transcript.len() > MAX_TRANSCRIPT_EVENTS || *bytes > MAX_TRANSCRIPT_BYTES {
            if let Some(evicted) = transcript.pop_front() {
                let s = evicted.estimated_size();
                *bytes = bytes.saturating_sub(s);
            } else {
                break;
            }
        }
    }

    /// Subscribes to the event stream and collects all `TextDelta` payloads
    /// until an `AgentEnd` event is received, returning the accumulated
    /// response text.
    ///
    /// Pi emits `TurnComplete` after each tool-execution turn (before the
    /// agent's text response in the next turn), so we must wait for `AgentEnd`
    /// which signals the agent has truly finished.
    ///
    /// Every event is recorded in the bounded transcript and the
    /// activity timestamps are updated accordingly.
    #[tracing::instrument(skip_all, fields(session_id = %self.id))]
    pub async fn collect_response(&self) -> Result<String, PiRpcError> {
        let mut rx = self.rpc.subscribe();
        let mut accumulated = String::new();

        loop {
            let event = match rx.recv().await {
                Ok(event) => event,
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(
                        session_id = %self.id,
                        dropped = n,
                        "broadcast receiver lagged \u{2014} skipping dropped events"
                    );
                    continue;
                }
                Err(e) => {
                    return Err(PiRpcError::IoError(std::io::Error::other(e.to_string())));
                }
            };

            // Update activity bookkeeping.
            self.touch_activity(&event);

            // Append to bounded transcript.
            self.push_transcript(event.clone()).await;

            // Capture extension tool-call args (Phase 1 — structured-output
            // bypass for the delimiter parsers).
            self.maybe_capture_tool_args(&event);

            match &event {
                PiEvent::TextDelta(text) => {
                    accumulated.push_str(text);
                }
                PiEvent::AgentEnd => {
                    self.on_turn_end();
                    break;
                }
                PiEvent::Error(msg) => {
                    self.on_turn_end();
                    return Err(PiRpcError::ProcessCrashed {
                        exit_code: None,
                        stderr: msg.clone(),
                    });
                }
                // ThinkingDelta, ToolExecution*, Message*, TurnComplete, Turn*,
                // Heartbeat, legacy ToolUse/ToolResult -- recorded but not
                // accumulated into text.
                _ => {}
            }
        }

        tracing::debug!(
            session_id = %self.id,
            response_len = accumulated.len(),
            event_count = self.event_count(),
            "pi session: collect_response complete"
        );
        Ok(accumulated)
    }

    /// Subscribes to the event stream and collects response with a callback
    /// invoked for each event (for real-time streaming to the frontend).
    ///
    /// Returns the accumulated text when AgentEnd is received, or after
    /// TURN_COMPLETE_GRACE elapses post-TurnComplete with no further events.
    #[tracing::instrument(skip(self, on_event), fields(session_id = %self.id))]
    pub async fn collect_response_streaming<F>(&self, mut on_event: F) -> Result<String, PiRpcError>
    where
        F: FnMut(&PiEvent),
    {
        const TURN_COMPLETE_GRACE: Duration = Duration::from_secs(30);
        // Pi emits `agent_end` ~1ms BEFORE `auto_retry_start` when a
        // transient provider error trips its built-in retry path. If we
        // break on `AgentEnd` immediately the auto-retry's continued
        // streaming falls on the floor (see Hyvemind issue: stuck Task
        // session at 13:23:36 → final plan dropped at 13:26:22). Hold
        // termination open for this many ms after `AgentEnd` to give
        // a trailing `AutoRetryStart` a chance to arrive. Empirically Pi
        // emits the retry event within ~2ms; 2s is a wide safety margin
        // that's still imperceptible at the end of a normal turn.
        const AGENT_END_RETRY_GRACE: Duration = Duration::from_millis(2000);
        let mut rx = self.rpc.subscribe();
        let mut accumulated = String::new();
        let inactivity_timeout = Duration::from_secs(300);
        let mut turn_complete_at: Option<std::time::Instant> = None;
        // Set when we see AgentEnd. While Some, the next AutoRetryStart
        // cancels the pending break; otherwise the grace timer expires
        // and we terminate normally.
        let mut agent_end_at: Option<std::time::Instant> = None;
        // True between AutoRetryStart and AutoRetryEnd. While true,
        // subsequent AgentEnd events are NOT terminal (Pi will keep
        // streaming after the retry).
        let mut retry_in_flight = false;

        loop {
            // Pick the tightest applicable timeout. AGENT_END_RETRY_GRACE
            // takes precedence over the longer TurnComplete grace because
            // the retry-detection window must close quickly to avoid
            // adding latency to normal turns.
            let recv_timeout = if let Some(t) = agent_end_at {
                AGENT_END_RETRY_GRACE.saturating_sub(t.elapsed())
            } else if let Some(t) = turn_complete_at {
                TURN_COMPLETE_GRACE.saturating_sub(t.elapsed())
            } else {
                inactivity_timeout
            };
            let event = match tokio::time::timeout(recv_timeout, rx.recv()).await {
                Ok(Ok(event)) => event,
                Ok(Err(broadcast::error::RecvError::Lagged(n))) => {
                    tracing::warn!(
                        session_id = %self.id,
                        dropped = n,
                        "broadcast receiver lagged \u{2014} skipping dropped events"
                    );
                    continue;
                }
                Ok(Err(e)) => {
                    self.on_turn_end();
                    return Err(PiRpcError::IoError(std::io::Error::other(e.to_string())));
                }
                Err(_) => {
                    if agent_end_at.is_some() {
                        // AgentEnd was real (no retry followed within the
                        // grace window) — terminate normally.
                        self.on_turn_end();
                        tracing::info!(
                            session_id = %self.id,
                            response_len = accumulated.len(),
                            grace_ms = AGENT_END_RETRY_GRACE.as_millis() as u64,
                            "collect_response_streaming completed via AgentEnd (no auto-retry within grace)"
                        );
                        return Ok(accumulated);
                    }
                    if turn_complete_at.is_some() {
                        self.on_turn_end();
                        tracing::info!(
                            session_id = %self.id,
                            response_len = accumulated.len(),
                            grace_secs = TURN_COMPLETE_GRACE.as_secs(),
                            "collect_response_streaming completed via TurnComplete grace (no AgentEnd received)"
                        );
                        return Ok(accumulated);
                    }
                    self.on_turn_end();
                    tracing::warn!(session_id = %self.id, "collect_response_streaming timed out after 5m of inactivity");
                    return Err(PiRpcError::Timeout);
                }
            };

            // Update activity bookkeeping.
            self.touch_activity(&event);

            // Append to bounded transcript.
            self.push_transcript(event.clone()).await;

            // Capture extension tool-call args (Phase 1) — must run BEFORE
            // the caller callback so by the time the caller decides whether
            // to short-circuit on a structured response the captured map
            // already holds the payload.
            self.maybe_capture_tool_args(&event);

            // Invoke the caller's callback for real-time processing.
            on_event(&event);

            match &event {
                PiEvent::TextDelta(text) => {
                    accumulated.push_str(text);
                    turn_complete_at = None;
                    // A delta after AgentEnd means Pi resumed (e.g. from
                    // a retry whose start event we may have missed) —
                    // clear the pending break so the new content isn't
                    // truncated by the grace timer.
                    agent_end_at = None;
                }
                PiEvent::AgentEnd => {
                    if retry_in_flight {
                        // Auto-retry is mid-flight; this AgentEnd is the
                        // pre-retry one and must not terminate the loop.
                        tracing::debug!(
                            session_id = %self.id,
                            "ignoring AgentEnd during in-flight auto-retry"
                        );
                        continue;
                    }
                    // Defer the break: if AutoRetryStart arrives within
                    // AGENT_END_RETRY_GRACE we'll cancel; otherwise the
                    // timeout arm above terminates the loop normally.
                    agent_end_at = Some(std::time::Instant::now());
                }
                PiEvent::AutoRetryStart {
                    attempt,
                    max_attempts,
                    delay_ms,
                    ..
                } => {
                    tracing::warn!(
                        session_id = %self.id,
                        attempt,
                        max_attempts,
                        delay_ms,
                        "pi auto-retry started — holding session open"
                    );
                    agent_end_at = None;
                    turn_complete_at = None;
                    retry_in_flight = true;
                }
                PiEvent::AutoRetryEnd { success, attempt } => {
                    retry_in_flight = false;
                    if *success {
                        tracing::info!(
                            session_id = %self.id,
                            attempt,
                            "pi auto-retry succeeded — resuming stream"
                        );
                        // Streaming will continue with the next event; do
                        // not terminate. Clear any AgentEnd still pending.
                        agent_end_at = None;
                    } else {
                        tracing::error!(
                            session_id = %self.id,
                            attempt,
                            "pi auto-retry exhausted — treating as terminal error"
                        );
                        self.on_turn_end();
                        return Err(PiRpcError::ProcessCrashed {
                            exit_code: None,
                            stderr: "pi auto-retry exhausted".to_string(),
                        });
                    }
                }
                PiEvent::TurnComplete => {
                    turn_complete_at = Some(std::time::Instant::now());
                }
                PiEvent::TurnStart | PiEvent::ToolExecutionStart { .. } => {
                    turn_complete_at = None;
                    agent_end_at = None;
                }
                PiEvent::Error(msg) => {
                    self.on_turn_end();
                    return Err(PiRpcError::ProcessCrashed {
                        exit_code: None,
                        stderr: msg.clone(),
                    });
                }
                _ => {}
            }
        }
    }

    // -----------------------------------------------------------------------
    // Activity tracking
    // -----------------------------------------------------------------------

    /// Update the activity timestamp and event counter. If `event` is a
    /// text/thinking delta, also bumps the text-only counter and timestamp
    /// (used by Nurse to distinguish "model is actually generating output"
    /// from "RPC chatter / stats polls").
    fn touch_activity(&self, event: &PiEvent) {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        self.last_event_at.store(now_ms, Ordering::Relaxed);
        self.event_counter.fetch_add(1, Ordering::Relaxed);
        if matches!(event, PiEvent::TextDelta(_) | PiEvent::ThinkingDelta(_)) {
            self.last_text_event_at.store(now_ms, Ordering::Relaxed);
            self.text_event_counter.fetch_add(1, Ordering::Relaxed);
        }
        if matches!(
            event,
            PiEvent::ToolExecutionStart { .. }
                | PiEvent::ToolExecutionUpdate { .. }
                | PiEvent::ToolExecutionEnd { .. }
                | PiEvent::ToolUse { .. }
                | PiEvent::ToolResult { .. }
        ) {
            self.last_tool_event_at.store(now_ms, Ordering::Relaxed);
            self.tool_event_counter.fetch_add(1, Ordering::Relaxed);
        }
        if is_nurse_relevant_event(event) {
            self.nurse_activity_counter.fetch_add(1, Ordering::Relaxed);
        }
        match event {
            PiEvent::MessageStart => {
                self.last_message_start_at.store(now_ms, Ordering::Relaxed);
                self.messages_in_flight.fetch_add(1, Ordering::Relaxed);
            }
            PiEvent::MessageEnd => {
                self.messages_in_flight.fetch_sub(1, Ordering::Relaxed);
            }
            _ => {}
        }

        // Fan the event out to the Nurse bus. Cap per-event evidence so a
        // multi-megabyte tool result can't blow up the broadcast ring.
        if let Some(bus) = &self.bus {
            let cap = crate::tunables::nurse_max_evidence_bytes();
            let truncated = event.truncated(cap);
            bus.publish_owned(crate::nurse::bus::NurseBusEvent::Event {
                session_id: self.id.clone(),
                event: truncated,
                observed_at: std::time::Instant::now(),
            });
        }
    }

    /// Called on every `busy -> !busy` transition (AgentEnd, TurnComplete
    /// grace expiry, error, timeout). Clears the busy flag.
    /// `needs_respawn` is consulted by the maintenance loop on its next
    /// sweep rather than acting inline so the IPC streaming path stays
    /// fast and uninterrupted.
    fn on_turn_end(&self) {
        self.busy.store(false, Ordering::SeqCst);
    }

    /// Returns a snapshot of the bounded transcript window.
    ///
    /// Note: the transcript is capped at `MAX_TRANSCRIPT_EVENTS` events and
    /// `MAX_TRANSCRIPT_BYTES` bytes; older events have been evicted. The
    /// authoritative full history lives in the Pi session's JSONL file on
    /// disk (`~/.hyvemind/chat-sessions/{id}.jsonl`).
    pub async fn get_transcript(&self) -> Vec<PiEvent> {
        self.transcript.lock().await.iter().cloned().collect()
    }

    /// Maximum bytes per event's text content when preparing transcript
    /// for the nurse diagnostic prompt.
    const NURSE_TRANSCRIPT_EVENT_MAX_BYTES: usize = 2_000;

    /// Returns the last `n` non-instrumentation events from the transcript,
    /// or all if fewer than `n`. Text content within each event is truncated
    /// to avoid overflowing LLM context.
    ///
    /// `SessionStats` and `Heartbeat` events are filtered out before the
    /// last-`n` window is selected. Both are host-side instrumentation —
    /// `SessionStats` is the response to Hyvemind's own `get_session_stats`
    /// poll (driven from `commands::chat`'s 2.5 s usage-poll loop), and
    /// `Heartbeat` is Pi's stdout keepalive. Including them would let the
    /// last-N window be 100 % polling chatter during long model waits,
    /// causing the Nurse LLM to misread host polling as agent activity and
    /// fire a false-positive steer ("you're in a polling loop").
    pub async fn get_recent_transcript(&self, n: usize) -> Vec<PiEvent> {
        let transcript = self.transcript.lock().await;
        let filtered: Vec<&PiEvent> = transcript
            .iter()
            .filter(|ev| !matches!(ev, PiEvent::SessionStats(_) | PiEvent::Heartbeat))
            .collect();
        let start = filtered.len().saturating_sub(n);
        filtered
            .into_iter()
            .skip(start)
            .map(|event| event.truncated(Self::NURSE_TRANSCRIPT_EVENT_MAX_BYTES))
            .collect()
    }

    /// Returns the unix-millis timestamp of the last event received.
    pub fn last_activity_ms(&self) -> u64 {
        self.last_event_at.load(Ordering::Relaxed)
    }

    /// Returns the unix-millis timestamp of the last prompt sent (0 if
    /// no prompt has been sent yet on this session).
    pub fn last_prompt_sent_ms(&self) -> u64 {
        self.last_prompt_sent_at.load(Ordering::Relaxed)
    }

    /// Returns the total number of events received in this session.
    pub fn event_count(&self) -> u64 {
        self.event_counter.load(Ordering::Relaxed)
    }

    /// Returns the number of Nurse-relevant events received in this
    /// session. Host instrumentation and keepalives are excluded so Nurse
    /// LLM check-ins can cheaply determine whether anything real changed
    /// since their previous review.
    pub fn nurse_activity_count(&self) -> u64 {
        self.nurse_activity_counter.load(Ordering::Relaxed)
    }

    /// Returns the number of text/thinking delta events seen on this
    /// session. Used by Nurse to detect "model is silently spinning"
    /// stalls, where the overall `event_count` is rising (heartbeats,
    /// stats polls) but no actual content is being generated.
    pub fn text_event_count(&self) -> u64 {
        self.text_event_counter.load(Ordering::Relaxed)
    }

    /// Returns the unix-millis timestamp of the most recent text/thinking
    /// delta. Returns 0 if no delta has been observed yet.
    pub fn last_text_event_ms(&self) -> u64 {
        self.last_text_event_at.load(Ordering::Relaxed)
    }

    /// Returns the unix-millis timestamp of the most recent tool-execution
    /// event. Returns 0 if no tool event has been observed yet.
    pub fn last_tool_event_ms(&self) -> u64 {
        self.last_tool_event_at.load(Ordering::Relaxed)
    }

    /// Returns `Some(duration_ms)` if Pi is currently mid-message — at
    /// least one `MessageStart` has arrived without a matching
    /// `MessageEnd` — else `None`. The duration is
    /// `now - last_message_start_at`.
    ///
    /// Used by the Nurse stall classifier to recognise "model API call in
    /// flight" so it doesn't intervene while the provider is legitimately
    /// composing a large response (e.g. claude-opus-4-6 with thinking=high
    /// silently composing a multi-kilobyte tool-call argument can take 3–5
    /// minutes with zero `TextDelta`s).
    ///
    /// Tool-result messages also open/close `MessageStart`/`MessageEnd`,
    /// but their lifetime is microseconds, so the signal is dominated by
    /// in-flight assistant messages whenever the duration is non-trivial.
    pub fn awaiting_model_for_ms(&self) -> Option<u64> {
        if self.messages_in_flight.load(Ordering::Relaxed) <= 0 {
            return None;
        }
        let start = self.last_message_start_at.load(Ordering::Relaxed);
        if start == 0 {
            return None;
        }
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        Some(now_ms.saturating_sub(start))
    }

    /// Returns the total number of prompts sent on this session.
    pub fn turn_count(&self) -> u64 {
        self.turn_count.load(Ordering::Relaxed)
    }

    /// Returns `true` if the underlying process is still running.
    pub fn is_alive(&self) -> bool {
        self.rpc.is_alive()
    }

    /// Audit 2.3: returns the OS PID of the spawned Pi subprocess. `None`
    /// when the session was created by a test-only path that did not
    /// capture a PID, or when the underlying client genuinely has no PID.
    pub fn pid(&self) -> Option<u32> {
        self.rpc.pid()
    }

    /// Returns a snapshot of all stderr output captured from the child Pi
    /// process so far. Useful when the session has failed and you need
    /// diagnostic text before killing the process.
    pub async fn stderr_snapshot(&self) -> String {
        self.rpc.stderr_snapshot().await
    }

    /// Returns `true` if the session is currently processing a prompt.
    pub fn is_busy(&self) -> bool {
        self.busy.load(Ordering::SeqCst)
    }

    /// Returns `true` if the session is currently pinned (in use by a
    /// foreground operation) and must not be evicted.
    pub fn is_pinned(&self) -> bool {
        self.pinned.load(Ordering::SeqCst)
    }

    /// Returns `true` if the deferred-respawn flag is set.
    pub fn needs_respawn(&self) -> bool {
        self.needs_respawn.load(Ordering::SeqCst)
    }

    /// Mark this session for deferred respawn. Checked by the maintenance
    /// loop; will trigger kill+graveyard on the next sweep where the
    /// session is not busy.
    pub fn mark_needs_respawn(&self) {
        self.needs_respawn.store(true, Ordering::SeqCst);
    }

    /// Cancellation token signalled when this session is force-killed.
    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancel_token.clone()
    }

    /// Returns a clone of the current owner tag.
    pub fn owner(&self) -> SessionOwner {
        match self.owner.read() {
            Ok(g) => g.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    /// Sets the owner tag. Called by the command layer immediately after
    /// spawning a session.
    pub fn set_owner(&self, owner: SessionOwner) {
        match self.owner.write() {
            Ok(mut g) => *g = owner.clone(),
            Err(poisoned) => *poisoned.into_inner() = owner.clone(),
        }
        if let Some(bus) = &self.bus {
            bus.publish_owned(crate::nurse::bus::NurseBusEvent::OwnerChanged {
                session_id: self.id.clone(),
                owner,
            });
        }
    }

    /// Internal: set the pinned flag. Call sites are restricted to
    /// `PiManager::get_and_pin_session()` and `SessionPinGuard::Drop`.
    pub fn set_pinned(&self, pinned: bool) {
        self.pinned.store(pinned, Ordering::SeqCst);
    }

    /// Force-kill this session: send Abort, wait up to 1s for graceful
    /// exit, escalate to SIGKILL, wait up to 2s for the OS to reap.
    /// Clears the busy flag and cancels the session token so background
    /// tasks holding an `Arc<PiSession>` (usage poll, etc.) drop their
    /// clones promptly.
    ///
    /// Returns only after the child process has actually exited (or the
    /// bounded 3-4s shutdown window has elapsed; in that worst case the
    /// underlying `Child::kill_on_drop(true)` is the last-resort safety
    /// net). This is in contrast to the previous behavior which sent
    /// Abort + slept 200ms + closed stdin and returned without awaiting
    /// the reap — leaving zombie Pi processes after stop/kill.
    #[tracing::instrument(skip_all, fields(session_id = %self.id))]
    pub async fn force_kill(&self) -> Result<(), PiRpcError> {
        let r = self.rpc.force_kill().await;
        self.busy.store(false, Ordering::SeqCst);
        self.cancel_token.cancel();
        r
    }

    /// Test-only: feed an event into the same activity / transcript path
    /// `collect_response` would use, without needing a spawned Pi child or
    /// the mock broadcast plumbing. Use this when a test wants to drive
    /// `awaiting_model_for_ms`, the transcript trimmer, or the
    /// text/tool/stall counters with a hand-crafted sequence.
    #[cfg(test)]
    pub(crate) async fn record_event_for_test(&self, event: PiEvent) {
        self.touch_activity(&event);
        self.push_transcript(event).await;
    }
}

impl PiSession {
    /// Publish `SessionEnded` exactly once. Both [`PiManager::kill_session`]
    /// (via this method) and [`Drop::drop`] call this; the CAS guarantees a
    /// single emit per session.
    pub fn publish_session_ended(&self, reason: crate::nurse::bus::SessionEndReason) {
        if self
            .ended_published
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Relaxed,
            )
            .is_ok()
        {
            if let Some(bus) = &self.bus {
                bus.publish_owned(crate::nurse::bus::NurseBusEvent::SessionEnded {
                    session_id: self.id.clone(),
                    reason,
                    ended_at: std::time::Instant::now(),
                });
            }
        }
    }
}

impl Drop for PiSession {
    fn drop(&mut self) {
        self.publish_session_ended(crate::nurse::bus::SessionEndReason::Dropped);
        // When `PiSession` is dropped, its `PiRpcClient` is dropped, which
        // (audit 2.9) now actively aborts its task handles, calls
        // `Child::start_kill` synchronously, schedules the reap on a
        // detached task, and releases its `OwnedSemaphorePermit` to the
        // process pool — all without waiting for the monitor task to
        // notice. `kill_on_drop(true)` on the underlying `Child` is the
        // last-resort safety net.
        //
        // For graceful shutdown, callers should use `session.shutdown()` or
        // `PiManager::kill_session` which calls `rpc.shutdown()` before dropping.
        // Cancel the token in case any background task is still holding an
        // Arc<PiSession> clone — it should bail.
        self.cancel_token.cancel();
        tracing::debug!(session_id = %self.id, "PiSession dropped -- process will be killed on drop");
    }
}

/// Free function backing `PiSession::maybe_capture_tool_args` — pulled out
/// so the per-event capture logic can be unit-tested without spawning a
/// real Pi subprocess. Inserts the model's `args` payload into `map`
/// keyed by the tool name, but only when the event is for a registered
/// Hyvemind extension tool and `args` is not JSON null.
fn capture_extension_tool_args(
    map: &Arc<std::sync::Mutex<std::collections::HashMap<String, serde_json::Value>>>,
    event: &PiEvent,
) {
    if let PiEvent::ToolExecutionStart { name, args, .. } = event {
        if args.is_null() {
            return;
        }
        if !crate::pi::rpc::HYVEMIND_EXTENSION_TOOLS.contains(&name.as_str()) {
            return;
        }
        if let Ok(mut map) = map.lock() {
            map.insert(name.clone(), args.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::sync::Semaphore;

    /// Build a PiSession-equivalent transcript trimmer without needing an
    /// actual Pi subprocess. Verifies the bounded-deque semantics.
    #[tokio::test]
    async fn transcript_evicts_when_over_event_cap() {
        let transcript: Arc<AsyncMutex<VecDeque<PiEvent>>> =
            Arc::new(AsyncMutex::new(VecDeque::new()));
        let bytes: Arc<AsyncMutex<usize>> = Arc::new(AsyncMutex::new(0));

        async fn push(
            transcript: &Arc<AsyncMutex<VecDeque<PiEvent>>>,
            bytes: &Arc<AsyncMutex<usize>>,
            event: PiEvent,
        ) {
            let size = event.estimated_size();
            let mut t = transcript.lock().await;
            let mut b = bytes.lock().await;
            t.push_back(event);
            *b = b.saturating_add(size);
            while t.len() > MAX_TRANSCRIPT_EVENTS || *b > MAX_TRANSCRIPT_BYTES {
                if let Some(evicted) = t.pop_front() {
                    *b = b.saturating_sub(evicted.estimated_size());
                } else {
                    break;
                }
            }
        }

        for i in 0..(MAX_TRANSCRIPT_EVENTS + 50) {
            push(&transcript, &bytes, PiEvent::TextDelta(format!("e{}", i))).await;
        }

        let t = transcript.lock().await;
        assert_eq!(t.len(), MAX_TRANSCRIPT_EVENTS);
        // The oldest events should have been evicted; the last event must
        // be the most-recently pushed one.
        match t.back().unwrap() {
            PiEvent::TextDelta(s) => assert_eq!(s, &format!("e{}", MAX_TRANSCRIPT_EVENTS + 49)),
            other => panic!("expected TextDelta, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn transcript_evicts_when_over_byte_cap() {
        let transcript: Arc<AsyncMutex<VecDeque<PiEvent>>> =
            Arc::new(AsyncMutex::new(VecDeque::new()));
        let bytes: Arc<AsyncMutex<usize>> = Arc::new(AsyncMutex::new(0));

        // Push a few large events that together exceed the byte cap.
        let big = "x".repeat(8 * 1024 * 1024); // 8 MB per event
        for _ in 0..16 {
            let event = PiEvent::TextDelta(big.clone());
            let size = event.estimated_size();
            let mut t = transcript.lock().await;
            let mut b = bytes.lock().await;
            t.push_back(event);
            *b += size;
            while t.len() > MAX_TRANSCRIPT_EVENTS || *b > MAX_TRANSCRIPT_BYTES {
                if let Some(evicted) = t.pop_front() {
                    *b = b.saturating_sub(evicted.estimated_size());
                } else {
                    break;
                }
            }
        }

        let b = bytes.lock().await;
        assert!(*b <= MAX_TRANSCRIPT_BYTES);
    }

    #[tokio::test]
    async fn session_owner_evictability() {
        let task = SessionOwner::Task {
            task_id: "t".into(),
        };
        assert!(task.is_idle_evictable());
        assert!(task.is_reconcile_evictable());

        let review = SessionOwner::Review { job_id: "j".into() };
        assert!(!review.is_idle_evictable());
        assert!(!review.is_reconcile_evictable());

        let swarm = SessionOwner::Swarm {
            swarm_id: "s".into(),
            role: "scout".into(),
        };
        assert!(!swarm.is_idle_evictable());
        assert!(!swarm.is_reconcile_evictable());

        let merge = SessionOwner::Merge {
            job_id: "j".into(),
            round: 1,
            swarm_id: None,
        };
        assert!(!merge.is_idle_evictable());
        assert!(!merge.is_reconcile_evictable());

        let unknown = SessionOwner::Unknown;
        assert!(unknown.is_idle_evictable());
        assert!(!unknown.is_reconcile_evictable());
    }

    // Note: full force_kill / pin / cancellation tests require a spawned
    // Pi child. Those are covered by integration tests gated on the `pi`
    // binary being present (see app/src-tauri/tests/).
    #[allow(dead_code)]
    async fn _unused_semaphore_compile_check() {
        // Ensure the OwnedSemaphorePermit dependency still type-checks.
        let s = Arc::new(Semaphore::new(1));
        let _p = s.acquire_owned().await.unwrap();
    }

    /// Regression: the Nurse-facing transcript window must omit host-side
    /// instrumentation events (`SessionStats` from chat.rs's 2.5 s usage
    /// poll, `Heartbeat` from Pi's stdout keepalive). Otherwise during a
    /// long silent model call the last-N window is 100 % polling chatter
    /// and the Nurse LLM concludes the agent is "stuck in a polling loop"
    /// — the false-positive we observed in session 1f4a5171.
    #[tokio::test]
    async fn recent_transcript_filters_instrumentation_events() {
        let (session, _mock) = crate::pi::mock::mock_session("filter-test");

        assert_eq!(session.nurse_activity_count(), 0);
        session
            .record_event_for_test(PiEvent::TextDelta("hello".into()))
            .await;
        assert_eq!(session.nurse_activity_count(), 1);
        for _ in 0..10 {
            session
                .record_event_for_test(PiEvent::SessionStats(crate::pi::events::PiSessionStats {
                    input: 0,
                    output: 0,
                    reasoning_tokens: 0,
                    cache_read: 0,
                    cache_write: 0,
                    total_tokens: 0,
                    cost: 0.0,
                    context_tokens: 0,
                    context_window: 0,
                    context_percent: 0.0,
                }))
                .await;
            session.record_event_for_test(PiEvent::Heartbeat).await;
        }
        assert_eq!(
            session.nurse_activity_count(),
            1,
            "instrumentation must not advance Nurse activity"
        );
        session
            .record_event_for_test(PiEvent::TextDelta("world".into()))
            .await;
        assert_eq!(session.nurse_activity_count(), 2);

        let recent = session.get_recent_transcript(40).await;
        assert!(
            recent
                .iter()
                .all(|ev| !matches!(ev, PiEvent::SessionStats(_) | PiEvent::Heartbeat)),
            "recent transcript must not contain SessionStats or Heartbeat: {:?}",
            recent
        );
        assert_eq!(
            recent.len(),
            2,
            "expected only the two TextDeltas to survive filtering"
        );
    }

    /// `awaiting_model_for_ms` returns `Some(dur)` while a `MessageStart`
    /// is open (no matching `MessageEnd`), and `None` once `MessageEnd`
    /// closes it.
    #[tokio::test]
    async fn awaiting_model_for_ms_tracks_message_lifetime() {
        let (session, _mock) = crate::pi::mock::mock_session("await-test");

        // No messages yet — signal must be absent.
        assert!(session.awaiting_model_for_ms().is_none());

        session.record_event_for_test(PiEvent::MessageStart).await;
        // Sleep just enough for now-millis to advance.
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;

        let await_dur = session
            .awaiting_model_for_ms()
            .expect("awaiting after MessageStart");
        assert!(
            await_dur > 0,
            "expected non-zero await duration, got {}",
            await_dur
        );

        session.record_event_for_test(PiEvent::MessageEnd).await;
        assert!(
            session.awaiting_model_for_ms().is_none(),
            "signal must clear once MessageEnd arrives"
        );

        // A subsequent MessageStart re-opens the signal.
        session.record_event_for_test(PiEvent::MessageStart).await;
        assert!(session.awaiting_model_for_ms().is_some());
    }

    /// SessionStats events should NOT keep the awaiting-model signal alive
    /// or extinguish it — they don't carry message boundaries.
    #[tokio::test]
    async fn session_stats_do_not_affect_awaiting_model_signal() {
        let (session, _mock) = crate::pi::mock::mock_session("await-stats-test");
        session.record_event_for_test(PiEvent::MessageStart).await;
        for _ in 0..5 {
            session
                .record_event_for_test(PiEvent::SessionStats(crate::pi::events::PiSessionStats {
                    input: 0,
                    output: 0,
                    reasoning_tokens: 0,
                    cache_read: 0,
                    cache_write: 0,
                    total_tokens: 0,
                    cost: 0.0,
                    context_tokens: 0,
                    context_window: 0,
                    context_percent: 0.0,
                }))
                .await;
        }
        assert!(
            session.awaiting_model_for_ms().is_some(),
            "SessionStats must not close an open assistant message"
        );
    }

    // -- Phase 1: extension tool-args capture round-trip --------------------

    fn empty_args_map(
    ) -> Arc<std::sync::Mutex<std::collections::HashMap<String, serde_json::Value>>> {
        Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()))
    }

    #[test]
    fn capture_extension_tool_args_records_handoff_payload() {
        let map = empty_args_map();
        let event = PiEvent::ToolExecutionStart {
            tool_call_id: "tc-1".into(),
            name: "submit_handoff".into(),
            args: serde_json::json!({
                "feature_id": "feat-1",
                "run_id": "run-abc",
                "success_state": "success",
            }),
        };
        capture_extension_tool_args(&map, &event);
        let stored = map.lock().unwrap().get("submit_handoff").cloned();
        assert!(stored.is_some());
        let payload = stored.unwrap();
        assert_eq!(payload["feature_id"], "feat-1");
    }

    #[test]
    fn capture_extension_tool_args_ignores_built_in_tools() {
        let map = empty_args_map();
        let event = PiEvent::ToolExecutionStart {
            tool_call_id: "tc-2".into(),
            name: "read".into(),
            args: serde_json::json!({"path": "foo.rs"}),
        };
        capture_extension_tool_args(&map, &event);
        assert!(map.lock().unwrap().is_empty());
    }

    #[test]
    fn capture_extension_tool_args_ignores_null_args() {
        // Older Pi versions emit `tool_execution_start` without an `args`
        // field for built-in tools. Our parser defaults that to JSON null;
        // we must not record null entries because `take_tool_args` would
        // then return a useless Null that callers might try to deserialise.
        let map = empty_args_map();
        let event = PiEvent::ToolExecutionStart {
            tool_call_id: "tc-3".into(),
            name: "submit_handoff".into(),
            args: serde_json::Value::Null,
        };
        capture_extension_tool_args(&map, &event);
        assert!(map.lock().unwrap().is_empty());
    }

    #[test]
    fn capture_extension_tool_args_overwrites_on_repeat_call() {
        // If the model calls the tool twice (e.g. it corrects itself), the
        // most recent args win. Documents the simple last-write-wins
        // semantics so downstream consumers know what to expect.
        let map = empty_args_map();
        let first = PiEvent::ToolExecutionStart {
            tool_call_id: "tc-a".into(),
            name: "submit_plan".into(),
            args: serde_json::json!({"plan_markdown": "first"}),
        };
        let second = PiEvent::ToolExecutionStart {
            tool_call_id: "tc-b".into(),
            name: "submit_plan".into(),
            args: serde_json::json!({"plan_markdown": "second"}),
        };
        capture_extension_tool_args(&map, &first);
        capture_extension_tool_args(&map, &second);
        let stored = map.lock().unwrap().get("submit_plan").cloned().unwrap();
        assert_eq!(stored["plan_markdown"], "second");
    }

    // -- Audit 6.2: ChunkSink forwarder ------------------------------------

    /// Minimal in-memory [`ChunkSink`] used to assert that
    /// [`forward_chunk_to_sink`] forwards each chunk verbatim and in
    /// arrival order. The collected chunks are kept as a Vec so the test
    /// can confirm both the boundaries and the order of writes.
    #[derive(Debug, Default)]
    struct VecSink {
        chunks: std::sync::Mutex<Vec<String>>,
    }

    impl super::super::chunk_sink::ChunkSink for VecSink {
        fn write_chunk(&self, chunk: &str) {
            self.chunks.lock().unwrap().push(chunk.to_string());
        }
    }

    #[test]
    fn forward_chunk_to_sink_records_each_chunk_in_order() {
        let sink = std::sync::Arc::new(VecSink::default());
        super::forward_chunk_to_sink(sink.as_ref(), "hello ");
        super::forward_chunk_to_sink(sink.as_ref(), "world");
        super::forward_chunk_to_sink(sink.as_ref(), "!");

        let chunks = sink.chunks.lock().unwrap().clone();
        assert_eq!(
            chunks,
            vec!["hello ".to_string(), "world".into(), "!".into()]
        );
    }
}
