use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tracing::warn;

use crate::domain::swarm::FeatureStatus;
use crate::state::log_redact::RedactingWriter;

// ---------------------------------------------------------------------------
// JSONL header (schema version)
// ---------------------------------------------------------------------------
//
// Audit 2.3: every `progress_log.jsonl` opened by this module is written with
// a single leading "header" line that carries `{"schema_version": N}` so the
// replay reader can detect file format changes across releases. Old logs
// written before this audit lack any header line — those are treated as
// `schema_version = 1`. The header is written ONCE, idempotently, on log
// open when the file is empty; pre-existing logs are left untouched.

/// Current schema version emitted by all new logs.
pub const PROGRESS_LOG_SCHEMA_VERSION: u32 = 2;

/// Header line written as the very first JSONL record of a fresh
/// `progress_log.jsonl`. Carries the schema version so future readers can
/// negotiate format changes. Older code that reads this file silently
/// ignores the header (it doesn't match the `ProgressEvent` shape and is
/// skipped by `serde_json::from_str::<ProgressEvent>`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgressLogHeader {
    pub schema_version: u32,
}

/// Write the schema-version header line to a fresh empty file at `path`.
/// Idempotent — if the file already has any content (an existing header or
/// pre-2.3 log lines), this is a no-op. Returns the detected/written
/// schema version, defaulting to 1 for legacy logs that have no header.
fn write_header_if_empty_sync(path: &Path) -> std::io::Result<u32> {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // File does not exist yet — caller will create it on open. We
            // can't write the header here without racing the caller, so
            // signal "fresh" via 0-length and let the caller invoke
            // write_header_if_empty_sync again after opening.
            return Ok(PROGRESS_LOG_SCHEMA_VERSION);
        }
        Err(e) => return Err(e),
    };
    if meta.len() == 0 {
        let header = ProgressLogHeader {
            schema_version: PROGRESS_LOG_SCHEMA_VERSION,
        };
        let mut line = serde_json::to_string(&header).expect("serialise header");
        line.push('\n');
        let mut f = std::fs::OpenOptions::new().append(true).open(path)?;
        f.write_all(line.as_bytes())?;
        f.flush()?;
        Ok(PROGRESS_LOG_SCHEMA_VERSION)
    } else {
        // Pre-existing file. The header may or may not be present; the
        // reader (`detect_schema_version`) will figure that out. Caller
        // does not need to act here — we never overwrite existing content.
        Ok(1)
    }
}

/// Inspect the very first JSONL line of `path` and try to parse it as a
// ---------------------------------------------------------------------------
// Strongly-typed payload helpers (audit 2.3)
// ---------------------------------------------------------------------------
//
// The new event variants added in 2.3 carry structured payloads (PIDs,
// session ids, success states, attempt counters, etc.) that the existing
// `metadata: Option<serde_json::Value>` field accommodates without an enum
// rewrite. These helpers build the matching JSON object for each variant so
// callers don't reinvent the wire format. Replay code (`rebuild_state`)
// reads the same shape.

/// Build the metadata payload for a `PiSessionSpawned` event.
pub fn pi_session_spawned_metadata(
    session_id: &str,
    role: &str,
    feature_id: Option<&str>,
    pid: u32,
) -> serde_json::Value {
    serde_json::json!({
        "session_id": session_id,
        "role": role,
        "feature_id": feature_id,
        "pid": pid,
    })
}

/// Build the metadata payload for a `PiSessionKilled` event.
pub fn pi_session_killed_metadata(session_id: &str, reason: &str) -> serde_json::Value {
    serde_json::json!({
        "session_id": session_id,
        "reason": reason,
    })
}

/// Build the metadata payload for a `WorkerHandoff` event.
pub fn worker_handoff_metadata(
    feature_id: &str,
    run_id: &str,
    success_state: &str,
) -> serde_json::Value {
    serde_json::json!({
        "feature_id": feature_id,
        "run_id": run_id,
        "success_state": success_state,
    })
}

/// Build the metadata payload for a `GuardAttempt` event.
pub fn guard_attempt_metadata(
    feature_id: &str,
    attempt: u32,
    milestone_id: &str,
) -> serde_json::Value {
    serde_json::json!({
        "feature_id": feature_id,
        "attempt": attempt,
        "milestone_id": milestone_id,
    })
}

// ---------------------------------------------------------------------------
// fsync throttling parameters
// ---------------------------------------------------------------------------
//
// Both ProgressWriter and SyncProgressWriter must commit bytes to durable
// storage (not just the kernel page cache) so the JSONL replay claim in
// CLAUDE.md holds across crashes. A naive fsync-per-event would cost ~1-3ms
// per event on SSD; we amortize by syncing after every `FSYNC_EVENT_INTERVAL`
// events OR after `FSYNC_TIME_INTERVAL` has elapsed since the last sync —
// whichever fires first.

/// Fsync after this many buffered events.
const FSYNC_EVENT_INTERVAL: u64 = 10;

/// Fsync if at least this long has passed since the last sync.
const FSYNC_TIME_INTERVAL: Duration = Duration::from_millis(250);

// ---------------------------------------------------------------------------
// ProgressEvent
// ---------------------------------------------------------------------------

