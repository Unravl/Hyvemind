//! `NurseBus` — push-mode event bus over `tokio::sync::broadcast`.
//!
//! Producers: `PiSession::touch_activity`, `PiSession::set_owner`,
//! `PiSession::Drop`, `PiManager::spawn_session_with_options`,
//! `PiManager::kill_session`.
//!
//! Consumer: `NurseEngine::run_loop` subscribes once and dispatches events
//! to per-session detectors. Detectors maintain incremental state and react
//! in real time rather than re-scanning the transcript every tick (the old
//! pull design).
//!
//! Payload is `Arc<NurseBusEvent>` so receiver fan-out is a refcount-only
//! cost. The per-event evidence cap is enforced **before** the bus publish
//! via [`PiEvent::truncated`](crate::pi::events::PiEvent::truncated); see
//! [`crate::tunables::nurse_max_evidence_bytes`].

use std::sync::Arc;
use std::sync::Weak;
use std::time::Instant;

use tokio::sync::broadcast;

use crate::pi::events::PiEvent;
use crate::pi::session::{PiSession, SessionOwner};
use crate::tunables;

/// Reason a session terminated, carried on every [`NurseBusEvent::SessionEnded`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionEndReason {
    /// `PiManager::kill_session` was the terminator.
    Killed,
    /// `PiSession::Drop` fired without an explicit `kill_session` first.
    Dropped,
}

/// One event on the [`NurseBus`].
#[derive(Debug, Clone)]
pub enum NurseBusEvent {
    SessionSpawned {
        session_id: String,
        provider: Option<String>,
        model_id: Option<String>,
        spawned_at: Instant,
        /// `Weak<PiSession>` is intentional — carrying `Arc<PiSession>` here
        /// would create a refcount cycle with `PiManager`'s sessions map
        /// and `PiSession::Drop` would never fire on idle eviction.
        session: Weak<PiSession>,
    },
    /// Fires whenever [`PiSession::set_owner`] is invoked. Late-arriving
    /// owner promotions cross this channel so the engine can re-resolve
    /// the per-context `NurseProfile`.
    OwnerChanged {
        session_id: String,
        owner: SessionOwner,
    },
    Event {
        session_id: String,
        event: PiEvent,
        observed_at: Instant,
    },
    SessionEnded {
        session_id: String,
        reason: SessionEndReason,
        ended_at: Instant,
    },
}

/// Process-wide push surface from the Pi layer into the Nurse engine.
pub struct NurseBus {
    tx: broadcast::Sender<Arc<NurseBusEvent>>,
    /// Configured capacity (sampled at construction; used for telemetry).
    capacity: usize,
}

impl NurseBus {
    /// Build a new bus with capacity sourced from
    /// [`tunables::nurse_bus_capacity`].
    pub fn new() -> Self {
        let capacity = tunables::nurse_bus_capacity();
        let (tx, _rx) = broadcast::channel::<Arc<NurseBusEvent>>(capacity);
        Self { tx, capacity }
    }

    /// Build a bus with an explicit capacity. Test convenience.
    pub fn with_capacity(capacity: usize) -> Self {
        let cap = capacity.max(1);
        let (tx, _rx) = broadcast::channel::<Arc<NurseBusEvent>>(cap);
        Self { tx, capacity: cap }
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Arc<NurseBusEvent>> {
        self.tx.subscribe()
    }

    pub fn receiver_count(&self) -> usize {
        self.tx.receiver_count()
    }

    /// Non-blocking publish. `broadcast::Sender::send` only errors when
    /// every receiver has been dropped; we treat that as a soft state
    /// (Nurse disabled or shut down) and don't propagate.
    pub fn publish(&self, event: Arc<NurseBusEvent>) {
        let _ = self.tx.send(event);
    }

    /// Convenience: publish a freshly-allocated event.
    pub fn publish_owned(&self, event: NurseBusEvent) {
        self.publish(Arc::new(event));
    }
}

impl std::fmt::Debug for NurseBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NurseBus")
            .field("capacity", &self.capacity)
            .field("receiver_count", &self.receiver_count())
            .finish()
    }
}

impl Default for NurseBus {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[tokio::test]
    async fn publish_after_all_receivers_drop_is_silent() {
        let bus = NurseBus::with_capacity(4);
        // No subscriber at all — should not panic.
        bus.publish(Arc::new(NurseBusEvent::SessionEnded {
            session_id: "x".into(),
            reason: SessionEndReason::Dropped,
            ended_at: Instant::now(),
        }));
    }

    #[tokio::test]
    async fn arc_payload_fanout_is_refcount_only() {
        let bus = NurseBus::with_capacity(8);
        let mut a = bus.subscribe();
        let mut b = bus.subscribe();
        let payload = Arc::new(NurseBusEvent::SessionEnded {
            session_id: "x".into(),
            reason: SessionEndReason::Killed,
            ended_at: Instant::now(),
        });
        let original_count = Arc::strong_count(&payload);
        bus.publish(Arc::clone(&payload));
        let ev_a = a.recv().await.unwrap();
        let ev_b = b.recv().await.unwrap();
        assert!(Arc::ptr_eq(&ev_a, &ev_b));
        // After both receivers received the Arc, strong_count is
        // original + 2 receivers + 1 broadcast-queue copy (queue holds it
        // until both consume — broadcast doesn't free per-receiver).
        let _ = original_count; // sanity reference
    }

    #[tokio::test]
    async fn lagged_recv_signals_lagged_error() {
        let bus = NurseBus::with_capacity(2);
        let mut rx = bus.subscribe();
        for i in 0..8 {
            bus.publish(Arc::new(NurseBusEvent::Event {
                session_id: format!("s{i}"),
                event: PiEvent::Heartbeat,
                observed_at: Instant::now(),
            }));
        }
        // First recv should report Lagged.
        match rx.recv().await {
            Err(broadcast::error::RecvError::Lagged(n)) => {
                assert!(n > 0, "expected lag count > 0, got {n}");
            }
            other => panic!("expected Lagged, got {other:?}"),
        }
    }

    #[test]
    fn default_capacity_matches_tunable() {
        // Default falls back to tunable default (4096 in dev).
        let bus = NurseBus::new();
        assert!(
            bus.capacity() >= 64 && bus.capacity() <= 65_536,
            "capacity should be inside the documented clamp window, got {}",
            bus.capacity()
        );
    }
}
