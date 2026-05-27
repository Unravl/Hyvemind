//! Per-swarm append-only swarm-activity transcript.
//!
//! Mirrors `state/progress.rs` in shape: a schema-versioned JSONL file with
//! a truncation-tolerant reader. Each line is one `swarm-activity` Tauri
//! event payload, augmented with a monotonic per-swarm `seq` number so the
//! frontend can ask "give me everything after seq N" on mount and replay
//! the history into the SwarmControl panel.
//!
//! # Why a separate file from `progress_log.jsonl`?
//!
//! `progress_log.jsonl` is the swarm's structural lifecycle log (one event
//! per feature transition). `activity_log.jsonl` is the firehose: every
//! agent text/thinking delta after the 50ms/256-byte coalescer, every tool
//! call, every agent start/end. Two-orders-of-magnitude difference in
//! volume — mixing them would make `rebuild_state` quadratic in delta
//! count.
//!
//! # Durability
//!
//! Writes are `BufWriter`-buffered; the caller drives `flush_and_sync()`
//! per channel drain (~50ms cadence in `commands/swarms.rs`). We do NOT
//! fsync per write — the coalescer already batches at that cadence and
//! the persistence cost would dominate the live stream. Crash-loss window
//! is therefore bounded at ~50ms of in-flight deltas, which is the same
//! window the frontend would have missed during the coalescer hold anyway.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Seek, SeekFrom, Write};
use std::path::Path;
use tracing::warn;

/// Current schema version emitted by all new logs.
pub const ACTIVITY_LOG_SCHEMA_VERSION: u32 = 1;

/// Header line written as the very first JSONL record of a fresh
/// `activity_log.jsonl`. Carries the schema version so future readers can
/// negotiate format changes. Header is also valid JSON; the reader checks
/// for it before treating a parse miss as a malformed-line warning.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ActivityLogHeader {
    schema_version: u32,
}

/// Paginated reader output. `events` is in ascending `seq` order; each
/// element is the raw payload object with its embedded `seq` field. When
/// `next_seq` is `Some(n)`, the caller can request the next page with
/// `after_seq = Some(n)`. `None` means the reader hit the file tail.
#[derive(Debug, Clone, Serialize)]
pub struct SwarmActivityLogPage {
    pub events: Vec<serde_json::Value>,
    pub next_seq: Option<u64>,
}

/// Append-only writer over one swarm's `activity_log.jsonl`.
///
/// Maintains an internal monotonic `seq` counter that's seeded from the
/// file's tail on construction so seq numbers stay gap-free across pause/
/// resume / process-restart. The writer mutates each appended payload in
/// place to inject `"seq": N` before serialising, so the persisted JSON
/// and the IPC-emitted JSON share an identical `seq`.
pub struct ActivityWriter {
    writer: BufWriter<File>,
    next_seq: u64,
}

impl std::fmt::Debug for ActivityWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActivityWriter")
            .field("next_seq", &self.next_seq)
            .finish()
    }
}

impl ActivityWriter {
    /// Open (or create) the activity log at `path` in append mode. Scans
    /// the existing file (if any) to recover the last seq number written
    /// so the next `append` continues the sequence without a gap.
    pub fn new(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!(
                    "failed to create directory for activity log: {}",
                    parent.display()
                )
            })?;
        }

        // Scan first to learn the highest seq present. Open separately for
        // read so we don't disturb the append handle.
        let last_seq = scan_last_seq(path)?;

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("failed to open activity log at {}", path.display()))?;

        Ok(Self {
            writer: BufWriter::new(file),
            next_seq: last_seq + 1,
        })
    }

    /// Append a single event to the log. Mutates `payload` in place to add
    /// `"seq": N` so the caller can forward the exact same payload to the
    /// frontend. Returns the assigned seq. Writes the schema-version header
    /// line on first append to an empty file.
    ///
    /// `payload` MUST be a JSON object — non-objects are wrapped in a
    /// synthetic envelope so we never lose data, but callers should always
    /// pass objects (every `swarm-activity` event already is one).
    pub fn append(&mut self, payload: &mut serde_json::Value) -> Result<u64> {
        // Header on first ever write. We detect "first ever" by checking
        // the underlying file's current length — append mode positions the
        // cursor at EOF on every write, so a zero-length file means we
        // haven't written anything yet AND nothing else has either.
        let file_len = self
            .writer
            .get_ref()
            .metadata()
            .map(|m| m.len())
            .unwrap_or(0);
        if file_len == 0 {
            let header = ActivityLogHeader {
                schema_version: ACTIVITY_LOG_SCHEMA_VERSION,
            };
            let mut header_line = serde_json::to_string(&header)
                .context("failed to serialise activity log header")?;
            header_line.push('\n');
            self.writer
                .write_all(header_line.as_bytes())
                .context("failed to write activity log header")?;
        }

        let seq = self.next_seq;
        self.next_seq = self.next_seq.saturating_add(1);

        // Inject the seq into the payload so the persisted line and the
        // IPC-emitted payload carry the same value. Wrap non-objects in an
        // envelope rather than losing the original.
        if !payload.is_object() {
            let owned = std::mem::take(payload);
            let mut wrapper = serde_json::Map::new();
            wrapper.insert("payload".to_string(), owned);
            *payload = serde_json::Value::Object(wrapper);
        }
        let obj = payload
            .as_object_mut()
            .expect("payload normalised to object above");
        obj.insert("seq".to_string(), serde_json::Value::Number(seq.into()));

        let mut line =
            serde_json::to_string(payload).context("failed to serialise activity log event")?;
        line.push('\n');
        self.writer
            .write_all(line.as_bytes())
            .context("failed to write activity log line")?;

        Ok(seq)
    }

    /// Flush the buffered bytes to the kernel and `fdatasync(2)` the file
    /// so the writes are durable across a crash. Call once per channel
    /// drain — not per write.
    pub fn flush_and_sync(&mut self) -> Result<()> {
        self.writer
            .flush()
            .context("failed to flush activity log")?;
        self.writer
            .get_ref()
            .sync_data()
            .context("failed to fsync activity log")?;
        Ok(())
    }
}