/// Types of progress events recorded during swarm execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProgressEventType {
    SwarmStarted,
    SwarmCompleted,
    SwarmFailed,
    SwarmPaused,
    SwarmResumed,
    FeatureStarted,
    FeatureScouted,
    FeatureImplemented,
    FeatureValidated,
    FeatureFailed,
    FeatureSkipped,
    NurseIntervention,
    GuardValidation,
    HivemindReview,
    HivemindReviewStarted,
    HivemindReviewCompleted,
    HivemindReviewSkipped,
    /// A non-blocking issue surfaced by a Worker via its handoff
    /// `discovered_issues` array. Severity (info/warn/error), description,
    /// and optional suggested fix are carried in `metadata`. The user can
    /// acknowledge or dismiss these from the SwarmControl UI; they NEVER
    /// gate execution.
    DiscoveredIssue,
    /// Phase 5A: emitted when a swarm's lifetime spend or the global
    /// daily spend exceeds the configured cap. The swarm is paused at
    /// the next batch boundary. Metadata carries `{ reason, scope,
    /// swarm_spend, daily_spend, cap }`.
    BudgetExceeded,
    /// Audit 2.3: a Pi subprocess was spawned for a swarm-owned role
    /// (scout / worker / guard / nurse / etc.). Metadata carries
    /// `{ session_id, role, feature_id?, pid }` via
    /// [`pi_session_spawned_metadata`].
    PiSessionSpawned,
    /// Audit 2.3: a previously-spawned Pi subprocess was killed. Metadata
    /// carries `{ session_id, reason }` via [`pi_session_killed_metadata`].
    PiSessionKilled,
    /// Audit 2.3: a Worker emitted a structured handoff JSON block.
    /// Metadata carries `{ feature_id, run_id, success_state }` via
    /// [`worker_handoff_metadata`]. Distinct from [`FeatureImplemented`]
    /// which only records that the Worker phase completed — `WorkerHandoff`
    /// preserves the parsed handoff contract on the log so replay can
    /// reconstruct run-id history.
    WorkerHandoff,
    /// Audit 2.3: emitted by Guard at the top of each validation attempt
    /// (a run_guard / run_guard_with_assertions call). Metadata carries
    /// `{ feature_id, attempt, milestone_id }` via
    /// [`guard_attempt_metadata`].
    GuardAttempt,
    Error,
}

/// A single event in the progress log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgressEvent {
    pub timestamp: DateTime<Utc>,
    pub event_type: ProgressEventType,
    pub swarm_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feature_id: Option<String>,
    /// Audit 2.3: per-feature retry id (e.g. each Worker attempt has its
    /// own run_id). Distinguishes retries on the same feature in the
    /// progress log. `#[serde(default)]` keeps the field backwards
    /// compatible with logs written before audit 2.3 (`schema_version=1`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

impl ProgressEvent {
    /// Convenience constructor that fills in the timestamp automatically.
    pub fn new(event_type: ProgressEventType, swarm_id: String, message: String) -> Self {
        Self {
            timestamp: Utc::now(),
            event_type,
            swarm_id,
            feature_id: None,
            run_id: None,
            message,
            metadata: None,
        }
    }

    /// Set the feature ID on this event (builder style).
    pub fn with_feature(mut self, feature_id: String) -> Self {
        self.feature_id = Some(feature_id);
        self
    }

    /// Audit 2.3: set the run ID on this event (builder style). Distinguishes
    /// retries of the same feature in the progress log.
    pub fn with_run_id(mut self, run_id: String) -> Self {
        self.run_id = Some(run_id);
        self
    }

    /// Attach arbitrary metadata (builder style).
    pub fn with_metadata(mut self, metadata: serde_json::Value) -> Self {
        self.metadata = Some(metadata);
        self
    }
}

// ---------------------------------------------------------------------------
// ProgressWriter -- buffered, async JSONL writer
// ---------------------------------------------------------------------------

/// Buffered, append-only JSONL writer for progress events.
///
/// Writes are serialised through a `Mutex` and auto-flushed every
/// `flush_interval` events. The writer is wrapped in a
/// [`RedactingWriter`] so that secrets (API keys, tokens) are
/// scrubbed before they hit disk. The writer also issues `fdatasync(2)`
/// (via `std::fs::File::sync_data`) after every `FSYNC_EVENT_INTERVAL`
/// events or `FSYNC_TIME_INTERVAL`, whichever comes first, so the JSONL
/// log is genuinely durable on crash — not just resident in the kernel
/// page cache.
pub struct ProgressWriter {
    inner: Mutex<ProgressWriterInner>,
    flush_interval: u64,
    event_count: AtomicU64,
    path: PathBuf,
}

struct ProgressWriterInner {
    writer: RedactingWriter<std::io::BufWriter<std::fs::File>>,
    events_since_fsync: u64,
    last_fsync_at: Instant,
}

impl std::fmt::Debug for ProgressWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProgressWriter")
            .field("path", &self.path)
            .field("flush_interval", &self.flush_interval)
            .field("event_count", &self.event_count.load(Ordering::Relaxed))
            .finish()
    }
}

