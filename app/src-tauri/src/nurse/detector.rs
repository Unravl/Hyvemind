//! Detector trait + per-detector context + registry.
//!
//! Detectors are the bee-colony's senses. Every `Detector` observes the
//! per-session [`PiEvent`](crate::pi::events::PiEvent) stream and emits
//! [`SignalDelta`]s; the engine applies them to a per-session
//! [`SessionHealth`](crate::nurse::health::SessionHealth) and runs the
//! three-tier decision pipeline.
//!
//! Detector state is opaque per-detector data (`Box<dyn Any + Send + Sync>`)
//! stored on `SessionState`. The engine never inspects it; the detector
//! downcasts via `ctx.state.downcast_mut::<MyState>()` at the top of every
//! `observe` / `tick`. Each registered detector gets a fresh state from
//! `on_session_started` whenever a new session is observed.

use std::any::Any;
use std::sync::Weak;
use std::time::Instant;

use smallvec::SmallVec;

use crate::nurse::config::{NurseProfile, ProfileConfig};
use crate::nurse::health::Signal;
use crate::nurse::snapshot::{ProviderStateSnapshot, TunableDef};
use crate::pi::events::PiEvent;
use crate::pi::session::PiSession;

/// Output of an `observe` / `tick` invocation.
pub enum SignalDelta {
    Raise(Signal),
    Clear {
        detector: &'static str,
        dedup_key: String,
    },
}

/// Classification of detector tick frequency / cost. Slow detectors are
/// dispatched on a dedicated probe task so a blocking syscall cannot bog
/// down the engine's main event loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickKind {
    Fast,
    Slow,
}

/// Per-tick context handed to every detector. The detector's mutable state
/// is exposed as `&'a mut dyn Any`; downcast via
/// `ctx.state.downcast_mut::<MyState>().expect("state shape mismatch")`.
pub struct DetectorContext<'a> {
    /// Session this detector is observing. Use the engine's `try_upgrade`
    /// helper rather than calling `.upgrade()` directly so a failed upgrade
    /// returns a synthetic `session_gone_unobserved` signal as data, which
    /// the engine applies after dropping any held locks.
    pub session: &'a Weak<PiSession>,
    /// The detector's opaque per-session state.
    pub state: &'a mut dyn Any,
    /// Monotonic clock for detector-internal duration math.
    pub now: Instant,
    /// Epoch milliseconds for comparison against [`PiSession`] wall-clock
    /// timestamps (e.g. `PiSession::last_text_event_ms`).
    pub now_wall_ms: u64,
    pub profile: NurseProfile,
    /// Resolved per-profile config the engine snapshotted for this tick.
    /// Detectors read their per-detector tuning from here
    /// (e.g. `ctx.profile_config.stall.stalled_secs`) so user edits made
    /// via the Profiles UI take effect on the next tick. Falls back to
    /// `ProfileConfig::default_for(profile)` when the user hasn't saved
    /// any overrides — never `None`.
    pub profile_config: &'a ProfileConfig,
    /// Snapshot of provider breaker states taken once per engine tick.
    pub provider_state: &'a ProviderStateSnapshot,
    /// Cached metadata captured on `SessionSpawned`.
    pub provider: Option<&'a str>,
    pub model_id: Option<&'a str>,
}

