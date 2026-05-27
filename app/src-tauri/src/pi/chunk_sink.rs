// Internal name — surfaces as "Tasks" in the UI. See PRODUCT.md §3.
//! A thin trait abstraction for downstream consumers of streaming Pi text
//! chunks (text/thinking deltas).
//!
//! The `pi/` subtree must not depend on any higher-level subsystem
//! (`hivemind/`, `core/`, etc.). Concrete sinks live in those subsystems
//! and implement [`ChunkSink`]; `pi/` only sees the trait. This is the
//! piece that lets `hivemind::merge_capture::MergeCapture` receive every
//! text/thinking delta without forcing `pi/` to know about hivemind.

/// Sink for streaming text chunks observed during a Pi session.
///
/// Implementations are expected to be cheap to clone via `Arc` and safe
/// to call from inside the streaming closure (which is invoked once per
/// `PiEvent::TextDelta` / `PiEvent::ThinkingDelta`). Writes must be
/// non-blocking enough not to stall the broadcast bus; `MergeCapture`
/// keeps a synchronous `BufWriter` behind a `Mutex` and treats each
/// chunk as an inline write — see `hivemind/merge_capture.rs` for the
/// canonical implementation.
pub trait ChunkSink: Send + Sync + std::fmt::Debug {
    /// Append `chunk` to the sink. Implementations decide how to handle
    /// transient I/O errors (typically: swallow + log).
    fn write_chunk(&self, chunk: &str);
}
