//! In-memory [`PiTransport`] used by unit/integration tests so the
//! bee-agent layer (Queen, Scout, Worker, Guard) can be exercised without
//! spawning a real Pi subprocess.
//!
//! Compiled only under `#[cfg(any(test, feature = "test-mocks"))]`.
//!
//! ## Audit 7.2
//!
//! Pairs with [`super::transport::PiTransport`]. The mock is a thin shell
//! around two pieces of state:
//!
//! - a `broadcast::Sender<PiEvent>` that the test drives via `emit*`
//!   helpers; subscribers (typically `PiSession::collect_response*`) see
//!   the same stream they'd see from a real Pi process.
//! - a `Mutex<Vec<PiCommand>>` that records every `send_command` call so
//!   tests can assert *what* the agent sent and *in what order*.
//!
//! The mock also has knobs for the three failure paths tests care about:
//! [`MockRpcClient::close_stdout`] (stream EOF), [`MockRpcClient::crash`]
//! (synthetic `PiEvent::Error`), and [`MockRpcClient::fail_send`] (force
//! `send_command` to return `StdinClosed`).

#![cfg(any(test, feature = "test-mocks"))]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as SyncMutex};

use async_trait::async_trait;
use tokio::sync::{broadcast, Mutex};

use super::events::PiEvent;
use super::rpc::{PiCommand, PiRpcError};
use super::transport::PiTransport;

/// Broadcast channel capacity for the mock event stream.
///
/// Sized comfortably above any single test's expected emission count so we
/// never trip `broadcast::error::SendError(Lagged)` and lose events the
/// test then asserts on.
const MOCK_CHANNEL_CAPACITY: usize = 1024;

/// Test-only [`PiTransport`] backed by an in-memory broadcast channel and a
/// command log.
///
/// Clone-shareable via `Arc` (the constructor returns one). All methods
/// are inherently-`async` only where the trait requires it; the test
/// helpers (`emit*`, `sent_commands`, `crash`, `close_stdout`) are
/// synchronous so tests stay easy to read.
pub struct MockRpcClient {
    /// Authoritative broadcast sender. Held by `Arc` so `subscribe()`
    /// always yields a live receiver even if the test happened to spawn
    /// the session before subscribing.
    event_tx: broadcast::Sender<PiEvent>,
    /// Every command observed via `send_command`, in arrival order.
    /// `Mutex` rather than `RwLock` because the assertion side (read) and
    /// the agent side (push) never overlap in practice but we want the
    /// push to be cheap.
    sent: Mutex<Vec<PiCommand>>,
    /// Mutable liveness flag. `crash` / `close_stdout` flip this off so
    /// `is_alive` reflects the simulated state.
    alive: AtomicBool,
    /// When set, `send_command` returns `PiRpcError::StdinClosed` so tests
    /// can exercise the "Pi died mid-prompt" branch.
    fail_send: AtomicBool,
    /// Events `emit`'d before any subscriber existed. Production
    /// `PiRpcClient` doesn't need this because Pi is alive and emitting
    /// only *after* `PiSession::collect_response*` has subscribed; mock
    /// tests routinely pre-seed the script *before* `run_scout` /
    /// `run_worker` runs, so without a backlog those events would be
    /// silently dropped by `tokio::sync::broadcast` (new subscribers
    /// only see messages sent *after* they subscribed). The buffer is
    /// drained into the channel the first time `subscribe()` is called.
    ///
    /// Held under the same `SyncMutex` that gates `subscribe()` and
    /// `emit()` so the "no subscriber yet" check and the push happen
    /// atomically — no chance of an emit landing in the gap between
    /// `event_tx.receiver_count() == 0` and the subsequent push.
    pending: SyncMutex<Vec<PiEvent>>,
}

/// Build an `Arc<PiSession>` backed by a fresh [`MockRpcClient`] and
/// return both ends so the test can script Pi events / assert on the
/// commands the agent ultimately sent.
///
/// Test sequence is always:
///
/// ```ignore
/// let (session, mock) = mock_session("scout-1");
/// // Pre-load the model's reply...
/// mock.emit_text_chunk("### Plan\n...");
/// mock.emit_agent_end();
/// // ...then run the agent against `session`. PiSession::collect_response
/// // observes the scripted stream synchronously.
/// let result = run_scout(&session, &feature, dir, "").await.unwrap();
/// ```
pub fn mock_session(
    id: impl Into<String>,
) -> (
    std::sync::Arc<super::session::PiSession>,
    std::sync::Arc<MockRpcClient>,
) {
    let mock = MockRpcClient::new();
    let transport: std::sync::Arc<dyn PiTransport> = mock.clone();
    let session = std::sync::Arc::new(super::session::PiSession::new(id.into(), transport));
    (session, mock)
}

