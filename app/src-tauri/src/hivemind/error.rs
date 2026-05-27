//! Typed errors for the Hivemind review orchestrator.
//!
//! The engine historically returned `anyhow::Error` for every failure mode,
//! which conflated *user-initiated cancellation* with *provider/runtime
//! failure* at the IPC boundary. The UI then showed "failed" for both cases,
//! which is wrong — pressing the Cancel button is not an error and should be
//! communicated to the user as "cancelled by user".
//!
//! [`OrchestratorError`] is intentionally tiny — just `Cancelled`. Anywhere
//! the engine returns `anyhow::Result<T>` we lift the discriminant by
//! attaching this enum as the error source (via `.context()` / direct
//! `Err(OrchestratorError::...)`) so the call-site wrapper in
//! `commands/hivemind.rs` can dispatch on it with
//! `e.downcast_ref::<OrchestratorError>()` and emit the correct event variant.

use thiserror::Error;

/// Discriminated outcome of a non-success exit from the review engine.
///
/// Construct with [`OrchestratorError::Cancelled`] when a `CancellationToken`
/// fires (the user pressed Stop / the parent task was aborted).
#[derive(Debug, Clone, Error)]
pub enum OrchestratorError {
    /// User-initiated cancellation. Not a failure — the user explicitly
    /// asked for the run to stop. UI should render this as a neutral
    /// "cancelled" state, not a red "failed" state.
    #[error("review cancelled")]
    Cancelled,
}

impl OrchestratorError {
    /// Returns `true` for [`OrchestratorError::Cancelled`].
    pub fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled)
    }
}

/// Inspect an `anyhow::Error` and decide whether it represents a
/// user-initiated cancellation. Used by the IPC layer to choose between
/// emitting a `"cancelled"` vs a `"failed"` event without losing the
/// underlying error message.
///
/// Falls back to a textual match on legacy `"cancelled"` strings so older
/// call sites that have not been migrated yet still classify correctly.
pub fn is_cancellation(err: &anyhow::Error) -> bool {
    if let Some(o) = err.downcast_ref::<OrchestratorError>() {
        return o.is_cancelled();
    }
    // Legacy fallback: any nested cancellation marker.
    for cause in err.chain() {
        let s = cause.to_string();
        if s.contains("review cancelled") || s.contains("hivemind run cancelled") {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;

    #[test]
    fn cancelled_variant_is_cancelled() {
        let e = OrchestratorError::Cancelled;
        assert!(e.is_cancelled());
        assert_eq!(e.to_string(), "review cancelled");
    }

    #[test]
    fn is_cancellation_detects_typed_variant() {
        let e: anyhow::Error = OrchestratorError::Cancelled.into();
        assert!(is_cancellation(&e));
    }

    #[test]
    fn is_cancellation_detects_typed_variant_under_context() {
        let e: anyhow::Error =
            anyhow::Error::new(OrchestratorError::Cancelled).context("during round 2");
        assert!(is_cancellation(&e));
    }

    #[test]
    fn is_cancellation_detects_legacy_string() {
        let e = anyhow!("hivemind run cancelled");
        assert!(is_cancellation(&e));
    }

    #[test]
    fn is_cancellation_returns_false_for_generic_error() {
        let e = anyhow!("provider timeout");
        assert!(!is_cancellation(&e));
    }
}
