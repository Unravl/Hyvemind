use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::Utc;
use serde::Serialize;
use tokio::sync::Mutex;

use crate::state::log_redact::RedactingWriter;

#[derive(Debug, Serialize)]
struct ReviewLogEvent {
    timestamp: String,
    review_id: String,
    event: String,
    data: serde_json::Value,
}

pub struct ReviewLogger {
    writer: Mutex<RedactingWriter<std::io::BufWriter<std::fs::File>>>,
    review_id: String,
    pub path: PathBuf,
}

impl ReviewLogger {
    pub async fn new(reviews_dir: &Path, review_id: &str) -> Result<Self> {
        tokio::fs::create_dir_all(reviews_dir).await?;
        let path = reviews_dir.join(format!("{}.jsonl", review_id));
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("failed to open review log {}", path.display()))?;
        Ok(Self {
            writer: Mutex::new(RedactingWriter::new(std::io::BufWriter::new(file))),
            review_id: review_id.to_string(),
            path,
        })
    }

    pub async fn log(&self, event: &str, data: serde_json::Value) {
        let entry = ReviewLogEvent {
            timestamp: Utc::now().to_rfc3339(),
            review_id: self.review_id.clone(),
            event: event.to_string(),
            data,
        };
        if let Ok(mut line) = serde_json::to_string(&entry) {
            line.push('\n');
            let mut w = self.writer.lock().await;
            let _ = w.write_all(line.as_bytes());
            let _ = w.flush();
        }
    }
}

/// Returns Some(logger) only when HYVEMIND_DEBUG=1
pub async fn create_if_debug(reviews_dir: &Path, review_id: &str) -> Option<Arc<ReviewLogger>> {
    if std::env::var("HYVEMIND_DEBUG").as_deref() != Ok("1") {
        return None;
    }
    ReviewLogger::new(reviews_dir, review_id)
        .await
        .ok()
        .map(Arc::new)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn review_log_redacts_anthropic_keys_in_output() {
        let dir = tempfile::tempdir().unwrap();
        let logger = ReviewLogger::new(dir.path(), "test-review-redact")
            .await
            .unwrap();

        let mut data = serde_json::Map::new();
        data.insert(
            "error".to_string(),
            serde_json::Value::String(
                "API call failed: error body contained sk-ant-abc123def456xyz".to_string(),
            ),
        );

        logger
            .log("model_call_failed", serde_json::Value::Object(data))
            .await;

        // Drop logger to flush
        std::mem::drop(logger);

        let path = dir.path().join("test-review-redact.jsonl");
        let content = std::fs::read_to_string(&path).unwrap();

        assert!(
            content.contains("***REDACTED***") || content.contains("[redacted-api-key]"),
            "expected redacted marker in output: {}",
            content
        );
        assert!(
            !content.contains("sk-ant-abc123def456xyz"),
            "secret key leaked into review log: {}",
            content
        );

        // Verify that non-secret content is preserved
        assert!(
            content.contains("API call failed"),
            "context lost: {}",
            content
        );
    }
}
