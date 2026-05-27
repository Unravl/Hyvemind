//! Pi transport trait — abstraction over a live `PiRpcClient` so that the
//! swarm/agent layer (Queen, Scout, Worker, Guard) can be exercised against
//! an in-memory mock without spawning a real Pi subprocess.
//!
//! ## Audit 7.2
//!
//! Before this commit, [`crate::pi::session::PiSession`] held a concrete
//! `PiRpcClient` directly and there was no seam between agent code and the
//! Pi subprocess machinery. End-to-end tests for the bee agents had no way
//! to drive a Pi session without actually spawning Pi — so the swarm
//! orchestrator was effectively untested end-to-end.
//!
//! `PiTransport` is the minimal contract `PiSession` needs from anything
//! pretending to be a Pi RPC client. `PiRpcClient` implements it for free;
//! [`super::mock::MockRpcClient`] implements it in pure tokio (no
//! subprocess) so the same `PiSession` API can be driven from a unit test.
//!
//! ### Why these methods
//!
//! - `send_command` — what `PiSession::send_prompt` / `steer` / `follow_up`
//!   / `abort` / `set_model` / `set_thinking_level` ultimately call.
//! - `subscribe` — what `PiSession::collect_response*` and
//!   `subscribe_events` need to consume the live event stream.
//! - `shutdown` — graceful exit (Abort + 200 ms grace).
//! - `force_kill` — bounded kill sequence used by `PiSession::force_kill`
//!   and `PiManager::kill_session`.
//! - `is_alive` — fast (non-locking) liveness probe used by the eviction
//!   loop and `send_command` guards.
//! - `stderr_snapshot` — last-resort context for error reporting; the
//!   underlying `PiRpcClient` keeps it under an async mutex so this stays
//!   async on the trait too.
//! - `pid` — Audit 2.3 progress events tag every spawn/kill with the OS
//!   PID; `PiSession::pid()` forwards from the transport.
//!
//! ### Deviations from the source audit plan
//!
//! The plan listed five methods (`send_command`, `subscribe`, `shutdown`,
//! `is_alive`, `stderr_snapshot`). Two more were unavoidable to preserve
//! `PiSession`'s public behaviour:
//!
//! - `force_kill` — `PiSession::force_kill` would otherwise have to fall
//!   back to the polite `shutdown` path, which is a real behaviour change
//!   in stop / pause flows. Adding the method keeps the contract intact.
//! - `pid` — `PiSession::pid()` is called from Audit 2.3 progress events
//!   (`pi_session_spawned` / `pi_session_killed`). Falling back to `None`
//!   for tests is fine — the production path still returns the real PID.
//!
//! `stderr_snapshot` is kept `async` so the existing `tokio::sync::Mutex`
//! inside `PiRpcClient` doesn't need to be swapped for a `std::sync::Mutex`
//! (the buffer is locked from inside async stderr reader tasks).

use async_trait::async_trait;
use tokio::sync::broadcast;

use super::events::PiEvent;
use super::rpc::{PiCommand, PiRpcError};

/// Object-safe transport contract used by [`crate::pi::session::PiSession`].
///
/// Implementors are typically `Arc`-shared (`Arc<dyn PiTransport>`) so that
/// the session can be cloned cheaply alongside its forwarder tasks.
///
/// See module docs for the rationale behind each method.
#[async_trait]
pub trait PiTransport: Send + Sync + std::fmt::Debug {
    /// Send a single command to the underlying Pi process (or mock).
    ///
    /// Owned `PiCommand` rather than `&PiCommand` so callers can build the
    /// command inline without naming the variant twice — and so a mock
    /// implementation can stash the command in its log without an extra
    /// clone.
    async fn send_command(&self, cmd: PiCommand) -> Result<(), PiRpcError>;

    /// New broadcast receiver on the event stream.
    fn subscribe(&self) -> broadcast::Receiver<PiEvent>;

    /// Bounded force-kill: Abort + 1s grace + SIGKILL + 2s reap window.
    /// Production transport returns only after the process has exited (or
    /// the bounded 3s window has elapsed). Mock implementations may
    /// return immediately.
    async fn force_kill(&self) -> Result<(), PiRpcError>;

    /// Liveness probe. Returns `true` while the underlying process (or
    /// mock) is still considered alive.
    fn is_alive(&self) -> bool;

    /// OS process id, if known. Production transport returns the spawned
    /// child's PID captured at spawn time; mocks return `None`.
    fn pid(&self) -> Option<u32> {
        None
    }

    /// Returns a snapshot of all stderr output captured so far from the
    /// child process. The default implementation returns an empty string,
    /// which is appropriate for mock/in-memory transports.
    async fn stderr_snapshot(&self) -> String {
        String::new()
    }
}

#[async_trait]
impl PiTransport for super::rpc::PiRpcClient {
    async fn send_command(&self, cmd: PiCommand) -> Result<(), PiRpcError> {
        // Existing `PiRpcClient::send_command` takes `&PiCommand`. Forward
        // by reference so the production path stays a single allocation.
        super::rpc::PiRpcClient::send_command(self, &cmd).await
    }

    fn subscribe(&self) -> broadcast::Receiver<PiEvent> {
        super::rpc::PiRpcClient::subscribe(self)
    }

    async fn force_kill(&self) -> Result<(), PiRpcError> {
        super::rpc::PiRpcClient::force_kill_child(self).await
    }

    // NEW: forward stderr_snapshot to PiRpcClient's existing method
    async fn stderr_snapshot(&self) -> String {
        super::rpc::PiRpcClient::stderr_snapshot(self).await
    }

    fn is_alive(&self) -> bool {
        super::rpc::PiRpcClient::is_alive(self)
    }

    fn pid(&self) -> Option<u32> {
        super::rpc::PiRpcClient::pid(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    // Compile-time check: `Arc<dyn PiTransport>` is what `PiSession` holds.
    #[allow(dead_code)]
    fn _trait_is_object_safe(_: Arc<dyn PiTransport>) {}

    /// Runtime smoke: the mock transport satisfies the trait and can be
    /// coerced to `Arc<dyn PiTransport>`. This is the same coercion
    /// `PiManager::with_transport_factory` performs in production.
    #[tokio::test]
    async fn mock_satisfies_trait_via_arc_coercion() {
        let mock = super::super::mock::MockRpcClient::new();
        let transport: Arc<dyn PiTransport> = mock;
        assert!(transport.is_alive());
        assert!(transport.pid().is_none());
    }
}
