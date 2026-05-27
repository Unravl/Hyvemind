//! Utility modules shared across subsystems.
//!
//! Currently exposes the [`supervise`] panic-safety harness; see that module
//! for how to wrap fire-and-forget `tokio::spawn` bodies so panics do not
//! silently kill long-running pipelines (item 2.12 of the architecture audit).

pub mod supervise;