impl std::fmt::Debug for MockRpcClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MockRpcClient")
            .field("alive", &self.alive.load(Ordering::SeqCst))
            .field("subscribers", &self.event_tx.receiver_count())
            .finish()
    }
}

impl MockRpcClient {
    /// Build a fresh mock transport. Returns an `Arc` because that is what
    /// `PiSession::new` wants.
    pub fn new() -> Arc<Self> {
        let (event_tx, _) = broadcast::channel::<PiEvent>(MOCK_CHANNEL_CAPACITY);
        Arc::new(Self {
            event_tx,
            sent: Mutex::new(Vec::new()),
            alive: AtomicBool::new(true),
            fail_send: AtomicBool::new(false),
            pending: SyncMutex::new(Vec::new()),
        })
    }

    /// Push an arbitrary `PiEvent` onto the stream.
    ///
    /// If a receiver is already subscribed the event is sent immediately
    /// (mirroring production). If no receiver is subscribed yet, the event
    /// is parked in the `pending` backlog so the first call to
    /// `subscribe()` can replay it — without this `tokio::sync::broadcast`
    /// would silently drop the message (new subscribers only see messages
    /// sent *after* they subscribed).
    ///
    /// The check-and-push is performed under the same `pending` mutex
    /// `subscribe()` takes, so there is no race window where an event
    /// emitted just before subscribe slips through both branches.
    pub fn emit(&self, event: PiEvent) {
        let mut pending = self.pending.lock().expect("mock pending mutex poisoned");
        if self.event_tx.receiver_count() == 0 {
            pending.push(event);
        } else {
            // Drop the lock before sending so receivers that are already
            // dispatching their own callbacks can't deadlock against us.
            drop(pending);
            let _ = self.event_tx.send(event);
        }
    }

    /// Convenience: emit a single text-delta event. Used to script the
    /// model's streaming response one chunk at a time.
    pub fn emit_text_chunk(&self, text: impl Into<String>) {
        self.emit(PiEvent::TextDelta(text.into()));
    }

    /// Convenience: emit the terminal `AgentEnd` that
    /// `PiSession::collect_response*` waits for.
    pub fn emit_agent_end(&self) {
        self.emit(PiEvent::AgentEnd);
    }

    /// Simulate Pi closing stdout without a terminal event by pushing
    /// the synthetic error a mid-stream `collect_response*` would see.
    /// `alive` is left untouched so the *current* turn (the one whose
    /// `send_command` already shipped) still surfaces the close via
    /// `collect_response`'s `Error` branch — exactly like production
    /// where `pi/rpc.rs`'s stdout-reader fallback injects the error
    /// event before the subprocess monitor flips `alive`.
    ///
    /// If a test also needs subsequent `send_command` calls to fail
    /// (e.g. asserting that a follow-up prompt after the close cannot
    /// be queued), call [`Self::mark_process_dead`] in addition.
    pub fn close_stdout(&self) {
        self.emit(PiEvent::Error(
            "pi process stdout closed unexpectedly".to_string(),
        ));
    }

    /// Explicitly mark the simulated process as dead. After this,
    /// `is_alive()` returns `false` and `send_command` returns
    /// `StdinClosed` / `ProcessUnavailable`. Independent of
    /// [`Self::close_stdout`] / [`Self::crash`] which model the
    /// event-stream side only — production code paths and tests
    /// generally observe a close error *first* and only later see
    /// `send_command` fail.
    #[allow(dead_code)]
    pub fn mark_process_dead(&self) {
        self.alive.store(false, Ordering::SeqCst);
    }

    /// Simulate a crash error visible to a *mid-stream* `collect_response`.
    /// Emits a synthetic `PiEvent::Error` carrying `reason` but leaves
    /// `alive` untouched so any pending `send_command` (called via e.g.
    /// `PiSession::send_prompt`) still succeeds. This models the
    /// production shape where Pi has just started a turn, errors out, and
    /// `pi/rpc.rs` injects an `Error` event before the subprocess monitor
    /// has flipped `alive`.
    ///
    /// If you instead need to simulate "the process is dead, the next
    /// send_command should fail", use [`Self::close_stdout`] (which flips
    /// `alive` and emits the standard stdout-closed error).
    pub fn crash(&self, reason: impl Into<String>) {
        self.emit(PiEvent::Error(reason.into()));
    }

    /// Drain and return the full command log in arrival order.
    pub async fn sent_commands(&self) -> Vec<PiCommand> {
        self.sent.lock().await.clone()
    }

    /// Force the next (and every subsequent) `send_command` to return
    /// `PiRpcError::StdinClosed`. Used by the "stdin failure" path in the
    /// Scout test.
    pub fn fail_send(&self) {
        self.fail_send.store(true, Ordering::SeqCst);
    }
}

