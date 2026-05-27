//! Shared non-blocking file writer used by every always-on Nurse log.
//!
//! Each writer holds a bounded `mpsc::channel`. Producers `try_send`; on
//! `Full` the record is dropped and a per-writer counter increments. A
//! dedicated tokio task drains the channel and appends to disk; the engine
//! never blocks on disk I/O.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::tunables;

/// Path-resolver callback. The writer task calls this with the line's
/// JSON value to decide which on-disk file the line lands in (e.g. daily
/// rotation: `decisions.jsonl.YYYY-MM-DD`).
pub type PathResolver = Arc<dyn Fn(&serde_json::Value) -> PathBuf + Send + Sync + 'static>;

/// A non-blocking append-only JSONL writer.
#[derive(Debug)]
pub struct JsonlWriter {
    tx: mpsc::Sender<serde_json::Value>,
    dropped_counter: Arc<AtomicU64>,
    name: &'static str,
    shutdown: CancellationToken,
}

impl JsonlWriter {
    pub fn spawn(
        name: &'static str,
        resolver: PathResolver,
        dropped_counter: Arc<AtomicU64>,
    ) -> Self {
        let depth = tunables::nurse_observability_queue_depth();
        let (tx, rx) = mpsc::channel::<serde_json::Value>(depth);
        let shutdown = CancellationToken::new();
        let shutdown_clone = shutdown.clone();
        let counter_clone = Arc::clone(&dropped_counter);
        tokio::spawn(async move {
            run_writer(name, rx, resolver, counter_clone, shutdown_clone).await;
        });
        Self {
            tx,
            dropped_counter,
            name,
            shutdown,
        }
    }

    pub fn write(&self, line: serde_json::Value) {
        match self.tx.try_send(line) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.dropped_counter.fetch_add(1, Ordering::Relaxed);
                tracing::warn!(
                    writer = %self.name,
                    "nurse observability queue full; dropping newest line"
                );
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                tracing::debug!(
                    writer = %self.name,
                    "nurse observability channel closed; line dropped"
                );
            }
        }
    }

    pub fn shutdown(&self) {
        self.shutdown.cancel();
    }
}

async fn run_writer(
    name: &'static str,
    mut rx: mpsc::Receiver<serde_json::Value>,
    resolver: PathResolver,
    _dropped_counter: Arc<AtomicU64>,
    shutdown: CancellationToken,
) {
    loop {
        tokio::select! {
            biased;
            _ = shutdown.cancelled() => break,
            maybe = rx.recv() => {
                match maybe {
                    Some(line) => {
                        let path = (resolver)(&line);
                        if let Err(e) = append_line(&path, &line).await {
                            tracing::warn!(
                                writer = %name,
                                path = %path.display(),
                                error = %e,
                                "nurse observability append failed"
                            );
                        }
                    }
                    None => break,
                }
            }
        }
    }
}

async fn append_line(path: &std::path::Path, line: &serde_json::Value) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.exists() {
            tokio::fs::create_dir_all(parent).await?;
        }
    }
    let mut bytes = serde_json::to_vec(line).unwrap_or_default();
    bytes.push(b'\n');
    let mut f = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await?;
    f.write_all(&bytes).await?;
    f.flush().await
}

/// Helper: today's `YYYY-MM-DD` string for daily-rotated files.
pub fn today_yyyy_mm_dd() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[tokio::test]
    async fn writer_appends_to_resolved_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("out.jsonl");
        let path_clone = path.clone();
        let resolver: PathResolver = Arc::new(move |_line| path_clone.clone());
        let counter = Arc::new(AtomicU64::new(0));
        let w = JsonlWriter::spawn("test", resolver, counter);
        w.write(serde_json::json!({ "hello": "world" }));
        w.write(serde_json::json!({ "n": 2 }));
        // Let the writer drain.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(contents.lines().count(), 2);
        w.shutdown();
    }
}
