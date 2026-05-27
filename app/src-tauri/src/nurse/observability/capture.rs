//! Tier-3 classifier prompt / response captures.
//!
//! Each Tier-3 LLM call writes its full prompt to
//! `~/.hyvemind/debug/nurse/captures/{decision_id}-prompt.txt` before
//! the call returns, and its full response to `…-response.txt` after.
//! A crash mid-call leaves the prompt visible with no response — an
//! unambiguous post-hoc signal that "the classifier was invoked but
//! never returned".

use std::path::PathBuf;

use tokio::io::AsyncWriteExt;

use crate::tunables;

#[derive(Debug)]
pub struct ClassifierCapture {
    root: PathBuf,
}

impl ClassifierCapture {
    pub fn new(root: PathBuf) -> Self {
        let captures = root.join("captures");
        if !captures.exists() {
            let _ = std::fs::create_dir_all(&captures);
        }
        Self { root }
    }

    /// Read-only accessor for the Nurse observability root (parent of
    /// `captures/`). Used by the IPC layer to compose post-hoc reads.
    pub fn root(&self) -> &std::path::Path {
        &self.root
    }

    pub fn prompt_path(&self, decision_id: &str) -> PathBuf {
        self.root
            .join("captures")
            .join(format!("{}-prompt.txt", decision_id))
    }

    pub fn response_path(&self, decision_id: &str) -> PathBuf {
        self.root
            .join("captures")
            .join(format!("{}-response.txt", decision_id))
    }

    /// Synchronous write — used before the provider call returns so the
    /// file is on disk by the time `classifier_invoked` is logged.
    pub fn write_prompt_sync(&self, decision_id: &str, body: &str) -> std::io::Result<PathBuf> {
        let path = self.prompt_path(decision_id);
        let truncated = truncate_for_capture(body);
        std::fs::write(&path, truncated.as_bytes())?;
        Ok(path)
    }

    /// Async write — used after the provider call returns. Async because
    /// the response body can be large (many KB) and the engine is already
    /// off the hot path here.
    pub async fn write_response(&self, decision_id: &str, body: &str) -> std::io::Result<PathBuf> {
        let path = self.response_path(decision_id);
        let truncated = truncate_for_capture(body);
        let mut f = tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)
            .await?;
        f.write_all(truncated.as_bytes()).await?;
        f.flush().await?;
        Ok(path)
    }

    /// Resolve a relative path under `~/.hyvemind/` for log embedding.
    pub fn relative(&self, path: &std::path::Path) -> PathBuf {
        let home = dirs::home_dir().unwrap_or_default();
        match path.strip_prefix(home.join(".hyvemind")) {
            Ok(p) => p.to_path_buf(),
            Err(_) => path.to_path_buf(),
        }
    }
}

fn truncate_for_capture(body: &str) -> std::borrow::Cow<'_, str> {
    let max = tunables::nurse_capture_max_bytes() as usize;
    if body.len() <= max {
        std::borrow::Cow::Borrowed(body)
    } else {
        let mut end = max;
        while end > 0 && !body.is_char_boundary(end) {
            end -= 1;
        }
        let dropped = body.len() - end;
        std::borrow::Cow::Owned(format!(
            "{}\n--- [truncated, {} bytes dropped] ---\n",
            &body[..end],
            dropped
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn prompt_write_and_truncation() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cap = ClassifierCapture::new(tmp.path().to_path_buf());
        let path = cap.write_prompt_sync("dec1", "hello world").unwrap();
        assert!(path.exists());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello world");
    }

    #[tokio::test]
    async fn response_write_truncates_past_cap() {
        std::env::set_var("HYVEMIND_NURSE_CAPTURE_MAX_BYTES", "32768");
        let tmp = tempfile::TempDir::new().unwrap();
        let cap = ClassifierCapture::new(tmp.path().to_path_buf());
        let big = "x".repeat(100_000);
        let path = cap.write_response("decBig", &big).await.unwrap();
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(
            on_disk.contains("[truncated,"),
            "response not truncated: len = {}",
            on_disk.len()
        );
        std::env::remove_var("HYVEMIND_NURSE_CAPTURE_MAX_BYTES");
    }
}