#[async_trait]
impl PiTransport for MockRpcClient {
    async fn send_command(&self, cmd: PiCommand) -> Result<(), PiRpcError> {
        if self.fail_send.load(Ordering::SeqCst) {
            return Err(PiRpcError::StdinClosed);
        }
        if !self.alive.load(Ordering::SeqCst) {
            // Mirror PiRpcClient's behaviour: if a Pi child has exited,
            // send_command yields StdinClosed when the simulated process
            // has been killed.
            return Err(PiRpcError::StdinClosed);
        }
        self.sent.lock().await.push(cmd);
        Ok(())
    }

    fn subscribe(&self) -> broadcast::Receiver<PiEvent> {
        // Hold `pending` while we subscribe AND drain so any concurrent
        // `emit()` either lands in the same backlog (and then in this
        // drain) or lands in the channel AFTER the receiver exists. No
        // event is ever lost or duplicated.
        let mut pending = self.pending.lock().expect("mock pending mutex poisoned");
        let rx = self.event_tx.subscribe();
        for event in pending.drain(..) {
            let _ = self.event_tx.send(event);
        }
        rx
    }

    async fn force_kill(&self) -> Result<(), PiRpcError> {
        self.alive.store(false, Ordering::SeqCst);
        Ok(())
    }

    fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn emit_and_subscribe_roundtrip() {
        let mock = MockRpcClient::new();
        let mut rx = mock.subscribe();
        mock.emit_text_chunk("hello");
        mock.emit_agent_end();
        let a = rx.recv().await.unwrap();
        let b = rx.recv().await.unwrap();
        match a {
            PiEvent::TextDelta(s) => assert_eq!(s, "hello"),
            other => panic!("expected TextDelta, got {:?}", other),
        }
        assert!(matches!(b, PiEvent::AgentEnd));
    }

    #[tokio::test]
    async fn send_command_records_in_order() {
        let mock = MockRpcClient::new();
        mock.send_command(PiCommand::Prompt {
            message: "one".into(),
            images: None,
        })
        .await
        .unwrap();
        mock.send_command(PiCommand::Abort {}).await.unwrap();
        let log = mock.sent_commands().await;
        assert_eq!(log.len(), 2);
        match &log[0] {
            PiCommand::Prompt { message, .. } => assert_eq!(message, "one"),
            other => panic!("unexpected {:?}", other),
        }
        assert!(matches!(log[1], PiCommand::Abort {}));
    }

    #[tokio::test]
    async fn crash_emits_error_without_flipping_alive() {
        // Audit 7.2: `crash()` simulates a mid-turn error event; the
        // mock stays "alive" so any pending `send_command` still
        // succeeds (mirrors the production race where Pi emits an
        // Error event before its subprocess monitor sees the exit).
        // Use `close_stdout` if you need alive == false.
        let mock = MockRpcClient::new();
        let mut rx = mock.subscribe();
        assert!(mock.is_alive());
        mock.crash("simulated");
        assert!(mock.is_alive(), "crash should NOT flip alive");
        match rx.recv().await.unwrap() {
            PiEvent::Error(msg) => assert_eq!(msg, "simulated"),
            other => panic!("expected Error, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn fail_send_returns_stdin_closed() {
        let mock = MockRpcClient::new();
        mock.fail_send();
        let err = mock
            .send_command(PiCommand::Abort {})
            .await
            .expect_err("should fail");
        assert!(matches!(err, PiRpcError::StdinClosed));
    }

    #[tokio::test]
    async fn close_stdout_pushes_synthetic_error_without_killing() {
        // Audit 7.2: `close_stdout` only emits the event so a
        // mid-stream `collect_response` can surface it; it does NOT
        // flip alive (so the in-flight `send_command` that's already
        // committed still succeeds, matching production).
        let mock = MockRpcClient::new();
        let mut rx = mock.subscribe();
        mock.close_stdout();
        assert!(mock.is_alive(), "close_stdout should not flip alive");
        match rx.recv().await.unwrap() {
            PiEvent::Error(msg) => assert!(msg.contains("stdout closed")),
            other => panic!("expected Error, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn mark_process_dead_blocks_subsequent_send_command() {
        let mock = MockRpcClient::new();
        assert!(mock.is_alive());
        mock.mark_process_dead();
        assert!(!mock.is_alive());
        let err = mock
            .send_command(PiCommand::Abort {})
            .await
            .expect_err("dead process should reject send_command");
        assert!(matches!(err, PiRpcError::StdinClosed));
    }

    #[tokio::test]
    async fn force_kill_flips_alive() {
        let mock = MockRpcClient::new();
        mock.force_kill().await.unwrap();
        assert!(!mock.is_alive());
    }

    #[tokio::test]
    async fn pid_default_is_none() {
        let mock = MockRpcClient::new();
        assert!(mock.pid().is_none());
    }
}
