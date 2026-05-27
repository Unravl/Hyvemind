//! Per-session signal raise/clear JSONL stream.
//!
//! Path: `~/.hyvemind/debug/nurse/signals/{session_id}.jsonl`. Rotates to
//! `.1` / `.2` / `.3` when exceeding `HYVEMIND_NURSE_SIGNAL_STREAM_MAX_BYTES`
//! (default 4 MiB); only the three most recent files are retained per
//! session. Files are pruned 24h after the engine receives `SessionEnded`.

use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::writer::{JsonlWriter, PathResolver};
use crate::tunables;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalStreamKind {
    Raise,
    Clear,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalStreamRow {
    pub ts: chrono::DateTime<chrono::Utc>,
    pub ts_unix_ms: u64,
    pub session_id: String,
    pub kind: SignalStreamKind,
    pub detector: String,
    pub severity: crate::nurse::health::Severity,
    pub dedup_key: String,
    pub summary: String,
    pub evidence: serde_json::Value,
    pub session_tier_after: crate::nurse::health::Tier,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_decision_id: Option<String>,
}

#[derive(Debug)]
pub struct SignalStream {
    writer: JsonlWriter,
    root: PathBuf,
}

impl SignalStream {
    pub fn new(root: PathBuf, dropped_counter: Arc<AtomicU64>) -> Self {
        let signals_root = root.join("signals");
        let _ = std::fs::create_dir_all(&signals_root);
        let signals_root_for_resolver = signals_root.clone();
        let resolver: PathResolver = Arc::new(move |line| {
            let sid = line
                .get("session_id")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let safe_sid: String = sid
                .chars()
                .map(|c| {
                    if c.is_alphanumeric() || c == '-' || c == '_' {
                        c
                    } else {
                        '_'
                    }
                })
                .collect();
            let primary = signals_root_for_resolver.join(format!("{}.jsonl", safe_sid));
            maybe_rotate(&primary).unwrap_or(primary)
        });
        let writer = JsonlWriter::spawn("signals", resolver, dropped_counter);
        Self { writer, root }
    }

    pub fn root(&self) -> &std::path::Path {
        &self.root
    }

    pub fn write(&self, row: SignalStreamRow) {
        match serde_json::to_value(&row) {
            Ok(v) => self.writer.write(v),
            Err(e) => tracing::warn!(error = %e, "failed to serialize SignalStreamRow"),
        }
    }

    pub fn shutdown(&self) {
        self.writer.shutdown();
    }
}

/// Best-effort rotation: if `primary` is past the cap, shift `.2 → .3`,
/// `.1 → .2`, `primary → .1`, and return `primary` (which the writer
/// will then create fresh).
fn maybe_rotate(primary: &std::path::Path) -> std::io::Result<PathBuf> {
    let cap = tunables::nurse_signal_stream_max_bytes();
    let meta = std::fs::metadata(primary);
    let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
    if size < cap {
        return Ok(primary.to_path_buf());
    }
    let p1 = primary.with_extension("jsonl.1");
    let p2 = primary.with_extension("jsonl.2");
    let p3 = primary.with_extension("jsonl.3");
    let _ = std::fs::remove_file(&p3);
    if p2.exists() {
        let _ = std::fs::rename(&p2, &p3);
    }
    if p1.exists() {
        let _ = std::fs::rename(&p1, &p2);
    }
    let _ = std::fs::rename(primary, &p1);
    Ok(primary.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nurse::health::{Severity, Tier};

    #[tokio::test]
    async fn write_creates_per_session_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dropped = Arc::new(AtomicU64::new(0));
        let s = SignalStream::new(tmp.path().to_path_buf(), Arc::clone(&dropped));
        let ts = chrono::Utc::now();
        s.write(SignalStreamRow {
            ts,
            ts_unix_ms: ts.timestamp_millis().max(0) as u64,
            session_id: "sess-abc".into(),
            kind: SignalStreamKind::Raise,
            detector: "stall".into(),
            severity: Severity::Warn,
            dedup_key: "stall".into(),
            summary: "idle".into(),
            evidence: serde_json::json!({"idle_ms": 60_000}),
            session_tier_after: Tier::Warning,
            active_decision_id: None,
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let path = tmp.path().join("signals").join("sess-abc.jsonl");
        assert!(path.exists());
        let txt = std::fs::read_to_string(&path).unwrap();
        assert!(txt.contains("\"detector\":\"stall\""));
        s.shutdown();
    }
}
