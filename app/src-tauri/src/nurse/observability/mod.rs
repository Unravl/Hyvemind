//! Always-on Nurse observability: per-decision chains, classifier
//! captures, per-session signal streams, and bus-level telemetry.
//!
//! Every surface lives under `~/.hyvemind/debug/nurse/` and is **not**
//! gated on `HYVEMIND_DEBUG=1` — when Nurse misfires in production, the
//! user opens Claude Code and points it at this directory to reconstruct
//! the full causal chain.
//!
//! Every writer is fed via a small bounded `mpsc::channel` and consumed
//! by a dedicated tokio task per file family. Engine code never blocks
//! on disk I/O; queue-full = drop newest + increment
//! `NurseHealth.observability_dropped`.

pub mod bus_telemetry;
pub mod capture;
pub mod decision_log;
pub mod signal_stream;
pub mod writer;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::tunables;

/// Resolve the Nurse observability root, creating it on demand.
/// Returns `~/.hyvemind/debug/nurse/` on Unix; mirrored on Windows by
/// `dirs::home_dir()`.
pub fn nurse_debug_root() -> std::io::Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "home directory not found")
    })?;
    let root = home.join(".hyvemind").join("debug").join("nurse");
    std::fs::create_dir_all(&root)?;
    std::fs::create_dir_all(root.join("captures"))?;
    std::fs::create_dir_all(root.join("signals"))?;
    Ok(root)
}

/// Roll-up handle held by `NurseEngine` so callers can fan out a single
/// event to every always-on writer with one method call.
pub struct ObservabilityHandles {
    pub root: PathBuf,
    pub decisions: Arc<decision_log::DecisionLogger>,
    pub captures: Arc<capture::ClassifierCapture>,
    pub signals: Arc<signal_stream::SignalStream>,
    pub bus: Arc<bus_telemetry::BusTelemetry>,
    pub dropped_counter: Arc<std::sync::atomic::AtomicU64>,
}

impl ObservabilityHandles {
    pub fn new() -> std::io::Result<Self> {
        let root = nurse_debug_root()?;
        let dropped_counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let decisions = Arc::new(decision_log::DecisionLogger::new(
            root.clone(),
            Arc::clone(&dropped_counter),
        ));
        let captures = Arc::new(capture::ClassifierCapture::new(root.clone()));
        let signals = Arc::new(signal_stream::SignalStream::new(
            root.clone(),
            Arc::clone(&dropped_counter),
        ));
        let bus = Arc::new(bus_telemetry::BusTelemetry::new(
            root.clone(),
            Arc::clone(&dropped_counter),
        ));
        Ok(Self {
            root,
            decisions,
            captures,
            signals,
            bus,
            dropped_counter,
        })
    }

    /// Prune debug files older than the configured retention windows.
    /// Runs synchronously at startup so the listing is small and the work
    /// is bounded. Returns `(decisions_pruned, captures_pruned, bus_pruned)`.
    pub fn prune_on_startup(&self) -> std::io::Result<(usize, usize, usize)> {
        let decisions_age = std::time::Duration::from_secs(
            tunables::nurse_decision_log_retention_days() * 24 * 60 * 60,
        );
        let bus_age =
            std::time::Duration::from_secs(tunables::nurse_bus_log_retention_days() * 24 * 60 * 60);
        Ok((
            prune_older_than(&self.root, "decisions.jsonl.", decisions_age)?,
            prune_older_than(&self.root.join("captures"), "", decisions_age)?,
            prune_older_than(&self.root, "bus.jsonl.", bus_age)?,
        ))
    }
}

fn prune_older_than(
    dir: &Path,
    name_prefix: &str,
    max_age: std::time::Duration,
) -> std::io::Result<usize> {
    let mut pruned = 0usize;
    if !dir.exists() {
        return Ok(0);
    }
    let now = std::time::SystemTime::now();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if !name.starts_with(name_prefix) {
                continue;
            }
        }
        let meta = entry.metadata()?;
        if !meta.is_file() {
            continue;
        }
        if let Ok(mtime) = meta.modified() {
            if let Ok(age) = now.duration_since(mtime) {
                if age > max_age {
                    if std::fs::remove_file(&path).is_ok() {
                        pruned += 1;
                    }
                }
            }
        }
    }
    Ok(pruned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observability_handles_construct_creates_directories() {
        // Use a scoped temp home for isolation. We can't easily redirect
        // `dirs::home_dir()`, so this test just exercises the directory
        // creation helper instead.
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().join("nurse");
        std::fs::create_dir_all(&root.join("captures")).unwrap();
        std::fs::create_dir_all(&root.join("signals")).unwrap();
        assert!(root.exists());
    }

    #[test]
    fn prune_older_than_respects_age() {
        let tmp = tempfile::TempDir::new().unwrap();
        let f = tmp.path().join("decisions.jsonl.2020-01-01");
        std::fs::write(&f, "x").unwrap();
        // Brand-new file → not pruned with a 30-day window.
        let pruned = prune_older_than(
            tmp.path(),
            "decisions.jsonl.",
            std::time::Duration::from_secs(30 * 24 * 3600),
        )
        .unwrap();
        assert_eq!(pruned, 0);
        assert!(f.exists());
    }
}