impl Drop for ActivityWriter {
    fn drop(&mut self) {
        // Best-effort durability backstop. The owning forwarder task
        // should call flush_and_sync() per drain, but if the task is
        // cancelled mid-buffer we still want what we have on disk.
        if let Err(e) = self.writer.flush() {
            warn!(
                "ActivityWriter::drop: final flush failed: {} (recent activity may be lost)",
                e
            );
        }
    }
}

/// Reader for paging through a swarm's activity log.
pub struct ActivityReader;

impl ActivityReader {
    /// Read events with `seq > after_seq` (strictly greater), capped at
    /// `limit` entries. Returns the events plus a `next_seq` cursor when
    /// the page is full (so the caller knows to ask for more).
    ///
    /// A missing or empty file returns an empty page — this is not an
    /// error (the swarm may simply not have produced activity yet).
    /// Malformed / truncated tail lines are logged and skipped.
    pub fn page(path: &Path, after_seq: Option<u64>, limit: u32) -> Result<SwarmActivityLogPage> {
        if !path.exists() {
            return Ok(SwarmActivityLogPage {
                events: Vec::new(),
                next_seq: None,
            });
        }

        let file =
            File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
        let reader = BufReader::new(file);

        let limit_usize = limit as usize;
        let mut events: Vec<serde_json::Value> = Vec::new();
        let mut last_seq: Option<u64> = None;
        let mut more_remain = false;

        for (line_no, line) in reader.lines().enumerate() {
            let line = match line {
                Ok(l) => l,
                Err(e) => {
                    warn!(
                        "io error reading line {} of {}: {}",
                        line_no + 1,
                        path.display(),
                        e
                    );
                    continue;
                }
            };
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            let value: serde_json::Value = match serde_json::from_str(trimmed) {
                Ok(v) => v,
                Err(e) => {
                    warn!(
                        "skipping malformed line {} in {}: {}",
                        line_no + 1,
                        path.display(),
                        e
                    );
                    continue;
                }
            };

            // Skip the schema-version header. We accept either a bare
            // header line or any object that doesn't carry a `seq` field
            // (defensive — the header is the only such expected line).
            let Some(obj) = value.as_object() else {
                continue;
            };
            if obj.contains_key("schema_version") && !obj.contains_key("seq") {
                continue;
            }
            let seq = match obj.get("seq").and_then(|v| v.as_u64()) {
                Some(s) => s,
                None => {
                    warn!(
                        "skipping line {} in {}: missing seq field",
                        line_no + 1,
                        path.display()
                    );
                    continue;
                }
            };

            if let Some(after) = after_seq {
                if seq <= after {
                    continue;
                }
            }

            if events.len() >= limit_usize {
                // Caller has a full page; flag that more events likely
                // exist past this point.
                more_remain = true;
                break;
            }

            last_seq = Some(seq);
            events.push(value);
        }

        let next_seq = if more_remain { last_seq } else { None };
        Ok(SwarmActivityLogPage { events, next_seq })
    }
}

