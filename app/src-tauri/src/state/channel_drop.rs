//! Rate-limited drop-warning counters for bounded mpsc channels.
//!
//! Bounded channels (`mpsc::channel(N)`) can return `TrySendError::Full`
//! when a slow consumer can't keep up with a fast producer. Logging a warn
//! per drop would itself flood the log, so this helper coalesces drops into
//! a single warn message per (named) counter per 5 seconds.
//!
//! Usage:
//! ```ignore
//! use crate::state::channel_drop::DropWarner;
//! static WARN: DropWarner = DropWarner::new("swarm_activity");
//! if tx.try_send(payload).is_err() {
//!     WARN.note_drop();
//! }
//! ```

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

/// Window between successive warn lines. Drops inside the window are
/// counted; the next attempted log after the window emits the running total
/// and resets the counter.
const WARN_WINDOW_SECS: i64 = 5;

/// Tracks accumulated channel drops for a single named producer and emits a
/// rate-limited `tracing::warn!` line. Construct as a `static` so it lives
/// for the whole process and the counter is shared across all clones of the
/// producer.
pub struct DropWarner {
    /// Human-readable channel name (used in the warn message and to make
    /// log greps easy).
    name: &'static str,
    /// Total drops observed since the last warn line was emitted.
    dropped_in_window: AtomicU64,
    /// Last warn emission time, as Unix epoch seconds. `i64::MIN` means
    /// "never logged". An atomic CAS picks one thread per window to emit
    /// the warn line — without it we'd race and double-log.
    last_warn_epoch_secs: AtomicI64,
}

impl DropWarner {
    /// Create a new drop-warner counter. Intended to be used as a `static`.
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            dropped_in_window: AtomicU64::new(0),
            last_warn_epoch_secs: AtomicI64::new(i64::MIN),
        }
    }

    /// Record one dropped message. Emits a rate-limited warn line at most
    /// once per `WARN_WINDOW_SECS`, including the running count of drops
    /// in the window.
    pub fn note_drop(&self) {
        // Bump the in-window counter unconditionally; the eventual warn
        // emission consumes-and-resets it below.
        self.dropped_in_window.fetch_add(1, Ordering::Relaxed);
        let now_secs = chrono::Utc::now().timestamp();
        let last = self.last_warn_epoch_secs.load(Ordering::Relaxed);
        // `last == MIN` means "never logged" — always allow the first
        // warn through so a single drop isn't invisible forever.
        let due = last == i64::MIN || now_secs.saturating_sub(last) >= WARN_WINDOW_SECS;
        if !due {
            return;
        }
        // Single-writer election via CAS — only the thread that successfully
        // swaps `last` actually emits + resets the counter.
        if self
            .last_warn_epoch_secs
            .compare_exchange(last, now_secs, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
        {
            let dropped = self.dropped_in_window.swap(0, Ordering::Relaxed);
            // `dropped` is the authoritative count we're reporting. Use it,
            // not the pre-increment `count`, so the number matches what the
            // counter was reset by.
            tracing::warn!(
                channel = self.name,
                dropped,
                window_secs = WARN_WINDOW_SECS,
                "bounded mpsc channel full — dropped messages; consumer is too slow"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warner_counts_drops_without_panicking() {
        // Smoke test only: we can't easily assert on tracing output here.
        // Make sure `note_drop` is safe to call many times and that the
        // counter increments + resets behave under contention-free use.
        static W: DropWarner = DropWarner::new("test_channel");
        for _ in 0..1000 {
            W.note_drop();
        }
        // Counter may have been reset by the rate-limited emit; either way
        // it should never panic and should be a small bounded value.
        let v = W.dropped_in_window.load(Ordering::Relaxed);
        assert!(v <= 1000);
    }
}
