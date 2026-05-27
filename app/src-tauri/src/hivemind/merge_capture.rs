// Internal name — surfaces as "Tasks" in the UI. See PRODUCT.md §3.
//! Per-(job, round) durable capture of a hivemind merge stream.
//!
//! When the frontend orchestrates a merge phase, it registers a
//! [`MergeCapture`] keyed by the merge Pi session id. The chat-event chunk
//! emitter (in `commands/chat.rs`) looks up the capture for each streaming
//! session and appends the chunk to a dedicated text file under
//! `~/.hyvemind/reviews/{review_id}/merge-r{N}.txt`.
//!
//! On host-process death the file survives — the startup sweep in
//! [`crate::hivemind::store::HivemindStore::sweep_interrupted_merges`] then
//! flips any `running` row to `interrupted` so the UI can offer a resume
//! affordance.
//!
//! The struct is cheaply clonable via the surrounding `Arc<MergeCapture>`
//! the chat handler stores in [`crate::state::app_state::AppState`].

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::pi::chunk_sink::ChunkSink;
use crate::state::sync::SyncRwLock;

/// Tracks an in-flight merge stream for a single (job_id, round) pair.
///
/// The writer is wrapped in `Arc<Mutex<BufWriter<File>>>` (synchronous
/// `std` types) so each chunk can be appended inline from the streaming
/// callback in `commands/chat.rs`. Writes must land in true arrival order;
/// previously the chunks were dispatched to `tokio::spawn` tasks which
/// raced for the lock and produced interleaved bytes on disk.
/// `bytes_written` is updated lock-free as each chunk lands and is read
/// out at completion to record the final `output_len` in SQLite.
pub struct MergeCapture {
    pub job_id: String,
    pub round: i64,
    pub output_path: PathBuf,
    pub session_id: String,
    pub writer: Arc<Mutex<BufWriter<File>>>,
    pub bytes_written: Arc<AtomicU64>,
    /// Wall-clock instant of the most recent chunk write. Used by the
    /// maintenance sweep in `pi::eviction` to TTL out abandoned
    /// captures (frontend crashed mid-merge, etc.).
    last_chunk_at: Arc<Mutex<Instant>>,
}

impl MergeCapture {
    /// Update the last-chunk timestamp. Call after each successful
    /// write to disk.
    pub fn touch(&self) {
        if let Ok(mut t) = self.last_chunk_at.lock() {
            *t = Instant::now();
        }
    }

    /// Returns the wall-clock instant of the most recent chunk write
    /// (or the capture's creation time if no chunk has landed yet).
    pub fn last_chunk_at(&self) -> Instant {
        match self.last_chunk_at.lock() {
            Ok(g) => *g,
            Err(poisoned) => *poisoned.into_inner(),
        }
    }
}

/// Manual `Debug` impl — `BufWriter<File>` does not implement `Debug`, so
/// we summarise the capture by its identity instead of dumping the writer.
/// Required by the `ChunkSink: Debug` bound.
impl fmt::Debug for MergeCapture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MergeCapture")
            .field("job_id", &self.job_id)
            .field("round", &self.round)
            .field("session_id", &self.session_id)
            .field("output_path", &self.output_path)
            .field("bytes_written", &self.bytes_written.load(Ordering::Relaxed))
            .finish()
    }
}

/// `MergeCapture` is the canonical [`ChunkSink`] used by the streaming
/// chunk forwarder in `pi/session.rs`. The trait impl mirrors the inline
/// write path the rest of the codebase already exercises (sync std I/O,
/// flush per chunk, byte counter for the SQLite `output_len` column).
impl ChunkSink for MergeCapture {
    fn write_chunk(&self, chunk: &str) {
        let bytes = chunk.as_bytes();
        if let Ok(mut w) = self.writer.lock() {
            if w.write_all(bytes).is_ok() {
                let _ = w.flush();
                self.bytes_written
                    .fetch_add(bytes.len() as u64, Ordering::Relaxed);
                self.touch();
            }
        }
    }
}

impl MergeCapture {
    /// Open (or truncate) the capture file at `path`.
    ///
    /// A fresh attempt always starts clean: the unique index on
    /// `(job_id, round_number)` in the `merge_runs` table guarantees that
    /// retries replace the prior row, so the on-disk file should match the
    /// new attempt's contents — not be appended to.
    pub async fn open(path: &Path, job_id: String, round: i64, session_id: String) -> Result<Self> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.with_context(|| {
                format!("failed to create merge capture dir {}", parent.display())
            })?;
        }

        let path_buf = path.to_path_buf();
        let file = tokio::task::spawn_blocking(move || {
            OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&path_buf)
        })
        .await
        .map_err(|e| anyhow::anyhow!("spawn_blocking join failed: {}", e))?
        .with_context(|| format!("failed to open merge capture file {}", path.display()))?;

        Ok(Self {
            job_id,
            round,
            output_path: path.to_path_buf(),
            session_id,
            writer: Arc::new(Mutex::new(BufWriter::new(file))),
            bytes_written: Arc::new(AtomicU64::new(0)),
            last_chunk_at: Arc::new(Mutex::new(Instant::now())),
        })
    }
}