/// Scan the file at `path` for the highest `seq` value present in any
/// event line. Returns 0 if the file is missing, empty, header-only, or
/// contains only malformed lines (meaning the next seq to write is 1).
///
/// Truncation-tolerant: malformed lines are skipped, IO errors mid-scan
/// are logged but not fatal. We open with `BufReader` and stream — log
/// files can grow to millions of lines and we must not OOM.
fn scan_last_seq(path: &Path) -> Result<u64> {
    if !path.exists() {
        return Ok(0);
    }
    let mut file = File::open(path)
        .with_context(|| format!("failed to open {} for seq scan", path.display()))?;
    // Defensive: rewind in case the OS handed us a non-zero cursor.
    let _ = file.seek(SeekFrom::Start(0));
    let reader = BufReader::new(file);

    let mut max_seq: u64 = 0;
    for line in reader.lines() {
        let Ok(line) = line else { continue };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            continue;
        };
        if let Some(seq) = value.get("seq").and_then(|v| v.as_u64()) {
            if seq > max_seq {
                max_seq = seq;
            }
        }
    }
    Ok(max_seq)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    fn ev(kind: &str) -> serde_json::Value {
        json!({
            "kind": kind,
            "swarm_id": "swarm-1",
            "feature_id": "feat-a",
            "agent": "worker",
            "session_id": "sess-1",
            "timestamp": "2025-01-02T03:04:05Z",
            "text": "hello",
        })
    }

    #[test]
    fn missing_file_returns_empty_page() {
        let td = TempDir::new().unwrap();
        let path = td.path().join("does-not-exist.jsonl");
        let page = ActivityReader::page(&path, None, 100).expect("page");
        assert!(page.events.is_empty());
        assert!(page.next_seq.is_none());
    }

    #[test]
    fn empty_file_returns_empty_page() {
        let td = TempDir::new().unwrap();
        let path = td.path().join("activity_log.jsonl");
        std::fs::write(&path, b"").unwrap();
        let page = ActivityReader::page(&path, None, 100).expect("page");
        assert!(page.events.is_empty());
        assert!(page.next_seq.is_none());
    }

    #[test]
    fn roundtrip_100_events_paged_in_chunks_of_30() {
        let td = TempDir::new().unwrap();
        let path = td.path().join("activity_log.jsonl");
        {
            let mut w = ActivityWriter::new(&path).expect("open writer");
            for i in 0..100 {
                let mut p = ev("text");
                p["index"] = json!(i);
                let seq = w.append(&mut p).expect("append");
                assert_eq!(seq, (i as u64) + 1);
                assert_eq!(p["seq"].as_u64(), Some((i as u64) + 1));
            }
            w.flush_and_sync().expect("sync");
        }

        // Page through in chunks of 30.
        let mut after: Option<u64> = None;
        let mut collected: Vec<u64> = Vec::new();
        loop {
            let page = ActivityReader::page(&path, after, 30).expect("page");
            for e in &page.events {
                collected.push(e["seq"].as_u64().expect("seq"));
            }
            match page.next_seq {
                Some(n) => after = Some(n),
                None => break,
            }
        }
        let expected: Vec<u64> = (1..=100).collect();
        assert_eq!(collected, expected);
    }

    #[test]
    fn header_written_exactly_once_on_first_append() {
        let td = TempDir::new().unwrap();
        let path = td.path().join("activity_log.jsonl");
        {
            let mut w = ActivityWriter::new(&path).expect("open 1");
            let mut p = ev("text");
            w.append(&mut p).expect("append 1");
            w.flush_and_sync().expect("sync");
        }
        // Reopen and write more; the header must NOT be duplicated.
        {
            let mut w = ActivityWriter::new(&path).expect("open 2");
            let mut p = ev("text");
            w.append(&mut p).expect("append 2");
            w.flush_and_sync().expect("sync");
        }
        let body = std::fs::read_to_string(&path).expect("read");
        let header_lines = body
            .lines()
            .filter(|l| {
                serde_json::from_str::<ActivityLogHeader>(l.trim()).is_ok()
                    && !l.contains("\"seq\"")
            })
            .count();
        assert_eq!(header_lines, 1, "header must be written exactly once");
    }

    #[test]
    fn truncated_tail_is_skipped() {
        let td = TempDir::new().unwrap();
        let path = td.path().join("activity_log.jsonl");
        {
            let mut w = ActivityWriter::new(&path).expect("open");
            for _ in 0..50 {
                let mut p = ev("text");
                w.append(&mut p).expect("append");
            }
            w.flush_and_sync().expect("sync");
        }

        // Truncate the file mid-line: keep all 50 events, drop the trailing
        // newline of the last line, then append a partial line so the tail
        // is genuinely malformed (not just missing a \n).
        let mut bytes = std::fs::read(&path).expect("read");
        // Remove the trailing newline if present.
        if bytes.last() == Some(&b'\n') {
            bytes.pop();
        }
        // Slice off the last 5 chars to corrupt the JSON tail.
        let new_len = bytes.len().saturating_sub(5);
        bytes.truncate(new_len);
        std::fs::write(&path, &bytes).expect("write truncated");

        // Now append a fragment that won't parse.
        let mut handle = OpenOptions::new().append(true).open(&path).expect("reopen");
        handle.write_all(b"{\"partial\":").expect("write partial");

        let page = ActivityReader::page(&path, None, 1000).expect("page");
        // 49 valid lines survive: 50 events written, last one corrupted +
        // the synthetic trailing partial which also fails parse.
        assert_eq!(
            page.events.len(),
            49,
            "expected 49 valid events from truncated log, got {}",
            page.events.len()
        );
        // Seqs must be 1..=49 in order.
        for (i, e) in page.events.iter().enumerate() {
            assert_eq!(e["seq"].as_u64(), Some((i as u64) + 1));
        }
    }

    #[test]
    fn seq_monotonic_across_reopens() {
        let td = TempDir::new().unwrap();
        let path = td.path().join("activity_log.jsonl");
        {
            let mut w = ActivityWriter::new(&path).expect("open 1");
            for _ in 0..10 {
                let mut p = ev("text");
                w.append(&mut p).expect("append");
            }
            w.flush_and_sync().expect("sync");
        }
        let mut last_seq = 0u64;
        {
            let mut w = ActivityWriter::new(&path).expect("open 2");
            for _ in 0..5 {
                let mut p = ev("text");
                last_seq = w.append(&mut p).expect("append");
            }
            w.flush_and_sync().expect("sync");
        }
        assert_eq!(last_seq, 15, "seq must continue at 15 across reopen");

        let page = ActivityReader::page(&path, None, 1000).expect("page");
        assert_eq!(page.events.len(), 15);
        let seqs: Vec<u64> = page
            .events
            .iter()
            .map(|e| e["seq"].as_u64().expect("seq"))
            .collect();
        assert_eq!(seqs, (1..=15).collect::<Vec<_>>());
    }

    #[test]
    fn after_seq_filters_strictly_greater() {
        let td = TempDir::new().unwrap();
        let path = td.path().join("activity_log.jsonl");
        {
            let mut w = ActivityWriter::new(&path).expect("open");
            for _ in 0..100 {
                let mut p = ev("text");
                w.append(&mut p).expect("append");
            }
            w.flush_and_sync().expect("sync");
        }
        let page = ActivityReader::page(&path, Some(50), 1000).expect("page");
        assert_eq!(page.events.len(), 50);
        assert_eq!(page.events.first().unwrap()["seq"].as_u64(), Some(51));
        assert_eq!(page.events.last().unwrap()["seq"].as_u64(), Some(100));
        assert!(page.next_seq.is_none(), "caller has caught up");
    }

    #[test]
    fn after_seq_past_tail_returns_empty() {
        let td = TempDir::new().unwrap();
        let path = td.path().join("activity_log.jsonl");
        {
            let mut w = ActivityWriter::new(&path).expect("open");
            for _ in 0..10 {
                let mut p = ev("text");
                w.append(&mut p).expect("append");
            }
            w.flush_and_sync().expect("sync");
        }
        let page = ActivityReader::page(&path, Some(999), 1000).expect("page");
        assert!(page.events.is_empty());
        assert!(page.next_seq.is_none());
    }

    #[test]
    fn limit_set_when_more_events_remain() {
        let td = TempDir::new().unwrap();
        let path = td.path().join("activity_log.jsonl");
        {
            let mut w = ActivityWriter::new(&path).expect("open");
            for _ in 0..20 {
                let mut p = ev("text");
                w.append(&mut p).expect("append");
            }
            w.flush_and_sync().expect("sync");
        }
        let page = ActivityReader::page(&path, None, 5).expect("page");
        assert_eq!(page.events.len(), 5);
        assert_eq!(page.next_seq, Some(5));
    }

    #[test]
    fn non_object_payload_is_wrapped() {
        let td = TempDir::new().unwrap();
        let path = td.path().join("activity_log.jsonl");
        let mut w = ActivityWriter::new(&path).expect("open");
        let mut p = json!("just a string");
        let seq = w.append(&mut p).expect("append");
        assert_eq!(seq, 1);
        assert_eq!(p["seq"].as_u64(), Some(1));
        assert_eq!(p["payload"].as_str(), Some("just a string"));
        w.flush_and_sync().expect("sync");
    }
}