/// Per-session detector contract.
///
/// Implementations MUST NOT mutate `engine.sessions` directly. Detectors
/// operate exclusively on the `&mut SessionState` exposed via `ctx.state`;
/// any cross-session effect happens as a `SignalDelta` that the engine
/// applies after dropping the sessions lock. Breaking this rule
/// reintroduces the deadlock the periodic-sweep code was specifically
/// rewritten to avoid.
///
/// Every raised [`Signal`] MUST populate `evidence` with everything an
/// operator (or a future Claude Code session reading
/// `signals/{session_id}.jsonl` 30 days later) needs to understand the
/// raise without reading source code. This is the
/// observability-completeness invariant.
pub trait Detector: Send + Sync + 'static {
    fn name(&self) -> &'static str;

    /// One-line human description rendered in the Detector Activity tab.
    fn description(&self) -> &'static str {
        ""
    }

    /// Construct a fresh detector-internal state for this session. Called
    /// the first time the engine sees the session (either via
    /// `SessionSpawned` or `reconcile_from_pi_manager`).
    fn on_session_started(&self, _ctx: &DetectorContext<'_>) -> Box<dyn Any + Send + Sync> {
        Box::new(())
    }

    /// Called once per detector when the session terminates. Lets the
    /// detector flush per-session bookkeeping (open tool_call_id map,
    /// MinHash sketches) before the engine removes the entry.
    fn on_session_ended(&self, _ctx: &DetectorContext<'_>) {}

    /// Per-event hook. Returns a small vector of signal deltas — `Raise`
    /// or `Clear` of one or more `(detector, dedup_key)` pairs.
    fn observe(
        &self,
        _event: &PiEvent,
        _ctx: &mut DetectorContext<'_>,
    ) -> SmallVec<[SignalDelta; 2]> {
        SmallVec::new()
    }

    /// Periodic hook invoked at the engine's tick cadence. Default is
    /// `Fast`; detectors that need to do I/O or async-bridge work should
    /// override `tick_kind()` to `Slow` so they run on the probe task.
    fn tick(&self, _ctx: &mut DetectorContext<'_>) -> SmallVec<[SignalDelta; 2]> {
        SmallVec::new()
    }

    fn tick_kind(&self) -> TickKind {
        TickKind::Fast
    }

    /// User-tunable knobs surfaced on the Profiles tab. The default
    /// returns an empty vec so detectors with no tunables cost nothing
    /// in the auto-generated UI.
    fn config_schema(&self) -> Vec<TunableDef> {
        Vec::new()
    }
}

/// Owned collection of registered detectors. Held by the engine.
pub struct DetectorRegistry {
    detectors: Vec<Box<dyn Detector>>,
}

impl DetectorRegistry {
    pub fn new() -> Self {
        Self {
            detectors: Vec::new(),
        }
    }

    pub fn register<D: Detector>(&mut self, detector: D) {
        self.detectors.push(Box::new(detector));
    }

    pub fn iter(&self) -> impl Iterator<Item = &dyn Detector> {
        self.detectors.iter().map(|d| d.as_ref())
    }

    pub fn len(&self) -> usize {
        self.detectors.len()
    }

    pub fn is_empty(&self) -> bool {
        self.detectors.is_empty()
    }
}

impl Default for DetectorRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nurse::config::NurseProfile;
    use crate::nurse::snapshot::ProviderStateSnapshot;

    struct DummyDetector;
    impl Detector for DummyDetector {
        fn name(&self) -> &'static str {
            "dummy"
        }
    }

    #[test]
    fn registry_registers_and_iterates() {
        let mut reg = DetectorRegistry::new();
        assert!(reg.is_empty());
        reg.register(DummyDetector);
        assert_eq!(reg.len(), 1);
        assert_eq!(reg.iter().next().unwrap().name(), "dummy");
    }

    #[test]
    fn default_observe_and_tick_emit_nothing() {
        let d = DummyDetector;
        let weak: Weak<PiSession> = Weak::new();
        let mut state: Box<dyn Any + Send + Sync> = Box::new(());
        let provider_state = ProviderStateSnapshot::default();
        let profile_config = ProfileConfig::default_for(NurseProfile::Default);
        let mut ctx = DetectorContext {
            session: &weak,
            state: state.as_mut(),
            now: Instant::now(),
            now_wall_ms: 0,
            profile: NurseProfile::Default,
            profile_config: &profile_config,
            provider_state: &provider_state,
            provider: None,
            model_id: None,
        };
        let observed = d.observe(&PiEvent::Heartbeat, &mut ctx);
        assert!(observed.is_empty());
        let ticked = d.tick(&mut ctx);
        assert!(ticked.is_empty());
    }
}