/// Type alias for the merge-capture registry the rest of the codebase
/// stores in `AppState`. Keyed by Pi session id (the value the streaming
/// closure looks up).
pub type MergeCaptureRegistry = Arc<SyncRwLock<HashMap<String, Arc<MergeCapture>>>>;

/// Default TTL used by [`sweep_idle_captures`] when callers do not have a
/// reason to pick something else. Matches the value the eviction loop
/// used to hard-code before audit 6.2.
pub const DEFAULT_MERGE_CAPTURE_TTL: Duration = Duration::from_secs(30 * 60);

/// Snapshot-then-act sweep over `registry`: drops entries whose Pi
/// session is no longer alive (not in `live_session_ids`) or whose last
/// chunk write is older than `ttl`. Returns the number of entries
/// removed.
///
/// The function used to live in `pi/eviction.rs`; moving it into
/// `hivemind/` keeps the `pi/ → hivemind/` arrow gone (audit 6.2) so the
/// maintenance loop only needs to know about generic Pi state. The
/// caller (typically the lib-level wiring that owns the registry) is
/// responsible for invoking this on whatever cadence makes sense — the
/// process maintenance loop runs every 30s and that is the only current
/// scheduling. A poisoned lock is treated the same as a healthy one
/// (we recover via `into_inner`) because losing the sweep window for
/// one tick is much cheaper than panicking the maintenance task.
pub fn sweep_idle_captures(
    registry: &MergeCaptureRegistry,
    live_session_ids: &HashSet<String>,
    ttl: Duration,
) -> usize {
    let now = Instant::now();

    // Snapshot candidate stale entries under the read lock to keep the
    // critical section short.
    let candidates: Vec<String> = {
        let map = match registry.read() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        map.iter()
            .filter_map(|(sid, cap)| {
                let age_stale = now.duration_since(cap.last_chunk_at()) > ttl;
                let dead_session = !live_session_ids.contains(sid);
                if age_stale || dead_session {
                    Some(sid.clone())
                } else {
                    None
                }
            })
            .collect()
    };

    if candidates.is_empty() {
        return 0;
    }

    // Reacquire as a write lock and re-check staleness before removing.
    let mut map = match registry.write() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    let now = Instant::now();
    let mut removed = 0usize;
    for sid in &candidates {
        let still_stale = map
            .get(sid)
            .map(|cap| {
                now.duration_since(cap.last_chunk_at()) > ttl || !live_session_ids.contains(sid)
            })
            .unwrap_or(false);
        if still_stale && map.remove(sid).is_some() {
            removed += 1;
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::Ordering;
    use tempfile::TempDir;

    /// Mirrors the inline-write code path in `commands/chat.rs::send_message`
    /// so the test exercises the exact pattern the streaming callback uses.
    fn write_chunk_inline(cap: &MergeCapture, text: &str) {
        let bytes = text.as_bytes();
        let mut w = cap.writer.lock().expect("writer lock poisoned");
        if w.write_all(bytes).is_ok() {
            let _ = w.flush();
            cap.bytes_written
                .fetch_add(bytes.len() as u64, Ordering::Relaxed);
        }
    }

    #[tokio::test]
    async fn test_capture_writes_chunks_to_file() {
        let tmp = TempDir::new().expect("tempdir");
        let path = tmp.path().join("hmr-x").join("merge-r1.txt");

        let capture = MergeCapture::open(&path, "job-abc".to_string(), 1, "sess-1".to_string())
            .await
            .expect("open capture");

        let chunks = ["hello ", "world", "!"];
        for chunk in chunks {
            write_chunk_inline(&capture, chunk);
        }

        // Flush the BufWriter explicitly so contents are visible on disk.
        {
            let mut w = capture.writer.lock().expect("writer lock poisoned");
            w.flush().expect("final flush");
        }

        let contents = tokio::fs::read_to_string(&path).await.expect("read back");
        assert_eq!(contents, "hello world!");
        assert_eq!(capture.bytes_written.load(Ordering::Relaxed), 12);
    }

    /// Reproduces the original bug shape: a stream of many small adjacent
    /// chunks must land on disk in the exact order they arrived. With the
    /// previous `tokio::spawn`-per-chunk implementation the scheduler could
    /// reorder writes (small adjacent chunks could end up interleaved on
    /// disk). Driving writes inline against the synchronous mutex preserves
    /// order by construction.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_capture_preserves_chunk_order_under_contention() {
        let tmp = TempDir::new().expect("tempdir");
        let path = tmp.path().join("hmr-y").join("merge-r1.txt");

        let capture = Arc::new(
            MergeCapture::open(&path, "job-xyz".to_string(), 1, "sess-2".to_string())
                .await
                .expect("open capture"),
        );

        // Produce 200 distinguishable chunks. Each chunk is short and
        // adjacent in the stream — exactly the shape that triggered the
        // interleave bug when small chunks arrived back-to-back.
        let chunks: Vec<String> = (0..200).map(|i| format!("[{:04}]", i)).collect();
        let expected: String = chunks.concat();

        // Drive writes from a separate spawned task to mimic the real
        // streaming closure being invoked from inside an async runtime;
        // the writer task itself must remain serial.
        let cap_for_writer = capture.clone();
        let chunks_for_writer = chunks.clone();
        tokio::spawn(async move {
            for chunk in &chunks_for_writer {
                write_chunk_inline(&cap_for_writer, chunk);
            }
        })
        .await
        .expect("writer task");

        // Final flush, then read back.
        {
            let mut w = capture.writer.lock().expect("writer lock poisoned");
            w.flush().expect("final flush");
        }

        let contents = tokio::fs::read_to_string(&path).await.expect("read back");
        assert_eq!(
            contents, expected,
            "chunks must land on disk in arrival order"
        );
        assert_eq!(
            capture.bytes_written.load(Ordering::Relaxed) as usize,
            expected.len()
        );
    }

    // -- Audit 6.2: ChunkSink impl + sweep_idle_captures --------------------

    /// `MergeCapture::write_chunk` (via the `ChunkSink` trait) must
    /// produce byte-identical output to the inline-write code path used
    /// in `commands/chat.rs`. This guards against the impl drifting.
    #[tokio::test]
    async fn chunk_sink_impl_writes_chunks_to_disk_in_order() {
        let tmp = TempDir::new().expect("tempdir");
        let path = tmp.path().join("hmr-cs").join("merge-r1.txt");

        let capture = MergeCapture::open(&path, "job-cs".into(), 1, "sess-cs".into())
            .await
            .expect("open capture");

        // Route through the trait surface, not the inline helper.
        let sink: &dyn ChunkSink = &capture;
        sink.write_chunk("alpha ");
        sink.write_chunk("beta ");
        sink.write_chunk("gamma");

        let contents = tokio::fs::read_to_string(&path).await.expect("read back");
        assert_eq!(contents, "alpha beta gamma");
        assert_eq!(
            capture.bytes_written.load(Ordering::Relaxed) as usize,
            "alpha beta gamma".len()
        );
    }

    /// Registering a capture, then calling `sweep_idle_captures` with no
    /// live session ids and a 0-duration TTL, must remove the entry.
    /// This is the path the lib-level wiring exercises every maintenance
    /// tick once a Pi session has been killed.
    #[tokio::test]
    async fn sweep_idle_captures_removes_captures_for_dead_sessions() {
        let tmp = TempDir::new().expect("tempdir");
        let path = tmp.path().join("hmr-sw").join("merge-r1.txt");
        let capture = Arc::new(
            MergeCapture::open(&path, "job-sw".into(), 1, "sess-dead".into())
                .await
                .expect("open capture"),
        );

        let registry: MergeCaptureRegistry = Arc::new(SyncRwLock::new(HashMap::new()));
        registry
            .write()
            .unwrap()
            .insert("sess-dead".to_string(), capture.clone());

        // Empty live-session set: the registered entry's session is "dead".
        let live: HashSet<String> = HashSet::new();
        let removed = sweep_idle_captures(&registry, &live, Duration::from_secs(60 * 60));
        assert_eq!(removed, 1);
        assert!(registry.read().unwrap().get("sess-dead").is_none());
    }

    /// Live sessions whose last chunk landed inside the TTL window must
    /// survive the sweep.
    #[tokio::test]
    async fn sweep_idle_captures_keeps_fresh_live_captures() {
        let tmp = TempDir::new().expect("tempdir");
        let path = tmp.path().join("hmr-keep").join("merge-r1.txt");
        let capture = Arc::new(
            MergeCapture::open(&path, "job-keep".into(), 1, "sess-live".into())
                .await
                .expect("open capture"),
        );
        // Force a recent touch.
        capture.touch();

        let registry: MergeCaptureRegistry = Arc::new(SyncRwLock::new(HashMap::new()));
        registry
            .write()
            .unwrap()
            .insert("sess-live".to_string(), capture);

        let mut live: HashSet<String> = HashSet::new();
        live.insert("sess-live".to_string());
        let removed = sweep_idle_captures(&registry, &live, Duration::from_secs(60 * 60));
        assert_eq!(removed, 0);
        assert!(registry.read().unwrap().get("sess-live").is_some());
    }
}
