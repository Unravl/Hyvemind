//! Bus-level lifecycle JSONL: `~/.hyvemind/debug/nurse/bus.jsonl.YYYY-MM-DD`.
//!
//! Records SessionSpawned / OwnerChanged / SessionEnded plus capacity-
//! pressure breaches, `RecvError::Lagged(n)` events, post-lag suppression
//! window entries, and dropped events.

use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::writer::{today_yyyy_mm_dd, JsonlWriter, PathResolver};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BusEventKind {
    SessionSpawned,
    SessionEnded,
    OwnerChanged,
    Lag,
    PostLagSuppressionEntered,
    PostLagSuppressionExited,
    CapacityPressure,
    DroppedForUnknownSession,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BusTelemetryRow {
    pub ts: chrono::DateTime<chrono::Utc>,
    pub ts_unix_ms: u64,
    pub kind: BusEventKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub data: serde_json::Value,
}

#[derive(Debug)]
pub struct BusTelemetry {
    writer: JsonlWriter,
    root: PathBuf,
}

impl BusTelemetry {
    pub fn new(root: PathBuf, dropped_counter: Arc<AtomicU64>) -> Self {
        let root_for_resolver = root.clone();
        let resolver: PathResolver = Arc::new(move |_line| {
            root_for_resolver.join(format!("bus.jsonl.{}", today_yyyy_mm_dd()))
        });
        let writer = JsonlWriter::spawn("bus", resolver, dropped_counter);
        Self { writer, root }
    }

    pub fn root(&self) -> &std::path::Path {
        &self.root
    }

    pub fn write(&self, row: BusTelemetryRow) {
        match serde_json::to_value(&row) {
            Ok(v) => self.writer.write(v),
            Err(e) => tracing::warn!(error = %e, "failed to serialize BusTelemetryRow"),
        }
    }

    pub fn shutdown(&self) {
        self.writer.shutdown();
    }
}

pub fn row(
    kind: BusEventKind,
    session_id: Option<String>,
    data: serde_json::Value,
) -> BusTelemetryRow {
    let ts = chrono::Utc::now();
    BusTelemetryRow {
        ts,
        ts_unix_ms: ts.timestamp_millis().max(0) as u64,
        kind,
        session_id,
        data,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn lag_event_logs_dropped_count() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dropped = Arc::new(AtomicU64::new(0));
        let bus = BusTelemetry::new(tmp.path().to_path_buf(), Arc::clone(&dropped));
        bus.write(row(
            BusEventKind::Lag,
            None,
            serde_json::json!({"dropped_count": 7}),
        ));
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let path = tmp.path().join(format!("bus.jsonl.{}", today_yyyy_mm_dd()));
        let txt = std::fs::read_to_string(&path).unwrap();
        assert!(txt.contains("\"dropped_count\":7"));
        bus.shutdown();
    }
}