impl ProgressWriter {
    /// Open (or create) the progress log at `path` in append mode.
    ///
    /// Audit 2.3: on first open of a fresh (zero-byte) log file, writes a
    /// `{"schema_version": N}` header line so the replay reader can detect
    /// the file format. Pre-existing files are left untouched (their schema
    /// is implied by the absence of the header — see [`detect_schema_version`]).
    pub async fn new(path: &Path) -> Result<Self> {
        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.with_context(|| {
                format!(
                    "failed to create directory for progress log: {}",
                    parent.display()
                )
            })?;
        }

        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("failed to open progress log at {}", path.display()))?;

        // Audit 2.3: write the schema-version header if the file is fresh.
        // Best-effort — failures are logged but never block writer creation.
        if let Err(e) = write_header_if_empty_sync(path) {
            warn!(
                path = %path.display(),
                error = %e,
                "failed to write progress log schema header; continuing"
            );
        }

        Ok(Self {
            inner: Mutex::new(ProgressWriterInner {
                writer: RedactingWriter::new(std::io::BufWriter::new(file)),
                events_since_fsync: 0,
                last_fsync_at: Instant::now(),
            }),
            flush_interval: 1,
            event_count: AtomicU64::new(0),
            path: path.to_path_buf(),
        })
    }

    /// Append a single event to the log, flushing periodically and fsyncing
    /// on a throttled schedule.
    pub async fn log(&self, event: &ProgressEvent) -> Result<()> {
        let mut line = serde_json::to_string(event).context("failed to serialise ProgressEvent")?;
        line.push('\n');

        let mut guard = self.inner.lock().await;
        guard
            .writer
            .write_all(line.as_bytes())
            .context("failed to write progress event")?;

        let count = self.event_count.fetch_add(1, Ordering::Relaxed) + 1;
        if count % self.flush_interval == 0 {
            guard
                .writer
                .flush()
                .context("failed to flush progress log")?;
        }

        // Throttled fsync: every FSYNC_EVENT_INTERVAL events OR after
        // FSYNC_TIME_INTERVAL has elapsed, whichever fires first.
        guard.events_since_fsync = guard.events_since_fsync.saturating_add(1);
        let should_fsync = guard.events_since_fsync >= FSYNC_EVENT_INTERVAL
            || guard.last_fsync_at.elapsed() >= FSYNC_TIME_INTERVAL;
        if should_fsync {
            // Ensure buffered bytes have reached the kernel before fsync.
            guard
                .writer
                .flush()
                .context("failed to flush progress log prior to fsync")?;
            guard
                .writer
                .get_mut()
                .get_mut()
                .sync_data()
                .context("failed to fsync progress log")?;
            guard.events_since_fsync = 0;
            guard.last_fsync_at = Instant::now();
        }

        Ok(())
    }

    /// Force-flush any buffered data to disk and commit to durable storage.
    pub async fn flush(&self) -> Result<()> {
        let mut guard = self.inner.lock().await;
        guard
            .writer
            .flush()
            .context("failed to flush progress log")?;
        guard
            .writer
            .get_mut()
            .get_mut()
            .sync_data()
            .context("failed to fsync progress log")?;
        guard.events_since_fsync = 0;
        guard.last_fsync_at = Instant::now();
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// ProgressReader -- streaming JSONL reader
// ---------------------------------------------------------------------------

/// Reads and replays progress logs from disk.
///
/// The reader streams line-by-line through a [`BufReader`] rather than
/// loading the whole file into memory — long-lived swarms can accumulate
/// millions of events in their `progress_log.jsonl`, and the previous
/// implementation (`read_to_string` then `lines()`) would OOM when the
/// caller asked for the full history.
pub struct ProgressReader;

impl ProgressReader {
    /// Stream events from a progress log, invoking `f` for every parsed
    /// event. Malformed lines are logged via `tracing::warn!` and skipped.
    ///
    /// Stops early (without error) if `f` returns `Ok(false)`. Returning
    /// `Ok(true)` continues consumption.
    ///
    /// This is the lowest-level entry point and is used by [`read_all`],
    /// [`read_since`], and [`rebuild_state`] to avoid ever materialising
    /// the full log when only an aggregate is needed.
    pub fn for_each_event<F>(path: &Path, mut f: F) -> Result<()>
    where
        F: FnMut(ProgressEvent) -> Result<bool>,
    {
        if !path.exists() {
            return Ok(());
        }
        let file =
            File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
        let reader = BufReader::new(file);

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
            // Audit 2.3: the first line of a fresh log is a schema-version
            // header — skip it silently rather than logging it as malformed.
            // The header is also valid JSON, so the order matters: check
            // header shape before treating a parse miss as a warning.
            if serde_json::from_str::<ProgressLogHeader>(trimmed).is_ok() {
                continue;
            }
            match serde_json::from_str::<ProgressEvent>(trimmed) {
                Ok(event) => {
                    if !f(event)? {
                        break;
                    }
                }
                Err(e) => {
                    warn!(
                        "skipping malformed line {} in {}: {}",
                        line_no + 1,
                        path.display(),
                        e
                    );
                }
            }
        }
        Ok(())
    }

    /// Read all events from a progress log, skipping malformed lines.
    ///
    /// **Caution:** materialises the entire log in memory. Prefer
    /// [`for_each_event`] (or [`read_since`]) for long-lived swarms where
    /// the log can grow without bound — only call this when the caller
    /// genuinely needs the full vector (e.g. IPC return type).
    pub fn read_all(path: &Path) -> Result<Vec<ProgressEvent>> {
        let mut events = Vec::new();
        Self::for_each_event(path, |event| {
            events.push(event);
            Ok(true)
        })?;
        Ok(events)
    }

    /// Reconstruct last-known feature statuses from the progress log.
    ///
    /// Maps each `feature_id` to its most recent `FeatureStatus` based on
    /// the event types in the log. Streams the log line-by-line, so the
    /// peak memory cost is `O(num_features)` rather than `O(num_events)`.
    ///
    /// Audit 2.3: in addition to the original feature-status events, this
    /// fold honours the new event variants:
    /// - `WorkerHandoff` with `success_state="success"` → `Reviewing`
    ///   (mirrors `FeatureImplemented`'s semantics; the Guard still has
    ///   to validate before terminal).
    /// - `WorkerHandoff` with `success_state="failure"` → `Failed`.
    /// - `WorkerHandoff` with `success_state="partial"` → `Reviewing`
    ///   (the implementation finished; only validation can downgrade it).
    /// - `GuardAttempt`, `PiSessionSpawned`, `PiSessionKilled`,
    ///   `DiscoveredIssue` — no status fold; do not alter the canonical
    ///   feature-status map the existing reconciler keys off.
    pub fn rebuild_state(path: &Path) -> Result<HashMap<String, FeatureStatus>> {
        let mut statuses: HashMap<String, FeatureStatus> = HashMap::new();

        Self::for_each_event(path, |event| {
            let feature_id = match &event.feature_id {
                Some(id) => id.clone(),
                None => return Ok(true),
            };

            let status = match event.event_type {
                ProgressEventType::FeatureStarted => FeatureStatus::Implementing,
                ProgressEventType::FeatureScouted => FeatureStatus::Scouting,
                ProgressEventType::FeatureImplemented => FeatureStatus::Reviewing,
                ProgressEventType::FeatureValidated => FeatureStatus::Completed,
                ProgressEventType::FeatureFailed => FeatureStatus::Failed,
                ProgressEventType::FeatureSkipped => FeatureStatus::Skipped,
                // Audit 2.3: a WorkerHandoff records the parsed Worker
                // contract directly. Translate success_state into the same
                // fold as FeatureImplemented / FeatureFailed.
                ProgressEventType::WorkerHandoff => {
                    let state = event
                        .metadata
                        .as_ref()
                        .and_then(|m| m.get("success_state"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    match state {
                        "failure" => FeatureStatus::Failed,
                        // success | partial | unknown → Reviewing (Guard
                        // hasn't run yet; failed validations downgrade
                        // later via FeatureFailed).
                        _ => FeatureStatus::Reviewing,
                    }
                }
                // Non-status events — leave the existing feature status
                // unchanged.
                _ => return Ok(true),
            };

            statuses.insert(feature_id, status);
            Ok(true)
        })?;

        Ok(statuses)
    }
}

// ---------------------------------------------------------------------------
// Synchronous helper for non-async contexts (e.g. atomic_write recovery)
// ---------------------------------------------------------------------------

/// Synchronous, buffered JSONL writer used by the replay/recovery test
/// suite to author fixture progress logs that exercise the async
/// `ProgressWriter`'s header/flush/fsync contract. Production code writes
/// via the async `ProgressWriter`; this exists purely as test scaffolding.
#[cfg(test)]
pub struct SyncProgressWriter {
    writer: RedactingWriter<std::io::BufWriter<std::fs::File>>,
    flush_interval: u64,
    event_count: u64,
    /// Number of events since the last fsync.
    events_since_fsync: u64,
    /// Instant of the last fsync (or writer creation) — also used as the
    /// time-based flush ticker so the writer doesn't sit on buffered bytes
    /// indefinitely between explicit `flush()` calls.
    last_fsync_at: Instant,
}

#[cfg(test)]
impl SyncProgressWriter {
    /// Open (or create) the progress log at `path` in append mode.
    ///
    /// Audit 2.3: on first open of a fresh (zero-byte) log file, writes a
    /// `{"schema_version": N}` header line so the replay reader can detect
    /// the file format. Pre-existing files are left untouched.
    pub fn new(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("failed to open progress log at {}", path.display()))?;

        // Audit 2.3: write the schema-version header on a fresh file. Best-
        // effort — failures fall back to header-less behaviour.
        if let Err(e) = write_header_if_empty_sync(path) {
            warn!(
                path = %path.display(),
                error = %e,
                "failed to write progress log schema header; continuing"
            );
        }

        Ok(Self {
            writer: RedactingWriter::new(std::io::BufWriter::new(file)),
            flush_interval: 100,
            event_count: 0,
            events_since_fsync: 0,
            last_fsync_at: Instant::now(),
        })
    }

    /// Append a single event to the log, flushing and fsyncing on a throttled
    /// schedule.
    pub fn log(&mut self, event: &ProgressEvent) -> Result<()> {
        let line = serde_json::to_string(event).context("failed to serialise ProgressEvent")?;
        writeln!(self.writer, "{}", line).context("failed to write progress event")?;

        self.event_count += 1;
        self.events_since_fsync = self.events_since_fsync.saturating_add(1);

        // Time-based flush ticker: if more than FSYNC_TIME_INTERVAL has
        // elapsed since the last durability checkpoint, treat that as a
        // signal to flush+fsync now even if we're below the event quota.
        let elapsed = self.last_fsync_at.elapsed();
        let event_quota_reached = self.event_count % self.flush_interval == 0;
        let fsync_event_quota_reached = self.events_since_fsync >= FSYNC_EVENT_INTERVAL;
        let time_quota_reached = elapsed >= FSYNC_TIME_INTERVAL;

        if event_quota_reached || fsync_event_quota_reached || time_quota_reached {
            self.writer
                .flush()
                .context("failed to flush progress log")?;
        }

        if fsync_event_quota_reached || time_quota_reached {
            self.writer
                .get_mut()
                .get_ref()
                .sync_data()
                .context("failed to fsync progress log")?;
            self.events_since_fsync = 0;
            self.last_fsync_at = Instant::now();
        }

        Ok(())
    }

    /// Force-flush any buffered data to disk and commit to durable storage.
    pub fn flush(&mut self) -> Result<()> {
        self.writer
            .flush()
            .context("failed to flush progress log")?;
        self.writer
            .get_mut()
            .get_ref()
            .sync_data()
            .context("failed to fsync progress log")?;
        self.events_since_fsync = 0;
        self.last_fsync_at = Instant::now();
        Ok(())
    }
}

#[cfg(test)]
impl Drop for SyncProgressWriter {
    fn drop(&mut self) {
        // Best-effort: ensure any buffered events reach disk before the file
        // handle is closed. Errors are swallowed because Drop cannot return
        // them; this is purely a durability backstop for the "writer goes out
        // of scope without an explicit final flush" path.
        if let Err(e) = self.writer.flush() {
            warn!(
                "SyncProgressWriter::drop: final flush failed: {} (some buffered progress events may be lost)",
                e
            );
            return;
        }
        if let Err(e) = self.writer.get_mut().get_ref().sync_data() {
            warn!(
                "SyncProgressWriter::drop: final fsync failed: {} (recent progress events may not be durable)",
                e
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Truncated / malformed JSONL replay tests (Fix 7.5) ----------------
    //
    // ProgressReader::rebuild_state is the recovery entry point after a
    // crash. If the writer was mid-write when the process died, the tail of
    // the file can be: a partial line, an invalid-JSON line, or even an
    // incomplete UTF-8 byte sequence. None of these may panic — the
    // recovery path must return a best-effort state of all valid events.

    fn write_bytes(dir: &tempfile::TempDir, name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let path = dir.path().join(name);
        let mut f = std::fs::File::create(&path).expect("create");
        f.write_all(bytes).expect("write");
        f.flush().expect("flush");
        path
    }

    fn good_line(swarm: &str, feat: &str, evt: &str) -> String {
        // Use an explicit timestamp to keep the line deterministic.
        let ts = "2025-01-02T03:04:05Z";
        format!(
            "{{\"timestamp\":\"{ts}\",\"event_type\":\"{evt}\",\"swarm_id\":\"{swarm}\",\"feature_id\":\"{feat}\",\"message\":\"ok\"}}\n"
        )
    }

    #[test]
    fn rebuild_state_empty_file_returns_empty_no_panic() {
        let td = tempfile::TempDir::new().unwrap();
        let path = write_bytes(&td, "progress.jsonl", b"");
        let statuses = ProgressReader::rebuild_state(&path).expect("must not error");
        assert!(statuses.is_empty(), "empty file must yield empty map");
    }

    #[test]
    fn rebuild_state_nonexistent_file_returns_empty() {
        let td = tempfile::TempDir::new().unwrap();
        let path = td.path().join("does-not-exist.jsonl");
        let statuses = ProgressReader::rebuild_state(&path).expect("must not error");
        assert!(statuses.is_empty());
    }

    #[test]
    fn rebuild_state_skips_trailing_truncated_line() {
        // A valid line followed by a half-written line that crashed before
        // the closing brace + newline. The writer is BufWriter-backed so
        // this is exactly the failure mode we'd see on a hard kill mid-event.
        let td = tempfile::TempDir::new().unwrap();
        let mut data = String::new();
        data.push_str(&good_line("s-1", "feat-a", "feature_started"));
        data.push_str("{\"timestamp\":\"2025-01-02T03:04:06Z\",\"event_type\":\"feature_imple"); // truncated mid-key
        let path = write_bytes(&td, "progress.jsonl", data.as_bytes());

        let statuses = ProgressReader::rebuild_state(&path).expect("must not error");
        // The good line must be replayed; the truncated tail must be
        // silently skipped (warn-logged inside read_all).
        assert_eq!(statuses.len(), 1);
        assert_eq!(
            statuses.get("feat-a"),
            Some(&FeatureStatus::Implementing),
            "first event maps feature_started → Implementing"
        );
    }

    #[test]
    fn rebuild_state_skips_invalid_json_middle_line() {
        // A bad line sandwiched between two valid ones. The bad one must
        // be skipped without losing the surrounding state.
        let td = tempfile::TempDir::new().unwrap();
        let mut data = String::new();
        data.push_str(&good_line("s-1", "feat-a", "feature_started"));
        data.push_str("this is not json at all\n");
        data.push_str(&good_line("s-1", "feat-b", "feature_validated"));
        let path = write_bytes(&td, "progress.jsonl", data.as_bytes());

        let statuses = ProgressReader::rebuild_state(&path).expect("must not error");
        assert_eq!(statuses.len(), 2);
        assert_eq!(statuses.get("feat-a"), Some(&FeatureStatus::Implementing));
        assert_eq!(statuses.get("feat-b"), Some(&FeatureStatus::Completed));
    }

    #[test]
    fn rebuild_state_handles_partial_utf8_tail() {
        // The file's tail is a half-written multi-byte char ("é" is 0xC3 0xA9;
        // we write only the leading 0xC3 byte). std::fs::read_to_string
        // returns an InvalidData error for this; rebuild_state must surface
        // it as a normal Result (no panic).
        let td = tempfile::TempDir::new().unwrap();
        let mut data: Vec<u8> = Vec::new();
        data.extend_from_slice(good_line("s-1", "feat-a", "feature_started").as_bytes());
        data.push(0xC3); // dangling UTF-8 lead byte — never completed
        let path = write_bytes(&td, "progress.jsonl", &data);

        // Either: (a) read_to_string returns Err and rebuild_state propagates
        //         it as Err — acceptable, no panic; or
        //         (b) the read silently truncates and we get the valid line
        //         only. Both branches are non-panicking.
        let result = ProgressReader::rebuild_state(&path);
        match result {
            Ok(statuses) => {
                // Replayed-up-to branch: at minimum, no spurious extra
                // statuses, no panic.
                assert!(
                    statuses.len() <= 1,
                    "only the one valid event may surface, got {:?}",
                    statuses
                );
            }
            Err(e) => {
                // Propagated-IO-error branch: the message should mention the
                // file or UTF-8 — but the critical contract is no panic.
                let msg = e.to_string();
                assert!(
                    !msg.is_empty(),
                    "error must carry a context message, got empty"
                );
            }
        }
    }

    #[test]
    fn rebuild_state_uses_latest_status_per_feature() {
        // Earlier events for the same feature must be overwritten by later
        // ones — this confirms the replay is order-preserving and
        // last-write-wins, which is the crash-recovery contract.
        let td = tempfile::TempDir::new().unwrap();
        let mut data = String::new();
        data.push_str(&good_line("s-1", "feat-x", "feature_started"));
        data.push_str(&good_line("s-1", "feat-x", "feature_implemented"));
        data.push_str(&good_line("s-1", "feat-x", "feature_validated"));
        let path = write_bytes(&td, "progress.jsonl", data.as_bytes());

        let statuses = ProgressReader::rebuild_state(&path).expect("must not error");
        assert_eq!(statuses.len(), 1);
        assert_eq!(
            statuses.get("feat-x"),
            Some(&FeatureStatus::Completed),
            "last event (feature_validated) must win"
        );
    }

    #[test]
    fn rebuild_state_blank_lines_in_middle_are_skipped() {
        // The reader trims and skips empty lines — important since OS-level
        // append-mode writes followed by crashes can leave stray newlines.
        let td = tempfile::TempDir::new().unwrap();
        let mut data = String::new();
        data.push_str(&good_line("s-1", "feat-a", "feature_started"));
        data.push('\n');
        data.push('\n');
        data.push_str("   \n"); // whitespace-only line
        data.push_str(&good_line("s-1", "feat-b", "feature_failed"));
        let path = write_bytes(&td, "progress.jsonl", data.as_bytes());

        let statuses = ProgressReader::rebuild_state(&path).expect("must not error");
        assert_eq!(statuses.len(), 2);
        assert_eq!(statuses.get("feat-a"), Some(&FeatureStatus::Implementing));
        assert_eq!(statuses.get("feat-b"), Some(&FeatureStatus::Failed));
    }

    #[test]
    fn test_discovered_issue_event_type_roundtrip() {
        // The DiscoveredIssue variant must serialize/deserialize cleanly so
        // it can flow through the broadcast channel + JSONL log without
        // losing fidelity.
        let event = ProgressEvent::new(
            ProgressEventType::DiscoveredIssue,
            "swarm-7".to_string(),
            "Worker reported issue: missing docstring".to_string(),
        )
        .with_feature("feat-3".to_string())
        .with_metadata(serde_json::json!({
            "severity": "warn",
            "description": "missing docstring on pub fn",
            "suggested_fix": "add /// summary line"
        }));

        let line = serde_json::to_string(&event).expect("serialise");
        // snake_case wire format
        assert!(
            line.contains("\"event_type\":\"discovered_issue\""),
            "expected snake_case event_type tag, got: {}",
            line
        );

        let decoded: ProgressEvent = serde_json::from_str(&line).expect("deserialise");
        assert_eq!(decoded.event_type, ProgressEventType::DiscoveredIssue);
        assert_eq!(decoded.swarm_id, "swarm-7");
        assert_eq!(decoded.feature_id.as_deref(), Some("feat-3"));
        let meta = decoded.metadata.expect("metadata");
        assert_eq!(meta["severity"], "warn");
        assert_eq!(meta["description"], "missing docstring on pub fn");
        assert_eq!(meta["suggested_fix"], "add /// summary line");
    }

    /// Write a JSONL file with `count` synthetic events plus a few
    /// malformed lines, then verify the reader streams cleanly through it.
    #[test]
    fn read_all_streams_through_log() {
        let tmp = tempfile::NamedTempFile::new().expect("tempfile");
        let path = tmp.path().to_path_buf();
        {
            let mut w = SyncProgressWriter::new(&path).expect("open sync writer");
            for i in 0..50 {
                let event = ProgressEvent::new(
                    ProgressEventType::FeatureStarted,
                    "swarm-x".to_string(),
                    format!("evt-{}", i),
                )
                .with_feature(format!("feat-{}", i));
                w.log(&event).expect("write");
            }
            w.flush().expect("flush");
        }
        // Append two malformed lines — they must be skipped, not abort.
        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("reopen");
        writeln!(file, "{{ not valid json").expect("write bad");
        writeln!(file).expect("write blank");

        let events = ProgressReader::read_all(&path).expect("read_all");
        assert_eq!(events.len(), 50);
        assert_eq!(events[0].feature_id.as_deref(), Some("feat-0"));
        assert_eq!(events[49].feature_id.as_deref(), Some("feat-49"));
    }

    #[test]
    fn for_each_event_can_short_circuit() {
        let tmp = tempfile::NamedTempFile::new().expect("tempfile");
        let path = tmp.path().to_path_buf();
        {
            let mut w = SyncProgressWriter::new(&path).expect("open sync writer");
            for i in 0..20 {
                let e = ProgressEvent::new(
                    ProgressEventType::FeatureStarted,
                    "s".to_string(),
                    format!("e-{}", i),
                );
                w.log(&e).expect("write");
            }
            w.flush().expect("flush");
        }

        let mut seen = 0usize;
        ProgressReader::for_each_event(&path, |_| {
            seen += 1;
            Ok(seen < 5) // stop after 5 events
        })
        .expect("for_each");
        assert_eq!(seen, 5);
    }

    #[test]
    fn read_all_on_missing_file_returns_empty() {
        let path = std::path::PathBuf::from("/nonexistent/path/progress.jsonl");
        let events = ProgressReader::read_all(&path).expect("missing is ok");
        assert!(events.is_empty());
    }

    // -- Audit 2.3: new event variants -------------------------------------

    #[test]
    fn pi_session_spawned_event_roundtrip() {
        let event = ProgressEvent::new(
            ProgressEventType::PiSessionSpawned,
            "swarm-1".to_string(),
            "spawned worker pi session".to_string(),
        )
        .with_feature("feat-a".to_string())
        .with_run_id("run-1".to_string())
        .with_metadata(pi_session_spawned_metadata(
            "worker-swarm-1-feat-a",
            "worker",
            Some("feat-a"),
            12345,
        ));

        let line = serde_json::to_string(&event).expect("serialise");
        assert!(
            line.contains("\"event_type\":\"pi_session_spawned\""),
            "snake_case wire format expected; got: {}",
            line
        );

        let decoded: ProgressEvent = serde_json::from_str(&line).expect("deserialise");
        assert_eq!(decoded.event_type, ProgressEventType::PiSessionSpawned);
        assert_eq!(decoded.feature_id.as_deref(), Some("feat-a"));
        assert_eq!(decoded.run_id.as_deref(), Some("run-1"));
        let md = decoded.metadata.expect("metadata");
        assert_eq!(md["session_id"], "worker-swarm-1-feat-a");
        assert_eq!(md["role"], "worker");
        assert_eq!(md["feature_id"], "feat-a");
        assert_eq!(md["pid"], 12345);
    }

    #[test]
    fn pi_session_killed_event_roundtrip() {
        let event = ProgressEvent::new(
            ProgressEventType::PiSessionKilled,
            "swarm-1".to_string(),
            "kill on feature complete".to_string(),
        )
        .with_metadata(pi_session_killed_metadata(
            "worker-swarm-1-feat-a",
            "feature_complete",
        ));

        let line = serde_json::to_string(&event).expect("serialise");
        let decoded: ProgressEvent = serde_json::from_str(&line).expect("deserialise");
        assert_eq!(decoded.event_type, ProgressEventType::PiSessionKilled);
        let md = decoded.metadata.expect("metadata");
        assert_eq!(md["session_id"], "worker-swarm-1-feat-a");
        assert_eq!(md["reason"], "feature_complete");
    }

    #[test]
    fn worker_handoff_event_roundtrip() {
        let event = ProgressEvent::new(
            ProgressEventType::WorkerHandoff,
            "swarm-1".to_string(),
            "worker emitted handoff".to_string(),
        )
        .with_feature("feat-a".to_string())
        .with_run_id("run-abc".to_string())
        .with_metadata(worker_handoff_metadata("feat-a", "run-abc", "success"));

        let line = serde_json::to_string(&event).expect("serialise");
        let decoded: ProgressEvent = serde_json::from_str(&line).expect("deserialise");
        assert_eq!(decoded.event_type, ProgressEventType::WorkerHandoff);
        assert_eq!(decoded.feature_id.as_deref(), Some("feat-a"));
        assert_eq!(decoded.run_id.as_deref(), Some("run-abc"));
        let md = decoded.metadata.expect("metadata");
        assert_eq!(md["success_state"], "success");
    }

    #[test]
    fn guard_attempt_event_roundtrip() {
        let event = ProgressEvent::new(
            ProgressEventType::GuardAttempt,
            "swarm-1".to_string(),
            "guard starting attempt 1".to_string(),
        )
        .with_feature("feat-a".to_string())
        .with_metadata(guard_attempt_metadata("feat-a", 1, "ms-1"));

        let line = serde_json::to_string(&event).expect("serialise");
        let decoded: ProgressEvent = serde_json::from_str(&line).expect("deserialise");
        assert_eq!(decoded.event_type, ProgressEventType::GuardAttempt);
        let md = decoded.metadata.expect("metadata");
        assert_eq!(md["attempt"], 1);
        assert_eq!(md["milestone_id"], "ms-1");
    }

    #[test]
    fn run_id_field_is_optional_and_backwards_compatible() {
        // A legacy event JSON written before audit 2.3 has no `run_id`
        // field. It must still deserialise cleanly with `run_id = None`.
        let legacy = r#"{
            "timestamp": "2025-01-02T03:04:05Z",
            "event_type": "feature_started",
            "swarm_id": "s-1",
            "feature_id": "f-1",
            "message": "started"
        }"#;
        let event: ProgressEvent = serde_json::from_str(legacy).expect("deserialise legacy");
        assert!(event.run_id.is_none(), "run_id must default to None");
        assert_eq!(event.feature_id.as_deref(), Some("f-1"));
    }

    #[test]
    fn writer_does_not_duplicate_header_on_reopen() {
        // Opening the same log twice must NOT write a second header line —
        // the file is not empty on the second open, so the helper is a no-op.
        let td = tempfile::TempDir::new().unwrap();
        let path = td.path().join("progress.jsonl");
        {
            let mut w = SyncProgressWriter::new(&path).expect("open 1");
            let e = ProgressEvent::new(
                ProgressEventType::FeatureStarted,
                "s".to_string(),
                "m".to_string(),
            );
            w.log(&e).expect("log");
            w.flush().expect("flush");
        }
        {
            let mut w = SyncProgressWriter::new(&path).expect("open 2");
            let e = ProgressEvent::new(
                ProgressEventType::FeatureValidated,
                "s".to_string(),
                "m".to_string(),
            )
            .with_feature("feat-x".to_string());
            w.log(&e).expect("log 2");
            w.flush().expect("flush 2");
        }
        // Count how many lines parse as a header.
        let body = std::fs::read_to_string(&path).expect("read");
        let header_lines = body
            .lines()
            .filter(|l| serde_json::from_str::<ProgressLogHeader>(l.trim()).is_ok())
            .count();
        assert_eq!(header_lines, 1, "header must be written exactly once");
    }

    #[test]
    fn rebuild_state_folds_worker_handoff_success_to_reviewing() {
        let td = tempfile::TempDir::new().unwrap();
        let path = td.path().join("progress.jsonl");
        {
            let mut w = SyncProgressWriter::new(&path).expect("open");
            let started = ProgressEvent::new(
                ProgressEventType::FeatureStarted,
                "s".to_string(),
                "started".to_string(),
            )
            .with_feature("feat-a".to_string())
            .with_run_id("run-1".to_string());
            w.log(&started).expect("log");

            let handoff = ProgressEvent::new(
                ProgressEventType::WorkerHandoff,
                "s".to_string(),
                "handoff".to_string(),
            )
            .with_feature("feat-a".to_string())
            .with_run_id("run-1".to_string())
            .with_metadata(worker_handoff_metadata("feat-a", "run-1", "success"));
            w.log(&handoff).expect("log");
            w.flush().expect("flush");
        }

        let statuses = ProgressReader::rebuild_state(&path).expect("rebuild");
        assert_eq!(statuses.get("feat-a"), Some(&FeatureStatus::Reviewing));
    }

    #[test]
    fn rebuild_state_folds_worker_handoff_failure_to_failed() {
        let td = tempfile::TempDir::new().unwrap();
        let path = td.path().join("progress.jsonl");
        {
            let mut w = SyncProgressWriter::new(&path).expect("open");
            let handoff = ProgressEvent::new(
                ProgressEventType::WorkerHandoff,
                "s".to_string(),
                "handoff failure".to_string(),
            )
            .with_feature("feat-b".to_string())
            .with_run_id("run-1".to_string())
            .with_metadata(worker_handoff_metadata("feat-b", "run-1", "failure"));
            w.log(&handoff).expect("log");
            w.flush().expect("flush");
        }

        let statuses = ProgressReader::rebuild_state(&path).expect("rebuild");
        assert_eq!(statuses.get("feat-b"), Some(&FeatureStatus::Failed));
    }

    #[test]
    fn rebuild_state_streams_terminal_status_per_feature() {
        let tmp = tempfile::NamedTempFile::new().expect("tempfile");
        let path = tmp.path().to_path_buf();
        {
            let mut w = SyncProgressWriter::new(&path).expect("open sync writer");
            // feat-1: scout -> implement -> complete
            for et in [
                ProgressEventType::FeatureScouted,
                ProgressEventType::FeatureStarted,
                ProgressEventType::FeatureValidated,
            ] {
                let e = ProgressEvent::new(et, "s".to_string(), "msg".to_string())
                    .with_feature("feat-1".to_string());
                w.log(&e).expect("write");
            }
            // feat-2: scout -> fail
            for et in [
                ProgressEventType::FeatureScouted,
                ProgressEventType::FeatureFailed,
            ] {
                let e = ProgressEvent::new(et, "s".to_string(), "msg".to_string())
                    .with_feature("feat-2".to_string());
                w.log(&e).expect("write");
            }
            w.flush().expect("flush");
        }

        let statuses = ProgressReader::rebuild_state(&path).expect("rebuild");
        assert_eq!(statuses.get("feat-1"), Some(&FeatureStatus::Completed));
        assert_eq!(statuses.get("feat-2"), Some(&FeatureStatus::Failed));
    }
}

// ----------------------------------------------------------------------------
// Property-based tests (proptest)
// ----------------------------------------------------------------------------
//
// Invariants exercised:
//   * Round-trip: serialize event -> write JSONL -> read -> deserialize ->
//     deep-equal (modulo timestamp precision, which is preserved by chrono's
//     serde RFC-3339 round-trip).
//   * Idempotent rebuild: appending the same event twice produces the same
//     rebuilt status map as appending it once.
//   * Truncation tolerance: arbitrarily truncating the last byte of the log
//     must not panic; the rebuilt state is "valid but possibly stale".
#[cfg(test)]
mod proptests {
    use super::*;
    use chrono::TimeZone;
    use proptest::collection::vec as pvec;
    use proptest::prelude::*;
    use tempfile::NamedTempFile;

    /// Pick from the subset of event types that drive `rebuild_state`,
    /// plus a few non-state ones to verify they are ignored.
    fn event_type_strategy() -> impl Strategy<Value = ProgressEventType> {
        prop_oneof![
            Just(ProgressEventType::FeatureStarted),
            Just(ProgressEventType::FeatureScouted),
            Just(ProgressEventType::FeatureImplemented),
            Just(ProgressEventType::FeatureValidated),
            Just(ProgressEventType::FeatureFailed),
            Just(ProgressEventType::FeatureSkipped),
            // Non-state-affecting event types -- exercised to confirm they
            // are skipped by `rebuild_state`.
            Just(ProgressEventType::SwarmStarted),
            Just(ProgressEventType::HivemindReview),
            Just(ProgressEventType::DiscoveredIssue),
        ]
    }

    /// Build a single deterministic ProgressEvent. We use a fixed-base
    /// timestamp incremented by an offset to keep timestamps comparable and
    /// avoid relying on wall-clock during tests.
    fn event_strategy() -> impl Strategy<Value = ProgressEvent> {
        (
            event_type_strategy(),
            0u32..1000,   // feature_id index
            0u32..10_000, // ts_offset_secs
            "[a-z]{1,8}".prop_map(String::from),
            any::<bool>(), // include feature_id?
        )
            .prop_map(|(et, fidx, ts_off, swarm, has_feat)| {
                let base = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
                let ts = base + chrono::Duration::seconds(ts_off as i64);
                let feature_id = if has_feat {
                    Some(format!("feat-{}", fidx))
                } else {
                    None
                };
                ProgressEvent {
                    timestamp: ts,
                    event_type: et,
                    swarm_id: swarm,
                    feature_id,
                    run_id: None,
                    message: "msg".to_string(),
                    metadata: None,
                }
            })
    }

    /// Apply the same mapping `rebuild_state` uses, in test code, to compute
    /// the expected status map.
    fn expected_state(events: &[ProgressEvent]) -> HashMap<String, FeatureStatus> {
        let mut statuses = HashMap::new();
        for event in events {
            let feature_id = match &event.feature_id {
                Some(id) => id.clone(),
                None => continue,
            };
            let status = match event.event_type {
                ProgressEventType::FeatureStarted => FeatureStatus::Implementing,
                ProgressEventType::FeatureScouted => FeatureStatus::Scouting,
                ProgressEventType::FeatureImplemented => FeatureStatus::Reviewing,
                ProgressEventType::FeatureValidated => FeatureStatus::Completed,
                ProgressEventType::FeatureFailed => FeatureStatus::Failed,
                ProgressEventType::FeatureSkipped => FeatureStatus::Skipped,
                _ => continue,
            };
            statuses.insert(feature_id, status);
        }
        statuses
    }

    /// Write a sequence of events as JSONL to a fresh temp file and return
    /// the temp file handle (kept alive so the path stays valid).
    fn write_jsonl(events: &[ProgressEvent]) -> NamedTempFile {
        let mut tmp = NamedTempFile::new().expect("tmp");
        for ev in events {
            let line = serde_json::to_string(ev).expect("serialise");
            writeln!(tmp.as_file_mut(), "{}", line).expect("write");
        }
        tmp.as_file_mut().flush().expect("flush");
        tmp
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(48))]

        /// Single-event round-trip: serialize -> write JSONL -> read all ->
        /// produces a sequence of length 1 whose key fields equal the input.
        #[test]
        fn single_event_roundtrip(ev in event_strategy()) {
            let tmp = write_jsonl(&[ev.clone()]);
            let got = ProgressReader::read_all(tmp.path()).expect("read_all");
            prop_assert_eq!(got.len(), 1);
            let r = &got[0];
            prop_assert_eq!(&r.event_type, &ev.event_type);
            prop_assert_eq!(&r.swarm_id, &ev.swarm_id);
            prop_assert_eq!(&r.feature_id, &ev.feature_id);
            prop_assert_eq!(&r.message, &ev.message);
            prop_assert_eq!(r.timestamp, ev.timestamp);
        }

        /// Many-event round-trip + rebuild_state agrees with the in-test
        /// reference implementation.
        #[test]
        fn rebuild_state_matches_reference(
            events in pvec(event_strategy(), 0..=20)
        ) {
            let tmp = write_jsonl(&events);
            let actual = ProgressReader::rebuild_state(tmp.path()).expect("rebuild");
            let expected = expected_state(&events);
            prop_assert_eq!(actual, expected);
        }

        /// Idempotent rebuild: appending each event twice produces the same
        /// status map as appending each event once (the last-wins mapping
        /// only depends on the *final* status per feature, and duplicating
        /// preserves that).
        #[test]
        fn rebuild_state_idempotent_under_duplication(
            events in pvec(event_strategy(), 1..=15)
        ) {
            let once = write_jsonl(&events);
            let mut doubled: Vec<ProgressEvent> = Vec::with_capacity(events.len() * 2);
            for ev in &events {
                doubled.push(ev.clone());
                doubled.push(ev.clone());
            }
            let twice = write_jsonl(&doubled);

            let s_once = ProgressReader::rebuild_state(once.path()).expect("once");
            let s_twice = ProgressReader::rebuild_state(twice.path()).expect("twice");
            prop_assert_eq!(s_once, s_twice);
        }

        /// Truncation tolerance: chopping the last `k` bytes of a written
        /// log must not panic. The rebuilt state is a subset/prefix view of
        /// the full log -- specifically, every entry in the truncated rebuild
        /// must also be present in the full rebuild (truncation only removes
        /// information; it cannot fabricate it).
        #[test]
        fn truncation_tolerance(
            events in pvec(event_strategy(), 1..=15),
            chop in 0usize..64
        ) {
            let tmp = write_jsonl(&events);
            // Read the full file, chop the tail, rewrite to a new file.
            let full = std::fs::read(tmp.path()).expect("read file");
            let keep = full.len().saturating_sub(chop);
            let truncated = &full[..keep];

            let tmp2 = NamedTempFile::new().expect("tmp2");
            std::fs::write(tmp2.path(), truncated).expect("write truncated");

            // Reading must not panic. read_all skips malformed lines.
            let full_state = ProgressReader::rebuild_state(tmp.path()).expect("full");
            let trunc_state =
                ProgressReader::rebuild_state(tmp2.path()).expect("truncated");

            // Truncated state can only *miss* entries vs full state for the
            // chopped-off events; it must never invent new feature_ids or
            // contradict the eventual status of entries it does carry that
            // aren't governed by a later (truncated) event.
            //
            // The simplest invariant we can assert: every key in the
            // truncated state is also present in the full state (the full
            // log saw every event the truncated log saw).
            for k in trunc_state.keys() {
                prop_assert!(
                    full_state.contains_key(k),
                    "truncated rebuild introduced feature_id {} not in full rebuild",
                    k
                );
            }
        }
    }
}
